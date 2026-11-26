// Consolidated UI Quality Invariant Suite (no redundant getter mirrors or
// test-only math copies): verifies live Slint Pill & Studio defaults, rendered
// VoicePill geometry across all 15 state-option combinations via ElementHandle,
// WCAG 2.x contrast ratios, pixel-grid alignment, and pointer target sizing.

mod support;

use i_slint_backend_testing::{ElementHandle, ElementRoot};
use slint::{ComponentHandle, SharedString};
use sotto_desktop::{FloatingPillWindow, StudioWindow};
use std::cell::Cell;
use std::rc::Rc;
use support::ui_quality::{
    contrast_ratio, is_crisp_at_scale, stage_bg, ACCENT, CARD_BG, CHROME_BG, COPY_BUTTON_H,
    COPY_BUTTON_W, INK_BODY, INK_MUTED, INK_PRIMARY, INK_SECONDARY, MIN_BODY_FONT_PX,
    MIN_TARGET_PX, PILL_WIDTHS, SAVE_BTN_BG, SAVE_BTN_INK, SUCCESS,
};

thread_local! {
    static BACKEND_READY: Cell<bool> = const { Cell::new(false) };
}

fn ensure_testing_backend() {
    BACKEND_READY.with(|ready| {
        if !ready.get() {
            i_slint_backend_testing::init_no_event_loop();
            ready.set(true);
        }
    });
}

#[test]
fn test_studio_and_pill_initial_defaults() {
    ensure_testing_backend();
    let pill = FloatingPillWindow::new().expect("FloatingPillWindow must instantiate");
    assert_eq!(pill.get_active_state(), 0);
    assert_eq!(pill.get_opt_idle(), 0);
    assert_eq!(pill.get_opt_listening(), 0);
    assert_eq!(pill.get_opt_transcribe(), 0);
    assert_eq!(pill.get_opt_success(), 0);
    assert_eq!(pill.get_opt_error(), 0);
    assert_eq!(pill.get_timer_text().as_str(), "0:00");
    assert!(pill.get_surface_opacity() > 0.0 && pill.get_surface_opacity() <= 1.0);

    let studio = StudioWindow::new().expect("StudioWindow must instantiate");
    assert_eq!(studio.get_active_tab(), 0);
    assert_eq!(studio.get_active_state(), 0);
    assert_eq!(studio.get_history_count(), 0);
    assert!(!studio.get_purge_confirm_armed());
    assert_eq!(studio.get_trigger_primary_display().as_str(), "F8");
    assert_eq!(studio.get_cancel_primary_display().as_str(), "Esc");
    assert_eq!(studio.get_provider_id().as_str(), "groq");
}

#[test]
fn test_floating_pill_all_state_variant_geometry_fits_transparent_host() {
    ensure_testing_backend();
    let pill = FloatingPillWindow::new().expect("FloatingPillWindow must instantiate");
    let root_size = pill.root_element().size();
    let host_w = root_size.width;
    let host_h = root_size.height;
    assert!((host_w - 140.0).abs() < 1.0 && (host_h - 60.0).abs() < 1.0, "host window size changed: {host_w}x{host_h}");
    pill.window().set_size(slint::PhysicalSize::new(140, 60));

    for state in 0..5i32 {
        pill.set_active_state(state);
        for opt in 0..3i32 {
            match state {
                0 => pill.set_opt_idle(opt),
                1 => pill.set_opt_listening(opt),
                2 => pill.set_opt_transcribe(opt),
                3 => pill.set_opt_success(opt),
                _ => pill.set_opt_error(opt),
            }
            i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(300));

            let handle = ElementHandle::find_by_element_id(&pill, "FloatingPillWindow::pill")
                .next()
                .expect("FloatingPillWindow::pill must exist");
            let pos = handle.absolute_position();
            let size = handle.size();
            let expected_w = PILL_WIDTHS[state as usize][opt as usize];

            assert!(
                (size.width - expected_w).abs() < 1.0,
                "state={state} opt={opt}: pill width expected {expected_w}, got {}",
                size.width
            );
            assert!(
                (size.height - 44.0).abs() < 1.0,
                "state={state} opt={opt}: pill height expected 44, got {}",
                size.height
            );
            // Centered horizontally & vertically inside the 140x60 host window with rounded-corner margin
            assert!(pos.x >= 8.0 && pos.x + size.width <= host_w - 8.0, "pill clipped horizontally at state={state} opt={opt}");
            assert!(pos.y >= 4.0 && pos.y + size.height <= host_h - 4.0, "pill clipped vertically at state={state} opt={opt}");
        }
    }
}

#[test]
fn test_studio_callback_forwarding_and_state_routing() {
    ensure_testing_backend();
    let studio = StudioWindow::new().expect("StudioWindow must instantiate");

    let copied = Rc::new(Cell::new(false));
    let copied_ref = Rc::clone(&copied);
    studio.on_copy_history_item(move |id, txt: SharedString| {
        if id == 42 && txt == "hello world" {
            copied_ref.set(true);
        }
    });
    studio.invoke_copy_history_item(42, "hello world".into());
    assert!(copied.get(), "copy_history_item callback must forward arguments");

    let cleared = Rc::new(Cell::new(false));
    let cleared_ref = Rc::clone(&cleared);
    studio.on_clear_all_history(move || cleared_ref.set(true));
    studio.invoke_clear_all_history();
    assert!(cleared.get(), "clear_all_history callback must fire");

    let saved = Rc::new(Cell::new(false));
    let saved_ref = Rc::clone(&saved);
    studio.on_save_customization(move || saved_ref.set(true));
    studio.invoke_save_customization();
    assert!(saved.get(), "save_customization callback must fire");
}

#[test]
fn test_wcag_aa_contrast_across_dark_surfaces_and_pill() {
    for (name, fg, bg, min_ratio) in [
        ("hero on studio card", INK_PRIMARY, CARD_BG, 4.5),
        ("body on studio card", INK_BODY, CARD_BG, 4.5),
        ("secondary on chrome", INK_SECONDARY, CHROME_BG, 4.5),
        ("muted on chrome", INK_MUTED, CHROME_BG, 4.5),
        ("save button ink on bg", SAVE_BTN_INK, SAVE_BTN_BG, 7.0),
        ("accent on card", ACCENT, CARD_BG, 4.5),
        ("success on card", SUCCESS, CARD_BG, 4.5),
        ("pill ink on vignette", INK_PRIMARY, "#090a0c", 7.0),
    ] {
        let ratio = contrast_ratio(fg, bg).unwrap_or_else(|| panic!("bad hex in {name}"));
        assert!(
            ratio >= min_ratio,
            "{name}: contrast {ratio:.2} < required {min_ratio:.1} ({fg} on {bg})"
        );
    }

    // Pill border contrast against dark stage canvases (IDE, Slate, OLED)
    for mode in 0..=2 {
        let bg = stage_bg(mode);
        let ratio = contrast_ratio(INK_PRIMARY, bg).unwrap();
        assert!(ratio >= 10.0, "stage mode {mode} ink contrast too low: {ratio:.2}");
    }
}

#[test]
fn test_pixel_grid_crispness_and_touch_target_floors() {
    for scale in [1.0f32, 1.5, 2.0] {
        assert!(is_crisp_at_scale(44.0, scale));
        assert!(is_crisp_at_scale(48.0, scale));
        assert!(is_crisp_at_scale(60.0, scale));
    }
    assert!(COPY_BUTTON_W >= MIN_TARGET_PX && COPY_BUTTON_H >= MIN_TARGET_PX);
    assert!(MIN_BODY_FONT_PX >= 10.5);
}

#[test]
fn test_studio_stage_preview_pill_centers_inside_stage_box_across_states() {
    ensure_testing_backend();
    let studio = StudioWindow::new().expect("StudioWindow must instantiate");
    studio.window().set_size(slint::PhysicalSize::new(1040, 840));
    studio.set_active_tab(2);
    studio.set_custom_viewport_y(0.0);

    let stage = ElementHandle::find_by_element_id(&studio, "StudioWindow::stage-box")
        .next()
        .expect("StudioWindow::stage-box must exist");
    let stage_pos = stage.absolute_position();
    let stage_size = stage.size();
    let stage_center_x = stage_pos.x + stage_size.width * 0.5;
    let stage_body_center_y = stage_pos.y + 34.0 + (stage_size.height - 34.0) * 0.5;

    for state in 0..=4i32 {
        studio.set_active_state(state);
        studio.set_bg_mode(state % 4);
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(300));

        let pill = ElementHandle::find_by_element_id(&studio, "VoicePill::root")
            .next()
            .expect("VoicePill::root inside Studio stage must be visible");
        let ppos = pill.absolute_position();
        let psize = pill.size();
        let pill_cx = ppos.x + psize.width * 0.5;
        let pill_cy = ppos.y + psize.height * 0.5;
        assert!(
            (pill_cx - stage_center_x).abs() <= 1.5 && (pill_cy - stage_body_center_y).abs() <= 1.5,
            "state={state}: preview pill not centered in stage-box ({pill_cx:.1},{pill_cy:.1} vs {stage_center_x:.1},{stage_body_center_y:.1})"
        );
    }
}
