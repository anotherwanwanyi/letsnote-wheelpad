// 6-state FSM mirroring FUN_1400046a0 — see analysis/RE-findings.md §4
// and analysis/linux-design.md §5.

use crate::config::Scroll;
use crate::detector::{
    engagement_swept_angle, radial_gate_ok, within_horizontal_arc, CircularDetector, TouchSample,
    TRIGGER_ANGLE,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FsmState {
    Idle,
    Contact { origin: TouchSample },
    Moving { engage_start: TouchSample },
    Scrolling,
    // The FSM has an explicit Debounce state to preserve the structure
    // of the original Windows WheelPad FSM (FUN_1400046a0 case 5).
    //
    // The Windows timer expression decompiled to `abs(int) < 1`, which
    // is literally "true only on the same millisecond as the lift" —
    // almost certainly a Ghidra-reconstruction artifact for a bound
    // check that was originally `< CONST_TIMEOUT_MS`.
    //
    // Since the literal expression makes the timer-still-active branch
    // effectively unreachable on Windows, real Windows users do not
    // experience debounce-based quick relift. We mirror that: enter
    // Debounce on lift and exit to Idle on the very next frame, with no
    // timer check. We deliberately do NOT expose a TOML knob for this —
    // see DECISIONS.md D-011-followup.
    Debounce,
}

#[derive(Clone, Copy, Debug)]
pub struct TouchFrame {
    pub contact: bool,
    pub pos: Option<TouchSample>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    None,
    GrabPhysical,
    ReleasePhysical,
    EmitWheelV(i32),
    EmitWheelH(i32),
}

pub struct Fsm {
    state: FsmState,
    center_x: i32,
    center_y: i32,
}

impl Fsm {
    pub fn new(center_x: i32, center_y: i32) -> Self {
        Self {
            state: FsmState::Idle,
            center_x,
            center_y,
        }
    }

    pub fn state(&self) -> FsmState {
        self.state
    }

    /// Advance the FSM by one touch frame. Mutates the supplied detector
    /// (which holds the chord-angle accumulator and 20-unit-dead-band
    /// shift register) and the scroll config. Returns up to two actions
    /// the caller must perform this frame.
    ///
    /// The function returns a small fixed array rather than allocating
    /// because the maximum is two actions: a grab/release transition
    /// plus a wheel emit. Callers iterate `actions.iter().copied()` and
    /// skip `Action::None`.
    pub fn step(
        &mut self,
        frame: TouchFrame,
        detector: &mut CircularDetector,
        scroll: &Scroll,
    ) -> [Action; 2] {
        let mut out: [Action; 2] = [Action::None, Action::None];
        let mut push_action = |a: Action| {
            for slot in out.iter_mut() {
                if matches!(slot, Action::None) {
                    *slot = a;
                    return;
                }
            }
        };

        // Master gate. If scrolling is disabled at the config level we
        // still consume frames so the daemon stays alive (D-007), but
        // never advance past Idle and never emit ticks.
        if !scroll.enable {
            self.state = FsmState::Idle;
            return out;
        }

        match (self.state, frame.contact, frame.pos) {
            // ---------- Idle (state 1) ----------
            (FsmState::Idle, false, _) | (FsmState::Idle, true, None) => {
                // Stay idle.
            }
            (FsmState::Idle, true, Some(s)) => {
                // Fresh touch-down: radial-gate classifier (FUN_140005a00).
                if radial_gate_ok(self.center_x, self.center_y, s, scroll.detect_area_width) {
                    // Outside dead zone → MOVING. Capture engage_start
                    // here, matching DAT_14003cc18 being set at
                    // FUN_1400046a0 line 203 only on the state 1 → state 3
                    // transition. The detector's accumulator and history
                    // are NOT reset here; that happens on the
                    // Moving → Scrolling transition (mirroring
                    // FUN_1400046a0 line 151 which zeros DAT_14003cb00).
                    self.state = FsmState::Moving { engage_start: s };
                } else {
                    // Inside dead zone → CONTACT (trap).
                    self.state = FsmState::Contact { origin: s };
                }
            }

            // ---------- Contact (state 2) — dead-zone trap (D-020) ----------
            (FsmState::Contact { .. }, false, _) => {
                // Finger lifted. Per FUN_1400046a0 case 2 lines 118-126,
                // Contact only exits to Idle on lift; cross-gate movement
                // while in Contact does NOT transition to Moving. To
                // engage scrolling the user must lift and re-touch
                // outside the gate. See DECISIONS.md D-020.
                self.state = FsmState::Idle;
            }
            (FsmState::Contact { .. }, true, _) => {
                // Stay trapped in Contact regardless of where the finger
                // is now. Strict Windows-faithful dead-zone semantics.
            }

            // ---------- Moving (state 3) — engagement candidate ----------
            (FsmState::Moving { .. }, false, _) | (FsmState::Moving { .. }, true, None) => {
                // Lift before engagement → Idle. No accumulator reset
                // needed (we never set it in this state).
                self.state = FsmState::Idle;
            }
            (FsmState::Moving { engage_start }, true, Some(s)) => {
                if !radial_gate_ok(self.center_x, self.center_y, s, scroll.detect_area_width) {
                    // Slipped back into the dead zone — fall back to
                    // Contact (FUN_1400046a0 case 3, lines 127-137).
                    self.state = FsmState::Contact { origin: s };
                } else {
                    let swept =
                        engagement_swept_angle(self.center_x, self.center_y, engage_start, s);
                    if swept.abs() > TRIGGER_ANGLE {
                        // Engagement! Reset detector and grab the pad.
                        detector.on_gesture_start();
                        self.state = FsmState::Scrolling;
                        push_action(Action::GrabPhysical);
                        // Fall through to feed the engaging sample into
                        // the detector immediately so the first tick can
                        // emit on this very frame if the gesture is fast
                        // enough.
                        detector.push_if_moved(s);
                        let ticks = detector.step(scroll.sensitivity);
                        if ticks != 0 {
                            emit(
                                &mut push_action,
                                ticks,
                                scroll,
                                self.center_x,
                                self.center_y,
                                s,
                            );
                        }
                    }
                    // else stay in Moving until either lift, slip-back,
                    // or swept-angle threshold.
                }
            }

            // ---------- Scrolling (state 4) ----------
            (FsmState::Scrolling, false, _) | (FsmState::Scrolling, true, None) => {
                // Lift → Debounce. Release the grab so the user's cursor
                // is free immediately. The grab is NOT held across
                // Debounce (linux-design.md §8).
                self.state = FsmState::Debounce;
                push_action(Action::ReleasePhysical);
            }
            (FsmState::Scrolling, true, Some(s)) => {
                detector.push_if_moved(s);
                let ticks = detector.step(scroll.sensitivity);
                if ticks != 0 {
                    emit(
                        &mut push_action,
                        ticks,
                        scroll,
                        self.center_x,
                        self.center_y,
                        s,
                    );
                }
            }

            // ---------- Debounce (state 5) — structural marker only ----------
            (FsmState::Debounce, _, _) => {
                // Always transition to Idle on the next frame, regardless
                // of whether the finger is now down or up. See
                // DECISIONS.md D-011-followup and the comment on
                // FsmState::Debounce above.
                //
                // Note: if a finger is already down here we go to Idle
                // this frame; the next frame's classifier will see
                // contact=true and run the fresh-touch radial-gate test
                // exactly as if it had just touched down.
                self.state = FsmState::Idle;
            }
        }

        out
    }

    /// Force release of any active grab and transition to Idle. Used by
    /// the watchdog (linux-design.md §14 risk 13) and by SIGTERM cleanup.
    pub fn force_release(&mut self) -> Action {
        let prev = self.state;
        self.state = FsmState::Idle;
        match prev {
            FsmState::Scrolling => Action::ReleasePhysical,
            _ => Action::None,
        }
    }
}

/// Apply reverse flags and the arc gate, then push the appropriate
/// EmitWheelV / EmitWheelH action. The horizontal arc is only consulted
/// when `horizontal_enable = true` — vertical scroll is never angle-gated
/// (linux-design.md §5 "Vertical scroll is NOT angle-gated").
fn emit(
    push: &mut impl FnMut(Action),
    ticks: i32,
    scroll: &Scroll,
    center_x: i32,
    center_y: i32,
    current: TouchSample,
) {
    if scroll.horizontal_enable
        && within_horizontal_arc(
            center_x,
            center_y,
            current,
            scroll.horizontal_start,
            scroll.horizontal_end,
        )
    {
        let signed = if scroll.reverse_horizontal {
            -ticks
        } else {
            ticks
        };
        push(Action::EmitWheelH(signed));
    } else {
        let signed = if scroll.reverse_vertical {
            -ticks
        } else {
            ticks
        };
        push(Action::EmitWheelV(signed));
    }
}
