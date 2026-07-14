// Synthetic tests for the circular scrolling detector.

use std::f64::consts::PI;

use letsnote_wheelpad::detector::{
    engagement_swept_angle, radial_gate_ok, within_horizontal_arc, CircularDetector, TouchSample,
    WheelDelta, TRIGGER_ANGLE,
};

/// Generate N samples around a circle. Y is screen-down, so a positive
/// sweep is clockwise in screen space (consistent with WheelPad's
/// internal sign convention).
fn circle_samples(
    center_x: i32,
    center_y: i32,
    r: f64,
    start_rad: f64,
    total_sweep_rad: f64,
    n: usize,
) -> Vec<TouchSample> {
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64 / (n - 1).max(1) as f64;
        let theta = start_rad + total_sweep_rad * t;
        let x = center_x + (r * theta.cos()).round() as i32;
        let y = center_y + (r * theta.sin()).round() as i32;
        out.push(TouchSample { x, y });
    }
    out
}

fn run_gesture(samples: &[TouchSample], sensitivity: i32) -> WheelDelta {
    let mut d = CircularDetector::new();
    d.on_gesture_start();
    let mut total = WheelDelta::default();
    for s in samples {
        if d.push_if_moved(*s) {
            total += d.step(sensitivity);
        }
    }
    total
}

#[test]
fn push_if_moved_reports_deadband_and_stationary_samples() {
    let mut d = CircularDetector::new();
    let start = TouchSample { x: 100, y: 100 };

    assert!(d.push_if_moved(start), "the first sample must be stored");
    assert!(
        !d.push_if_moved(start),
        "an identical stationary sample must be rejected"
    );
    assert!(
        !d.push_if_moved(TouchSample { x: 108, y: 100 }),
        "movement exactly on the 8-unit deadband must be rejected"
    );
    assert!(
        d.push_if_moved(TouchSample { x: 109, y: 100 }),
        "movement past the deadband must be stored"
    );
}

#[test]
fn full_clockwise_circle_emits_negative_ticks() {
    // A clockwise circle in screen-Y-down integrates positive radians;
    // positive overflow returns negative ticks (Windows internal sign
    // convention). Passing the value through to uinput unchanged
    // scrolls DOWN according to the detector's internal sign convention.
    let samples = circle_samples(500, 500, 200.0, 0.0, 2.0 * PI, 40);
    let total = run_gesture(&samples, 0);
    assert!(
        total.discrete < 0,
        "clockwise circle should produce negative ticks (got {total:?})"
    );
    assert!(
        total.discrete.abs() >= 1,
        "full circle should produce at least one tick"
    );
    assert!(total.v120 < 0, "clockwise v120 must have the same sign");
}

#[test]
fn full_counterclockwise_circle_emits_positive_ticks() {
    let samples = circle_samples(500, 500, 200.0, 0.0, -2.0 * PI, 40);
    let total = run_gesture(&samples, 0);
    assert!(
        total.discrete > 0,
        "counterclockwise circle should produce positive ticks (got {total:?})"
    );
    assert!(total.v120 > 0);
}

#[test]
fn straight_line_produces_zero_ticks() {
    let samples: Vec<_> = (0..20)
        .map(|i| TouchSample {
            x: 100 + i * 30,
            y: 500,
        })
        .collect();
    let total = run_gesture(&samples, 0);
    assert_eq!(
        total,
        WheelDelta::default(),
        "straight line must not scroll"
    );
}

#[test]
fn zig_zag_does_not_engage() {
    // Alternating sign deltas larger than π/4 → per-delta π/4 reject
    // truncates history below 3 valid deltas, so step returns 0
    // every packet.
    let zigs: Vec<TouchSample> = [
        (100, 100),
        (130, 200),
        (160, 100),
        (190, 200),
        (220, 100),
        (250, 200),
        (280, 100),
        (310, 200),
        (340, 100),
        (370, 200),
        (400, 100),
        (430, 200),
        (460, 100),
        (490, 200),
        (520, 100),
        (550, 200),
        (580, 100),
        (610, 200),
        (640, 100),
        (670, 200),
    ]
    .iter()
    .map(|(x, y)| TouchSample { x: *x, y: *y })
    .collect();
    let total = run_gesture(&zigs, 0);
    assert_eq!(
        total,
        WheelDelta::default(),
        "zig-zag should not produce wheel movement (got {total:?})"
    );
}

#[test]
fn half_circle_does_not_exceed_full() {
    let full = run_gesture(&circle_samples(500, 500, 200.0, 0.0, 2.0 * PI, 40), 0);
    let half = run_gesture(&circle_samples(500, 500, 200.0, 0.0, PI, 20), 0);
    assert!(full.discrete < 0 && half.discrete < 0);
    assert!(
        half.discrete.abs() <= full.discrete.abs(),
        "half ({half:?}) should not exceed full ({full:?})"
    );
}

#[test]
fn reverse_circle_has_opposite_sign() {
    let cw = run_gesture(&circle_samples(500, 500, 200.0, 0.0, 2.0 * PI, 40), 0);
    let ccw = run_gesture(&circle_samples(500, 500, 200.0, 0.0, -2.0 * PI, 40), 0);
    assert!(cw.discrete < 0 && ccw.discrete > 0, "cw={cw:?} ccw={ccw:?}");
}

#[test]
fn sign_convention_positive_overflow_yields_negative_tick() {
    // Populate history, then manually push the accumulator past +π and
    // verify a subsequent step drains negative. This sidesteps any
    // dependence on real gesture geometry; it directly exercises the
    // emit branch.
    let mut d = CircularDetector::new();
    d.on_gesture_start();
    for s in circle_samples(500, 500, 200.0, 0.0, 0.5, 5) {
        if d.push_if_moved(s) {
            let _ = d.step(0);
        }
    }
    d.set_accumulator_for_test(PI + 0.01);
    let delta = d.step(0);
    assert_eq!(delta.discrete, -1);
}

#[test]
fn high_resolution_movement_precedes_a_legacy_notch() {
    // Three points are also the circular-intent minimum. The high-res path
    // can use their first curvature delta without waiting for the legacy
    // detector's five-point noise gate.
    let samples = circle_samples(385, 385, 340.0, 0.0, 8.0 * PI / 180.0, 3);
    let delta = run_gesture(&samples, 0);

    assert_ne!(delta.v120, 0, "the early arc should emit a v120 fraction");
    assert_eq!(
        delta.discrete, 0,
        "the same early arc should not yet emit a whole legacy notch"
    );
}

#[test]
fn stationary_pause_preserves_history_for_high_resolution_resume() {
    let mut detector = CircularDetector::new();
    let initial = circle_samples(385, 385, 340.0, 0.0, 8.0 * PI / 180.0, 3);
    let mut initial_delta = WheelDelta::default();
    for sample in &initial {
        if detector.push_if_moved(*sample) {
            initial_delta += detector.step(0);
        }
    }
    assert_ne!(initial_delta.v120, 0);

    let stationary = *initial.last().unwrap();
    for _ in 0..100 {
        assert!(!detector.push_if_moved(stationary));
    }

    let resumed = circle_samples(385, 385, 340.0, 10.0 * PI / 180.0, 0.0, 1)[0];
    assert!(detector.push_if_moved(resumed));
    let resumed_delta = detector.step(0);
    assert_ne!(
        resumed_delta.v120, 0,
        "resuming the same contact should not refill detector history"
    );
}

#[test]
fn sub_v120_rounding_residue_is_not_lost() {
    // Each accepted step contributes less than half a v120 unit at the
    // lowest sensitivity. Retained floating-point residue must still turn
    // many such steps into visible integer high-resolution movement.
    let samples = circle_samples(0, 0, 5000.0, 0.0, 0.2, 101);
    let delta = run_gesture(&samples, -4);

    assert!(
        (-20..=-17).contains(&delta.v120),
        "unexpected accumulated fractional movement: {delta:?}"
    );
    assert_eq!(delta.discrete, 0);
}

#[test]
fn extended_low_sensitivity_reduces_high_resolution_distance() {
    let samples = circle_samples(500, 500, 200.0, 0.0, PI, 40);
    let lowest = run_gesture(&samples, -4);
    let previous_lowest = run_gesture(&samples, -2);

    assert!(lowest.v120.abs() < previous_lowest.v120.abs());
    assert!(lowest.v120.abs() * 2 <= previous_lowest.v120.abs() + 1);
}

#[test]
fn engagement_swept_angle_is_signed() {
    let start = TouchSample { x: 200, y: 0 };
    let end = TouchSample {
        x: (200.0 * (PI / 4.0).cos()).round() as i32,
        y: (200.0 * (PI / 4.0).sin()).round() as i32,
    };
    let swept = engagement_swept_angle(0, 0, start, end);
    assert!(swept > 0.0);
    assert!((swept - PI / 4.0).abs() < 0.01);

    let swept_rev = engagement_swept_angle(0, 0, end, start);
    assert!(swept_rev < 0.0);
}

#[test]
fn engagement_threshold_pi_over_24() {
    // Use r = 5000 instead of 200 so i32 rounding error
    // (arctan(0.5 / r)) is well below the ±0.001 rad margin we use to
    // straddle the trigger. At r = 200 the rounding error was ~0.0025
    // rad, larger than the margin, which made the boundary test
    // flaky.
    const R: f64 = 5000.0;
    let start = TouchSample { x: R as i32, y: 0 };

    // Just past π/24 (7.5°) → above trigger.
    let theta = TRIGGER_ANGLE + 0.001;
    let end_above = TouchSample {
        x: (R * theta.cos()).round() as i32,
        y: (R * theta.sin()).round() as i32,
    };
    assert!(engagement_swept_angle(0, 0, start, end_above).abs() > TRIGGER_ANGLE);

    // Just shy of π/24 → below trigger.
    let theta = TRIGGER_ANGLE - 0.001;
    let end_below = TouchSample {
        x: (R * theta.cos()).round() as i32,
        y: (R * theta.sin()).round() as i32,
    };
    assert!(engagement_swept_angle(0, 0, start, end_below).abs() < TRIGGER_ANGLE);
}

#[test]
fn radial_gate_default_width_requires_outer_ring() {
    // DetectAreaWidth = 0 → r ≥ 200 units from center.
    assert!(!radial_gate_ok(500, 500, TouchSample { x: 500, y: 500 }, 0));
    assert!(!radial_gate_ok(500, 500, TouchSample { x: 600, y: 500 }, 0)); // r = 100
    assert!(radial_gate_ok(500, 500, TouchSample { x: 700, y: 500 }, 0)); // r = 200
    assert!(radial_gate_ok(500, 500, TouchSample { x: 800, y: 500 }, 0)); // r = 300
}

#[test]
fn radial_gate_max_width_engages_anywhere() {
    // DetectAreaWidth = 10 → r ≥ 0 (whole pad active).
    assert!(radial_gate_ok(500, 500, TouchSample { x: 500, y: 500 }, 10));
    assert!(radial_gate_ok(500, 500, TouchSample { x: 600, y: 500 }, 10));
}

#[test]
fn horizontal_arc_default_45_to_135_is_bottom_edge() {
    // Defaults: horizontal_start=2 → 45°, horizontal_end=6 → 135°.
    // In screen-Y-down, the wedge 45°→90°→135° is the BOTTOM half-rim.
    let south = TouchSample { x: 500, y: 700 }; // dy=+200 → 90°
    let north = TouchSample { x: 500, y: 300 }; // dy=-200 → -90° = 270°
    let east = TouchSample { x: 700, y: 500 };
    let west = TouchSample { x: 300, y: 500 };
    assert!(within_horizontal_arc(500, 500, south, 2, 6));
    assert!(!within_horizontal_arc(500, 500, north, 2, 6));
    assert!(!within_horizontal_arc(500, 500, east, 2, 6));
    assert!(!within_horizontal_arc(500, 500, west, 2, 6));
}

#[test]
fn horizontal_arc_wraparound() {
    // start > end → arc wraps across 0°. start=14 (315°), end=2 (45°)
    // → 315°..360° + 0°..45°.
    let east = TouchSample { x: 700, y: 500 }; // 0°
    let southeast = TouchSample {
        x: 500 + (200.0_f64 * (PI / 4.0).cos()).round() as i32,
        y: 500 + (200.0_f64 * (PI / 4.0).sin()).round() as i32,
    }; // 45°
    let southwest = TouchSample {
        x: 500 + (200.0_f64 * (3.0 * PI / 4.0).cos()).round() as i32,
        y: 500 + (200.0_f64 * (3.0 * PI / 4.0).sin()).round() as i32,
    }; // 135°
    assert!(within_horizontal_arc(500, 500, east, 14, 2));
    assert!(within_horizontal_arc(500, 500, southeast, 14, 2));
    assert!(!within_horizontal_arc(500, 500, southwest, 14, 2));
}
