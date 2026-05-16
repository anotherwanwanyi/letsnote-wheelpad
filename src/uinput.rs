// Virtual wheel device — exposes REL_WHEEL{,_HI_RES} and REL_HWHEEL{,_HI_RES}.
// The physical touchpad keeps driving the cursor; we only contribute scroll.
// See linux-design.md §9.

use std::path::Path;

use evdev::{
    uinput::{VirtualDevice, VirtualDeviceBuilder},
    AttributeSet, BusType, EventType, InputEvent, InputId, RelativeAxisType,
};

use crate::error::{Error, Result};

const DEVICE_NAME: &str = "Let's Note WheelPad (virtual wheel)";
const VENDOR_ID: u16 = 0x6c6e; // ASCII "ln"
const PRODUCT_ID: u16 = 0x7770; // ASCII "wp"
const VERSION: u16 = 1;

/// One wheel "tick" = 120 hi-res units; same value real mice emit. Keeps
/// libinput's smooth-scroll math happy on Wayland and X11 alike.
const HI_RES_STEP: i32 = 120;

pub struct UinputDevice {
    dev: VirtualDevice,
}

impl UinputDevice {
    pub fn create() -> Result<Self> {
        if !Path::new("/dev/uinput").exists() {
            return Err(Error::UinputMissing);
        }

        let mut rel = AttributeSet::<RelativeAxisType>::new();
        rel.insert(RelativeAxisType::REL_WHEEL);
        rel.insert(RelativeAxisType::REL_WHEEL_HI_RES);
        rel.insert(RelativeAxisType::REL_HWHEEL);
        rel.insert(RelativeAxisType::REL_HWHEEL_HI_RES);

        let dev = VirtualDeviceBuilder::new()
            .map_err(|source| Error::UinputCreate { source })?
            .name(DEVICE_NAME)
            .input_id(InputId::new(
                BusType::BUS_VIRTUAL,
                VENDOR_ID,
                PRODUCT_ID,
                VERSION,
            ))
            .with_relative_axes(&rel)
            .map_err(|source| Error::UinputCreate { source })?
            .build()
            .map_err(|source| Error::UinputCreate { source })?;

        Ok(Self { dev })
    }

    /// Emit `ticks` vertical wheel notches. Positive = scroll up
    /// (libinput convention). The caller already applied
    /// `reverse_vertical` flipping; this function does not interpret
    /// signs further.
    pub fn emit_v(&mut self, ticks: i32) -> Result<()> {
        if ticks == 0 {
            return Ok(());
        }
        let events = [
            InputEvent::new(
                EventType::RELATIVE,
                RelativeAxisType::REL_WHEEL_HI_RES.0,
                ticks * HI_RES_STEP,
            ),
            InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_WHEEL.0, ticks),
        ];
        self.dev
            .emit(&events)
            .map_err(|source| Error::UinputWrite { source })
    }

    /// Emit `ticks` horizontal wheel notches. Positive = scroll right.
    pub fn emit_h(&mut self, ticks: i32) -> Result<()> {
        if ticks == 0 {
            return Ok(());
        }
        let events = [
            InputEvent::new(
                EventType::RELATIVE,
                RelativeAxisType::REL_HWHEEL_HI_RES.0,
                ticks * HI_RES_STEP,
            ),
            InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_HWHEEL.0, ticks),
        ];
        self.dev
            .emit(&events)
            .map_err(|source| Error::UinputWrite { source })
    }
}
