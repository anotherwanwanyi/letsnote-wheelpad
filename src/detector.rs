// Port of FUN_140005bf0 — see analysis/RE-findings.md §6 and analysis/linux-design.md §6.

use std::collections::VecDeque;
use std::f64::consts::PI;
use std::ops::AddAssign;

const PI2: f64 = 2.0 * PI;

/// Early circular-intent threshold. The old Windows-faithful π/12 (15°)
/// gate leaked a visibly long pointer movement before capture; π/24 (7.5°)
/// is paired with the FSM's curvature and tangential-motion checks.
pub const TRIGGER_ANGLE: f64 = PI / 24.0;
pub const NOISE_REJECT_ANGLE: f64 = PI / 4.0;
pub const ZONE_RADIANS: f64 = PI / 8.0;
/// Eight device units keeps high-resolution updates frequent while still
/// filtering coordinate quantisation and stationary reports.
pub const SAMPLE_DEADBAND_SQ: i64 = 64;
pub const SENSITIVITY_TABLE: [i32; 7] = [5, 7, 10, 14, 20, 28, 40];

/// Fixed at 20 to match Windows WheelPad exactly (DAT_14003cbec clamp at
/// FUN_1400046a0 lines 65-67). The earlier design (D-021) scaled this
/// from a startup-measured packet rate; hardware testing on the CF-SV2
/// showed that scaling subtly changed scrolling startup feel, so we
/// revert to Windows-faithful behaviour. See DECISIONS.md D-021-followup.
pub const HISTORY_CAPACITY: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchSample {
    pub x: i32,
    pub y: i32,
}

/// One logical wheel update in both Linux representations.
///
/// `v120` is the high-resolution delta where 120 units equal one detent.
/// `discrete` is the legacy whole-detent approximation. Consumers choose
/// one stream; they must not add the two values together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WheelDelta {
    pub v120: i32,
    pub discrete: i32,
}

impl WheelDelta {
    pub fn is_zero(self) -> bool {
        self.v120 == 0 && self.discrete == 0
    }

    pub fn reversed(self) -> Self {
        Self {
            v120: -self.v120,
            discrete: -self.discrete,
        }
    }
}

impl AddAssign for WheelDelta {
    fn add_assign(&mut self, rhs: Self) {
        self.v120 += rhs.v120;
        self.discrete += rhs.discrete;
    }
}

pub struct CircularDetector {
    history: VecDeque<TouchSample>,
    last_stored: Option<TouchSample>,
    accumulator: f64,
    v120_remainder: f64,
}

impl Default for CircularDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl CircularDetector {
    pub fn new() -> Self {
        Self {
            history: VecDeque::with_capacity(HISTORY_CAPACITY),
            last_stored: None,
            accumulator: 0.0,
            v120_remainder: 0.0,
        }
    }

    pub fn on_gesture_start(&mut self) {
        self.history.clear();
        self.last_stored = None;
        self.accumulator = 0.0;
        self.v120_remainder = 0.0;
    }

    /// The movement dead band is computed
    /// against the **previously stored sample**, not the engagement-start
    /// point. It was reduced from the Windows-faithful 20 units to 8 so a
    /// resumed gesture can produce high-resolution updates promptly.
    ///
    /// Returns `true` only when `s` was added to the history. Callers must
    /// not run [`Self::step`] when this returns `false`: doing so would
    /// integrate the same curvature history again for every stationary
    /// touch frame and generate scroll ticks after the finger has stopped.
    pub fn push_if_moved(&mut self, s: TouchSample) -> bool {
        if let Some(prev) = self.last_stored {
            let dx = (s.x - prev.x) as i64;
            let dy = (s.y - prev.y) as i64;
            if dx * dx + dy * dy <= SAMPLE_DEADBAND_SQ {
                return false;
            }
        }
        self.last_stored = Some(s);
        if self.history.len() == HISTORY_CAPACITY {
            self.history.pop_back();
        }
        self.history.push_front(s);
        true
    }

    /// Integrate one stored sample into both high-resolution v120 movement
    /// and the legacy whole-detent approximation. Positive curvature maps
    /// to negative wheel movement, preserving the Windows sign convention.
    /// The user-facing reverse flip is applied later, in the FSM.
    pub fn step(&mut self, scroll_speed_adjust: i32) -> WheelDelta {
        let n = self.history.len();
        if n < 3 {
            return WheelDelta::default();
        }

        // 1. Per-pair motion-vector angles.
        let mut a = Vec::with_capacity(n - 1);
        for i in 0..n - 1 {
            let dx = (self.history[i].x - self.history[i + 1].x) as f64;
            let dy = (self.history[i].y - self.history[i + 1].y) as f64;
            a.push(dy.atan2(dx));
        }

        // 2. Pairwise differences with ±2π wrap and history-truncating
        //    π/4 reject (FUN_140005bf0 lines 80-92).
        let mut sum = 0.0;
        let available = a.len() - 1;
        let mut valid = available;
        for i in 0..a.len() - 1 {
            let mut d = a[i] - a[i + 1];
            if d > PI {
                d -= PI2;
            }
            if d < -PI {
                d += PI2;
            }
            if d.abs() > NOISE_REJECT_ANGLE {
                valid = i;
                break;
            }
            sum += d;
        }
        // With only three or four stored points, the intent classifier has
        // already established that this is a circle, so their one or two
        // curvature deltas are useful for immediate high-resolution output.
        // Once a longer history exists, keep the original three-delta gate:
        // fewer valid deltas then means the π/4 noise reject truncated it.
        if valid == 0 || (available >= 3 && valid < 3) {
            return WheelDelta::default();
        }

        // 3. Sensitivity-table-weighted mean. Convert the same continuous
        // curvature into v120 fractions immediately, retaining sub-unit
        // rounding residue across reports.
        let idx = (scroll_speed_adjust.clamp(-4, 2) + 4) as usize;
        let sensitivity = SENSITIVITY_TABLE[idx] as f64;
        let curvature_delta = sensitivity * (sum / valid as f64);
        let exact_v120 = self.v120_remainder - curvature_delta * (120.0 / PI2);
        let v120 = exact_v120.round() as i32;
        self.v120_remainder = exact_v120 - v120 as f64;

        // 4. WHILE-LOOP DRAIN — Linux deviation from Windows. See
        //    DECISIONS.md D-006. Windows FUN_140005bf0 (lines 113-136) is
        //    a single-pass branch that emits at most one tick per packet
        //    and silently loses angle on fast sweeps; we drain fully so
        //    that arbitrarily fast circles still scroll the proportional
        //    amount.
        //
        //    Sign convention preserved from Windows: positive accumulator
        //    overflow yields a tick value of -1. The user-visible
        //    reverse-direction flip is applied by the FSM, not here. A
        //    clockwise gesture (which integrates
        //    positive in screen-Y-down coords) therefore returns negative
        //    ticks; passing the value through to uinput unchanged scrolls
        //    the page DOWN, matching Windows.
        //
        //    Known quirk preserved as a comment for archaeology:
        //    FUN_140005bf0 line 120 contains a defensive clamp that snaps
        //    the accumulator to -π after the +2π correction rather than
        //    to 0. Our while-loop makes the clamp unreachable, but the
        //    note remains so future readers don't think the quirk was
        //    overlooked.
        let mut ticks: i32 = 0;
        if valid >= 3 {
            self.accumulator += curvature_delta;
            while self.accumulator > PI {
                self.accumulator -= PI2;
                ticks -= 1;
            }
            while self.accumulator < -PI {
                self.accumulator += PI2;
                ticks += 1;
            }
        }
        WheelDelta {
            v120,
            discrete: ticks,
        }
    }

    /// Test-only setter. Visible to integration tests under `tests/`.
    #[doc(hidden)]
    pub fn set_accumulator_for_test(&mut self, v: f64) {
        self.accumulator = v;
    }
}

/// Engagement gate — state 3 → state 4 transition test. Center-relative
/// atan2 sweep from the engagement-start point to the current sample,
/// with symmetric ±2π wrap (we deliberately use symmetric form across
/// the daemon — see RE-findings.md §5 footnote on the asymmetric wrap).
pub fn engagement_swept_angle(
    center_x: i32,
    center_y: i32,
    engage_start: TouchSample,
    current: TouchSample,
) -> f64 {
    let ax = (engage_start.x - center_x) as f64;
    let ay = (engage_start.y - center_y) as f64;
    let bx = (current.x - center_x) as f64;
    let by = (current.y - center_y) as f64;
    let mut d = by.atan2(bx) - ay.atan2(ax);
    if d > PI {
        d -= PI2;
    }
    if d < -PI {
        d += PI2;
    }
    d
}

/// Radial gate from FUN_140005a00 line 31 and FUN_1400046a0 lines 129/187.
/// Returns true if the centered sample is in the outer ring (i.e., outside
/// the inner dead-zone radius).
pub fn radial_gate_ok(
    center_x: i32,
    center_y: i32,
    s: TouchSample,
    detect_area_width: i32,
) -> bool {
    let dx = (s.x - center_x) as i64;
    let dy = (s.y - center_y) as i64;
    let r2 = dx * dx + dy * dy;
    let w = (10 - detect_area_width.clamp(0, 10)) as i64;
    r2 >= (w * w) * 400
}

/// Horizontal-arc test (FUN_140005a00 lines 65-74). Returns true if the
/// centered sample's atan2 lies within the configured wedge. Wraparound
/// (`start > end`) is handled by splitting the test. Caller guarantees
/// `horizontal_enable = true`; this function MUST NOT be called when
/// horizontal scrolling is disabled (see linux-design.md §5 "Vertical
/// scroll is NOT angle-gated").
pub fn within_horizontal_arc(
    center_x: i32,
    center_y: i32,
    s: TouchSample,
    horizontal_start: i32,
    horizontal_end: i32,
) -> bool {
    let dx = (s.x - center_x) as f64;
    let dy = (s.y - center_y) as f64;
    let mut theta = dy.atan2(dx);
    if theta < 0.0 {
        theta += PI2;
    }
    let start = horizontal_start as f64 * ZONE_RADIANS;
    let end = horizontal_end as f64 * ZONE_RADIANS;
    if start <= end {
        theta >= start && theta <= end
    } else {
        // Wedge wraps across 2π.
        theta >= start || theta <= end
    }
}
