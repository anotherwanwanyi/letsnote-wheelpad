// Cursor suppression — wrapper around evdev's EVIOCGRAB. We acquire only
// while the FSM is in Scrolling and release on lift, on watchdog timeout,
// or on Drop. See linux-design.md §8.

use evdev::Device;

use crate::error::{Error, Result};

pub struct Grabber {
    grabbed: bool,
}

impl Grabber {
    pub fn new() -> Self {
        Self { grabbed: false }
    }

    pub fn is_active(&self) -> bool {
        self.grabbed
    }

    /// Idempotent acquire. Calling while already grabbed is a no-op.
    pub fn acquire(&mut self, device: &mut Device) -> Result<()> {
        if self.grabbed {
            return Ok(());
        }
        device
            .grab()
            .map_err(|e| Error::Grab {
                source: nix::errno::Errno::from_i32(e.raw_os_error().unwrap_or(0)),
            })?;
        self.grabbed = true;
        Ok(())
    }

    /// Idempotent release. Calling while not grabbed is a no-op so the
    /// watchdog and SIGTERM paths can call unconditionally.
    pub fn release(&mut self, device: &mut Device) -> Result<()> {
        if !self.grabbed {
            return Ok(());
        }
        device
            .ungrab()
            .map_err(|e| Error::Grab {
                source: nix::errno::Errno::from_i32(e.raw_os_error().unwrap_or(0)),
            })?;
        self.grabbed = false;
        Ok(())
    }
}

impl Default for Grabber {
    fn default() -> Self {
        Self::new()
    }
}
