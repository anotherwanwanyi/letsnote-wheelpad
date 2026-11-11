// WheelPad FSM. The original Windows states are augmented with explicit
// MultiTouch and Passthrough arbitration states for Linux/libinput.

use std::f64::consts::PI;

use crate::config::Scroll;
use crate::detector::{
    engagement_swept_angle, radial_gate_ok_with_radius, within_horizontal_arc, CircularDetector,
    TouchSample, WheelDelta, TRIGGER_ANGLE,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollAxis {
    Vertical,
    Horizontal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsmState {
    Idle,
    Contact,
    Moving {
        tracking_id: i32,
    },
    /// Two or more contacts were observed before circular scrolling was
    /// locked in. Keep forwarding the physical stream to libinput and do
    /// not reconsider circular scrolling until every finger has lifted.
    MultiTouch,
    /// The pending outer-ring contact was classified as ordinary pointer
    /// input. Forward it until all-up and never capture it mid-gesture.
    Passthrough,
    Scrolling {
        tracking_id: i32,
        axis: ScrollAxis,
        /// `false` during the brief early-preemption window right after
        /// capture so a slightly delayed second finger can still preempt
        /// into `MultiTouch`; `true` once single-finger scrolling is
        /// established.
        locked: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackedTouch {
    pub slot: usize,
    pub tracking_id: i32,
    pub pos: TouchSample,
}

#[derive(Clone, Debug)]
pub struct TouchFrame {
    pub contact: bool,
    /// All active type-B multitouch slots, ordered by slot number.
    pub touches: Vec<TrackedTouch>,
}

/// Side effects the FSM asks the runtime to perform. The runtime also
/// derives whether to hold, replay, or suppress touchpad frames from the
/// state transition, so the FSM does not emit forwarding actions itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    None,
    EmitWheelV(WheelDelta),
    EmitWheelH(WheelDelta),
}

pub struct Fsm {
    state: FsmState,
    center_x: i32,
    center_y: i32,
    pad_radius: Option<i32>,
    intent_samples: Vec<TouchSample>,
    early_scroll_frames: usize,
}

const INTENT_SAMPLE_DEADBAND_SQ: i64 = 64; // 8 device units
const INTENT_MIN_SAMPLES: usize = 3;
const INTENT_MAX_SAMPLES: usize = 20;
const INTENT_MIN_TURN: f64 = 3.5 * PI / 180.0; // 3.5° of net chord-direction curvature
const INTENT_TANGENTIAL_RATIO: f64 = 1.5;
const POINTER_RADIAL_RATIO: f64 = 1.5;
const POINTER_MIN_RADIAL_TRAVEL: f64 = 40.0;
const POINTER_STRAIGHT_SWEEP: f64 = PI / 12.0; // 15°
/// Number of single-finger frames after initial `Scrolling` capture during
/// which a newly arriving second finger can still preempt into `MultiTouch`.
const EARLY_SCROLL_PREEMPT_FRAMES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IntentDecision {
    Pending,
    Circular,
    Pointer,
}

impl Fsm {
    pub fn new(center_x: i32, center_y: i32) -> Self {
        Self {
            state: FsmState::Idle,
            center_x,
            center_y,
            pad_radius: None,
            intent_samples: Vec::with_capacity(INTENT_MAX_SAMPLES),
            early_scroll_frames: 0,
        }
    }

    pub fn with_radius(center_x: i32, center_y: i32, radius: i32) -> Self {
        Self {
            state: FsmState::Idle,
            center_x,
            center_y,
            pad_radius: Some(radius.max(1)),
            intent_samples: Vec::with_capacity(INTENT_MAX_SAMPLES),
            early_scroll_frames: 0,
        }
    }

    pub fn state(&self) -> FsmState {
        self.state
    }

    pub fn is_scrolling(&self) -> bool {
        matches!(self.state, FsmState::Scrolling { .. })
    }

    /// Advance the FSM by one touch frame. Mutates the supplied detector
    /// (which holds the chord-angle accumulator, v120 rounding residue,
    /// and position history) and the scroll config. Returns the (at most
    /// one) wheel-emission action the runtime needs to perform.
    ///
    /// The runtime derives event-forwarding decisions from the states
    /// immediately before and after this call; no grab/release actions
    /// flow through this return value.
    pub fn step(
        &mut self,
        frame: &TouchFrame,
        detector: &mut CircularDetector,
        scroll: &Scroll,
    ) -> Action {
        // Master gate. If scrolling is disabled at the config level we
        // still consume frames so the daemon stays alive, but
        // never advance past Idle and never emit ticks.
        if !scroll.enable {
            self.state = FsmState::Idle;
            self.intent_samples.clear();
            self.early_scroll_frames = 0;
            return Action::None;
        }

        let has_contact = frame.contact || !frame.touches.is_empty();

        match self.state {
            // ---------- Idle (state 1) ----------
            FsmState::Idle if !has_contact => Action::None,
            FsmState::Idle if frame.touches.len() > 1 => {
                self.state = FsmState::MultiTouch;
                Action::None
            }
            FsmState::Idle if frame.touches.is_empty() => {
                // BTN_TOUCH asserted before any MT slot tracking ID arrived;
                // treat as ordinary passthrough until all-up.
                self.state = FsmState::Passthrough;
                Action::None
            }
            FsmState::Idle => {
                let touch = frame.touches[0];
                let s = touch.pos;
                // Fresh touch-down: radial-gate classifier (FUN_140005a00),
                // scaled to the touchpad's actual radius when known.
                if radial_gate_ok_with_radius(
                    self.center_x,
                    self.center_y,
                    self.pad_radius,
                    s,
                    scroll.detect_area_width,
                ) {
                    // Outside dead zone → pending circular intent. Raw
                    // frames are held by the runtime until this state is
                    // resolved as circular or ordinary pointer input.
                    self.state = FsmState::Moving {
                        tracking_id: touch.tracking_id,
                    };
                    self.intent_samples.clear();
                    self.intent_samples.push(s);
                } else {
                    // Inside dead zone → CONTACT (trap).
                    self.state = FsmState::Contact;
                }
                Action::None
            }

            // ---------- Contact (state 2) — dead-zone trap ----------
            FsmState::Contact if !has_contact => {
                // Finger lifted. Per FUN_1400046a0 case 2 lines 118-126,
                // Contact only exits to Idle on lift; cross-gate movement
                // while in Contact does NOT transition to Moving.
                self.state = FsmState::Idle;
                Action::None
            }
            FsmState::Contact => {
                // Stay trapped regardless of where the finger is now.
                Action::None
            }

            // ---------- Moving (state 3) — engagement candidate ----------
            FsmState::Moving { .. } if !has_contact => {
                // Lift before engagement → Idle.
                self.state = FsmState::Idle;
                self.intent_samples.clear();
                Action::None
            }
            FsmState::Moving { .. } if frame.touches.len() > 1 => {
                // Multi-finger gestures take priority until every contact
                // has lifted. This prevents a two-finger scroll or pinch
                // from being captured later by the circular recognizer.
                self.state = FsmState::MultiTouch;
                self.intent_samples.clear();
                Action::None
            }
            FsmState::Moving { .. } if frame.touches.is_empty() => {
                // Slot released while BTN_TOUCH is still clearing; flush
                // held frames as ordinary pointer input until all-up.
                self.state = FsmState::Passthrough;
                self.intent_samples.clear();
                Action::None
            }
            FsmState::Moving { tracking_id } => {
                let touch = frame.touches[0];
                if touch.tracking_id != tracking_id {
                    // One finger was replaced by another without an
                    // all-up frame. Do not splice two physical contacts
                    // into one candidate trajectory.
                    self.state = FsmState::Passthrough;
                    self.intent_samples.clear();
                    return Action::None;
                }
                let s = touch.pos;
                if !radial_gate_ok_with_radius(
                    self.center_x,
                    self.center_y,
                    self.pad_radius,
                    s,
                    scroll.detect_area_width,
                ) {
                    // Slipped back into the dead zone: this was ordinary
                    // pointer movement. Flush the held frames and lock in
                    // passthrough until all contacts lift.
                    self.state = FsmState::Passthrough;
                    self.intent_samples.clear();
                    Action::None
                } else {
                    match self.observe_intent_sample(s) {
                        IntentDecision::Pending => Action::None,
                        IntentDecision::Pointer => {
                            self.state = FsmState::Passthrough;
                            self.intent_samples.clear();
                            Action::None
                        }
                        IntentDecision::Circular => {
                            // Lock horizontal vs. vertical axis once at capture
                            // based on where the gesture started, so continuous
                            // full-circle scrolling doesn't flip between V and H
                            // when passing through the bottom sector.
                            let start_sample = self.intent_samples.first().copied().unwrap_or(s);
                            let axis = if scroll.horizontal_enable
                                && within_horizontal_arc(
                                    self.center_x,
                                    self.center_y,
                                    start_sample,
                                    scroll.horizontal_start,
                                    scroll.horizontal_end,
                                ) {
                                ScrollAxis::Horizontal
                            } else {
                                ScrollAxis::Vertical
                            };

                            // Seed the detector with every held candidate
                            // sample instead of throwing away the motion
                            // that established circular intent.
                            detector.on_gesture_start();
                            let mut delta = WheelDelta::default();
                            for sample in self.intent_samples.drain(..) {
                                if detector.push_polar_sample(sample) {
                                    delta += detector.step_polar(
                                        self.center_x,
                                        self.center_y,
                                        scroll.sensitivity,
                                    );
                                }
                            }
                            self.early_scroll_frames = 0;
                            self.state = FsmState::Scrolling {
                                tracking_id,
                                axis,
                                locked: false,
                            };
                            if !delta.is_zero() {
                                emit(delta, scroll, axis)
                            } else {
                                Action::None
                            }
                        }
                    }
                }
            }

            // ---------- MultiTouch — passthrough owns this contact set ----------
            FsmState::MultiTouch if !has_contact => {
                self.state = FsmState::Idle;
                Action::None
            }
            FsmState::MultiTouch => Action::None,

            // ---------- Passthrough — ordinary pointer owns this stream ----------
            FsmState::Passthrough if !has_contact => {
                self.state = FsmState::Idle;
                Action::None
            }
            FsmState::Passthrough => Action::None,

            // ---------- Scrolling (state 4) ----------
            FsmState::Scrolling { .. } if !has_contact => {
                // Lift → Idle directly. Returning straight to Idle on the
                // all-up frame ensures the runtime suppresses this final
                // lift frame (`previous_scrolling == true`) while the
                // very next touch-down frame starts from `Idle` and is
                // properly held for intent classification instead of
                // leaking through a dummy Debounce transition.
                self.state = FsmState::Idle;
                self.early_scroll_frames = 0;
                Action::None
            }
            FsmState::Scrolling { locked: false, .. } if frame.touches.len() > 1 => {
                // A second finger arrived during the brief early-preemption
                // window right after circular capture (e.g. a fast 2/3-finger
                // swipe where Finger 1 landed ~20ms before Finger 2). Preempt
                // into MultiTouch so the runtime flushes the held initial
                // frames to libinput as a complete multi-finger gesture.
                self.state = FsmState::MultiTouch;
                self.early_scroll_frames = 0;
                detector.on_gesture_start();
                Action::None
            }
            FsmState::Scrolling {
                tracking_id,
                axis,
                locked,
            } => {
                self.early_scroll_frames = self.early_scroll_frames.saturating_add(1);
                if !locked && self.early_scroll_frames >= EARLY_SCROLL_PREEMPT_FRAMES {
                    self.state = FsmState::Scrolling {
                        tracking_id,
                        axis,
                        locked: true,
                    };
                }

                // Circular scrolling owns the stream once locked, even
                // if more fingers are added later. Continue following only
                // the original tracking ID; never jump to a different slot.
                let tracked = frame
                    .touches
                    .iter()
                    .find(|touch| touch.tracking_id == tracking_id);
                if let Some(touch) = tracked {
                    if !detector.push_polar_sample(touch.pos) {
                        return Action::None;
                    }
                    let delta =
                        detector.step_polar(self.center_x, self.center_y, scroll.sensitivity);
                    if !delta.is_zero() {
                        emit(delta, scroll, axis)
                    } else {
                        Action::None
                    }
                } else {
                    Action::None
                }
            }
        }
    }

    /// Reset state to Idle and clear the detector's accumulator and
    /// history. Used by the watchdog when Scrolling has persisted
    /// without packet progress; restoring Idle resumes touchpad
    /// passthrough so the cursor isn't frozen indefinitely. We reset
    /// the detector too so a fresh gesture after the watchdog kick
    /// doesn't start from a stale half-filled history.
    pub fn force_idle(&mut self, detector: &mut CircularDetector) {
        self.state = FsmState::Idle;
        self.intent_samples.clear();
        self.early_scroll_frames = 0;
        detector.on_gesture_start();
    }

    /// Resolve a still-pending candidate as ordinary pointer input.
    ///
    /// The runtime uses this as a safety valve when too many raw frames
    /// have accumulated without enough meaningful movement to classify
    /// the gesture. Returning `true` tells it to replay the held frames.
    pub fn cancel_pending(&mut self) -> bool {
        if matches!(self.state, FsmState::Moving { .. }) {
            self.state = FsmState::Passthrough;
            self.intent_samples.clear();
            true
        } else {
            false
        }
    }

    fn observe_intent_sample(&mut self, sample: TouchSample) -> IntentDecision {
        if let Some(previous) = self.intent_samples.last() {
            let dx = (sample.x - previous.x) as i64;
            let dy = (sample.y - previous.y) as i64;
            if dx * dx + dy * dy <= INTENT_SAMPLE_DEADBAND_SQ {
                return IntentDecision::Pending;
            }
        }
        if self.intent_samples.len() < INTENT_MAX_SAMPLES {
            self.intent_samples.push(sample);
        }

        classify_intent(self.center_x, self.center_y, &self.intent_samples)
    }
}

fn classify_intent(center_x: i32, center_y: i32, samples: &[TouchSample]) -> IntentDecision {
    if samples.len() < INTENT_MIN_SAMPLES {
        return IntentDecision::Pending;
    }

    let start = samples[0];
    let current = *samples.last().expect("intent history is non-empty");
    let swept = engagement_swept_angle(center_x, center_y, start, current);
    let mut tangential = 0.0;
    let mut radial = 0.0;
    for pair in samples.windows(2) {
        let (a0, r0) = polar(center_x, center_y, pair[0]);
        let (a1, r1) = polar(center_x, center_y, pair[1]);
        let da = wrap_angle(a1 - a0);
        tangential += ((r0 + r1) * 0.5 * da).abs();
        radial += (r1 - r0).abs();
    }

    let mut turn_sum = 0.0;
    for triple in samples.windows(3) {
        let a0 = segment_angle(triple[0], triple[1]);
        let a1 = segment_angle(triple[1], triple[2]);
        let turn = wrap_angle(a1 - a0);
        turn_sum += turn;
    }

    // Use net curvature rather than requiring every local turn to have
    // the same sign. Real evdev traces contain quantisation and finger
    // jitter, so a single tiny counter-turn must not permanently reject
    // an otherwise clear circular trajectory.
    let aligned_turn = turn_sum * swept.signum();

    if swept.abs() >= TRIGGER_ANGLE
        && tangential >= radial * INTENT_TANGENTIAL_RATIO
        && aligned_turn >= INTENT_MIN_TURN
    {
        IntentDecision::Circular
    } else if (radial >= POINTER_MIN_RADIAL_TRAVEL && radial > tangential * POINTER_RADIAL_RATIO)
        || (swept.abs() >= POINTER_STRAIGHT_SWEEP && aligned_turn < INTENT_MIN_TURN)
        || samples.len() >= INTENT_MAX_SAMPLES
    {
        IntentDecision::Pointer
    } else {
        // Crossing the earliest circular threshold without yet having
        // enough curvature is inconclusive, not proof of pointer intent.
        // Keep observing so a slowly developing or slightly noisy circle
        // can still capture this same contact stream.
        IntentDecision::Pending
    }
}

fn polar(center_x: i32, center_y: i32, sample: TouchSample) -> (f64, f64) {
    let x = (sample.x - center_x) as f64;
    let y = (sample.y - center_y) as f64;
    (y.atan2(x), x.hypot(y))
}

fn segment_angle(from: TouchSample, to: TouchSample) -> f64 {
    ((to.y - from.y) as f64).atan2((to.x - from.x) as f64)
}

fn wrap_angle(mut angle: f64) -> f64 {
    let two_pi = 2.0 * PI;
    if angle > PI {
        angle -= two_pi;
    }
    if angle < -PI {
        angle += two_pi;
    }
    angle
}

/// Apply reverse flags for the locked scroll axis and return the
/// appropriate `EmitWheelV` / `EmitWheelH` action.
fn emit(delta: WheelDelta, scroll: &Scroll, axis: ScrollAxis) -> Action {
    match axis {
        ScrollAxis::Horizontal => {
            let signed = if scroll.reverse_horizontal {
                delta.reversed()
            } else {
                delta
            };
            Action::EmitWheelH(signed)
        }
        ScrollAxis::Vertical => {
            let signed = if scroll.reverse_vertical {
                delta.reversed()
            } else {
                delta
            };
            Action::EmitWheelV(signed)
        }
    }
}
