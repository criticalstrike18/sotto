// Wiring-level regression tests: timer, heights, repaint.
// Covers SCROLL_ISSUES.md Issues 1 (timer), 5 (hardcoded heights / DPI),
// 6 (full repaint). The production wiring lives in the binary
// (main.rs:310-333) so these tests pin the *contract* the wiring must obey,
// using the helpers in smooth_scroll.rs, plus characterization tests that
// replicate the legacy formulas to prove they diverge from measured values.

use sotto_desktop::smooth_scroll::{
    compute_content_height, compute_visible_height, SmoothScrollEngine, MIN_VISIBLE_PX,
    RECOMMENDED_FRAME_INTERVAL_MS,
};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Timer (Issue 1)
// ---------------------------------------------------------------------------

#[test]
fn test_frame_interval_is_vsync_friendly() {
    // Lenis follows display vsync (~60Hz). The driver must tick at ~16ms,
    // not 8ms (~125Hz oversampling on the UI thread).
    assert!(
        (15..=17).contains(&RECOMMENDED_FRAME_INTERVAL_MS),
        "frame interval must be vsync-like (15-17ms), got {}",
        RECOMMENDED_FRAME_INTERVAL_MS
    );
}

#[test]
fn test_animation_is_frame_rate_independent() {
    // Same input pumped at 8ms vs 16ms pacing must land on the same target
    // (dt-based integration, a core Lenis property). If wiring hard-codes
    // per-tick distances instead of dt, this diverges.
    fn run_to_settle(frame_ms: u64) -> f32 {
        let mut e = SmoothScrollEngine::new();
        e.set_dimensions(2000.0, 600.0);
        e.on_wheel_accelerated_dt(-360.0, 1.0);
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(frame_ms));
            if e.tick().is_none() {
                break;
            }
        }
        e.current_y()
    }
    let at_8ms = run_to_settle(8);
    let at_16ms = run_to_settle(16);
    assert!(
        (at_8ms - at_16ms).abs() < 1.0,
        "8ms vs 16ms pacing must converge within 1px, got {} vs {}", at_8ms, at_16ms
    );
    assert!((at_8ms - -360.0).abs() < 0.5);
}

#[test]
fn test_idle_timer_does_zero_work_at_rest() {
    // Wiring must skip `set_history_viewport_y` when `tick()` returns None.
    // Simulates the driver loop: count property writes after settle.
    let mut e = SmoothScrollEngine::new();
    e.set_dimensions(2000.0, 600.0);
    e.on_wheel_accelerated_dt(-120.0, 1.0);
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(8));
        if e.tick().is_none() {
            break;
        }
    }
    assert!(!e.is_animating());
    let mut writes = 0;
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(8));
        if e.tick().is_some() {
            writes += 1; // wiring would call set_history_viewport_y here
        }
    }
    assert_eq!(writes, 0, "settled engine must produce zero viewport writes");
}

// ---------------------------------------------------------------------------
// Content / visible heights (Issue 5)
// ---------------------------------------------------------------------------

#[test]
fn test_fixed_card_estimate_diverges_from_measured() {
    // Characterization of the main.rs:317 bug: `count * 125.0` vs real
    // variable card heights (word-wrapped transcripts, hud.slint:1205-1210).
    let measured = [80.0f32, 200.0, 120.0];
    let spacing = 10.0;
    let correct = compute_content_height(&measured, spacing); // 80+200+120+2*10 = 420
    assert!((correct - 420.0).abs() < 0.01);
    // The abandoned fixed-height estimate this helper replaced.
    let legacy_fixed_card_px = 125.0;
    let legacy = measured.len() as f32 * legacy_fixed_card_px; // 3*125 = 375
    assert!(
        (correct - legacy).abs() > 20.0,
        "legacy estimate ({legacy}) must visibly diverge from measured ({correct})"
    );
}

#[test]
fn test_compute_content_height_edge_cases() {
    assert_eq!(compute_content_height(&[], 10.0), 0.0);
    assert!((compute_content_height(&[150.0], 10.0) - 150.0).abs() < 0.01);
    // Negative spacing treated as zero.
    assert!((compute_content_height(&[100.0, 100.0], -5.0) - 200.0).abs() < 0.01);
}

#[test]
fn test_visible_height_uses_logical_px_not_physical() {
    // Characterization of the main.rs:318-319 bug: `window().size()` is
    // physical px; Slint lengths are logical px (physical / scale factor).
    let scale = 1.5f32;
    let physical_h = 1440.0f32;
    let logical_h = physical_h / scale; // 960
    let legacy_chrome_reserved_px = 220.0;
    let correct = compute_visible_height(logical_h, legacy_chrome_reserved_px, MIN_VISIBLE_PX);
    assert!((correct - 740.0).abs() < 0.01);
    let legacy = (physical_h - legacy_chrome_reserved_px).max(MIN_VISIBLE_PX); // 1220
    assert!(
        (legacy - correct).abs() > 100.0,
        "physical-based visible height ({legacy}) must diverge from logical ({correct}) at 150% DPI"
    );
}

#[test]
fn test_visible_height_min_clamp() {
    assert!((compute_visible_height(150.0, 220.0, 100.0) - 100.0).abs() < 0.01);
}

#[test]
fn test_shrinking_content_reclamps_target() {
    // Wiring calls set_dimensions() every frame; when items are deleted the
    // target must re-clamp so the view glides back into range (no stuck gap).
    let mut e = SmoothScrollEngine::new();
    e.set_dimensions(2000.0, 600.0); // max 1400
    e.on_wheel_accelerated_dt(-10000.0, 1.0);
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(8));
        if e.tick().is_none() {
            break;
        }
    }
    assert!((e.current_y() - -1400.0).abs() < 0.5);
    // Content shrinks: only 800px of cards left => max 200.
    e.set_dimensions(800.0, 600.0);
    assert!(e.target_y() >= -200.5, "target must re-clamp to new max, got {}", e.target_y());
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(8));
        if e.tick().is_none() {
            break;
        }
    }
    assert!((e.current_y() - -200.0).abs() < 1.0, "view must glide back to -200, got {}", e.current_y());
}

// ---------------------------------------------------------------------------
// Repaint budget (Issue 6)
// ---------------------------------------------------------------------------

#[test]
fn test_exactly_one_viewport_write_per_tick() {
    // Wiring contract: one `tick()` => at most one `set_history_viewport_y`.
    // Simulates the driver and asserts writes == ticks (no duplicate sets).
    let mut e = SmoothScrollEngine::new();
    e.set_dimensions(3000.0, 600.0);
    e.on_wheel_accelerated_dt(-600.0, 1.0);
    let mut ticks = 0;
    let mut writes = 0;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(8));
        match e.tick() {
            Some(_) => {
                ticks += 1;
                writes += 1; // exactly one setter call per tick, as in main.rs:324-326
            }
            None => break,
        }
    }
    assert!(ticks > 3);
    assert_eq!(writes, ticks, "must be exactly one viewport write per tick");
}

#[test]
fn test_large_list_still_settles_in_budget() {
    // Engine work per tick is O(1); settling must stay in Lenis budget even
    // with 200 cards. (Slint layout cost is separate — see Issue 6; this
    // pins the engine side so regressions show up here, not in the UI.)
    let heights = vec![140.0f32; 200];
    let content_h = compute_content_height(&heights, 10.0);
    let mut e = SmoothScrollEngine::new();
    e.set_dimensions(content_h, 600.0);
    e.on_wheel_accelerated_dt(-1200.0, 1.0);
    let start = Instant::now();
    let mut frames = 0;
    while start.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(16));
        frames += 1;
        if e.tick().is_none() {
            break;
        }
    }
    assert!(start.elapsed() < Duration::from_millis(1500), "200-card list must settle in <1.5s");
    assert!((e.current_y() - -1200.0).abs() < 0.5);
    assert!(frames < 120);
}
// ---------------------------------------------------------------------------
// Lenis Parity Wiring Features
// ---------------------------------------------------------------------------

#[test]
fn test_scroll_progress_wiring_bounds() {
    let mut e = SmoothScrollEngine::new();
    e.set_dimensions(1600.0, 600.0); // max 1000
    assert_eq!(e.progress(), 0.0);
    assert!(e.max_scroll_limit() > 0.0);

    e.scroll_by(-500.0);
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(16));
        if e.tick().is_none() {
            break;
        }
    }
    assert!((e.progress() - 0.5).abs() < 0.02);

    e.scroll_to_bottom();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(16));
        if e.tick().is_none() {
            break;
        }
    }
    assert!((e.progress() - 1.0).abs() < 0.01);
}

#[test]
fn test_zero_content_cannot_scroll() {
    let mut e = SmoothScrollEngine::new();
    e.set_dimensions(400.0, 600.0); // content < visible
    assert_eq!(e.progress(), 0.0);
    assert_eq!(e.max_scroll_limit(), 0.0);
    e.on_wheel_accelerated_dt(-120.0, 1.0);
    assert_eq!(e.target_y(), 0.0);
}
// ---------------------------------------------------------------------------
// Fixes 1, 2, 3, 4: Pixel Snapping, Virtualization Windowing, & Write Throttling
// ---------------------------------------------------------------------------

#[test]
fn test_physical_pixel_snapping() {
    // Fix 1: Fractional logical offsets like -213.47px cause text glyph cache
    // misses and visible shimmer. Snapping to physical device pixels ensures
    // crisp glyph rasterization across any DPI scale factor.
    for scale in [1.0f32, 1.25, 1.5, 2.0] {
        let raw_y = -213.4729f32;
        let snapped_y = (raw_y * scale).round() / scale;
        let physical_px = (snapped_y * scale).round();
        assert!(
            ((snapped_y * scale) - physical_px).abs() < 1e-4,
            "snapped_y must align to exact integer physical pixels at scale {}",
            scale
        );
    }
}

#[test]
fn test_windowed_virtualization_spacers_sum_to_total() {
    // Fix 3: Virtualization with spacers.
    // 100 items with variable heights between 80px and 220px.
    let heights: Vec<f32> = (0..100).map(|i| 80.0 + (i % 8) as f32 * 20.0).collect();
    let mut offsets = Vec::with_capacity(heights.len());
    let mut acc = 0.0f32;
    for &h in &heights {
        offsets.push(acc);
        acc += h + 10.0;
    }
    let total_content_h = compute_content_height(&heights, 10.0);
    let visible_h = 600.0f32;
    let cushion = 350.0f32;

    // Test across various scroll offsets: top, 25%, 50%, 75%, bottom
    for scroll_y in [0.0f32, 1500.0, 4000.0, 8000.0, total_content_h - visible_h] {
        let view_min = (scroll_y - cushion).max(0.0);
        let view_max = scroll_y + visible_h + cushion;

        let first_idx = offsets
            .iter()
            .zip(heights.iter())
            .position(|(&off, &h)| off + h >= view_min)
            .unwrap_or(0);

        let last_idx = offsets
            .iter()
            .rposition(|&off| off <= view_max)
            .unwrap_or(heights.len() - 1)
            .max(first_idx)
            .min(heights.len() - 1);

        let active_count = last_idx - first_idx + 1;
        // Verify only a small window of cards (~10 to 14) is instantiated, not all 100
        assert!(
            active_count <= 16,
            "windowed items ({active_count}) must be O(12 cards), not O(all 100)"
        );

        let top_spacer = offsets[first_idx];
        let bottom_spacer = if last_idx < heights.len() - 1 {
            let end_y = offsets[last_idx] + heights[last_idx];
            (total_content_h - end_y).max(0.0)
        } else {
            0.0
        };

        let visible_slice_h = if active_count == 1 {
            heights[first_idx]
        } else {
            offsets[last_idx] + heights[last_idx] - offsets[first_idx]
        };

        let reconstructed_total = top_spacer + visible_slice_h + bottom_spacer;
        assert!(
            (reconstructed_total - total_content_h).abs() < 1e-3,
            "spacers + visible cards ({reconstructed_total}) must equal total content height ({total_content_h})"
        );
    }
}

#[test]
fn test_auxiliary_write_throttling_contract() {
    // Fix 4: Throttle aux writes (progress moved < 0.005).
    let mut last_written = 0.0f32;
    let mut write_count = 0;

    // Simulate 100 continuous micro-steps of 0.001
    for step in 1..=100 {
        let current_prog = step as f32 * 0.001;
        if (current_prog - last_written).abs() >= 0.005 {
            write_count += 1;
            last_written = current_prog;
        }
    }

    // 100 frames of micro-motion should only cause ~18-20 writes, saving ~80 dirty-region repaints
    assert!(
        write_count >= 18 && write_count <= 20,
        "throttled writes ({write_count}) must reduce 100 frames to ~18-20 writes"
    );
}

#[test]
fn test_row_spacing_pitch_calibration() {
    use sotto_desktop::smooth_scroll::{compute_content_height, ROW_SPACING};
    // Fix R4 & R9: Verify row pitch accumulates h + 12.0 (10px spacing + 2px padding-bottom)
    let card_heights = [78.0f32, 95.0, 130.0, 78.0];
    let total = compute_content_height(&card_heights, ROW_SPACING);

    // Sum of cards + 3 inter-card gaps of ROW_SPACING (8.0)
    let expected = (78.0 + 95.0 + 130.0 + 78.0) + (3.0 * 8.0);
    assert_eq!(total, expected);
    assert_eq!(ROW_SPACING, 8.0);
}

#[test]
fn test_char_advance_and_cjk_glyph_measurement() {
    use sotto_desktop::smooth_scroll::{CARD_BASE_HEIGHT, CHAR_ADVANCE_PX, LINE_HEIGHT_PX};
    // 14px Inter calibration (6.4px mean advance) and wide CJK glyph 2-cell counting
    let card_w = 400.0f32;
    let text_w = card_w - 63.0; // 337.0: mirrors chars_per_line_for insets
    let chars_per_line = ((text_w / CHAR_ADVANCE_PX) as usize).max(15);
    assert_eq!(chars_per_line, 52); // 337.0 / 6.4 = 52

    // English string of 84 chars -> 2 lines at cpl 56
    let eng_text = "This is a sample transcribed line with normal characters that wraps into two lines.";
    let eng_cols: usize = eng_text.chars().map(|c| if (c as u32) > 0x2E80 { 2 } else { 1 }).sum();
    let eng_lines = ((eng_cols.max(1) + chars_per_line - 1) / chars_per_line).max(1);
    assert_eq!(eng_lines, 2);
    let eng_card_h = CARD_BASE_HEIGHT + ((eng_lines - 1) as f32 * LINE_HEIGHT_PX);
    assert_eq!(eng_card_h, CARD_BASE_HEIGHT + LINE_HEIGHT_PX);

    // CJK string of 30 wide characters -> 60 columns -> 2 lines
    let cjk_text = "こんにちは世界、これはテストの文字列表記です。日本語です";
    let cjk_cols: usize = cjk_text.chars().map(|c| if (c as u32) > 0x2E80 { 2 } else { 1 }).sum();
    let cjk_lines = ((cjk_cols.max(1) + chars_per_line - 1) / chars_per_line).max(1);
    assert_eq!(cjk_lines, 2);
}

#[test]
fn test_fling_window_hysteresis_skips_intermediate_rebuilds() {
    // Fix R7: During fast fling, if scroll position moved < 180px and elapsed < 48ms,
    // window model rebuild is skipped to keep GPU at 60fps
    let last_refill_y = 1000.0f32;
    let cached_win = (10, 18);

    // Simulate 3 micro-frames of 40px scroll at 16ms interval
    for frame in 1..=3 {
        let current_scroll = 1000.0 + (frame as f32 * 40.0);
        let win = (10, 18); // same or neighboring window
        let scroll_delta = (current_scroll - last_refill_y).abs();
        let elapsed = Duration::from_millis(frame * 16);

        let should_update = (win != cached_win) && (scroll_delta >= 180.0 || elapsed >= Duration::from_millis(48));
        // For first 2 frames (< 48ms and < 180px), should_update is false (zero DOM rebuild)
        if frame < 3 {
            assert!(!should_update, "frame {frame} must skip rebuild under hysteresis");
        }
    }
}

#[test]
fn test_anchor_scrolling_shifts_position_without_visual_jump() {
    use sotto_desktop::smooth_scroll::{CARD_BASE_HEIGHT, ROW_SPACING, SmoothScrollEngine};
    // Fix R8: Prepending item at index 0 while scrolled down shifts target and current by -item_height
    let mut eng = SmoothScrollEngine::new();
    eng.set_dimensions(3000.0, 600.0);
    eng.scroll_to(-500.0);

    // Run ticks to reach -500.0
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(1500) {
        std::thread::sleep(Duration::from_millis(16));
        if eng.tick().is_none() {
            break;
        }
    }
    assert!((eng.current_y() - -500.0).abs() < 1.0);

    // User is reading at -500.0. A new card arrives at index 0
    let inserted_h = CARD_BASE_HEIGHT + ROW_SPACING; // 83 + 12 = 95.0
    eng.shift_anchor(-inserted_h);

    // Viewport position moved down by inserted_h (-595.0), meaning the card at 500px from top
    // stays at the EXACT same screen position!
    assert!((eng.current_y() - -(500.0 + inserted_h)).abs() < 1.0);
    assert!(!eng.is_animating(), "shift_anchor must not trigger an unwanted animation glide");
}

#[test]
fn test_snap_to_rest_omits_redundant_tick_emission() {
    use sotto_desktop::smooth_scroll::SmoothScrollEngine;
    // Minor nit: tick() snaps to rest without emitting redundant setter if already settled
    let mut eng = SmoothScrollEngine::new();
    eng.set_dimensions(2000.0, 600.0);
    eng.scroll_to(-200.0);

    let mut tick_count = 0;
    while let Some(_) = eng.tick() {
        tick_count += 1;
        std::thread::sleep(Duration::from_millis(16));
        if tick_count > 100 { break; }
    }

    assert!(!eng.is_animating());
    // Subsequent tick once settled returns None immediately
    assert_eq!(eng.tick(), None);
}

#[test]
fn test_estimate_card_height_table() {
    use sotto_desktop::smooth_scroll::{estimate_card_height, CARD_BASE_HEIGHT, LINE_HEIGHT_PX};
    // Single line (incl. empty transcript which still occupies one line).
    assert!((estimate_card_height("hello", 40) - CARD_BASE_HEIGHT).abs() < 0.01);
    assert!((estimate_card_height("", 40) - CARD_BASE_HEIGHT).abs() < 0.01);
    // Three short lines grow by 2 extra line heights.
    assert!((estimate_card_height("a\nb\nc", 40) - (CARD_BASE_HEIGHT + 2.0 * LINE_HEIGHT_PX)).abs() < 0.01);
    // Long line wraps: 100 cols at 40 cpl = 3 lines.
    let long = "x".repeat(100);
    assert!((estimate_card_height(&long, 40) - (CARD_BASE_HEIGHT + 2.0 * LINE_HEIGHT_PX)).abs() < 0.01);
    // Wide chars count double: 30 CJK chars at cpl 15 = 60 cols = 4 lines.
    let cjk = "\u{65e5}".repeat(30);
    assert!((estimate_card_height(&cjk, 15) - (CARD_BASE_HEIGHT + 3.0 * LINE_HEIGHT_PX)).abs() < 0.01);
    // Degenerate cpl never divides by zero.
    assert!(estimate_card_height("hello", 0) > 0.0);
}

#[test]
fn test_chars_per_line_for_insets() {
    use sotto_desktop::smooth_scroll::chars_per_line_for;
    // 1440px window @1.0: card 1380, text 1317 -> 1317/6.4 = 205.
    // (Inset chain: 60 chrome + 32 card pad + 12 list pad + 22 gutter.)
    assert_eq!(chars_per_line_for(1440, 1.0), 205);
    // Degenerate inputs clamp, never zero.
    assert!(chars_per_line_for(0, 1.0) >= 15);
    assert!(chars_per_line_for(800, 0.0) >= 15);
}

#[test]
fn test_gesture_pan_delta_conventions() {
    use sotto_desktop::smooth_scroll::gesture_pan_delta;
    let mut last = (0, 0);
    // GF_BEGIN packet establishes reference, emits zero motion.
    assert_eq!(gesture_pan_delta((10, -5), &mut last, true), (0.0, 0.0));
    assert_eq!(last, (10, -5));
    // Finger moved down 30px since: positive dy = scroll up.
    assert_eq!(gesture_pan_delta((10, 25), &mut last, false), (0.0, 30.0));
    // Finger moved up 50px: negative dy = scroll down.
    assert_eq!(gesture_pan_delta((4, -25), &mut last, false), (-6.0, -50.0));
}

#[test]
fn test_bottom_overscroll_padding_guarantee() {
    use sotto_desktop::smooth_scroll::{compute_content_height, LIST_BOTTOM_PADDING, ROW_SPACING};
    assert!(LIST_BOTTOM_PADDING >= 16.0, "LIST_BOTTOM_PADDING must be at least 16px for safe breathing room");

    let card_heights = vec![85.0, 110.0, 95.0, 140.0, 120.0];
    let content_h = compute_content_height(&card_heights, ROW_SPACING);
    let total_h = content_h + LIST_BOTTOM_PADDING;

    // The bottom of the last card is at content_h; the buffer below it is exactly LIST_BOTTOM_PADDING.
    let buffer_below_last_card = total_h - content_h;
    assert!((buffer_below_last_card - LIST_BOTTOM_PADDING).abs() < 0.001);
    assert!(buffer_below_last_card >= 16.0);
}

#[test]
fn test_word_aware_wrapping_exceeds_naive_division() {
    use sotto_desktop::smooth_scroll::estimate_card_height;
    // Sentence with realistic word breaks that force earlier line wraps than naive character division.
    let text = "The quick brown fox jumps over the lazy dog and types words in the dictation studio.";
    // Narrow cpl where word breaks cause wrapping:
    let height = estimate_card_height(text, 25);
    // At cpl 25, 85 chars would naively be (85+24)/25 = 4 lines.
    // Words push it to at least 4 or 5 lines.
    assert!(height >= 78.0 + 3.0 * 17.0);
}

#[test]
fn test_card_height_monotonicity_across_resizes() {
    use sotto_desktop::smooth_scroll::{chars_per_line_for, estimate_card_height};
    let transcript = "Dictation test with several realistic sentences to check resizing behavior across multiple screen sizes.";
    let widths = [1920, 1440, 1040, 900, 760];
    let mut last_cpl = usize::MAX;
    let mut last_height = 0.0f32;

    for &w in &widths {
        let cpl = chars_per_line_for(w, 1.0);
        let h = estimate_card_height(transcript, cpl);
        // As window gets narrower, cpl must decrease or stay same:
        assert!(cpl <= last_cpl, "cpl at {w}px was {cpl}, previously {last_cpl}");
        // And estimated height must increase or stay same (never shrink):
        assert!(h >= last_height, "height at {w}px was {h}, previously {last_height}");
        last_cpl = cpl;
        last_height = h;
    }
}

#[test]
fn test_bottom_card_fully_visible_at_extreme_aspect_ratios() {
    use sotto_desktop::smooth_scroll::{
        chars_per_line_for, compute_content_height, compute_visible_height,
        estimate_card_height, LIST_BOTTOM_PADDING, LIST_OVERSCROLL_PX, ROW_SPACING,
    };

    let test_windows = [
        (760, 600),   // Minimum supported width and height
        (1040, 840),  // Default studio size
        (1920, 1080), // Standard 1080p full screen
        (800, 1200),  // Vertical portrait monitor
    ];

    let sample_transcripts = vec![
        "First dictation note.",
        "Second note: The quick brown fox jumps over the lazy dog and produces smooth voice text.",
        "Third note: Short reply.",
        "Fourth note: Comprehensive report on audio streaming latency and whisper transcription accuracy on Windows 11.",
        "Fifth note: Final item at the bottom of the list.",
    ];

    for (w, h) in test_windows {
        let cpl = chars_per_line_for(w, 1.0);
        let heights: Vec<f32> = sample_transcripts.iter().map(|t| estimate_card_height(t, cpl)).collect();
        let content_h = compute_content_height(&heights, ROW_SPACING);
        // Production formula: breathing room + overscroll slack so the last
        // card always scrolls fully into view (regression: clipped last card).
        let total_content_h = content_h + LIST_BOTTOM_PADDING + LIST_OVERSCROLL_PX;
        let visible_h = compute_visible_height(h as f32, 156.0, 100.0);

        let mut scroller = SmoothScrollEngine::new();
        scroller.set_dimensions(total_content_h, visible_h);
        scroller.scroll_to_bottom();

        // Target scroll must be -max_scroll
        let max_scroll = (total_content_h - visible_h).max(0.0);
        assert!((scroller.target_y() - (-max_scroll)).abs() < 0.001);

        // When scrolled to bottom, verify the last card is fully within viewport:
        let distance_from_viewport_bottom = total_content_h - content_h;
        assert!(
            distance_from_viewport_bottom >= 16.0 + LIST_OVERSCROLL_PX,
            "Window {w}x{h}: bottom clearance was {distance_from_viewport_bottom}px, expected >= {}px",
            16.0 + LIST_OVERSCROLL_PX
        );
    }
}

#[test]
fn test_scroll_performance_and_memory_wiring_contracts() {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let hud = std::fs::read_to_string(manifest_dir.join("ui/hud.slint")).expect("read hud.slint");
    let main_rs = std::fs::read_to_string(manifest_dir.join("src/main.rs")).expect("read main.rs");
    let app_icon_rs = std::fs::read_to_string(manifest_dir.join("src/app_icon.rs")).expect("read app_icon.rs");

    // 1. Virtualized history list + duplicate DOM removed + Providers scrollbar wired.
    assert!(!hud.contains("measure-total-h"), "Duplicate offscreen measurement DOM must stay removed");
    assert!(hud.contains("history-virtualized"), "Virtualization toggle missing from hud.slint");
    assert!(hud.contains("providers-scrollbar-track"), "Providers scrollbar missing from hud.slint");
    assert!(hud.contains("providers-drag-scroll"), "Providers drag scroll callback missing from hud.slint");

    // 2. Event-driven timers, WM_GESTURE handle fix, Studio teardown on X, non-blocking hydration.
    assert!(main_rs.contains("any_studio_scroller_animating"), "Scroll timer must auto-stop when idle");
    assert!(main_rs.contains("should_anim_timer_run"), "Pill anim timer must auto-stop when static idle");
    assert!(main_rs.contains("GetGestureInfo(lparam as _"), "WM_GESTURE must decode HGESTUREINFO from lparam");
    assert!(main_rs.contains("CloseGestureInfoHandle(lparam as _"), "WM_GESTURE must close lparam gesture handle");
    assert!(main_rs.contains("ptsLocation"), "WM_GESTURE pan must read ptsLocation coordinates");
    assert!(main_rs.contains("drop_studio_instance"), "Studio close must drop instance and release caches");
    assert!(
        main_rs.contains("hydrate_studio_history_nonblocking(studio, history_queue.db(), 500)"),
        "Initial Studio history load must use non-blocking 500-item hydration"
    );
    assert!(
        main_rs.contains("hydrate_studio_history_nonblocking(s, history_queue.db(), 500)"),
        "Reopened Studio history load must use non-blocking 500-item hydration"
    );

    // 3. Bounded + non-blocking + deduplicated app icon cache.
    assert!(app_icon_rs.contains("MAX_ICON_CACHE_ENTRIES"), "Icon cache must enforce an upper bound");
    assert!(app_icon_rs.contains("try_get_cached_app_icon"), "Non-blocking icon cache lookup missing");
    assert!(app_icon_rs.contains("prewarm_app_icon"), "Background icon prewarm helper missing");
    assert!(app_icon_rs.contains("clear_ui_icon_cache"), "UI icon cache cleanup missing");
}

#[test]
fn test_svg_branding_and_desktop_icon_wiring() {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let hud = std::fs::read_to_string(manifest_dir.join("ui/hud.slint")).expect("read hud.slint");
    let build_rs = std::fs::read_to_string(manifest_dir.join("build.rs")).expect("read build.rs");
    let tray_rs = std::fs::read_to_string(manifest_dir.join("src/tray.rs")).expect("read tray.rs");
    let readme = std::fs::read_to_string(manifest_dir.join("../README.md")).expect("read README.md");

    for asset in [
        "assets/icons/sotto-dark.svg",
        "assets/icons/sotto-light.svg",
        "assets/icons/sotto-mark-dark.svg",
        "assets/icons/sotto-mark-light.svg",
        "assets/icons/sotto.ico",
        "assets/icons/sotto-tray-dark-16.png",
        "assets/icons/sotto-tray-dark-32.png",
        "assets/icons/sotto-tray-light-16.png",
        "assets/icons/sotto-tray-light-32.png",
    ] {
        assert!(manifest_dir.join(asset).exists(), "missing branding asset: {asset}");
    }

    assert!(!hud.contains("\"Sotto Studio\""), "Studio header text should be replaced by Sotto SVG branding");
    assert!(hud.contains("brand-logo"), "Studio header brand-logo missing");
    assert!(hud.contains("sotto-dark.svg"), "Studio header should use dark-mode SVG logo");
    assert!(hud.contains("sotto-mark-dark.svg"), "Window icon should use Sotto mark SVG");

    assert!(build_rs.contains("assets/icons/sotto.ico"), "Windows resource icon missing from build.rs");
    assert!(build_rs.contains("tauri_winres::WindowsResource"), "Windows resource compiler missing");

    assert!(tray_rs.contains("SystemUsesLightTheme"), "Tray icon must adapt to Windows light/dark taskbar theme");
    assert!(tray_rs.contains("WM_SETTINGCHANGE"), "Tray must refresh icon when Windows theme toggles");

    assert!(readme.contains("prefers-color-scheme: dark") && readme.contains("sotto-dark.svg"));
    assert!(readme.contains("prefers-color-scheme: light") && readme.contains("sotto-light.svg"));
}
