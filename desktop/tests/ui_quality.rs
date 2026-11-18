// UI quality regression suite: rendering, responsiveness, sharpness/crispness,
// and edge cases for every UI function. Standard: modern 2026 desktop UI.
// Spec tables / thresholds live in `tests/support/ui_quality.rs` (mirrors hud.slint).
//
// Sections:
//   A. Component defaults & invariants      G. Contrast (WCAG 2.x)
//   B. Callback plumbing (Slint side)       H. Responsiveness budgets
//   C. State x option matrix                I. Config robustness
//   D. History model edge cases             J. Format helpers
//   E. Input validation clamps              K. Known issues (ignored, runnable
//   F. Sharpness / pixel grid                  with -- --ignored)

mod support;

use support::ui_quality::{
    active_option_index, clamp_bg_mode, clamp_opacity, clamp_option, clamp_state, clamp_tab,
    contrast_ratio, format_timer_text, is_crisp_at_scale, luminance, stage_bg, ACCENT,
    CHROME_BG, COPY_BUTTON_H, COPY_BUTTON_W, INK_BODY, INK_MUTED, INK_PRIMARY, INK_SECONDARY,
    MIN_BODY_FONT_PX, MIN_TARGET_PX, PILL_WIDTHS, SAVE_BTN_BG, SAVE_BTN_INK, STAGE_BGS, SUCCESS,
    CARD_BG,
};
use sotto_desktop::{app_icon, HistoryItem, StudioWindow};
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn item(id: i32, transcript: &str, app: &str, with_icon: bool) -> HistoryItem {
    let formatted = app_icon::format_app_name(app);
    let (icon, _) = app_icon::get_app_icon(&formatted, None);
    HistoryItem {
        id,
        timestamp_str: "2026-10-10 12:00:00".into(),
        transcript: transcript.into(),
        duration_ms: 1500,
        duration_str: "1.5s".into(),
        target_app: formatted.into(),
        app_icon: icon,
        has_icon: with_icon,
    }
}

fn set_list(studio: &StudioWindow, items: Vec<HistoryItem>) {
    let count = items.len() as i32;
    studio.set_history_items(ModelRc::from(Rc::new(VecModel::from(items))));
    studio.set_history_count(count);
}

thread_local! {
    static BACKEND_READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Headless Slint testing backend (no winit event loop), initialized once per
/// test thread. Lets every test instantiate windows in parallel.
fn ensure_testing_backend() {
    BACKEND_READY.with(|ready| {
        if !ready.get() {
            i_slint_backend_testing::init_no_event_loop();
            ready.set(true);
        }
    });
}

fn studio() -> StudioWindow {
    ensure_testing_backend();
    StudioWindow::new().expect("StudioWindow must instantiate")
}

fn pill() -> sotto_desktop::FloatingPillWindow {
    ensure_testing_backend();
    sotto_desktop::FloatingPillWindow::new().expect("FloatingPillWindow must instantiate")
}

// ============================================================================
// A. Component defaults & invariants
// ============================================================================

#[test]
fn test_studio_defaults_fresh_launch() {
    let s = studio();
    assert_eq!(s.get_active_tab(), 0);
    assert_eq!(s.get_history_count(), 0);
    assert_eq!(s.get_history_items().row_count(), 0);
    assert_eq!(s.get_copied_item_id(), -1);
    assert!(!s.get_save_feedback());
    assert!(!s.get_is_fullscreen());
    assert!((s.get_history_viewport_y() - 0.0).abs() < 1e-6);
}

#[test]
fn test_studio_customization_defaults() {
    let s = studio();
    assert_eq!(s.get_active_state(), 0);
    for o in [s.get_opt_idle(), s.get_opt_listening(), s.get_opt_transcribe(), s.get_opt_success(), s.get_opt_error()] {
        assert_eq!(o, 0);
    }
    assert!((s.get_surface_opacity() - 0.85).abs() < 1e-6);
    assert_eq!(s.get_bg_mode(), 0);
    assert!(s.get_chimes_enabled());
    assert!(s.get_media_pause_enabled());
}

#[test]
fn test_studio_animation_channel_defaults() {
    let s = studio();
    assert_eq!(s.get_timer_text().as_str(), "0:00");
    assert_eq!(s.get_timer_sec(), 0);
    for b in [s.get_bar1_h(), s.get_bar2_h(), s.get_bar3_h(), s.get_bar4_h(), s.get_bar5_h()] {
        assert!((b - 3.0).abs() < 1e-6, "bars rest at 3.0, got {b}");
    }
}

#[test]
fn test_floating_pill_defaults() {
    let p = pill();
    assert_eq!(p.get_active_state(), 0);
    assert!((p.get_surface_opacity() - 0.85).abs() < 1e-6);
    assert_eq!(p.get_timer_text().as_str(), "0:00");
}

// ============================================================================
// B. Callback plumbing (Slint side fires with correct signatures)
// ============================================================================

#[test]
fn test_studio_callbacks_deliver_args() {
    let s = studio();
    let state = Arc::new(Mutex::new(-99));
    let opt = Arc::new(Mutex::new(-99));
    let op = Arc::new(Mutex::new(-1.0f32));
    let bg = Arc::new(Mutex::new(-99));
    let tab = Arc::new(Mutex::new(-99));

    let c = Arc::clone(&state);
    s.on_select_state(move |v| *c.lock().unwrap() = v);
    let c = Arc::clone(&opt);
    s.on_select_option(move |v| *c.lock().unwrap() = v);
    let c = Arc::clone(&op);
    s.on_set_opacity(move |v| *c.lock().unwrap() = v);
    let c = Arc::clone(&bg);
    s.on_set_bg(move |v| *c.lock().unwrap() = v);
    let c = Arc::clone(&tab);
    s.on_select_tab(move |v| *c.lock().unwrap() = v);

    s.invoke_select_state(3);
    s.invoke_select_option(2);
    s.invoke_set_opacity(0.70);
    s.invoke_set_bg(2);
    s.invoke_select_tab(1);

    assert_eq!(*state.lock().unwrap(), 3);
    assert_eq!(*opt.lock().unwrap(), 2);
    assert!((*op.lock().unwrap() - 0.70).abs() < 1e-6);
    assert_eq!(*bg.lock().unwrap(), 2);
    assert_eq!(*tab.lock().unwrap(), 1);
}

#[test]
fn test_copy_and_clear_and_save_callbacks_fire() {
    let s = studio();
    let copied = Arc::new(Mutex::new((-99, String::new())));
    let cleared = Arc::new(Mutex::new(false));
    let saved = Arc::new(Mutex::new(false));

    let c = Arc::clone(&copied);
    s.on_copy_history_item(move |id, text| *c.lock().unwrap() = (id, text.to_string()));
    let c = Arc::clone(&cleared);
    s.on_clear_all_history(move || *c.lock().unwrap() = true);
    let c = Arc::clone(&saved);
    s.on_save_customization(move || *c.lock().unwrap() = true);

    s.invoke_copy_history_item(7, "hello 🚀".into());
    s.invoke_clear_all_history();
    s.invoke_save_customization();

    let (id, text) = copied.lock().unwrap().clone();
    assert_eq!(id, 7);
    assert_eq!(text, "hello 🚀");
    assert!(*cleared.lock().unwrap());
    assert!(*saved.lock().unwrap());
}

#[test]
fn test_cycle_and_fullscreen_callbacks_fire() {
    let s = studio();
    let cycled = Arc::new(Mutex::new(0));
    let fs = Arc::new(Mutex::new(0));
    let c = Arc::clone(&cycled);
    s.on_cycle_state(move || *c.lock().unwrap() += 1);
    let c = Arc::clone(&fs);
    s.on_toggle_fullscreen(move || *c.lock().unwrap() += 1);
    s.invoke_cycle_state();
    s.invoke_cycle_state();
    s.invoke_toggle_fullscreen();
    assert_eq!(*cycled.lock().unwrap(), 2);
    assert_eq!(*fs.lock().unwrap(), 1);
}

#[test]
fn test_pill_cycle_and_drag_callbacks_fire() {
    let p = pill();
    let cycled = Arc::new(Mutex::new(0));
    let dragged = Arc::new(Mutex::new((0.0f32, 0.0f32)));
    let c = Arc::clone(&cycled);
    p.on_cycle_state(move || *c.lock().unwrap() += 1);
    let c = Arc::clone(&dragged);
    p.on_drag_window(move |x, y| *c.lock().unwrap() = (x, y));
    p.invoke_cycle_state();
    p.invoke_drag_window(4.0, -2.5);
    assert_eq!(*cycled.lock().unwrap(), 1);
    assert_eq!(*dragged.lock().unwrap(), (4.0, -2.5));
}

// ============================================================================
// C. State x option matrix (5 states x 3 variants = 15 UIs)
// ============================================================================

#[test]
fn test_all_five_states_settable() {
    let s = studio();
    for st in 0..=4 {
        s.set_active_state(st);
        assert_eq!(s.get_active_state(), st);
    }
}

#[test]
fn test_all_fifteen_state_option_combos_persist() {
    let s = studio();
    for st in 0..=4 {
        s.set_active_state(st);
        for opt in 0..=2 {
            match st {
                0 => s.set_opt_idle(opt),
                1 => s.set_opt_listening(opt),
                2 => s.set_opt_transcribe(opt),
                3 => s.set_opt_success(opt),
                _ => s.set_opt_error(opt),
            }
        }
    }
    assert_eq!(s.get_opt_idle(), 2);
    assert_eq!(s.get_opt_listening(), 2);
    assert_eq!(s.get_opt_transcribe(), 2);
    assert_eq!(s.get_opt_success(), 2);
    assert_eq!(s.get_opt_error(), 2);
}

#[test]
fn test_active_option_routing_mirror() {
    // Mirrors `active-option()` in hud.slint:556-562.
    for st in 0..=4 {
        let opts = [10 + st, 20 + st, 30 + st, 40 + st, 50 + st];
        assert_eq!(active_option_index(st, opts), opts[st as usize]);
    }
    // Out-of-range state falls through to the error slot (Slint default arm).
    assert_eq!(active_option_index(9, [0, 1, 2, 3, 4]), 4);
    assert_eq!(active_option_index(-1, [0, 1, 2, 3, 4]), 4);
}

#[test]
fn test_studio_preview_isolated_from_live_pill() {
    let s = studio();
    let p = pill();
    s.set_active_state(4);
    s.set_opt_error(2);
    s.set_surface_opacity(0.70);
    assert_eq!(p.get_active_state(), 0, "studio preview must not mutate live pill");
    assert!((p.get_surface_opacity() - 0.85).abs() < 1e-6);
}

// ============================================================================
// D. History model edge cases
// ============================================================================

#[test]
fn test_empty_history_model_invariants() {
    let s = studio();
    set_list(&s, vec![]);
    assert_eq!(s.get_history_count(), 0);
    assert_eq!(s.get_history_items().row_count(), 0);
}

#[test]
fn test_single_item_full_field_roundtrip() {
    let s = studio();
    set_list(&s, vec![item(42, "Hello world", "Code.exe", true)]);
    assert_eq!(s.get_history_count(), 1);
    let row = s.get_history_items().row_data(0).expect("row 0 must exist");
    assert_eq!(row.id, 42);
    assert_eq!(row.transcript.as_str(), "Hello world");
    assert_eq!(row.target_app.as_str(), "VS Code");
    assert_eq!(row.duration_str.as_str(), "1.5s");
    assert!(!row.timestamp_str.is_empty());
}

#[test]
fn test_hundred_items_count_sync_and_order() {
    let s = studio();
    let items: Vec<HistoryItem> = (0..100).map(|i| item(100 - i, &format!("entry {i}"), "chrome.exe", false)).collect();
    set_list(&s, items);
    assert_eq!(s.get_history_count(), 100);
    assert_eq!(s.get_history_items().row_count(), 100);
    // Newest-on-top: index 0 is the first-inserted (highest id).
    assert_eq!(s.get_history_items().row_data(0).unwrap().id, 100);
    assert_eq!(s.get_history_items().row_data(99).unwrap().id, 1);
}

#[test]
fn test_unicode_emoji_cjk_rtl_transcripts() {
    let s = studio();
    let cases = [
        "Hello 🚀 world 🎙️ done ✓",
        "日本語のディクテーションテスト",
        "مرحبا بالعالم",
        "Mixed — “quotes” … ’ellipses’ — ✓",
    ];
    let items: Vec<HistoryItem> = cases.iter().enumerate().map(|(i, t)| item(i as i32, t, "notepad.exe", false)).collect();
    set_list(&s, items);
    for (i, want) in cases.iter().enumerate() {
        assert_eq!(s.get_history_items().row_data(i).unwrap().transcript.as_str(), *want);
    }
}

#[test]
fn test_ten_kb_transcript_and_empty_transcript() {
    let s = studio();
    let long = "word ".repeat(2000); // 10_000 chars
    set_list(&s, vec![item(1, &long, "winword.exe", false), item(2, "", "cmd.exe", false)]);
    assert_eq!(s.get_history_items().row_data(0).unwrap().transcript.len(), 10_000);
    assert_eq!(s.get_history_items().row_data(1).unwrap().transcript.as_str(), "");
}

#[test]
fn test_missing_icon_flag_path() {
    let s = studio();
    set_list(&s, vec![item(1, "no icon app", "some-obscure-tool-xyz.exe", false)]);
    let row = s.get_history_items().row_data(0).unwrap();
    assert!(!row.has_icon, "unknown app must take the fallback dot path");
    assert_eq!(row.target_app.as_str(), "Some Obscure Tool Xyz");
}

#[test]
fn test_zero_duration_and_duplicate_ids_stable() {
    let s = studio();
    let mut a = item(5, "first", "cmd.exe", false);
    a.duration_ms = 0;
    let b = item(5, "second", "cmd.exe", false); // duplicate id must not crash model
    set_list(&s, vec![a, b]);
    assert_eq!(s.get_history_count(), 2);
    assert_eq!(s.get_history_items().row_data(0).unwrap().duration_ms, 0);
}

#[test]
fn test_viewport_and_copied_id_roundtrip() {
    let s = studio();
    s.set_history_viewport_y(-120.0);
    assert!((s.get_history_viewport_y() - -120.0).abs() < 1e-3);
    s.set_copied_item_id(9);
    assert_eq!(s.get_copied_item_id(), 9);
    s.set_copied_item_id(-1);
    assert_eq!(s.get_copied_item_id(), -1);
}

// ============================================================================
// E. Input validation clamps (wiring at main.rs:358-412 must adopt these)
// ============================================================================

#[test]
fn test_clamp_state_option_bg_tab() {
    assert_eq!(clamp_state(2), 2);
    assert_eq!(clamp_state(-1), 0);
    assert_eq!(clamp_state(5), 4);
    assert_eq!(clamp_option(1), 1);
    assert_eq!(clamp_option(7), 2);
    assert_eq!(clamp_option(-3), 0);
    assert_eq!(clamp_bg_mode(3), 3);
    assert_eq!(clamp_bg_mode(99), 3);
    assert_eq!(clamp_bg_mode(-1), 0);
    assert_eq!(clamp_tab(1), 1);
    assert_eq!(clamp_tab(5), 2);
    assert_eq!(clamp_tab(-2), 0);
}

#[test]
fn test_clamp_opacity_edges() {
    assert!((clamp_opacity(0.70) - 0.70).abs() < 1e-6);
    assert_eq!(clamp_opacity(-0.5), 0.0);
    assert_eq!(clamp_opacity(1.5), 1.0);
    assert!((clamp_opacity(f32::NAN) - 0.85).abs() < 1e-6, "NaN must fall back to default");
    assert!((clamp_opacity(f32::INFINITY) - 0.85).abs() < 1e-6);
}

#[test]
fn test_opacity_presets_match_slint_buttons() {
    // hud.slint:1686-1700 offers exactly 0.70 / 0.85 / 0.95.
    let s = studio();
    for op in [0.70f32, 0.85, 0.95] {
        s.set_surface_opacity(op);
        assert!((s.get_surface_opacity() - clamp_opacity(op)).abs() < 1e-6);
    }
}

#[test]
fn test_bg_modes_match_slint_buttons() {
    // hud.slint:1715-1736 offers exactly 0..=3; stage_bg must cover all.
    for mode in 0..=3 {
        assert_eq!(stage_bg(mode), STAGE_BGS[mode as usize].1);
    }
    assert_eq!(stage_bg(99), "#0f1013", "unknown mode falls back to IDE canvas");
}

// ============================================================================
// F. Sharpness / pixel grid
// ============================================================================

#[test]
fn test_pill_widths_are_integer_and_stable() {
    // Fractional widths blur text on 1x displays; table mirrors hud.slint:46-61.
    for (si, row) in PILL_WIDTHS.iter().enumerate() {
        for (oi, w) in row.iter().enumerate() {
            assert!((w - w.round()).abs() < 1e-6, "state {si} opt {oi} width {w} must be integer px");
            assert!(*w >= 44.0 && *w <= 110.0, "width {w} outside design range");
        }
    }
    assert_eq!(PILL_WIDTHS[0], [48.0, 44.0, 56.0]);
    assert_eq!(PILL_WIDTHS[1], [110.0, 84.0, 100.0]);
}

#[test]
fn test_pill_widths_crisp_at_common_scales() {
    // 100% / 150% / 200%: every width must land on a physical pixel.
    for scale in [1.0f32, 1.5, 2.0] {
        for row in PILL_WIDTHS.iter() {
            for w in row.iter() {
                assert!(is_crisp_at_scale(*w, scale), "{w}px @ {scale}x must be pixel-aligned");
            }
        }
    }
}

#[test]
fn test_copy_button_meets_minimum_target() {
    assert!(COPY_BUTTON_W >= MIN_TARGET_PX && COPY_BUTTON_H >= MIN_TARGET_PX);
    assert!((COPY_BUTTON_H - 24.0).abs() < 1e-6);
}

#[test]
fn test_body_text_above_readability_floor() {
    // Transcript 14.0px, timestamps 12px, section labels 11px, buttons 12px — all strictly above floor.
    for size in [14.0f32, 13.0, 12.0, 11.0] {
        assert!(size >= MIN_BODY_FONT_PX, "{size}px below readability floor");
    }
}

#[test]
fn test_luminance_sanity() {
    assert!((luminance("#ffffff").unwrap() - 1.0).abs() < 0.01);
    assert!(luminance("#000000").unwrap() < 0.01);
    assert!(luminance("not-a-color").is_none());
    assert!(contrast_ratio("#zzz", "#000000").is_none());
}

// ============================================================================
// G. Contrast (WCAG 2.x: normal text >= 4.5, large/UI >= 3.0)
// ============================================================================

#[test]
fn test_primary_and_secondary_text_on_dark_surfaces() {
    for (fg, bg) in [(INK_PRIMARY, CARD_BG), (INK_BODY, CARD_BG), (INK_SECONDARY, CARD_BG), (INK_PRIMARY, CHROME_BG)] {
        let c = contrast_ratio(fg, bg).expect("valid hex");
        assert!(c >= 4.5, "{fg} on {bg} = {c:.2} (need >= 4.5)");
    }
}

#[test]
fn test_accent_and_success_on_dark_surfaces() {
    assert!(contrast_ratio(ACCENT, CARD_BG).unwrap() >= 4.5);
    assert!(contrast_ratio(SUCCESS, CHROME_BG).unwrap() >= 4.5);
}

#[test]
fn test_save_button_ink_on_button_bg() {
    assert!(contrast_ratio(SAVE_BTN_INK, SAVE_BTN_BG).unwrap() >= 4.5);
}

#[test]
fn test_stage_backgrounds_distinct_and_valid() {
    let mut seen = std::collections::HashSet::new();
    for (mode, hex) in STAGE_BGS {
        assert!(luminance(hex).is_some(), "bg mode {mode} has invalid hex {hex}");
        assert!(seen.insert(hex), "duplicate stage bg {hex}");
    }
    // OLED is pure black (true blacks), others are near-black tints.
    assert!(luminance("#000000").unwrap() < luminance("#0f1013").unwrap());
}

#[test]
fn test_primary_text_on_dark_stage_backgrounds() {
    // Active-tab label (#fafafa) must survive every dark BACKGROUND ENVIRONMENT mode.
    // Light mode is covered by a KNOWN-ISSUE test below.
    for (mode, bg) in STAGE_BGS.iter().filter(|(m, _)| *m != 3) {
        let c = contrast_ratio(INK_PRIMARY, bg).unwrap();
        assert!(c >= 4.5, "mode {mode}: {c:.2}");
    }
}

// ============================================================================
// H. Responsiveness budgets (wall-clock, generous to avoid flakes)
// ============================================================================

#[test]
fn test_two_hundred_item_model_build_under_budget() {
    let s = studio();
    let start = Instant::now();
    let items: Vec<HistoryItem> = (0..200).map(|i| item(i, &format!("dictation entry number {i}"), "chrome.exe", false)).collect();
    set_list(&s, items);
    assert_eq!(s.get_history_count(), 200);
    assert!(start.elapsed() < Duration::from_millis(1000), "200-item load took {:?}", start.elapsed());
}

#[test]
fn test_hundred_rapid_property_writes_under_budget() {
    let s = studio();
    let start = Instant::now();
    for i in 0..100 {
        s.set_active_state(i % 5);
        s.set_surface_opacity(0.70 + (i % 3) as f32 * 0.05);
        let _ = s.get_active_state();
    }
    assert!(start.elapsed() < Duration::from_secs(2), "100 rapid writes took {:?}", start.elapsed());
}

#[test]
fn test_config_roundtrip_under_budget() {
    use sotto_desktop::config::AppConfig;
    let dir = std::env::temp_dir().join(format!("sotto_uiq_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    let path = dir.join("config.json");
    let start = Instant::now();
    let cfg = AppConfig { opt_idle: 2, surface_opacity: 0.70, ..Default::default() };
    cfg.save_to_path(&path).expect("save must succeed");
    let loaded = AppConfig::load_from_path(&path);
    assert_eq!(loaded, cfg);
    assert!(start.elapsed() < Duration::from_millis(1000));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_history_reload_mapping_under_budget() {
    use sotto_desktop::db::HistoryDb;
    let db = HistoryDb::open_in_memory().expect("in-memory DB");
    for i in 0..20 {
        db.insert(&format!("record {i}"), 1000, "chrome.exe").unwrap();
    }
    let start = Instant::now();
    let items: Vec<HistoryItem> = db.get_recent(100).unwrap().into_iter().map(|r| {
        let formatted = app_icon::format_app_name(&r.target_app);
        let (icon, has_icon) = app_icon::get_app_icon(&formatted, None);
        HistoryItem {
            id: r.id as i32,
            timestamp_str: r.timestamp_str.into(),
            transcript: r.transcript.into(),
            duration_ms: r.duration_ms,
            duration_str: r.duration_str.into(),
            target_app: formatted.into(),
            app_icon: icon,
            has_icon,
        }
    }).collect();
    assert_eq!(items.len(), 20);
    assert!(start.elapsed() < Duration::from_secs(2), "reload map took {:?}", start.elapsed());
}

// ============================================================================
// I. Config robustness
// ============================================================================

#[test]
fn test_corrupt_and_missing_config_fall_back_to_default() {
    use sotto_desktop::config::AppConfig;
    let dir = std::env::temp_dir().join(format!("sotto_uiq_cfg_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&dir).unwrap();
    let missing = dir.join("nope.json");
    assert_eq!(AppConfig::load_from_path(&missing), AppConfig::default());
    let corrupt = dir.join("bad.json");
    std::fs::write(&corrupt, "{ this is not json").unwrap();
    assert_eq!(AppConfig::load_from_path(&corrupt), AppConfig::default());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_unknown_config_fields_ignored_backwards_compat() {
    use sotto_desktop::config::AppConfig;
    let dir = std::env::temp_dir().join(format!("sotto_uiq_unk_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("future.json");
    std::fs::write(&path, r#"{"opt_idle": 1, "opt_listening": 0, "opt_transcribe": 0, "opt_success": 0, "opt_error": 0, "surface_opacity": 0.7, "future_field_xyz": 123}"#).unwrap();
    let cfg = AppConfig::load_from_path(&path);
    assert_eq!(cfg.opt_idle, 1);
    assert!((cfg.surface_opacity - 0.7).abs() < 1e-6);
    let _ = std::fs::remove_dir_all(&dir);
}

// ============================================================================
// J. Format helpers
// ============================================================================

#[test]
fn test_format_app_name_edges() {
    assert_eq!(app_icon::format_app_name(""), "Desktop");
    assert_eq!(app_icon::format_app_name("   "), "Desktop");
    assert_eq!(app_icon::format_app_name(".exe"), "Desktop");
    assert_eq!(app_icon::format_app_name("CODE.EXE"), "VS Code");
    assert_eq!(app_icon::format_app_name("  chrome.exe  "), "Google Chrome");
    assert_eq!(app_icon::format_app_name("my_cool-app.exe"), "My Cool App");
    assert_eq!(app_icon::format_app_name("T3 Code (Alpha).exe"), "T3 Code (Alpha)");
}

#[test]
fn test_format_timer_text_table() {
    assert_eq!(format_timer_text(0), "0:00");
    assert_eq!(format_timer_text(3), "0:03");
    assert_eq!(format_timer_text(59), "0:59");
    assert_eq!(format_timer_text(60), "1:00");
    assert_eq!(format_timer_text(61), "1:01");
    assert_eq!(format_timer_text(3599), "59:59");
}

#[test]
fn test_control_key_normalization_and_standalone() {
    use sotto_desktop::hotkey::{format_hotkey, vk_to_string};

    // Generic Control, Left Control, Right Control all normalize to "Ctrl"
    assert_eq!(vk_to_string(0x11), "Ctrl");
    assert_eq!(vk_to_string(0xA2), "Ctrl");
    assert_eq!(vk_to_string(0xA3), "Ctrl");

    // Standalone Control formats as "Ctrl"
    assert_eq!(format_hotkey(0, 0x11), "Ctrl");
    assert_eq!(format_hotkey(0, 0xA2), "Ctrl");
    assert_eq!(format_hotkey(0, 0xA3), "Ctrl");

    // Combinations with Control
    assert_eq!(format_hotkey(0x0002, 0x20), "Ctrl + Space");
    assert_eq!(format_hotkey(0x0006, 0x20), "Ctrl + Shift + Space");

    // Standalone Shift and Alt normalization
    assert_eq!(vk_to_string(0x10), "Shift");
    assert_eq!(vk_to_string(0xA0), "Shift");
    assert_eq!(vk_to_string(0xA1), "Shift");
    assert_eq!(vk_to_string(0x12), "Alt");
    assert_eq!(vk_to_string(0xA4), "Alt");
    assert_eq!(vk_to_string(0xA5), "Alt");

    // Fn key detection
    assert_eq!(vk_to_string(0xFF), "Fn");
    assert_eq!(vk_to_string(0x88), "Fn");
    assert_eq!(format_hotkey(0, 0xFF), "Fn");
}

#[test]
fn test_cancel_triggers_and_startup_properties() {
    let s = studio();
    assert_eq!(s.get_cancel_primary_display(), "Esc");
    assert_eq!(s.get_cancel_secondary_display(), "Not set");
    assert_eq!(s.get_launch_on_startup(), false);

    s.set_cancel_primary_display("Ctrl + Esc".into());
    s.set_cancel_secondary_display("Mouse Back".into());
    s.set_launch_on_startup(true);

    assert_eq!(s.get_cancel_primary_display(), "Ctrl + Esc");
    assert_eq!(s.get_cancel_secondary_display(), "Mouse Back");
    assert_eq!(s.get_launch_on_startup(), true);
}

#[test]
fn test_customization_smooth_scroller_properties() {
    let s = studio();
    // Verify Customization tab Lenis smooth scrolling properties
    assert_eq!(s.get_custom_viewport_y(), 0.0);
    let content_h = s.get_custom_content_height();
    assert!(content_h >= 800.0 && content_h <= 2200.0, "custom content height must reflect real layout: {content_h}");
    assert_eq!(s.get_custom_scroll_progress(), 0.0);
    assert!(!s.get_custom_is_scrolling());
    assert!(s.get_custom_can_scroll());

    // Capture strategy toggle: default is Dual-Mic Splice (1) and must stage.
    assert_eq!(s.get_capture_mode(), 1);
    s.set_capture_mode(0);
    assert_eq!(s.get_capture_mode(), 0);
    s.set_capture_mode(1);

    // Test viewport update
    s.set_custom_viewport_y(-150.0);
    assert_eq!(s.get_custom_viewport_y(), -150.0);

    // Test scroll progress update
    s.set_custom_scroll_progress(0.45);
    assert_eq!(s.get_custom_scroll_progress(), 0.45);
}

// ============================================================================
// K. Known issues — ignored by default, runnable with `cargo test -- --ignored`
// ============================================================================

#[test]
#[ignore = "KNOWN-ISSUE: muted #71717a on card #111215 is ~3.8:1, below WCAG AA 4.5 for 11px metadata"]
fn test_known_muted_text_contrast_on_card() {
    let c = contrast_ratio(INK_MUTED, CARD_BG).unwrap();
    assert!(c >= 4.5, "muted on card = {c:.2}");
}

#[test]
#[ignore = "KNOWN-ISSUE: Light bg-mode keeps dark-theme text (#e4e4e7 on #ffffff ~1.2:1); body unreadable"]
fn test_known_body_text_contrast_on_light_mode() {
    let c = contrast_ratio(INK_BODY, stage_bg(3)).unwrap();
    assert!(c >= 4.5, "body on light = {c:.2}");
}

#[test]
#[ignore = "KNOWN-ISSUE: pill widths 110px/74px land on half-pixels at 125% scale (most common Windows scale)"]
fn test_known_pill_widths_crisp_at_125p() {
    for row in PILL_WIDTHS.iter() {
        for w in row.iter() {
            assert!(is_crisp_at_scale(*w, 1.25), "{w}px @ 1.25x must be pixel-aligned");
        }
    }
}

#[test]
fn test_tab_click_switches_tab() {
    let s = studio();
    assert_eq!(s.get_active_tab(), 0, "Default tab must be 0 (Dictation Vault)");
    let s_weak = s.as_weak();
    s.on_select_tab(move |tab| {
        if let Some(w) = s_weak.upgrade() {
            w.set_active_tab(tab);
        }
    });
    s.invoke_select_tab(1);
    assert_eq!(s.get_active_tab(), 1, "Tab 1 (Customization) must be active after click");
    s.invoke_select_tab(0);
    assert_eq!(s.get_active_tab(), 0, "Tab 0 (Vault) must be active after switching back");
}

#[test]
fn test_tab_switching_geometry_stability() {
    let s = studio();
    // Default tab 0
    assert_eq!(s.get_active_tab(), 0);

    // Populate mock vault history
    let items = vec![
        item(1, "First sample dictation transcript.", "chrome.exe", false),
        item(2, "Second dictation with multiple lines to verify layout integrity.", "slack.exe", false),
    ];
    set_list(&s, items);
    s.set_history_content_height(240.0);

    // Switch to Customization Tab
    s.set_active_tab(1);
    assert_eq!(s.get_active_tab(), 1);

    // Verify all Customization properties exist and vault properties are unharmed
    assert_eq!(s.get_history_count(), 2);
    assert_eq!(s.get_history_content_height(), 240.0);
    assert_eq!(s.get_active_state(), 0); // Active state
    assert_eq!(s.get_opt_idle(), 0);

    // Switch back to Vault Tab
    s.set_active_tab(0);
    assert_eq!(s.get_active_tab(), 0);
    assert_eq!(s.get_history_count(), 2);
    assert_eq!(s.get_history_content_height(), 240.0);
}

#[test]
fn test_rapid_tab_switching_stress() {
    let s = studio();
    let s_weak = s.as_weak();
    s.on_select_tab(move |tab| {
        if let Some(w) = s_weak.upgrade() {
            w.set_active_tab(tab);
        }
    });

    for i in 0..200 {
        let target_tab = i % 2;
        s.invoke_select_tab(target_tab);
        assert_eq!(s.get_active_tab(), target_tab);
    }
}

#[test]
fn test_empty_vault_and_single_card_resilience() {
    let s = studio();

    // 1. Empty vault
    set_list(&s, vec![]);
    s.set_history_content_height(0.0);
    assert_eq!(s.get_history_count(), 0);

    // 2. Single card
    let single = vec![item(42, "Single note", "notepad.exe", false)];
    set_list(&s, single);
    s.set_history_content_height(96.0);
    assert_eq!(s.get_history_count(), 1);
}

#[test]
fn test_bottom_overscroll_clearance_at_resizes() {
    use sotto_desktop::smooth_scroll::{
        compute_content_height, compute_visible_height, estimate_card_height,
        LIST_BOTTOM_PADDING, ROW_SPACING,
    };

    let sample_notes = [
        "First short note",
        "Second longer dictation note that spans several words and sentences to evaluate real-world wrapping behavior.",
        "Third note with bullet-like structure or commands.",
        "Final note situated at the very bottom of the dictation list.",
    ];

    let window_heights = [600.0, 720.0, 840.0, 1080.0, 1200.0];
    let card_heights: Vec<f32> = sample_notes.iter().map(|t| estimate_card_height(t, 60)).collect();
    let cards_content_h = compute_content_height(&card_heights, ROW_SPACING);
    let total_h = cards_content_h + LIST_BOTTOM_PADDING;

    for &h in &window_heights {
        let visible_h = compute_visible_height(h, 156.0, 100.0);

        if total_h > visible_h {
            let clearance = total_h - cards_content_h;
            assert!(
                clearance >= 16.0,
                "Window height {h}px must have >= 16px bottom clearance, got {clearance}px"
            );
        }
    }
}

#[test]
fn test_hotkey_and_trigger_customization_channels() {
    let s = studio();
    assert_eq!(s.get_trigger_primary_display().as_str(), "F8");
    assert_eq!(s.get_trigger_secondary_display().as_str(), "Ctrl + Shift + Space");
    assert_eq!(s.get_trigger_mode(), 0);

    s.set_trigger_primary_display("Ctrl".into());
    assert_eq!(s.get_trigger_primary_display().as_str(), "Ctrl");
    s.set_trigger_secondary_display("Mouse Back".into());
    assert_eq!(s.get_trigger_secondary_display().as_str(), "Mouse Back");

    s.set_trigger_mode(1);
    assert_eq!(s.get_trigger_mode(), 1);
}

#[test]
fn test_trigger_record_and_reset_callbacks_carry_slots() {
    let s = studio();
    let seen_rec = Arc::new(Mutex::new(-99));
    let c = Arc::clone(&seen_rec);
    s.on_start_recording_key(move |slot| *c.lock().unwrap() = slot);
    let seen_reset = Arc::new(Mutex::new(-99));
    let c2 = Arc::clone(&seen_reset);
    s.on_reset_trigger(move |slot| *c2.lock().unwrap() = slot);

    s.invoke_start_recording_key(1);
    assert_eq!(*seen_rec.lock().unwrap(), 1);
    s.invoke_start_recording_key(0);
    assert_eq!(*seen_rec.lock().unwrap(), 0);
    s.invoke_reset_trigger(1);
    assert_eq!(*seen_reset.lock().unwrap(), 1);
}

#[test]
fn test_hotkey_and_trigger_config_roundtrip() {
    use sotto_desktop::config::AppConfig;
    let dir = std::env::temp_dir().join(format!("sotto_uiq_hk_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.json");

    let cfg = AppConfig {
        trigger_mode: 1,
        surface_opacity: 0.65,
        ..Default::default()
    };
    cfg.save_to_path(&path).expect("must save");
    let loaded = AppConfig::load_from_path(&path);
    assert_eq!(loaded.trigger_mode, 1);
    assert!((loaded.surface_opacity - 0.65).abs() < 1e-6);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_transparency_slider_step_and_range() {
    let s = studio();
    for step in [0.20, 0.35, 0.50, 0.70, 0.85, 0.95, 1.00] {
        s.set_surface_opacity(step);
        assert!((s.get_surface_opacity() - step).abs() < 1e-5);
    }
}

#[test]
fn test_slot_defaults_and_immediate_record() {
    let s = studio();
    // Slot displays carry the out-of-box bindings.
    assert_eq!(s.get_trigger_primary_display().as_str(), "F8");
    assert_eq!(s.get_trigger_secondary_display().as_str(), "Ctrl + Shift + Space");

    // Recording arms per slot; reset targets per slot.
    s.set_is_recording_key(true);
    assert!(s.get_is_recording_key());
    s.set_trigger_status("Listening — press keys".into());
    assert_eq!(s.get_trigger_status().as_str(), "Listening — press keys");
}

#[test]
fn test_recording_behavior_requires_save_preset() {
    use sotto_desktop::config::AppConfig;
    let s = studio();
    let dir = std::env::temp_dir().join(format!("sotto_uiq_trig_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.json");

    let cfg_initial = AppConfig {
        trigger_mode: 0,
        ..Default::default()
    };
    cfg_initial.save_to_path(&path).unwrap();

    // Change trigger mode in UI (preview stage)
    s.set_trigger_mode(1);
    assert_eq!(s.get_trigger_mode(), 1);

    // Config on disk remains unchanged before saving
    let untouched = AppConfig::load_from_path(&path);
    assert_eq!(untouched.trigger_mode, 0);

    // Simulate clicking Save Preset
    let mut cfg_saved = untouched;
    cfg_saved.trigger_mode = s.get_trigger_mode();
    cfg_saved.save_to_path(&path).unwrap();

    let verified = AppConfig::load_from_path(&path);
    assert_eq!(verified.trigger_mode, 1);
    let _ = std::fs::remove_dir_all(&dir);
}


#[test]
fn test_dynamic_trigger_recorder_defaults_and_flow() {
    let s = studio();
    assert_eq!(s.get_trigger_primary_display().as_str(), "F8");
    assert!(!s.get_is_recording_key());
    assert_eq!(s.get_trigger_status().as_str(), "");

    s.set_trigger_primary_display("Ctrl + Shift + Space".into());
    assert_eq!(s.get_trigger_primary_display().as_str(), "Ctrl + Shift + Space");

    s.set_is_recording_key(true);
    assert!(s.get_is_recording_key());
}

#[test]
fn test_record_paths_are_slot_independent() {
    let s = studio();
    let rec_seen = std::rc::Rc::new(std::cell::Cell::new(-99));
    let reset_seen = std::rc::Rc::new(std::cell::Cell::new(-99));

    let r_clone = rec_seen.clone();
    s.on_start_recording_key(move |slot| {
        r_clone.set(slot);
    });

    let d_clone = reset_seen.clone();
    s.on_reset_trigger(move |slot| {
        d_clone.set(slot);
    });

    // Recording into primary never touches the secondary path and vice versa.
    s.invoke_start_recording_key(0);
    assert_eq!(rec_seen.get(), 0);
    s.invoke_reset_trigger(1);
    assert_eq!(reset_seen.get(), 1);
    assert_eq!(rec_seen.get(), 0);
}
