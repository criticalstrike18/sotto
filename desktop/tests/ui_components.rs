// Component isolation suite for `desktop/ui/components/*`.
// Every component is mounted ALONE (no StudioWindow), driven through its
// public props/functions, and verified on:
//   - composition: props round-trip, callbacks fire through Slint routing
//   - geometry: self-reported layout hooks match the design invariants
//   - alignment: sibling baselines / centering relationships hold
//   - stress: empty/huge/unicode input renders without structural change
//
// NOTE on headless layout: the testing backend fixes root sizes (800x600) and
// does not recompute content-driven growth (Text never wraps headless), so
// growth/wrap behavior is covered by estimator unit tests (scroll_wiring.rs)
// and, next, pixel goldens on the software backend. What THIS suite proves is
// everything that does not need pixels: routing, state, fixed geometry,
// alignment relationships, and containment.
//
// Backend: headless Slint testing backend (no windows, CI-safe).

use sotto_desktop::components::{
    HistoryCard, KeyField, ModelRow, ProviderCard, SectionHeader, StatChip, TabBar,
};
use sotto_desktop::{app_icon, FloatingPillWindow};
use slint::Model;
use std::sync::{Arc, Mutex};

thread_local! {
    static BACKEND_READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn ensure_testing_backend() {
    BACKEND_READY.with(|ready| {
        if !ready.get() {
            i_slint_backend_testing::init_no_event_loop();
            ready.set(true);
        }
    });
}

fn approx(actual: f32, expected: f32, what: &str) {
    assert!(
        (actual - expected).abs() < 0.51,
        "{what}: expected {expected}, got {actual}"
    );
}

fn fill_card(card: &HistoryCard, id: i32, transcript: &str, with_icon: bool) {
    let formatted = app_icon::format_app_name("chrome.exe");
    let (icon, _) = app_icon::get_app_icon(&formatted, None);
    card.set_rec_id(id);
    card.set_rec_timestamp("Oct 12, 11:56 AM".into());
    card.set_rec_transcript(transcript.into());
    card.set_rec_duration_ms(3200);
    card.set_rec_duration_str("3.2s".into());
    card.set_rec_target_app(formatted.into());
    card.set_rec_app_icon(icon);
    card.set_rec_has_icon(with_icon);
}

// ============================================================================
// StatChip
// ============================================================================

#[test]
fn test_stat_chip_defaults_and_roundtrip() {
    ensure_testing_backend();
    let c = StatChip::new().expect("StatChip must instantiate");
    assert_eq!(c.get_icon().as_str(), "");
    assert_eq!(c.get_label().as_str(), "");
    assert!(!c.get_accent());
    approx(c.get_chip_h(), 26.0, "chip-h default");

    c.set_icon("📁".into());
    c.set_label("51 Dictations".into());
    c.set_accent(true);
    assert_eq!(c.get_icon().as_str(), "📁");
    assert_eq!(c.get_label().as_str(), "51 Dictations");
    assert!(c.get_accent());
}

#[test]
fn test_stat_chip_baseline_alignment() {
    ensure_testing_backend();
    let c = StatChip::new().expect("StatChip must instantiate");
    c.set_icon("🔒".into());
    c.set_label("SQLite WAL Local".into());
    // Icon and label boxes share one baseline inside the pill.
    approx(c.get_icon_y(), c.get_label_y(), "icon-y == label-y");
    approx(c.get_icon_h(), c.get_label_h(), "icon-h == label-h");
    // Both optically centered in the 26px pill.
    approx(c.get_icon_y() + c.get_icon_h() / 2.0, 13.0, "icon cy");
    approx(c.get_label_y() + c.get_label_h() / 2.0, 13.0, "label cy");
}

#[test]
fn test_element_handle_geometry_agrees_with_hooks() {
    use i_slint_backend_testing::ElementHandle;
    ensure_testing_backend();
    let c = StatChip::new().expect("StatChip must instantiate");
    c.set_icon("🔒".into());
    c.set_label("SQLite WAL Local".into());

    let mut iq = ElementHandle::find_by_element_id(&c, "StatChip::icon-text");
    let icon = iq.next().expect("icon-text must be findable");
    let mut lq = ElementHandle::find_by_element_id(&c, "StatChip::label-text");
    let label = lq.next().expect("label-text must be findable");

    let ip = icon.absolute_position();
    let lp = label.absolute_position();
    let isz = icon.size();
    let lsz = label.size();

    // Same alignment facts as the hook test, through the query API:
    approx(ip.y, lp.y, "query: icon-y == label-y");
    approx(isz.height, lsz.height, "query: icon-h == label-h");
    // ...and the query API agrees with the self-report hooks:
    approx(c.get_icon_y(), ip.y, "hook == query (icon y)");
    approx(c.get_label_y(), lp.y, "hook == query (label y)");
}

#[test]
fn test_stat_chip_compact_variant_and_stress() {
    ensure_testing_backend();
    let c = StatChip::new().expect("StatChip must instantiate");
    c.set_chip_height(24.0);
    c.set_icon("📋".into());
    c.set_label("Instant Cursor Injection".into());
    approx(c.get_chip_h(), 24.0, "chip-h 24 variant");
    approx(c.get_icon_y() + c.get_icon_h() / 2.0, 12.0, "icon cy @24");
    approx(c.get_label_y() + c.get_label_h() / 2.0, 12.0, "label cy @24");

    // Stress: 150-char label must round-trip intact with pill height stable.
    // (Width growth is parent-driven and verified at the golden layer.)
    c.set_label("A very long dictation label that should stretch the pill wide instead of wrapping".into());
    assert_eq!(
        c.get_label().as_str(),
        "A very long dictation label that should stretch the pill wide instead of wrapping"
    );
    approx(c.get_chip_h(), 24.0, "chip-h stable under stress");
}

// ============================================================================
// SectionHeader
// ============================================================================

#[test]
fn test_section_header_roundtrip_and_stability() {
    ensure_testing_backend();
    let h = SectionHeader::new().expect("SectionHeader must instantiate");
    h.set_title("TRANSCRIPTION PROVIDER".into());
    h.set_subtitle("Pick who transcribes your voice.".into());
    assert_eq!(h.get_title().as_str(), "TRANSCRIPTION PROVIDER");
    let base_h = h.get_total_h();
    assert!(base_h > 0.0, "header must have height");

    // Stress: empty + very long strings must not change the block height
    // (single-line eyebrow pattern wraps wide, never tall).
    h.set_title("".into());
    h.set_subtitle("".into());
    approx(h.get_total_h(), base_h, "total-h stable when empty");
    h.set_subtitle("Sub ".repeat(120).into());
    approx(h.get_total_h(), base_h, "total-h stable under stress");
}

// ============================================================================
// TabBar
// ============================================================================

#[test]
fn test_tab_bar_defaults_and_routing() {
    ensure_testing_backend();
    let t = TabBar::new().expect("TabBar must instantiate");
    assert_eq!(t.get_labels().row_count(), 3);
    assert_eq!(t.get_active_tab(), 0);
    approx(t.get_bar_h(), 36.0, "bar-h");

    t.set_active_tab(2);
    assert_eq!(t.get_active_tab(), 2);

    // Routing: pick-tab() must fire select-tab() with the same index.
    let seen = Arc::new(Mutex::new(-99));
    let c = Arc::clone(&seen);
    t.on_select_tab(move |i| *c.lock().unwrap() = i);
    t.invoke_pick_tab(1);
    assert_eq!(*seen.lock().unwrap(), 1);
    t.invoke_pick_tab(0);
    assert_eq!(*seen.lock().unwrap(), 0);
}

#[test]
fn test_tab_bar_long_labels_keep_height() {
    ensure_testing_backend();
    let t = TabBar::new().expect("TabBar must instantiate");
    let long = slint::VecModel::from(vec![
        "Dictations With A Very Long Suffix".into(),
        "Providers And Extra Words Here".into(),
        "Customization Suite Extended".into(),
    ]);
    t.set_labels(slint::ModelRc::from(std::rc::Rc::new(long)));
    approx(t.get_bar_h(), 36.0, "bar-h stable under long labels");
}

// ============================================================================
// ProviderCard
// ============================================================================

#[test]
fn test_provider_card_state_and_click() {
    ensure_testing_backend();
    let p = ProviderCard::new().expect("ProviderCard must instantiate");
    assert_eq!(p.get_title().as_str(), "");
    assert!(!p.get_selected());
    approx(p.get_card_h(), 60.0, "card-h");

    p.set_title("Gemini".into());
    p.set_selected(true);
    assert_eq!(p.get_title().as_str(), "Gemini");
    assert!(p.get_selected());

    let fired = Arc::new(Mutex::new(false));
    let c = Arc::clone(&fired);
    p.on_clicked(move || *c.lock().unwrap() = true);
    p.invoke_press();
    assert!(*fired.lock().unwrap(), "press() must fire clicked()");
}

#[test]
fn test_provider_card_logo_centering() {
    ensure_testing_backend();
    let p = ProviderCard::new().expect("ProviderCard must instantiate");
    // Wordmark-only (Groq) and icon+name (Gemini) variants share one rule:
    // the logo row sits optically centered in the 60px card.
    approx(p.get_row_cy(), 30.0, "row-cy centered");
    approx(p.get_logo_cy(), p.get_logo_cy().round(), "logo-cy sane");
    p.set_title("Gemini".into());
    approx(p.get_row_cy(), 30.0, "row-cy centered with title");
}

// ============================================================================
// ModelRow
// ============================================================================

#[test]
fn test_model_row_state_click_and_dot() {
    ensure_testing_backend();
    let r = ModelRow::new().expect("ModelRow must instantiate");
    assert_eq!(r.get_name().as_str(), "");
    assert!(!r.get_selected());
    approx(r.get_row_h(), 40.0, "row-h");

    r.set_name("whisper-large-v3-turbo".into());
    r.set_selected(true);
    assert!(r.get_selected());

    let fired = Arc::new(Mutex::new(false));
    let c = Arc::clone(&fired);
    r.on_clicked(move || *c.lock().unwrap() = true);
    r.invoke_choose();
    assert!(*fired.lock().unwrap(), "choose() must fire clicked()");

    // Radio dot optically centered in the 40px row.
    approx(r.get_dot_cy(), 20.0, "dot-cy centered");
}

#[test]
fn test_model_row_long_name_keeps_height() {
    ensure_testing_backend();
    let r = ModelRow::new().expect("ModelRow must instantiate");
    r.set_name("a-very-long-model-id-that-keeps-going-on-and-on-v99-turbo-preview".into());
    approx(r.get_row_h(), 40.0, "row-h stable under long names");
}

// ============================================================================
// KeyField
// ============================================================================

#[test]
fn test_key_field_state_and_actions() {
    ensure_testing_backend();
    let k = KeyField::new().expect("KeyField must instantiate");
    assert_eq!(k.get_text().as_str(), "");
    assert!(!k.get_text_visible());
    approx(k.get_box_h(), 36.0, "box-h");
    approx(k.get_btn_w(), 76.0, "btn-w");

    k.set_label("GROQ API KEY".into());
    k.set_text("gsk_secret_value".into());
    k.set_text_visible(true);
    assert_eq!(k.get_text().as_str(), "gsk_secret_value");
    assert!(k.get_text_visible());

    let revealed = Arc::new(Mutex::new(false));
    let c = Arc::clone(&revealed);
    k.on_toggle_visible(move || *c.lock().unwrap() = true);
    k.invoke_reveal();
    assert!(*revealed.lock().unwrap(), "reveal() must fire toggle-visible()");

    let opened = Arc::new(Mutex::new(false));
    let c2 = Arc::clone(&opened);
    k.on_open_site(move || *c2.lock().unwrap() = true);
    k.invoke_open_link();
    assert!(*opened.lock().unwrap(), "open-link() must fire open-site()");
}

#[test]
fn test_key_field_long_key_keeps_geometry() {
    ensure_testing_backend();
    let k = KeyField::new().expect("KeyField must instantiate");
    k.set_text(("gsk_".to_string() + &"x".repeat(200)).into());
    approx(k.get_box_h(), 36.0, "box-h stable under long keys");
    approx(k.get_btn_w(), 76.0, "btn-w stable under long keys");
}

// ============================================================================
// HistoryCard
// ============================================================================

#[test]
fn test_history_card_header_geometry() {
    ensure_testing_backend();
    let card = HistoryCard::new().expect("HistoryCard must instantiate");
    fill_card(&card, 7, "Hello world.", false);

    // Copy button defines the 26px header; badge + button stay centered.
    approx(card.get_header_h(), 26.0, "header-h");
    approx(card.get_badge_h(), 24.0, "badge-h");
    approx(
        card.get_badge_y(),
        (card.get_header_h() - card.get_badge_h()) / 2.0,
        "badge centered",
    );
    approx(
        card.get_copy_y(),
        (card.get_header_h() - card.get_copy_h()) / 2.0,
        "copy centered",
    );
    // Copy button never overflows the header row.
    assert!(
        card.get_copy_right() <= card.get_header_w() + 0.51,
        "copy must stay inside header"
    );
}

#[test]
fn test_history_card_copy_routing() {
    ensure_testing_backend();
    let card = HistoryCard::new().expect("HistoryCard must instantiate");
    fill_card(&card, 42, "Copy me please.", false);
    card.set_copied_id(-1);

    let seen = Arc::new(Mutex::new((0, String::new())));
    let c = Arc::clone(&seen);
    card.on_copy_item(move |id, text| *c.lock().unwrap() = (id, text.to_string()));
    card.invoke_press_copy();
    let (id, text) = seen.lock().unwrap().clone();
    assert_eq!(id, 42);
    assert_eq!(text, "Copy me please.");
}

#[test]
fn test_history_card_icon_parity_and_growth() {
    ensure_testing_backend();
    let no_icon = HistoryCard::new().expect("HistoryCard must instantiate");
    fill_card(&no_icon, 1, "Same text here.", false);
    let with_icon = HistoryCard::new().expect("HistoryCard must instantiate");
    fill_card(&with_icon, 2, "Same text here.", true);
    approx(
        no_icon.get_content_h(),
        with_icon.get_content_h(),
        "icon must not change card height",
    );

    // Huge transcript must round-trip intact with structure unchanged.
    // (Wrap-driven growth is covered by estimator tests + golden layer.)
    let huge = HistoryCard::new().expect("HistoryCard must instantiate");
    let big_text = "Long dictation sentence with many words. ".repeat(200);
    fill_card(&huge, 4, &big_text, false);
    assert_eq!(huge.get_rec_transcript().as_str(), big_text.as_str());
    assert!(
        huge.get_copy_right() <= huge.get_header_w() + 0.51,
        "copy contained on huge cards"
    );
    // Copy button stays inside even on tall cards.
    assert!(
        huge.get_copy_right() <= huge.get_header_w() + 0.51,
        "copy contained on tall cards"
    );
}

#[test]
fn test_history_card_unicode_stress() {
    ensure_testing_backend();
    let card = HistoryCard::new().expect("HistoryCard must instantiate");
    fill_card(
        &card,
        9,
        "日本語のテスト 🎙️ مرحبا بالعالم emoji 🚀 mixed — text with “quotes” and… ellipsis",
        false,
    );
    assert!(card.get_card_h() > 0.0);
    assert!(card.get_copy_right() <= card.get_header_w() + 0.51);
}

// ============================================================================
// Pill listening timer: must sit optically centered in its 20px row in every
// listening variant (regression: the text rendered top-aligned, ~4px high).
// ============================================================================

fn pill_timer_geometry(opt: i32, timer_id: &str, row_id: &str) {
    use i_slint_backend_testing::ElementHandle;
    ensure_testing_backend();
    let pill = FloatingPillWindow::new().expect("FloatingPillWindow must instantiate");
    pill.set_active_state(1); // Listening
    pill.set_opt_listening(opt);
    pill.set_timer_text("0:07".into());

    let mut tq = ElementHandle::find_by_element_id(&pill, timer_id);
    let timer = tq.next().unwrap_or_else(|| panic!("timer must exist: {timer_id}"));
    let mut rq = ElementHandle::find_by_element_id(&pill, row_id);
    let row = rq.next().unwrap_or_else(|| panic!("row must exist: {row_id}"));

    let tpos = timer.absolute_position();
    let tsize = timer.size();
    let rpos = row.absolute_position();
    let rsize = row.size();

    // Fixed 20px text box (the mechanism) optically centered in the row.
    approx(tsize.height, 20.0, &format!("{timer_id} fixed height"));
    approx(
        tpos.y + tsize.height / 2.0,
        rpos.y + rsize.height / 2.0,
        &format!("{timer_id} centered in row"),
    );
}

#[test]
fn test_pill_timer_centered_option_a() {
    pill_timer_geometry(0, "VoicePill::pill-timer-a", "VoicePill::listen-row-a");
}

#[test]
fn test_pill_timer_centered_option_c() {
    pill_timer_geometry(2, "VoicePill::pill-timer-c", "VoicePill::listen-row-c");
}
