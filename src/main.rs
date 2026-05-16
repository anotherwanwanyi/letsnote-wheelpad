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
use letsnote_wheelpad::evdev::{history_capacity_for_rate, InputDevice};
use letsnote_wheelpad::fsm::{Action, Fsm};
use letsnote_wheelpad::grab::Grabber;
use letsnote_wheelpad::uinput::UinputDevice;

/// 5 second watchdog — if the FSM has been Scrolling without consuming a
/// packet for this long the grab is forcibly released and we drop back
/// to Idle. linux-design §14 risk 13.
const SCROLLING_WATCHDOG: Duration = Duration::from_secs(5);

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

    // 2. Create the virtual wheel device early — any failure here
    //    happens before we start grabbing.
    let mut uinput = UinputDevice::create()?;
    info!("uinput device created");

    // 3. Notify systemd we're ready. After this point dependent units
    //    can start.
    if let Err(e) = sd_notify_ready() {
        // Not fatal — Type=simple users will hit this path harmlessly.
        warn!("sd_notify Ready failed (acceptable outside systemd): {e}");
    }

    // 4. Measure the evdev packet rate over the first second and pick
    //    the history capacity (D-021). If measurement times out (no
    //    motion within the window) we fall back to the Windows default.
    let measured = input.measure_rate(50, Duration::from_secs(1));
    let capacity = match measured {
        Some(hz) => {
            let cap = history_capacity_for_rate(hz);
            info!(
                rate_hz = format!("{:.1}", hz),
                history_capacity = cap,
                "evdev rate measured"
            );
            cap
        }
        None => {
            info!("evdev rate measurement timed out; using Windows default (20)");
            20
        }
    };

    // 5. Build the algorithm and FSM.
    let mut detector = CircularDetector::new(capacity);
    let mut fsm = Fsm::new(input.center_x, input.center_y);
    let mut grabber = Grabber::new();

    // 6. Signal handling — SIGTERM / SIGINT set the static flag; the
    //    main loop checks it after each iteration.
    install_signal_handlers()?;

    // 7. Main loop.
    let raw_fd = input.device.as_raw_fd();
    let mut last_packet_during_scrolling: Option<Instant> = None;

    while !STOP.load(Ordering::Relaxed) {
        let timeout_ms: i32 = if grabber.is_active() {
            // While scrolling, cap the wait so the watchdog can fire.
            SCROLLING_WATCHDOG.as_millis() as i32
        } else {
            -1
        };
        // SAFETY: raw_fd is owned by `input.device` which outlives this
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
            if grabber.is_active() {
                warn!("scrolling watchdog fired — releasing grab");
                let _ = grabber.release(&mut input.device);
                let _ = fsm.force_release();
                last_packet_during_scrolling = None;
            }
            continue;
        }

        match input.next_frame() {
            Ok(Some(frame)) => {
                let actions = fsm.step(frame, &mut detector, &config.scroll);
                for action in actions.iter().copied() {
                    match action {
                        Action::None => {}
                        Action::GrabPhysical => {
                            if let Err(e) = grabber.acquire(&mut input.device) {
                                warn!("failed to grab physical pad: {e}");
                            } else {
                                debug!("grab acquired");
                            }
                            last_packet_during_scrolling = Some(Instant::now());
                        }
                        Action::ReleasePhysical => {
                            if let Err(e) = grabber.release(&mut input.device) {
                                warn!("failed to release grab: {e}");
                            } else {
                                debug!("grab released");
                            }
                            last_packet_during_scrolling = None;
                        }
                        Action::EmitWheelV(t) => {
                            if let Err(e) = uinput.emit_v(t) {
                                warn!("uinput emit_v failed: {e}");
                            }
                            debug!(ticks = t, "emit vertical");
                        }
                        Action::EmitWheelH(t) => {
                            if let Err(e) = uinput.emit_h(t) {
                                warn!("uinput emit_h failed: {e}");
                            }
                            debug!(ticks = t, "emit horizontal");
                        }
                    }
                }
                if grabber.is_active() {
                    last_packet_during_scrolling = Some(Instant::now());
                }
            }
            Ok(None) => {
                // No SYN_REPORT in this fetch batch — nothing to do.
            }
            Err(e) => {
                warn!("evdev read error: {e}");
                // On hot-unplug evdev returns ENODEV here. v1 behaviour
                // (linux-design.md §14 risk 9): log and exit so systemd
                // restarts us. A future enhancement could re-enumerate.
                break;
            }
        }

        if let Some(t) = last_packet_during_scrolling {
            if grabber.is_active() && t.elapsed() > SCROLLING_WATCHDOG {
                warn!("scrolling watchdog fired (post-frame) — releasing grab");
                let _ = grabber.release(&mut input.device);
                let _ = fsm.force_release();
                last_packet_during_scrolling = None;
            }
        }
    }

    info!("shutting down");
    let _ = grabber.release(&mut input.device);
    Ok(())
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
