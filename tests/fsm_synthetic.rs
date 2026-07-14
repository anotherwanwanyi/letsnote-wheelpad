// Synthetic tests for gesture arbitration and FSM state transitions.

use std::f64::consts::PI;

use letsnote_wheelpad::config::Scroll;
use letsnote_wheelpad::detector::{CircularDetector, TouchSample};
use letsnote_wheelpad::fsm::{Action, Fsm, FsmState, TouchFrame, TrackedTouch};

const PRIMARY_TRACKING_ID: i32 = 100;

fn default_scroll() -> Scroll {
    Scroll::default()
}

fn lift() -> TouchFrame {
    TouchFrame {
        contact: false,
        touches: Vec::new(),
    }
}

fn touch(x: i32, y: i32) -> TouchFrame {
    touch_frame(&[(0, PRIMARY_TRACKING_ID, x, y)])
}

fn touch_frame(touches: &[(usize, i32, i32, i32)]) -> TouchFrame {
    TouchFrame {
        contact: !touches.is_empty(),
        touches: touches
            .iter()
            .map(|&(slot, tracking_id, x, y)| TrackedTouch {
                slot,
                tracking_id,
                pos: TouchSample { x, y },
            })
            .collect(),
    }
}

fn drive(
    fsm: &mut Fsm,
    detector: &mut CircularDetector,
    scroll: &Scroll,
    frames: &[TouchFrame],
) -> Vec<Action> {
    let mut acc = Vec::new();
    for f in frames {
        let action = fsm.step(f, detector, scroll);
        if !matches!(action, Action::None) {
            acc.push(action);
        }
    }
    acc
}

#[test]
fn idle_to_contact_on_touchdown_inside_dead_zone() {
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();
    let _ = drive(&mut fsm, &mut det, &scroll, &[touch(510, 510)]);
    assert!(matches!(fsm.state(), FsmState::Contact { .. }));
}

#[test]
fn idle_to_moving_on_touchdown_outside_dead_zone() {
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();
    let _ = drive(&mut fsm, &mut det, &scroll, &[touch(720, 500)]); // r = 220
    assert!(matches!(fsm.state(), FsmState::Moving { .. }));
}

#[test]
fn contact_to_idle_on_lift() {
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();
    drive(&mut fsm, &mut det, &scroll, &[touch(510, 510), lift()]);
    assert!(matches!(fsm.state(), FsmState::Idle));
}

#[test]
fn contact_does_not_engage_on_cross_gate_movement() {
    // Under the strict Windows dead-zone semantics, once trapped in
    // Contact, even sliding outside the gate does not engage Moving.
    // The user must lift and re-touch.
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();
    // Touch inside dead zone, then slide outside.
    drive(
        &mut fsm,
        &mut det,
        &scroll,
        &[touch(510, 510), touch(550, 500), touch(720, 500)],
    );
    assert!(
        matches!(fsm.state(), FsmState::Contact { .. }),
        "expected to stay in Contact, got {:?}",
        fsm.state()
    );
}

#[test]
fn moving_to_idle_on_lift_before_engagement() {
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();
    drive(&mut fsm, &mut det, &scroll, &[touch(720, 500), lift()]);
    assert!(matches!(fsm.state(), FsmState::Idle));
}

#[test]
fn moving_to_passthrough_on_slip_back_into_dead_zone() {
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();
    // Engage outside, then slip back inside.
    drive(
        &mut fsm,
        &mut det,
        &scroll,
        &[touch(720, 500), touch(550, 500)],
    );
    assert!(matches!(fsm.state(), FsmState::Passthrough));
}

#[test]
fn moving_to_scrolling_on_early_curved_sweep() {
    // Three samples forming a curved sweep past π/24 engage Scrolling.
    // Candidate frames are held by the runtime until this transition.
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();

    // engage_start at angle 0, r=220
    let start = touch(720, 500);
    // Two 11.25° curved steps exceed the 7.5° intent threshold.
    let mid_theta = PI / 16.0;
    let mid = touch(
        500 + (220.0 * mid_theta.cos()).round() as i32,
        500 + (220.0 * mid_theta.sin()).round() as i32,
    );
    let theta = PI / 8.0;
    let end_x = 500 + (220.0 * theta.cos()).round() as i32;
    let end_y = 500 + (220.0 * theta.sin()).round() as i32;
    let end = touch(end_x, end_y);

    drive(&mut fsm, &mut det, &scroll, &[start, mid, end]);
    assert!(matches!(fsm.state(), FsmState::Scrolling { .. }));
}

#[test]
fn clockwise_early_curved_sweep_also_enters_scrolling() {
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();

    let frames: Vec<_> = (0..=2)
        .map(|i| {
            let theta = -(i as f64) * PI / 16.0;
            touch(
                500 + (220.0 * theta.cos()).round() as i32,
                500 + (220.0 * theta.sin()).round() as i32,
            )
        })
        .collect();
    drive(&mut fsm, &mut det, &scroll, &frames);

    assert!(matches!(fsm.state(), FsmState::Scrolling { .. }));
}

#[test]
fn dense_real_scale_circle_enters_scrolling_at_the_early_threshold() {
    // The real device reports 0..771 on both axes. Use its approximate
    // centre/radius and dense 2° steps rather than the much larger jumps
    // used by the original idealised tests.
    let mut fsm = Fsm::new(385, 385);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();
    let frames: Vec<_> = (0..=4)
        .map(|i| {
            let theta = i as f64 * PI / 90.0;
            touch(
                385 + (340.0 * theta.cos()).round() as i32,
                385 + (340.0 * theta.sin()).round() as i32,
            )
        })
        .collect();

    drive(&mut fsm, &mut det, &scroll, &frames);
    assert!(matches!(fsm.state(), FsmState::Scrolling { .. }));
}

#[test]
fn radial_sample_jitter_does_not_reject_a_circle() {
    let mut fsm = Fsm::new(385, 385);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();
    let radii = [340.0, 344.0, 338.0, 343.0, 340.0];
    let frames: Vec<_> = radii
        .iter()
        .enumerate()
        .map(|(i, radius)| {
            let theta = i as f64 * PI / 90.0;
            touch(
                385 + (radius * theta.cos()).round() as i32,
                385 + (radius * theta.sin()).round() as i32,
            )
        })
        .collect();

    drive(&mut fsm, &mut det, &scroll, &frames);
    assert!(matches!(fsm.state(), FsmState::Scrolling { .. }));
}

#[test]
fn inconclusive_motion_past_early_threshold_stays_pending() {
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();

    // A short tangent has crossed 7.5°, but three straight samples are
    // not enough evidence to permanently reject a developing circle.
    drive(
        &mut fsm,
        &mut det,
        &scroll,
        &[touch(720, 500), touch(720, 520), touch(720, 540)],
    );
    assert!(matches!(fsm.state(), FsmState::Moving { .. }));
}

#[test]
fn inconclusive_start_can_still_develop_into_a_circle() {
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();

    // Begin with the same short tangent as above, then bend onto the
    // r=220 circle. The old eager fallback entered Passthrough after the
    // third sample and could never recognise the fourth.
    drive(
        &mut fsm,
        &mut det,
        &scroll,
        &[
            touch(720, 500),
            touch(720, 520),
            touch(720, 540),
            touch(713, 553),
        ],
    );
    assert!(matches!(fsm.state(), FsmState::Scrolling { .. }));
}

#[test]
fn sustained_tangential_straight_line_becomes_passthrough() {
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();

    // Once the tangent reaches the larger straight-line decision angle,
    // its lack of curvature is clear enough to choose pointer input.
    drive(
        &mut fsm,
        &mut det,
        &scroll,
        &[
            touch(720, 500),
            touch(720, 520),
            touch(720, 540),
            touch(720, 560),
            touch(720, 580),
        ],
    );
    assert!(matches!(fsm.state(), FsmState::Passthrough));
}

#[test]
fn pending_samples_preheat_scroll_detector() {
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();

    let frames: Vec<_> = (0..=4)
        .map(|i| {
            let theta = i as f64 * PI / 16.0;
            touch(
                500 + (220.0 * theta.cos()).round() as i32,
                500 + (220.0 * theta.sin()).round() as i32,
            )
        })
        .collect();
    let actions = drive(&mut fsm, &mut det, &scroll, &frames);
    assert!(matches!(fsm.state(), FsmState::Scrolling { .. }));
    assert!(
        !actions.is_empty(),
        "held intent samples should contribute to the first scroll tick"
    );
}

#[test]
fn runtime_can_cancel_a_stalled_pending_candidate() {
    let mut fsm = Fsm::new(500, 500);
    let mut detector = CircularDetector::new();
    let scroll = default_scroll();

    let actions = drive(&mut fsm, &mut detector, &scroll, &[touch(720, 500)]);
    assert!(actions.is_empty());
    assert!(matches!(fsm.state(), FsmState::Moving { .. }));

    assert!(fsm.cancel_pending());
    assert_eq!(fsm.state(), FsmState::Passthrough);
    assert!(!fsm.cancel_pending());

    // Passthrough is sticky for the remainder of this contact stream.
    drive(&mut fsm, &mut detector, &scroll, &[touch(680, 550)]);
    assert_eq!(fsm.state(), FsmState::Passthrough);
}

#[test]
fn scrolling_to_debounce_on_lift() {
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();

    let start = touch(720, 500);
    let early_theta = PI / 16.0;
    let early = touch(
        500 + (220.0 * early_theta.cos()).round() as i32,
        500 + (220.0 * early_theta.sin()).round() as i32,
    );
    let theta = PI / 8.0;
    let mid_x = 500 + (220.0 * theta.cos()).round() as i32;
    let mid_y = 500 + (220.0 * theta.sin()).round() as i32;
    let mid = touch(mid_x, mid_y);

    drive(&mut fsm, &mut det, &scroll, &[start, early, mid]);
    assert!(matches!(fsm.state(), FsmState::Scrolling { .. }));

    drive(&mut fsm, &mut det, &scroll, &[lift()]);
    assert!(matches!(fsm.state(), FsmState::Debounce));
}

#[test]
fn stationary_frames_after_scrolling_do_not_emit_more_ticks() {
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();

    // Ten-degree steps at r=220 are farther apart than the detector's
    // 8-unit sample deadband. The first 20 degrees engage Scrolling;
    // the remaining arc fills the curvature history and emits ticks.
    let moving_frames: Vec<_> = (0..=18)
        .map(|i| {
            let theta = i as f64 * PI / 18.0;
            touch(
                500 + (220.0 * theta.cos()).round() as i32,
                500 + (220.0 * theta.sin()).round() as i32,
            )
        })
        .collect();
    let moving_actions = drive(&mut fsm, &mut det, &scroll, &moving_frames);
    assert!(matches!(fsm.state(), FsmState::Scrolling { .. }));
    assert!(
        !moving_actions.is_empty(),
        "the setup gesture must emit at least one scroll tick"
    );

    // Real touchpads may continue sending SYN_REPORT frames with the same
    // coordinates (or sub-deadband jitter) while a finger rests on them.
    // Those frames must not re-integrate the unchanged curvature history.
    let stationary = moving_frames.last().unwrap().clone();
    let stationary_frames = vec![stationary; 100];
    let stationary_actions = drive(&mut fsm, &mut det, &scroll, &stationary_frames);
    assert!(
        stationary_actions.is_empty(),
        "stationary frames unexpectedly emitted {stationary_actions:?}"
    );
    assert!(matches!(fsm.state(), FsmState::Scrolling { .. }));
}

#[test]
fn reverse_vertical_flips_high_resolution_and_legacy_streams() {
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let mut scroll = default_scroll();
    scroll.reverse_vertical = true;

    let frames: Vec<_> = (0..=18)
        .map(|i| {
            let theta = i as f64 * PI / 18.0;
            touch(
                500 + (220.0 * theta.cos()).round() as i32,
                500 + (220.0 * theta.sin()).round() as i32,
            )
        })
        .collect();
    let actions = drive(&mut fsm, &mut det, &scroll, &frames);
    let mut v120 = 0;
    let mut discrete = 0;
    for action in actions {
        if let Action::EmitWheelV(delta) = action {
            v120 += delta.v120;
            discrete += delta.discrete;
        }
    }

    assert!(v120 > 0, "reversed clockwise v120 should be positive");
    assert!(
        discrete > 0,
        "reversed clockwise legacy ticks should be positive"
    );
}

#[test]
fn second_finger_before_capture_prioritizes_multitouch_until_all_lift() {
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();

    drive(&mut fsm, &mut det, &scroll, &[touch(720, 500)]);
    assert!(matches!(fsm.state(), FsmState::Moving { .. }));

    // The primary finger has already swept 90 degrees here, well past
    // the circular threshold. The simultaneous second contact must win
    // arbitration before the candidate can be captured.
    let two_fingers = touch_frame(&[(0, PRIMARY_TRACKING_ID, 500, 720), (1, 101, 600, 600)]);
    let actions = drive(&mut fsm, &mut det, &scroll, &[two_fingers]);
    assert!(actions.is_empty());
    assert!(matches!(fsm.state(), FsmState::MultiTouch));

    // Removing the second finger without an all-up frame must not let the
    // remaining finger enter circular scrolling in the middle of a gesture.
    let far_around_the_rim = touch(500, 720);
    let actions = drive(&mut fsm, &mut det, &scroll, &[far_around_the_rim]);
    assert!(actions.is_empty());
    assert!(matches!(fsm.state(), FsmState::MultiTouch));

    drive(&mut fsm, &mut det, &scroll, &[lift()]);
    assert!(matches!(fsm.state(), FsmState::Idle));
}

#[test]
fn gesture_starting_with_multiple_fingers_is_passthrough_only() {
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();

    let frames: Vec<_> = (0..12)
        .map(|i| {
            let theta = i as f64 * PI / 18.0;
            touch_frame(&[
                (
                    0,
                    PRIMARY_TRACKING_ID,
                    500 + (220.0 * theta.cos()).round() as i32,
                    500 + (220.0 * theta.sin()).round() as i32,
                ),
                (1, 101, 600, 600),
            ])
        })
        .collect();
    let actions = drive(&mut fsm, &mut det, &scroll, &frames);
    assert!(actions.is_empty());
    assert!(matches!(fsm.state(), FsmState::MultiTouch));
}

#[test]
fn captured_scroll_stays_locked_to_original_tracking_id() {
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();
    const CAPTURED_ID: i32 = 101;
    const ADDED_ID: i32 = 202;

    // Deliberately start in slot 1, then add a lower-numbered slot after
    // capture. The recognizer must follow the tracking ID, not slot order.
    let start = touch_frame(&[(1, CAPTURED_ID, 720, 500)]);
    let early_theta = PI / 16.0;
    let early = touch_frame(&[(
        1,
        CAPTURED_ID,
        500 + (220.0 * early_theta.cos()).round() as i32,
        500 + (220.0 * early_theta.sin()).round() as i32,
    )]);
    let theta = PI / 8.0;
    let engage = touch_frame(&[(
        1,
        CAPTURED_ID,
        500 + (220.0 * theta.cos()).round() as i32,
        500 + (220.0 * theta.sin()).round() as i32,
    )]);
    drive(&mut fsm, &mut det, &scroll, &[start, early, engage]);
    assert!(matches!(
        fsm.state(),
        FsmState::Scrolling {
            tracking_id: CAPTURED_ID,
            slot: 1
        }
    ));

    // Keep moving the captured finger around the rim while a new finger
    // occupies slot 0. Circular scrolling remains active and emits ticks.
    let with_added_finger: Vec<_> = (3..=18)
        .map(|i| {
            let theta = i as f64 * PI / 18.0;
            touch_frame(&[
                (0, ADDED_ID, 510, 510),
                (
                    1,
                    CAPTURED_ID,
                    500 + (220.0 * theta.cos()).round() as i32,
                    500 + (220.0 * theta.sin()).round() as i32,
                ),
            ])
        })
        .collect();
    let actions = drive(&mut fsm, &mut det, &scroll, &with_added_finger);
    assert!(
        !actions.is_empty(),
        "the captured finger should keep producing circular scroll ticks"
    );

    // If the captured finger lifts first, never splice the remaining
    // finger into its trajectory. Ownership is retained until all-up.
    let replacement_motion: Vec<_> = (0..20)
        .map(|i| {
            let theta = i as f64 * PI / 9.0;
            touch_frame(&[(
                0,
                ADDED_ID,
                500 + (220.0 * theta.cos()).round() as i32,
                500 + (220.0 * theta.sin()).round() as i32,
            )])
        })
        .collect();
    let actions = drive(&mut fsm, &mut det, &scroll, &replacement_motion);
    assert!(actions.is_empty());
    assert!(matches!(
        fsm.state(),
        FsmState::Scrolling {
            tracking_id: CAPTURED_ID,
            slot: 1
        }
    ));

    drive(&mut fsm, &mut det, &scroll, &[lift()]);
    assert!(matches!(fsm.state(), FsmState::Debounce));
}

#[test]
fn force_idle_resets_state() {
    // Watchdog path: after force_idle the FSM is back at Idle even if
    // it was mid-Scrolling.
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();

    let start = touch(720, 500);
    let early_theta = PI / 16.0;
    let early = touch(
        500 + (220.0 * early_theta.cos()).round() as i32,
        500 + (220.0 * early_theta.sin()).round() as i32,
    );
    let theta = PI / 8.0;
    let mid = touch(
        500 + (220.0 * theta.cos()).round() as i32,
        500 + (220.0 * theta.sin()).round() as i32,
    );
    drive(&mut fsm, &mut det, &scroll, &[start, early, mid]);
    assert!(matches!(fsm.state(), FsmState::Scrolling { .. }));

    fsm.force_idle(&mut det);
    assert!(matches!(fsm.state(), FsmState::Idle));
}

#[test]
fn debounce_to_idle_on_next_frame_no_timer() {
    // Debounce always exits to Idle on the next frame regardless of
    // whether the finger is now down or up.
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();

    let start = touch(720, 500);
    let early_theta = PI / 16.0;
    let early = touch(
        500 + (220.0 * early_theta.cos()).round() as i32,
        500 + (220.0 * early_theta.sin()).round() as i32,
    );
    let theta = PI / 8.0;
    let mid_x = 500 + (220.0 * theta.cos()).round() as i32;
    let mid_y = 500 + (220.0 * theta.sin()).round() as i32;
    let mid = touch(mid_x, mid_y);

    drive(&mut fsm, &mut det, &scroll, &[start, early, mid, lift()]);
    assert!(matches!(fsm.state(), FsmState::Debounce));

    drive(&mut fsm, &mut det, &scroll, &[lift()]);
    assert!(matches!(fsm.state(), FsmState::Idle));
}

#[test]
fn debounce_to_idle_even_if_finger_back_down() {
    // The Debounce state has no re-engagement path. If a finger is down
    // on the very next frame, we still go to Idle this frame; the frame
    // *after* that re-runs the fresh-touch classifier.
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let scroll = default_scroll();

    let start = touch(720, 500);
    let early_theta = PI / 16.0;
    let early = touch(
        500 + (220.0 * early_theta.cos()).round() as i32,
        500 + (220.0 * early_theta.sin()).round() as i32,
    );
    let theta = PI / 8.0;
    let mid_x = 500 + (220.0 * theta.cos()).round() as i32;
    let mid_y = 500 + (220.0 * theta.sin()).round() as i32;
    let mid = touch(mid_x, mid_y);

    drive(&mut fsm, &mut det, &scroll, &[start, early, mid, lift()]);
    assert!(matches!(fsm.state(), FsmState::Debounce));

    drive(&mut fsm, &mut det, &scroll, &[touch(720, 500)]);
    assert!(matches!(fsm.state(), FsmState::Idle));
}

#[test]
fn disabled_scroll_holds_idle() {
    // When scroll.enable = false the daemon keeps reading frames, but
    // the FSM never advances past Idle.
    let mut fsm = Fsm::new(500, 500);
    let mut det = CircularDetector::new();
    let mut scroll = default_scroll();
    scroll.enable = false;
    drive(
        &mut fsm,
        &mut det,
        &scroll,
        &[touch(720, 500), touch(700, 520), lift()],
    );
    assert!(matches!(fsm.state(), FsmState::Idle));
}
