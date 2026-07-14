use std::os::fd::{AsRawFd, BorrowedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clap::Parser;
use nix::poll::{poll, PollFd, PollFlags};
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

use letsnote_wheelpad::config::Config;
use letsnote_wheelpad::detector::CircularDetector;
use letsnote_wheelpad::error::{Error, Result};
use letsnote_wheelpad::evdev::InputDevice;
use letsnote_wheelpad::fsm::{Action, Fsm, FsmState};
use letsnote_wheelpad::uinput::{UinputTouchpad, UinputWheel};

/// 5 second watchdog — if the FSM has been Scrolling without consuming a
/// packet for this long, force back to Idle so passthrough resumes and
/// the cursor unfreezes. linux-design §14 risk 13.
const SCROLLING_WATCHDOG: Duration = Duration::from_secs(5);

/// Bound raw-frame buffering when an outer-ring contact produces many
/// reports but too little meaningful motion for the intent classifier.
/// Normal moving gestures resolve much earlier (after 3–20 samples).
const MAX_PENDING_TOUCHPAD_FRAMES: usize = 64;

static STOP: AtomicBool = AtomicBool::new(false);

#[derive(Parser, Debug)]
#[command(
    name = "letsnote-wheelpad",
    version,
    about = "Userland daemon: Panasonic Let's Note WheelPad circular touchpad scroll on Linux"
)]
struct Args {
    /// Path to config file. Defaults to $XDG_CONFIG_HOME/letsnote-wheelpad/config.toml.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Override the evdev device path (e.g. /dev/input/event4). Bypasses
    /// the device_name_regex search.
    #[arg(long)]
    device: Option<PathBuf>,

    /// Increase logging verbosity to DEBUG.
    #[arg(long)]
    debug: bool,
}

fn main() {
    let args = Args::parse();
    if let Err(e) = run(args) {
        eprintln!("letsnote-wheelpad: {e}");
        std::process::exit(1);
    }
}

fn run(args: Args) -> Result<()> {
    let config_path = args.config.unwrap_or_else(Config::default_path);
    let config = Config::load(&config_path)?;

    init_tracing(&config, args.debug);

    info!(?config_path, "config loaded");
    debug!(?config, "effective config");

    // 1. Open the physical touchpad.
    let device_path = match args.device.or_else(|| config.device.clone()) {
        Some(p) => p,
        None => InputDevice::find_by_name(&config.device_name_regex)?,
    };
    info!(path = %device_path.display(), "opening touchpad");
    let mut input = InputDevice::open(&device_path)?;
    info!(
        x_range = %format!("[{}, {}]", input.abs_x_min, input.abs_x_max),
        y_range = %format!("[{}, {}]", input.abs_y_min, input.abs_y_max),
        center = %format!("({}, {})", input.center_x, input.center_y),
        "touchpad ranges queried"
    );

    // 2. Construct the virtual touchpad BEFORE we grab the physical
    //    pad — it has to read the physical pad's capabilities, and we
    //    want any uinput-creation failure to happen before libinput
    //    loses access. If uinput device creation fails (e.g., missing
    //    kernel module) we exit cleanly without a grab held.
    let mut vtouchpad = UinputTouchpad::create_from_physical(&input.device)?;
    info!("virtual touchpad created");

    // 3. Construct the virtual wheel — same lifecycle considerations.
    let mut vwheel = UinputWheel::create()?;
    info!("virtual wheel created");

    // 4. Grab the physical pad permanently. After this point libinput
    //    sees no events from the physical device; everything flows
    //    through our virtual touchpad. Releasing the grab is handled
    //    by `Drop` on `input.device` (and by the panic-safety cleanup
    //    after the main loop returns).
    input
        .device
        .grab()
        .map_err(|source| Error::Grab { source })?;
    info!("physical touchpad grabbed (passthrough mode)");

    // 5. Notify systemd we're ready.
    if let Err(e) = sd_notify_ready() {
        warn!("sd_notify Ready failed (acceptable outside systemd): {e}");
    }

    // 6. Build the algorithm and FSM. History capacity is fixed at 20
    //    to match Windows WheelPad exactly (D-021-followup).
    let mut detector = CircularDetector::new();
    let mut fsm = Fsm::new(input.center_x, input.center_y);

    // 7. Signal handling.
    install_signal_handlers()?;

    // 8. Main loop.
    let raw_fd = input.device.as_raw_fd();
    let mut last_packet_at = Instant::now();
    // Outer-ring single-finger frames are held here while the FSM decides
    // whether they are circular intent or ordinary pointer/multi-touch.
    // Each entry remains one SYN_REPORT-bounded physical frame.
    let mut pending_touchpad_frames = Vec::new();

    while !STOP.load(Ordering::Relaxed) {
        // While scrolling, cap the wait so the watchdog can fire when
        // packet flow truly stalls (not when a long deliberate scroll
        // is in progress — last_packet_at is reset on every frame).
        let timeout_ms: i32 = if fsm.is_scrolling() {
            let remaining = SCROLLING_WATCHDOG.saturating_sub(last_packet_at.elapsed());
            remaining.as_millis() as i32
        } else {
            -1
        };
        // SAFETY: raw_fd is owned by `input.device` which outlives the
        // borrow for the iteration; we never read/write through the
        // BorrowedFd ourselves.
        let borrowed = unsafe { BorrowedFd::borrow_raw(raw_fd) };
        let mut fds = [PollFd::new(&borrowed, PollFlags::POLLIN)];
        let n = match poll(&mut fds, timeout_ms) {
            Ok(n) => n,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => {
                error!("poll error: {e}");
                break;
            }
        };

        if n == 0 {
            // Timeout. Watchdog fires only if we've been stuck in
            // Scrolling without any packet for the full window.
            if fsm.is_scrolling() && last_packet_at.elapsed() >= SCROLLING_WATCHDOG {
                warn!("scrolling watchdog fired — forcing idle, resuming passthrough");
                fsm.force_idle(&mut detector);
            }
            continue;
        }

        let frames = match input.poll_frames() {
            Ok(fs) => fs,
            Err(e) => {
                warn!("evdev read error: {e}");
                // On hot-unplug evdev returns ENODEV. Log and exit so
                // systemd restarts us. A future enhancement could
                // re-enumerate the device.
                break;
            }
        };

        if frames.is_empty() {
            // No SYN_REPORT in this fetch (events were only partial).
            continue;
        }

        // Got at least one packet; reset the watchdog.
        last_packet_at = Instant::now();

        for pf in frames {
            // Snapshot pre-step state. Pending circular intent buffers
            // complete physical frames; capture discards them, while a
            // pointer/multi-touch decision replays them in original order.
            let prev_state = fsm.state();
            let action = fsm.step(&pf.frame, &mut detector, &config.scroll);
            let now_state = fsm.state();

            match action {
                Action::None => {}
                Action::EmitWheelV(t) => {
                    if let Err(e) = vwheel.emit_v(t) {
                        warn!("uinput emit_v failed: {e}");
                    }
                    debug!(ticks = t, "emit vertical");
                }
                Action::EmitWheelH(t) => {
                    if let Err(e) = vwheel.emit_h(t) {
                        warn!("uinput emit_h failed: {e}");
                    }
                    debug!(ticks = t, "emit horizontal");
                }
            }

            match forwarding_decision(prev_state, now_state) {
                ForwardingDecision::Hold => {
                    pending_touchpad_frames.push(pf.events);
                    if pending_touchpad_frames.len() >= MAX_PENDING_TOUCHPAD_FRAMES {
                        let cancelled = fsm.cancel_pending();
                        debug_assert!(cancelled);
                        debug!(
                            frames = pending_touchpad_frames.len(),
                            "pending intent buffer limit reached; choosing pointer passthrough"
                        );
                        for events in pending_touchpad_frames.drain(..) {
                            if let Err(e) = vtouchpad.forward(&events, false) {
                                warn!("virtual touchpad buffered forward failed: {e}");
                            }
                        }
                    }
                }
                ForwardingDecision::FlushHeld => {
                    pending_touchpad_frames.push(pf.events);
                    for events in pending_touchpad_frames.drain(..) {
                        if let Err(e) = vtouchpad.forward(&events, false) {
                            warn!("virtual touchpad buffered forward failed: {e}");
                        }
                    }
                }
                ForwardingDecision::DiscardHeld => pending_touchpad_frames.clear(),
                ForwardingDecision::Suppress => {}
                ForwardingDecision::Forward => {
                    debug_assert!(pending_touchpad_frames.is_empty());
                    if let Err(e) = vtouchpad.forward(&pf.events, false) {
                        warn!("virtual touchpad forward failed: {e}");
                    }
                }
            }
        }
    }

    info!("shutting down");
    // `Drop for InputDevice` releases EVIOCGRAB.
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ForwardingDecision {
    Forward,
    Hold,
    FlushHeld,
    DiscardHeld,
    Suppress,
}

fn forwarding_decision(previous: FsmState, current: FsmState) -> ForwardingDecision {
    let previous_pending = matches!(previous, FsmState::Moving { .. });
    let current_pending = matches!(current, FsmState::Moving { .. });
    let previous_scrolling = matches!(previous, FsmState::Scrolling { .. });
    let current_scrolling = matches!(current, FsmState::Scrolling { .. });

    if previous_pending && current_scrolling {
        ForwardingDecision::DiscardHeld
    } else if previous_scrolling || current_scrolling {
        ForwardingDecision::Suppress
    } else if current_pending {
        ForwardingDecision::Hold
    } else if previous_pending {
        ForwardingDecision::FlushHeld
    } else {
        ForwardingDecision::Forward
    }
}

fn init_tracing(config: &Config, debug_flag: bool) {
    let level = if debug_flag {
        "debug".to_string()
    } else {
        config.log.level.clone()
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("letsnote_wheelpad={level}")));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}

fn sd_notify_ready() -> std::io::Result<()> {
    use libsystemd::daemon::{notify, NotifyState};
    notify(false, &[NotifyState::Ready])
        .map(|_| ())
        .map_err(|e| std::io::Error::other(e.to_string()))
}

fn install_signal_handlers() -> Result<()> {
    use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};

    extern "C" fn handler(_sig: libc::c_int) {
        STOP.store(true, Ordering::Relaxed);
    }

    let action = SigAction::new(
        SigHandler::Handler(handler),
        SaFlags::empty(),
        SigSet::empty(),
    );
    unsafe {
        sigaction(Signal::SIGTERM, &action).map_err(|source| Error::Signal { source })?;
        sigaction(Signal::SIGINT, &action).map_err(|source| Error::Signal { source })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PENDING: FsmState = FsmState::Moving {
        tracking_id: 10,
        slot: 0,
    };
    const SCROLLING: FsmState = FsmState::Scrolling {
        tracking_id: 10,
        slot: 0,
    };

    #[test]
    fn pending_frames_are_held_then_discarded_on_capture() {
        assert_eq!(
            forwarding_decision(FsmState::Idle, PENDING),
            ForwardingDecision::Hold
        );
        assert_eq!(
            forwarding_decision(PENDING, PENDING),
            ForwardingDecision::Hold
        );
        assert_eq!(
            forwarding_decision(PENDING, SCROLLING),
            ForwardingDecision::DiscardHeld
        );
        assert_eq!(
            forwarding_decision(SCROLLING, FsmState::Debounce),
            ForwardingDecision::Suppress
        );
    }

    #[test]
    fn pending_frames_are_flushed_when_passthrough_wins() {
        assert_eq!(
            forwarding_decision(PENDING, FsmState::Passthrough),
            ForwardingDecision::FlushHeld
        );
        assert_eq!(
            forwarding_decision(PENDING, FsmState::MultiTouch),
            ForwardingDecision::FlushHeld
        );
        assert_eq!(
            forwarding_decision(PENDING, FsmState::Idle),
            ForwardingDecision::FlushHeld
        );
        assert_eq!(
            forwarding_decision(FsmState::Passthrough, FsmState::Passthrough),
            ForwardingDecision::Forward
        );
    }
}
