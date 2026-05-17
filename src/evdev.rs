// Physical touchpad input — opens the evdev node, queries EVIOCGABS for
// center coordinates, and turns the raw event stream into TouchFrames at
// each SYN_REPORT. See linux-design.md §5.

use std::path::{Path, PathBuf};

use evdev::{AbsoluteAxisType, Device, EventType, InputEvent, Key};

use crate::detector::TouchSample;
use crate::error::{Error, Result};
use crate::fsm::TouchFrame;

/// Maximum number of MT slots we track. The kernel exposes up to 10 in
/// practice; touchpads typically advertise 5. We sweep all slots when
/// rescanning for "lowest active" semantics (D-012), so the constant
/// only bounds the per-frame map size, not gesture logic.
const MAX_MT_SLOTS: usize = 16;

pub struct InputDevice {
    pub device: Device,
    pub path: PathBuf,
    pub abs_x_min: i32,
    pub abs_x_max: i32,
    pub abs_y_min: i32,
    pub abs_y_max: i32,
    pub center_x: i32,
    pub center_y: i32,
    /// Per-slot tracking IDs and last-seen (x, y). Slot index 0..MAX_MT_SLOTS-1.
    slots: [SlotState; MAX_MT_SLOTS],
    /// Current writing slot per ABS_MT_SLOT event.
    current_slot: usize,
    /// BTN_TOUCH summary; mirrors the kernel-reported any-finger-down state.
    contact: bool,
    /// Per-frame "dirty" markers for slots that received any update in the
    /// current SYN_REPORT window — used to honour the "lowest active slot"
    /// rule once events are flushed.
    dirty: u16,
}

#[derive(Clone, Copy, Debug, Default)]
struct SlotState {
    /// `-1` means inactive (kernel convention).
    tracking_id: i32,
    x: i32,
    y: i32,
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

        let mut slots = [SlotState::default(); MAX_MT_SLOTS];
        for s in slots.iter_mut() {
            s.tracking_id = -1;
        }

        Ok(Self {
            device,
            path: path.to_path_buf(),
            abs_x_min,
            abs_x_max,
            abs_y_min,
            abs_y_max,
            center_x,
            center_y,
            slots,
            current_slot: 0,
            contact: false,
            dirty: 0,
        })
    }

    /// Find a touchpad whose name matches `regex`. Returns the first match
    /// found via `/dev/input/event*` enumeration.
    pub fn find_by_name(regex_str: &str) -> Result<PathBuf> {
        let re = regex::Regex::new(regex_str).map_err(|source| Error::RegexInvalid {
            pattern: regex_str.to_string(),
            source,
        })?;
        for (path, device) in evdev::enumerate() {
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

    /// Block until the next SYN_REPORT and return the assembled frame.
    /// Returns `None` if no positional update was seen (e.g., a pure
    /// button-only sync) — the caller should treat this as "no new
    /// information this frame" rather than as an event drop.
    pub fn next_frame(&mut self) -> Result<Option<TouchFrame>> {
        let events: Vec<InputEvent> = self
            .device
            .fetch_events()
            .map_err(|source| Error::EvdevRead { source })?
            .collect();
        let mut frame_out: Option<TouchFrame> = None;
        for ev in events {
            match ev.event_type() {
                EventType::ABSOLUTE => self.apply_abs(ev.code(), ev.value()),
                EventType::KEY if ev.code() == Key::BTN_TOUCH.code() => {
                    self.contact = ev.value() != 0;
                }
                EventType::SYNCHRONIZATION if ev.code() == 0 => {
                    // SYN_REPORT
                    frame_out = Some(self.assemble_frame());
                }
                _ => {}
            }
        }
        Ok(frame_out)
    }

    fn apply_abs(&mut self, code: u16, value: i32) {
        let axis = AbsoluteAxisType(code);
        match axis {
            AbsoluteAxisType::ABS_MT_SLOT if (value as usize) < MAX_MT_SLOTS => {
                self.current_slot = value as usize;
            }
            AbsoluteAxisType::ABS_MT_TRACKING_ID => {
                let slot = self.current_slot;
                if slot < MAX_MT_SLOTS {
                    self.slots[slot].tracking_id = value;
                    self.dirty |= 1u16 << slot;
                }
            }
            AbsoluteAxisType::ABS_MT_POSITION_X => {
                let slot = self.current_slot;
                if slot < MAX_MT_SLOTS {
                    self.slots[slot].x = value;
                    self.dirty |= 1u16 << slot;
                }
            }
            AbsoluteAxisType::ABS_MT_POSITION_Y => {
                let slot = self.current_slot;
                if slot < MAX_MT_SLOTS {
                    self.slots[slot].y = value;
                    self.dirty |= 1u16 << slot;
                }
            }
            // Some pads also expose ABS_X / ABS_Y for the primary touch.
            // We deliberately ignore those — MT axes are authoritative.
            _ => {}
        }
    }

    fn assemble_frame(&mut self) -> TouchFrame {
        self.dirty = 0;
        // Lowest-numbered active slot wins (D-012). "Active" = tracking_id != -1.
        let chosen = self
            .slots
            .iter()
            .enumerate()
            .find(|(_, s)| s.tracking_id != -1);
        match (self.contact, chosen) {
            (true, Some((_, s))) => TouchFrame {
                contact: true,
                pos: Some(TouchSample { x: s.x, y: s.y }),
            },
            (true, None) => TouchFrame {
                contact: true,
                pos: None,
            },
            (false, _) => TouchFrame {
                contact: false,
                pos: None,
            },
        }
    }

}
