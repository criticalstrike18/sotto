// Lenis-parity smoothness tests for SmoothScrollEngine.
// See desktop/SCROLL_ISSUES.md. Each test encodes one clause of the contract.
// Engine uses Instant::now internally, so ticks are paced with real sleeps
// (~8ms ≈ production timer, ~16ms ≈ 60fps display rate).

use sotto_desktop::smooth_scroll::SmoothScrollEngine;
use std::time::{Duration, Instant};

fn test_engine() -> SmoothScrollEngine {
    let mut e = SmoothScrollEngine::new();
    // Roomy scroll area: 2000px content, 600px viewport => max_scroll 1400.
    e.set_dimensions(2000.0, 600.0);
    e
}

/// Pump ticks at ~60fps until settle or timeout. Returns (frames, positions).
fn pump_to_settle(e: &mut SmoothScrollEngine, frame: Duration, timeout: Duration) -> (usize, Vec<f32>) {
    let start = Instant::now();
    let mut frames = 0usize;
    let mut positions = vec![e.current_y()];
    while start.elapsed() < timeout {
        std::thread::sleep(frame);
        match e.tick() {
            Some(y) => {
                frames += 1;
                positions.push(y);
            }
            None => break,
        }
        if frames > 600 {
            break;
        }
    }
    (frames, positions)
}

fn max_step(positions: &[f32]) -> f32 {
    positions
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max)
}

#[test]
fn test_single_notch_down_converges_to_120px() {
    let mut e = test_engine();
    e.on_wheel_accelerated_dt(-120.0, 1.0); // scroll down one notch
    let (frames, positions) = pump_to_settle(&mut e, Duration::from_millis(16), Duration::from_secs(2));
    assert!(frames > 2, "must animate over multiple frames, not snap (got {frames})");
    let final_y = *positions.last().unwrap();
    assert!((final_y - -120.0).abs() < 0.5, "single notch must land at -120, got {final_y}");
}

#[test]
fn test_single_notch_up_from_bottom() {
    let mut e = test_engine();
    // Go to bottom first.
    e.on_wheel_accelerated_dt(-10000.0, 1.0);
    let _ = pump_to_settle(&mut e, Duration::from_millis(16), Duration::from_secs(2));
    assert!(e.current_y() < -1000.0);
    // One notch up.
    let before = e.current_y();
    e.on_wheel_accelerated_dt(120.0, 1.0);
    let (_, positions) = pump_to_settle(&mut e, Duration::from_millis(16), Duration::from_secs(2));
    let after = *positions.last().unwrap();
    assert!((after - (before + 120.0)).abs() < 0.5, "one notch up must add +120, {before}->{after}");
}

#[test]
fn test_motion_is_monotonic_no_jitter() {
    let mut e = test_engine();
    e.on_wheel_accelerated_dt(-240.0, 1.0);
    let (_, positions) = pump_to_settle(&mut e, Duration::from_millis(8), Duration::from_secs(2));
    for w in positions.windows(2) {
        assert!(w[1] <= w[0] + 0.001, "scrolling down must never move back up: {w:?}");
    }
}

#[test]
fn test_no_overshoot_past_target() {
    let mut e = test_engine();
    e.on_wheel_accelerated_dt(-120.0, 1.0);
    let (_, positions) = pump_to_settle(&mut e, Duration::from_millis(8), Duration::from_secs(2));
    for y in &positions {
        assert!(*y >= -120.5, "exponential lerp must not overshoot target: {y}");
        assert!(*y <= 0.5, "must not bounce above start: {y}");
    }
}

#[test]
fn test_per_frame_step_bounded_single_notch() {
    let mut e = test_engine();
    e.on_wheel_accelerated_dt(-120.0, 1.0);
    let (_, positions) = pump_to_settle(&mut e, Duration::from_millis(8), Duration::from_secs(2));
    let step = max_step(&positions);
    assert!(step < 65.0, "single notch max step must be Lenis-like (<65px), got {step}");
    assert!(step > 0.5, "must actually move each frame, got {step}");
}

#[test]
fn test_settles_within_lenis_time_budget() {
    let mut e = test_engine();
    e.on_wheel_accelerated_dt(-360.0, 1.0);
    let start = Instant::now();
    let (frames, _) = pump_to_settle(&mut e, Duration::from_millis(16), Duration::from_secs(2));
    let elapsed = start.elapsed();
    assert!(elapsed < Duration::from_millis(1500), "must settle in <1.5s like Lenis, took {elapsed:?}");
    assert!(frames >= 3, "must take >=3 frames (not instant), took {frames}");
    // After settle, tick returns None (engine at rest).
    assert!(e.tick().is_none(), "settled engine must return None");
}

#[test]
fn test_top_clamp_never_positive() {
    let mut e = test_engine();
    e.on_wheel_accelerated_dt(120.0, 1.0); // scroll up while already at top
    let (_, positions) = pump_to_settle(&mut e, Duration::from_millis(8), Duration::from_millis(500));
    for y in positions {
        assert!(y <= 0.001, "must never scroll above top (y>0): {y}");
    }
    assert!(e.current_y().abs() < 0.5);
}

#[test]
fn test_bottom_clamp_never_beyond_max() {
    let mut e = test_engine(); // max_scroll = 1400
    e.on_wheel_accelerated_dt(-10000.0, 1.0);
    let (_, positions) = pump_to_settle(&mut e, Duration::from_millis(8), Duration::from_secs(2));
    for y in positions {
        assert!(y >= -1400.5, "must never scroll past bottom: {y}");
    }
    assert!((e.current_y() - -1400.0).abs() < 0.5);
}

#[test]
fn test_rapid_burst_accumulates_then_glides() {
    let mut e = test_engine();
    for _ in 0..5 {
        e.on_wheel_accelerated_dt(-120.0, 1.0); // 5 notches in quick succession
    }
    let (_, positions) = pump_to_settle(&mut e, Duration::from_millis(8), Duration::from_secs(3));
    let final_y = *positions.last().unwrap();
    assert!((final_y - -600.0).abs() < 1.0, "5x burst must target -600, got {final_y}");
    let step = max_step(&positions);
    assert!(step < 130.0, "burst max step bounded (no teleport), got {step}");
}

#[test]
fn test_high_res_trackpad_deltas_proportional() {
    let mut e = test_engine();
    e.on_wheel_accelerated_dt(-30.0, 1.0); // small precision-touchpad delta
    let (_, positions) = pump_to_settle(&mut e, Duration::from_millis(8), Duration::from_secs(2));
    let final_y = *positions.last().unwrap();
    assert!((final_y - -30.0).abs() < 0.5, "small delta must move proportionally, got {final_y}");
}

#[test]
fn test_direction_reversal_mid_flight_no_snap() {
    let mut e = test_engine();
    e.on_wheel_accelerated_dt(-240.0, 1.0);
    std::thread::sleep(Duration::from_millis(50));
    let _ = e.tick();
    let mid = e.current_y();
    assert!(mid < -1.0, "must have started moving down, at {mid}");
    e.on_wheel_accelerated_dt(120.0, 1.0); // reverse one notch mid-flight
    let (_, positions) = pump_to_settle(&mut e, Duration::from_millis(8), Duration::from_secs(2));
    let step = max_step(&positions);
    assert!(step < 65.0, "reversal must not teleport, max step {step}");
    let final_y = *positions.last().unwrap();
    assert!((final_y - -120.0).abs() < 1.0, "net -240+120 must land at -120, got {final_y}");
}

#[test]
fn test_stall_dt_clamp_no_teleport() {
    let mut e = test_engine();
    e.on_wheel_accelerated_dt(-120.0, 1.0);
    // Simulate blocked UI thread (200ms stall, see SCROLL_ISSUES.md Issue 1).
    std::thread::sleep(Duration::from_millis(200));
    let y_after_stall = e.tick().unwrap_or(e.current_y());
    // dt clamped to 0.05 => factor = 1-exp(-15*0.05) ≈ 0.53 => step ≈ 63px max.
    let step = y_after_stall.abs();
    assert!(step < 90.0, "post-stall step must be clamped (no teleport), got {step}");
    assert!(y_after_stall > -120.0, "must not snap instantly to target after stall");
}
#[test]
fn test_scroll_by_keyboard_navigation_step() {
    let mut e = test_engine();
    e.scroll_by(-80.0); // ArrowDown
    let (frames, positions) = pump_to_settle(&mut e, Duration::from_millis(16), Duration::from_secs(2));
    assert!(frames > 2);
    let final_y = *positions.last().unwrap();
    assert!((final_y - -80.0).abs() < 0.5);

    // ArrowUp
    e.scroll_by(80.0);
    let (_, positions) = pump_to_settle(&mut e, Duration::from_millis(16), Duration::from_secs(2));
    let final_y = *positions.last().unwrap();
    assert!(final_y.abs() < 0.5);
}

#[test]
fn test_scroll_to_programmatic_glide() {
    let mut e = test_engine();
    e.scroll_to(-500.0);
    let (_, positions) = pump_to_settle(&mut e, Duration::from_millis(16), Duration::from_secs(2));
    let final_y = *positions.last().unwrap();
    assert!((final_y - -500.0).abs() < 0.5);

    // Scroll to top
    e.scroll_to_top();
    let (_, positions) = pump_to_settle(&mut e, Duration::from_millis(16), Duration::from_secs(2));
    let final_y = *positions.last().unwrap();
    assert!(final_y.abs() < 0.5);

    // Scroll to bottom (-1400)
    e.scroll_to_bottom();
    let (_, positions) = pump_to_settle(&mut e, Duration::from_millis(16), Duration::from_secs(2));
    let final_y = *positions.last().unwrap();
    assert!((final_y - -1400.0).abs() < 0.5);
}

#[test]
fn test_page_down_and_up() {
    let mut e = test_engine(); // visible_h = 600, 80% = 480
    e.scroll_page_down();
    let (_, positions) = pump_to_settle(&mut e, Duration::from_millis(16), Duration::from_secs(2));
    let final_y = *positions.last().unwrap();
    assert!((final_y - -480.0).abs() < 0.5);

    e.scroll_page_up();
    let (_, positions) = pump_to_settle(&mut e, Duration::from_millis(16), Duration::from_secs(2));
    let final_y = *positions.last().unwrap();
    assert!(final_y.abs() < 0.5);
}

#[test]
fn test_scroll_progress_normalization() {
    let mut e = test_engine(); // max_scroll = 1400
    assert_eq!(e.progress(), 0.0);

    e.scroll_to(-700.0);
    let _ = pump_to_settle(&mut e, Duration::from_millis(16), Duration::from_secs(2));
    assert!((e.progress() - 0.5).abs() < 0.02);

    e.scroll_to_bottom();
    let _ = pump_to_settle(&mut e, Duration::from_millis(16), Duration::from_secs(2));
    assert!((e.progress() - 1.0).abs() < 0.01);
}

#[test]
fn test_micro_jitter_deadband_rejection() {
    let mut e = test_engine();
    e.on_wheel_accelerated_dt(0.1, 1.0); // palm noise below JITTER_DEADBAND_PX
    assert_eq!(e.target_y(), 0.0);
    assert!(!e.is_animating());
}
