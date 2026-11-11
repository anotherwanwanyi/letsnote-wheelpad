// Physical touchpad input — opens the evdev node, queries EVIOCGABS for
// center coordinates, and turns the raw event stream into TouchFrames at
// each SYN_REPORT.

use std::path::{Path, PathBuf};

use evdev::{AbsoluteAxisType, BusType, Device, EventType, InputEvent, Key};

use crate::detector::TouchSample;
use crate::error::{Error, Result};
use crate::fsm::{TouchFrame, TrackedTouch};

/// Maximum number of MT slots we track. The kernel exposes up to 10 in
/// practice; touchpads typically advertise 5. We sweep all slots when
/// rescanning for "lowest active" semantics, so the constant
/// only bounds the per-frame map size, not gesture logic.
const MAX_MT_SLOTS: usize = 16;

pub struct InputDevice {
    pub device: Device,
    pub abs_x_min: i32,
    pub abs_x_max: i32,
    pub abs_y_min: i32,
    pub abs_y_max: i32,
    pub center_x: i32,
    pub center_y: i32,
    pub radius: i32,
    assembler: FrameAssembler,
}

#[derive(Debug)]
struct FrameAssembler {
    /// Per-slot tracking IDs and last-seen (x, y). Slot index 0..MAX_MT_SLOTS-1.
    slots: [SlotState; MAX_MT_SLOTS],
    /// Current writing slot per ABS_MT_SLOT event.
    current_slot: usize,
    /// BTN_TOUCH summary; mirrors the kernel-reported any-finger-down state.
    contact: bool,
    /// Trailing events not yet terminated by SYN_REPORT.
    pending_events: Vec<InputEvent>,
}

#[derive(Clone, Copy, Debug)]
struct SlotState {
    /// `-1` means inactive (kernel convention).
    tracking_id: i32,
    x: i32,
    y: i32,
}

/// One SYN_REPORT-bounded batch of physical events plus the
/// high-level frame assembled from them.
pub struct PhysicalFrame {
    pub frame: TouchFrame,
    /// All events from the underlying fetch, in original order.
    /// Forwarded verbatim to the virtual touchpad (minus the trailing
    /// SYN_REPORT, which `emit()` re-inserts).
    pub events: Vec<InputEvent>,
}

impl Drop for InputDevice {
    /// Panic-safe ungrab. The kernel releases EVIOCGRAB on FD close
    /// regardless, but explicitly calling ungrab here means the
    /// release happens deterministically during unwind, before the
    /// FD's file struct is reaped — closing the race where a daemon
    /// restart tries to re-grab before the kernel has finalized the
    /// old FD.
    fn drop(&mut self) {
        let _ = self.device.ungrab();
    }
}

impl InputDevice {
    /// Open the device at `path` and validate required capabilities.
    pub fn open(path: &Path) -> Result<Self> {
        let device = Device::open(path).map_err(|source| Error::EvdevOpen {
            path: path.to_path_buf(),
            source,
        })?;

        // Required capabilities.
        let abs = device.supported_absolute_axes();
        let keys = device.supported_keys();
        let has_x = abs.is_some_and(|a| a.contains(AbsoluteAxisType::ABS_MT_POSITION_X));
        let has_y = abs.is_some_and(|a| a.contains(AbsoluteAxisType::ABS_MT_POSITION_Y));
        let has_touch = keys.is_some_and(|k| k.contains(Key::BTN_TOUCH));
        if !has_x {
            return Err(Error::EvdevMissingCap {
                path: path.to_path_buf(),
                capability: "ABS_MT_POSITION_X",
            });
        }
        if !has_y {
            return Err(Error::EvdevMissingCap {
                path: path.to_path_buf(),
                capability: "ABS_MT_POSITION_Y",
            });
        }
        if !has_touch {
            return Err(Error::EvdevMissingCap {
                path: path.to_path_buf(),
                capability: "BTN_TOUCH",
            });
        }

        let abs_state = device
            .get_abs_state()
            .map_err(|source| Error::EvdevRead { source })?;
        let xi = abs_state[AbsoluteAxisType::ABS_MT_POSITION_X.0 as usize];
        let yi = abs_state[AbsoluteAxisType::ABS_MT_POSITION_Y.0 as usize];
        let abs_x_min = xi.minimum;
        let abs_x_max = xi.maximum;
        let abs_y_min = yi.minimum;
        let abs_y_max = yi.maximum;
        let center_x = (abs_x_min + abs_x_max) / 2;
        let center_y = (abs_y_min + abs_y_max) / 2;
        let radius = ((abs_x_max - abs_x_min).min(abs_y_max - abs_y_min) / 2).max(1);

        let has_slot = abs.is_some_and(|a| a.contains(AbsoluteAxisType::ABS_MT_SLOT));
        let current_slot = if has_slot {
            abs_state[AbsoluteAxisType::ABS_MT_SLOT.0 as usize]
                .value
                .max(0) as usize
        } else {
            0
        };
        let contact = device
            .get_key_state()
            .is_ok_and(|k| k.contains(Key::BTN_TOUCH));

        Ok(Self {
            device,
            abs_x_min,
            abs_x_max,
            abs_y_min,
            abs_y_max,
            center_x,
            center_y,
            radius,
            assembler: FrameAssembler::new(current_slot, contact),
        })
    }

    /// Find a physical touchpad whose name matches `regex`. Skips virtual
    /// uinput devices (such as our own mirrored touchpad) and companion
    /// event nodes that lack multitouch touchpad capabilities.
    pub fn find_by_name(regex_str: &str) -> Result<PathBuf> {
        let re = regex::Regex::new(regex_str).map_err(|source| Error::RegexInvalid {
            pattern: regex_str.to_string(),
            source,
        })?;
        for (path, device) in evdev::enumerate() {
            if device.input_id().bus_type() == BusType::BUS_VIRTUAL {
                continue;
            }
            if !has_touchpad_capabilities(&device) {
                continue;
            }
            if let Some(name) = device.name() {
                if re.is_match(name) {
                    return Ok(path);
                }
            }
        }
        Err(Error::DeviceNotFound {
            regex: regex_str.to_string(),
        })
    }

    /// Query the kernel for whether `BTN_TOUCH` is still physically pressed.
    /// If no finger is down in the kernel, clears any stale internal slot
    /// and contact state.
    pub fn verify_active_contact(&mut self) -> bool {
        let still_down = self
            .device
            .get_key_state()
            .is_ok_and(|k| k.contains(Key::BTN_TOUCH));
        if !still_down {
            self.assembler.clear_contact_state();
        }
        still_down
    }

    /// Block until events are available, then return ALL complete
    /// SYN_REPORT-bounded frames from this fetch. If the daemon was
    /// briefly descheduled and the kernel has buffered multiple
    /// frames, every one is returned so the caller can step the FSM
    /// and forward to the virtual touchpad per-frame — gluing N
    /// kernel batches into one virtual batch loses per-frame timing
    /// and corrupts libinput's state.
    ///
    /// Any trailing events without a closing SYN_REPORT are kept in
    /// internal state for the next call's first frame.
    pub fn poll_frames(&mut self) -> Result<Vec<PhysicalFrame>> {
        let events: Vec<InputEvent> = self
            .device
            .fetch_events()
            .map_err(|source| Error::EvdevRead { source })?
            .collect();
        let mut dropped = false;
        let frames = self.assembler.ingest_events(events, &mut dropped);
        if dropped {
            let _ = self.verify_active_contact();
        }
        Ok(frames)
    }
}

impl FrameAssembler {
    fn new(current_slot: usize, contact: bool) -> Self {
        Self {
            slots: [SlotState {
                tracking_id: -1,
                x: 0,
                y: 0,
            }; MAX_MT_SLOTS],
            current_slot,
            contact,
            pending_events: Vec::new(),
        }
    }

    fn clear_contact_state(&mut self) {
        self.contact = false;
        for slot in &mut self.slots {
            slot.tracking_id = -1;
        }
        self.pending_events.clear();
    }

    fn ingest_events<I>(&mut self, events: I, syn_dropped: &mut bool) -> Vec<PhysicalFrame>
    where
        I: IntoIterator<Item = InputEvent>,
    {
        let mut frames: Vec<PhysicalFrame> = Vec::new();
        for ev in events {
            if self.pending_events.is_empty()
                && ev.event_type() == EventType::ABSOLUTE
                && is_per_slot_mt_axis(AbsoluteAxisType(ev.code()))
                && self.current_slot < MAX_MT_SLOTS
            {
                // Prepend the active slot at the start of the frame if the
                // kernel omitted ABS_MT_SLOT via delta-compression. This
                // keeps the virtual touchpad's slot selection aligned even
                // after an earlier multi-slot gesture was suppressed.
                self.pending_events.push(InputEvent::new(
                    EventType::ABSOLUTE,
                    AbsoluteAxisType::ABS_MT_SLOT.0,
                    self.current_slot as i32,
                ));
            }
            self.pending_events.push(ev);
            match ev.event_type() {
                EventType::ABSOLUTE => self.apply_abs(ev.code(), ev.value()),
                EventType::KEY if ev.code() == Key::BTN_TOUCH.code() => {
                    self.contact = ev.value() != 0;
                }
                EventType::SYNCHRONIZATION if ev.code() == 3 => {
                    // SYN_DROPPED: kernel ring buffer overflowed. Discard
                    // the incomplete batch and signal caller to resync.
                    *syn_dropped = true;
                    self.pending_events.clear();
                }
                EventType::SYNCHRONIZATION if ev.code() == 0 => {
                    let frame = self.assemble_frame();
                    frames.push(PhysicalFrame {
                        frame,
                        events: std::mem::take(&mut self.pending_events),
                    });
                }
                _ => {}
            }
        }
        frames
    }

    fn apply_abs(&mut self, code: u16, value: i32) {
        let axis = AbsoluteAxisType(code);
        match axis {
            AbsoluteAxisType::ABS_MT_SLOT => {
                // Always update `current_slot` so out-of-range slot indices
                // are ignored by the `< MAX_MT_SLOTS` guards below instead
                // of overwriting the previously selected valid slot.
                self.current_slot = value as usize;
            }
            AbsoluteAxisType::ABS_MT_TRACKING_ID => {
                let slot = self.current_slot;
                if slot < MAX_MT_SLOTS {
                    self.slots[slot].tracking_id = value;
                }
            }
            AbsoluteAxisType::ABS_MT_POSITION_X => {
                let slot = self.current_slot;
                if slot < MAX_MT_SLOTS {
                    self.slots[slot].x = value;
                }
            }
            AbsoluteAxisType::ABS_MT_POSITION_Y => {
                let slot = self.current_slot;
                if slot < MAX_MT_SLOTS {
                    self.slots[slot].y = value;
                }
            }
            // Some pads also expose ABS_X / ABS_Y for the primary touch.
            // We deliberately ignore those — MT axes are authoritative.
            _ => {}
        }
    }

    fn assemble_frame(&mut self) -> TouchFrame {
        // Keep every active slot so the FSM can arbitrate one-finger
        // circular scrolling against two- and multi-finger libinput
        // gestures, and lock a captured gesture to its tracking ID.
        let touches = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, state)| state.tracking_id != -1)
            .map(|(slot, state)| TrackedTouch {
                slot,
                tracking_id: state.tracking_id,
                pos: TouchSample {
                    x: state.x,
                    y: state.y,
                },
            })
            .collect();
        TouchFrame {
            contact: self.contact,
            touches,
        }
    }
}

fn has_touchpad_capabilities(device: &Device) -> bool {
    let abs = device.supported_absolute_axes();
    let keys = device.supported_keys();
    abs.is_some_and(|a| {
        a.contains(AbsoluteAxisType::ABS_MT_POSITION_X)
            && a.contains(AbsoluteAxisType::ABS_MT_POSITION_Y)
    }) && keys.is_some_and(|k| k.contains(Key::BTN_TOUCH))
}

fn is_per_slot_mt_axis(axis: AbsoluteAxisType) -> bool {
    matches!(
        axis,
        AbsoluteAxisType::ABS_MT_TOUCH_MAJOR
            | AbsoluteAxisType::ABS_MT_TOUCH_MINOR
            | AbsoluteAxisType::ABS_MT_WIDTH_MAJOR
            | AbsoluteAxisType::ABS_MT_WIDTH_MINOR
            | AbsoluteAxisType::ABS_MT_ORIENTATION
            | AbsoluteAxisType::ABS_MT_POSITION_X
            | AbsoluteAxisType::ABS_MT_POSITION_Y
            | AbsoluteAxisType::ABS_MT_TOOL_TYPE
            | AbsoluteAxisType::ABS_MT_BLOB_ID
            | AbsoluteAxisType::ABS_MT_TRACKING_ID
            | AbsoluteAxisType::ABS_MT_PRESSURE
            | AbsoluteAxisType::ABS_MT_DISTANCE
            | AbsoluteAxisType::ABS_MT_TOOL_X
            | AbsoluteAxisType::ABS_MT_TOOL_Y
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn syn_report() -> InputEvent {
        InputEvent::new(EventType::SYNCHRONIZATION, 0, 0)
    }

    #[test]
    fn out_of_bounds_slot_does_not_corrupt_previous_slot() {
        let mut asm = FrameAssembler::new(0, false);
        let mut dropped = false;
        let frames = asm.ingest_events(
            [
                InputEvent::new(EventType::ABSOLUTE, AbsoluteAxisType::ABS_MT_SLOT.0, 0),
                InputEvent::new(
                    EventType::ABSOLUTE,
                    AbsoluteAxisType::ABS_MT_TRACKING_ID.0,
                    42,
                ),
                InputEvent::new(
                    EventType::ABSOLUTE,
                    AbsoluteAxisType::ABS_MT_POSITION_X.0,
                    500,
                ),
                InputEvent::new(
                    EventType::ABSOLUTE,
                    AbsoluteAxisType::ABS_MT_POSITION_Y.0,
                    600,
                ),
                // Out-of-range slot 20 must not overwrite slot 0.
                InputEvent::new(EventType::ABSOLUTE, AbsoluteAxisType::ABS_MT_SLOT.0, 20),
                InputEvent::new(
                    EventType::ABSOLUTE,
                    AbsoluteAxisType::ABS_MT_TRACKING_ID.0,
                    -1,
                ),
                InputEvent::new(
                    EventType::ABSOLUTE,
                    AbsoluteAxisType::ABS_MT_POSITION_X.0,
                    999,
                ),
                InputEvent::new(EventType::KEY, Key::BTN_TOUCH.code(), 1),
                syn_report(),
            ],
            &mut dropped,
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].frame.touches.len(), 1);
        assert_eq!(frames[0].frame.touches[0].slot, 0);
        assert_eq!(frames[0].frame.touches[0].tracking_id, 42);
        assert_eq!(
            frames[0].frame.touches[0].pos,
            TouchSample { x: 500, y: 600 }
        );
    }

    #[test]
    fn trailing_events_without_syn_report_are_retained_for_next_frame() {
        let mut asm = FrameAssembler::new(0, false);
        let mut dropped = false;

        let partial = asm.ingest_events(
            [
                InputEvent::new(
                    EventType::ABSOLUTE,
                    AbsoluteAxisType::ABS_MT_TRACKING_ID.0,
                    7,
                ),
                InputEvent::new(
                    EventType::ABSOLUTE,
                    AbsoluteAxisType::ABS_MT_POSITION_X.0,
                    300,
                ),
            ],
            &mut dropped,
        );
        assert!(partial.is_empty());

        let completed = asm.ingest_events(
            [
                InputEvent::new(
                    EventType::ABSOLUTE,
                    AbsoluteAxisType::ABS_MT_POSITION_Y.0,
                    400,
                ),
                InputEvent::new(EventType::KEY, Key::BTN_TOUCH.code(), 1),
                syn_report(),
            ],
            &mut dropped,
        );
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].frame.touches.len(), 1);
        assert_eq!(
            completed[0].frame.touches[0].pos,
            TouchSample { x: 300, y: 400 }
        );
        // Prepended ABS_MT_SLOT + 4 original events + SYN_REPORT = 6 events.
        assert_eq!(completed[0].events.len(), 6);
    }

    #[test]
    fn delta_compressed_frame_prepends_active_slot() {
        let mut asm = FrameAssembler::new(1, true);
        let mut dropped = false;
        let frames = asm.ingest_events(
            [
                InputEvent::new(
                    EventType::ABSOLUTE,
                    AbsoluteAxisType::ABS_MT_TRACKING_ID.0,
                    99,
                ),
                syn_report(),
            ],
            &mut dropped,
        );
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].events[0].event_type(), EventType::ABSOLUTE);
        assert_eq!(frames[0].events[0].code(), AbsoluteAxisType::ABS_MT_SLOT.0);
        assert_eq!(frames[0].events[0].value(), 1);
    }
}
