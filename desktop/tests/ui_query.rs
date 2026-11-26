// Overall-UI geometry suite: ElementHandle queries against the live
// StudioWindow (production composition, not isolated components).
// No event loop needed — geometry queries run headless. Requires the
// SLINT_EMIT_DEBUG_INFO build flag (set in build.rs).
//
// Backend: headless Slint testing backend (no windows, CI-safe).

use i_slint_backend_testing::{ElementHandle, ElementQuery};
use sotto_desktop::{app_icon, HistoryItem, StudioWindow};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::rc::Rc;

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

fn studio() -> StudioWindow {
    ensure_testing_backend();
    StudioWindow::new().expect("StudioWindow must instantiate")
}

fn find_one(root: &StudioWindow, path: &str) -> ElementHandle {
    let mut q = ElementHandle::find_by_element_id(root, path);
    q.next()
        .unwrap_or_else(|| panic!("element must be findable: {path}"))
}

fn item(id: i32, transcript: &str) -> HistoryItem {
    let formatted = app_icon::format_app_name("chrome.exe");
    let (icon, _) = app_icon::get_app_icon(&formatted, None);
    HistoryItem {
        id,
        timestamp_str: "Oct 12, 11:56 AM".into(),
        transcript: transcript.into(),
        duration_ms: 3200,
        duration_str: "3.2s".into(),
        target_app: formatted.into(),
        app_icon: icon,
        has_icon: false,
    }
}

fn approx(actual: f32, expected: f32, what: &str) {
    assert!(
        (actual - expected).abs() < 1.01,
        "{what}: expected {expected}, got {actual}"
    );
}

#[test]
fn test_stats_bar_rows_optically_centered() {
    let s = studio();
    s.set_history_count(3);

    let bar = find_one(&s, "StudioWindow::stats-bar");
    let chips = find_one(&s, "StudioWindow::chips-row");
    let actions = find_one(&s, "StudioWindow::actions-row");

    let bar_pos = bar.absolute_position();
    let bar_size = bar.size();
    approx(bar_size.height, 44.0, "stats bar height");

    for (name, row) in [("chips", chips), ("actions", actions)] {
        let pos = row.absolute_position();
        let size = row.size();
        approx(
            pos.y + size.height / 2.0,
            bar_pos.y + bar_size.height / 2.0,
            &format!("{name} row centered in bar"),
        );
    }
}

#[test]
fn test_purge_button_inside_bar() {
    let s = studio();
    s.set_history_count(5);

    let bar = find_one(&s, "StudioWindow::stats-bar");
    let purge = find_one(&s, "StudioWindow::purge-btn");

    let bar_pos = bar.absolute_position();
    let bar_size = bar.size();
    let purge_pos = purge.absolute_position();
    let purge_size = purge.size();

    assert!(purge_size.width > 0.0 && purge_size.height > 0.0, "purge must have size");
    approx(
        purge_pos.y + purge_size.height / 2.0,
        bar_pos.y + bar_size.height / 2.0,
        "purge centered in bar",
    );
    assert!(
        purge_pos.x >= bar_pos.x - 1.0
            && purge_pos.x + purge_size.width <= bar_pos.x + bar_size.width + 1.0,
        "purge must stay inside the bar horizontally"
    );
}

#[test]
fn test_purge_armed_buttons_inside_bar() {
    let s = studio();
    s.set_history_count(5);
    s.set_provider_chip_text("Groq whisper-large-v3-turbo".into());
    s.set_purge_confirm_armed(true);

    for (w, h) in [(760u32, 600u32), (1040u32, 840u32)] {
        s.window().set_size(slint::PhysicalSize::new(w, h));
        let bar = find_one(&s, "StudioWindow::stats-bar");
        let cancel = find_one(&s, "StudioWindow::purge-cancel-btn");
        let confirm = find_one(&s, "StudioWindow::purge-btn");

        let bar_pos = bar.absolute_position();
        let bar_size = bar.size();
        let c_pos = cancel.absolute_position();
        let c_size = cancel.size();
        let p_pos = confirm.absolute_position();
        let p_size = confirm.size();

        assert!(c_size.width > 0.0 && c_size.height > 0.0, "cancel must have size at {w}x{h}");
        assert!(p_size.width > c_size.width, "armed confirm button must expand at {w}x{h}");
        approx(
            c_pos.y + c_size.height / 2.0,
            bar_pos.y + bar_size.height / 2.0,
            &format!("cancel centered in bar at {w}x{h}"),
        );
        approx(
            p_pos.y + p_size.height / 2.0,
            bar_pos.y + bar_size.height / 2.0,
            &format!("confirm centered in bar at {w}x{h}"),
        );
        assert!(
            c_pos.x >= bar_pos.x - 1.0
                && c_pos.x + c_size.width <= p_pos.x + 1.0
                && p_pos.x + p_size.width <= bar_pos.x + bar_size.width + 1.0,
            "cancel and confirm buttons must sit non-overlapping inside bar at {w}x{h}"
        );
    }
}

#[test]
fn test_scrollbar_thumb_inside_track() {
    let s = studio();
    let items: Vec<HistoryItem> = (0..8)
        .map(|i| item(i, &"A reasonably long dictation transcript. ".repeat(6)))
        .collect();
    s.set_history_items(ModelRc::from(Rc::new(VecModel::from(items))));
    s.set_history_count(8);
    // Force scrollable state regardless of backend metrics.
    s.set_history_content_height(3000.0);

    let track = find_one(&s, "StudioWindow::scrollbar-track");
    let thumb = find_one(&s, "StudioWindow::scrollbar-thumb");

    let track_pos = track.absolute_position();
    let track_size = track.size();
    let thumb_pos = thumb.absolute_position();
    let thumb_size = thumb.size();

    assert!(thumb_size.height >= 36.0 - 1.0, "thumb respects minimum height");
    assert!(
        thumb_pos.y >= track_pos.y - 1.0
            && thumb_pos.y + thumb_size.height <= track_pos.y + track_size.height + 1.0,
        "thumb must stay inside the track"
    );
    approx(
        thumb_pos.x + thumb_size.width / 2.0,
        track_pos.x + track_size.width / 2.0,
        "thumb centered in track",
    );
}

#[test]
fn test_tab_bar_present_and_sized() {
    let s = studio();
    let tabs = find_one(&s, "StudioWindow::studio-tabs");
    let size = tabs.size();
    approx(size.height, 36.0, "tab bar height");
    assert!(size.width >= 400.0, "tab bar must fit three tabs");

    let logo = find_one(&s, "StudioWindow::brand-logo");
    let lpos = logo.absolute_position();
    let lsize = logo.size();
    let tpos = tabs.absolute_position();
    assert!(
        lsize.width >= 90.0 && lsize.width <= 180.0,
        "integrated brand lockup width must fit SVG mark + Sotto wordmark cleanly, got {}",
        lsize.width
    );
    approx(lsize.height, 36.0, "brand logo height matches tab bar");
    assert!(
        lpos.x + lsize.width < tpos.x,
        "brand logo must stay clear to the left of studio tabs"
    );
}

#[test]
fn test_last_card_reachable_at_max_scroll() {
    // Regression test for the clipped-last-card bug. Traversal only visits
    // laid-out (unclipped) elements, so a card cut below the viewport simply
    // disappears from queries. Only the LAST card carries a real image, making
    // it identifiable: at max scroll it must be found near the bottom edge.
    let s = studio();
    let marker = slint::Image::load_from_path(std::path::Path::new("assets/icons/check.svg"))
        .expect("marker asset must load");
    let mut items: Vec<HistoryItem> = (0..30)
        .map(|i| item(i, &"A reasonably long dictation transcript that wraps. ".repeat(4)))
        .collect();
    if let Some(last) = items.last_mut() {
        last.app_icon = marker;
        last.has_icon = true;
    }
    s.set_history_items(ModelRc::from(Rc::new(VecModel::from(items))));
    s.set_history_count(30);

    let view_h = s.get_history_viewport_height();
    assert!(view_h > 50.0, "viewport must be measurable");
    // Mirror the production formula (estimator + padding + overscroll).
    let cpl = sotto_desktop::smooth_scroll::chars_per_line_for(1040, 1.0);
    let sample = "A reasonably long dictation transcript that wraps. ".repeat(4);
    let heights: Vec<f32> = (0..30)
        .map(|_| sotto_desktop::smooth_scroll::estimate_card_height(&sample, cpl))
        .collect();
    let total = sotto_desktop::smooth_scroll::compute_content_height(
        &heights,
        sotto_desktop::smooth_scroll::ROW_SPACING,
    ) + sotto_desktop::smooth_scroll::LIST_BOTTOM_PADDING
        + sotto_desktop::smooth_scroll::LIST_OVERSCROLL_PX;
    s.set_history_content_height(total);
    s.set_history_viewport_y(-(total - view_h));

    let container = find_one(&s, "StudioWindow::history-container");
    let cpos = container.absolute_position();
    let csize = container.size();
    let bottom = cpos.y + csize.height;

    let imgs = ElementQuery::from_root(&s)
        .match_id("StudioWindow::history-container")
        .match_descendants()
        .match_id("HistoryCard::app-icon-img")
        .find_all();
    assert_eq!(imgs.len(), 1, "only the tail card inside history-container carries an app icon image");
    let marker_img = &imgs[0];
    let pos = marker_img.absolute_position();
    let size = marker_img.size();
    assert!(
        pos.y + size.height <= bottom + 1.0,
        "tail card must end inside the container"
    );
    assert!(
        pos.y + size.height >= bottom - 500.0,
        "tail card must actually reach the bottom region"
    );
}

#[test]
fn test_estimator_calibrated_against_layout() {
    // The content-height estimator must match laid-out card heights, or the
    // list drifts (cutoff when short, void when tall). History: base 78
    // clipped tails (-3px/card); base 83 + line 19 voided them (+5px/card).
    // Calibrated truth: single-line = 81px, 3-line = 115px, at 1040px.
    // Multi-width: residuals must stay within ±2px/card at 760/1040/1440.
    for width in [760u32, 1040u32, 1440u32] {
        let s = studio();
        s.window().set_size(slint::PhysicalSize::new(width, 840));
        let long_body = "The first thing I'm going to do is to make a video of the video. I'm going to show you how to use the video. I am now at the end of the day. ";
        let items: Vec<HistoryItem> = vec![
            item(0, "Short one."),
            item(1, "Short two."),
            item(2, &long_body.repeat(3)),
            item(3, &long_body.repeat(3)),
        ];
        s.set_history_items(ModelRc::from(Rc::new(VecModel::from(items))));
        s.set_history_count(4);

        let cards = ElementHandle::find_by_element_type_name(&s, "HistoryCard");
        let heights: Vec<f32> = cards.collect::<Vec<_>>().iter().map(|c| c.size().height).collect();
        assert_eq!(heights.len(), 4, "all four cards must lay out at {width}px");

        let scale = s.window().scale_factor().max(1.0);
        let cpl = sotto_desktop::smooth_scroll::chars_per_line_for(width, scale);
        let est = |t: &str| sotto_desktop::smooth_scroll::estimate_card_height(t, cpl);
        let expected = [
            est("Short one."),
            est("Short two."),
            est(&long_body.repeat(3)),
            est(&long_body.repeat(3)),
        ];

        // Order-independent aggregate: total rendered vs total estimated.
        let measured: f32 = heights.iter().sum();
        let predicted: f32 = expected.iter().sum();
        assert!(
            (measured - predicted).abs() <= 8.0,
            "at {width}px: estimator total {predicted:.0} must match laid-out total {measured:.0} (±2px/card)"
        );
        // Single-line cards pin the base constant exactly (the two shortest).
        let mut ordered = heights.clone();
        ordered.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for h in ordered.iter().take(2) {
            assert!(
                (h - 75.0).abs() <= 1.0,
                "at {width}px: single-line card must measure 75px, got {h:.0}"
            );
        }
    }
}
#[test]
fn test_scrollbar_gutter_clearance() {
    // The scrollbar lives in a dedicated 22px gutter: track inside gutter,
    // gutter pinned to the container's right edge, and no card content
    // (copy buttons = rightmost card elements) may reach into it.
    let s = studio();
    s.window().set_size(slint::PhysicalSize::new(1040, 840));
    let items: Vec<HistoryItem> = (0..10).map(|i| item(i, "A normal dictation note for gutter clearance.")).collect();
    s.set_history_items(ModelRc::from(Rc::new(VecModel::from(items))));
    s.set_history_count(10);
    s.set_history_content_height(3000.0);

    let container = find_one(&s, "StudioWindow::history-container");
    let cpos = container.absolute_position();
    let csize = container.size();
    let gutter = find_one(&s, "StudioWindow::scroll-gutter");
    let gpos = gutter.absolute_position();
    let gsize = gutter.size();

    approx(gsize.width, 22.0, "gutter width");
    approx(
        gpos.x + gsize.width,
        cpos.x + csize.width,
        "gutter pinned to container right edge",
    );

    let track = find_one(&s, "StudioWindow::scrollbar-track");
    let tpos = track.absolute_position();
    let tsize = track.size();
    assert!(
        tpos.x >= gpos.x - 1.0 && tpos.x + tsize.width <= gpos.x + gsize.width + 1.0,
        "track must stay inside the gutter"
    );

    let mut it = ElementHandle::find_by_element_id(&s, "HistoryCard::copy-btn");
    let mut count = 0;
    while let Some(btn) = it.next() {
        let pos = btn.absolute_position();
        let size = btn.size();
        assert!(
            pos.x + size.width <= gpos.x + 1.0,
            "card content must not reach into the gutter"
        );
        count += 1;
    }
    assert!(count > 0, "must find card buttons");
}

#[test]
fn test_chrome_stable_across_sizes() {
    let s = studio();
    s.set_history_count(4);
    for (w, h) in [(760u32, 600u32), (1440u32, 900u32)] {
        s.window().set_size(slint::PhysicalSize::new(w, h));
        let bar = find_one(&s, "StudioWindow::stats-bar");
        approx(bar.size().height, 44.0, &format!("bar height at {w}x{h}"));
        let tabs = find_one(&s, "StudioWindow::studio-tabs");
        approx(tabs.size().height, 36.0, &format!("tab height at {w}x{h}"));
        // Viewport formulas must match actual Slint container heights:
        // Tab 0 history-container = h - 156 (CHROME_RESERVED_PX)
        // Tab 1/2 content containers = h - 100 (CONTENT_CHROME_RESERVED_PX)
        approx(
            s.get_history_viewport_height(),
            h as f32 - sotto_desktop::smooth_scroll::CHROME_RESERVED_PX,
            &format!("viewport-height formula at {w}x{h}"),
        );
        approx(
            s.get_providers_viewport_height(),
            h as f32 - sotto_desktop::smooth_scroll::CONTENT_CHROME_RESERVED_PX,
            &format!("providers-viewport-height formula at {w}x{h}"),
        );
        approx(
            s.get_custom_viewport_height(),
            h as f32 - sotto_desktop::smooth_scroll::CONTENT_CHROME_RESERVED_PX,
            &format!("custom-viewport-height formula at {w}x{h}"),
        );
        let hist_container = find_one(&s, "StudioWindow::history-container");
        approx(
            hist_container.size().height,
            s.get_history_viewport_height(),
            &format!("history-container matches history-viewport-height at {w}x{h}"),
        );
        assert_eq!(s.get_active_tab(), 0);
        assert_eq!(s.get_history_count(), 4);
    }
}

#[test]
fn test_no_excess_void_at_max_scroll() {
    // Regression test for the end-of-list void: with the list scrolled to
    // the very bottom, live card content must reach near the container bottom
    // edge. An overestimated content-height parks the viewport past the real
    // content, leaving a dead void (and the thumb pinned at track bottom).
    // Standard: the lowest visible card button sits within 150px above the
    // container bottom (cards are ~90px pitch, so content always reaches).
    let s = studio();
    // Production geometry: the estimator is calibrated for 1040px wide.
    s.window().set_size(slint::PhysicalSize::new(1040, 840));
    // Long multi-line transcripts like real dictations: line-height error
    // accumulates per wrapped line, base error per card. Single-line fixtures
    // cannot reproduce end-of-list drift, so the first 24 cards are long.
    let long_body = "The first thing I'm going to do is to make a video of the video. I'm going to show you how to use the video. I am now at the end of the day. ";
    let mut items: Vec<HistoryItem> = (0..30).map(|i| item(i, &long_body.repeat(3))).collect();
    // Short tail cards guarantee a button near the bottom in a correct layout.
    for i in 24..30 {
        items[i as usize] = item(i, "Short tail.");
    }
    s.set_history_items(ModelRc::from(Rc::new(VecModel::from(items))));
    s.set_history_count(30);

    let view_h = s.get_history_viewport_height();
    let cpl = sotto_desktop::smooth_scroll::chars_per_line_for(1040, 1.0);
    let heights: Vec<f32> = (0..30)
        .map(|i| {
            let t = if i >= 24 {
                "Short tail.".to_string()
            } else {
                long_body.repeat(3)
            };
            sotto_desktop::smooth_scroll::estimate_card_height(&t, cpl)
        })
        .collect();
    let total = sotto_desktop::smooth_scroll::compute_content_height(
        &heights,
        sotto_desktop::smooth_scroll::ROW_SPACING,
    ) + sotto_desktop::smooth_scroll::LIST_BOTTOM_PADDING
        + sotto_desktop::smooth_scroll::LIST_OVERSCROLL_PX;
    s.set_history_content_height(total);
    s.set_history_viewport_y(-(total - view_h));

    let container = find_one(&s, "StudioWindow::history-container");
    let cpos = container.absolute_position();
    let csize = container.size();
    let bottom = cpos.y + csize.height;

    let mut best: Option<f32> = None;
    let mut it = ElementHandle::find_by_element_id(&s, "HistoryCard::copy-btn");
    while let Some(btn) = it.next() {
        let pos = btn.absolute_position();
        let size = btn.size();
        let edge = pos.y + size.height;
        best = Some(best.map_or(edge, |b: f32| b.max(edge)));
    }
    let best = best.expect("at least one card button must be visible at max scroll");
    // Standard: the design gap below the last card is 48px (16 pad + 32
    // slack). Bound at 100: catches 2x deviations with margin for raster
    // variance, across cards of any height.
    assert!(
        best >= bottom - 100.0,
        "void above bottom: lowest button ends {best:.0}, container bottom {bottom:.0}"
    );
    assert!(
        best <= bottom + 1.0,
        "content must not overflow the container"
    );
}

#[test]
fn test_no_excess_void_on_customization_tab_at_max_scroll() {
    let s = studio();
    s.window().set_size(slint::PhysicalSize::new(1040, 840));
    s.set_active_tab(2);

    let content_h = s.get_custom_content_height();
    assert!(content_h >= 700.0 && content_h <= 2200.0, "unexpected tab 2 content height: {content_h}");

    let container = find_one(&s, "StudioWindow::custom-container");
    let cpos = container.absolute_position();
    let csize = container.size();
    let container_bottom = cpos.y + csize.height;
    let visible_h = csize.height;

    // Simulate scrolling to bottom
    let max_scroll = (content_h - visible_h).max(0.0);
    s.set_custom_viewport_y(-max_scroll);

    // Query the lowest card on Tab 2 (Feedback & Media follows Startup).
    let bottom_card = find_one(&s, "StudioWindow::feedback_card");
    let bpos = bottom_card.absolute_position();
    let bsize = bottom_card.size();
    let bottom_edge = bpos.y + bsize.height;

    // The bottom card + bottom padding must land right near the container bottom edge.
    // Standard: no dead void exceeding 60px between lowest card bottom and container edge.
    let gap = container_bottom - bottom_edge;
    assert!(
        gap >= 0.0 && gap <= 60.0,
        "excess gap/void below customization content: gap is {gap:.1}px (bottom card edge: {bottom_edge:.1}, container bottom: {container_bottom:.1})"
    );
}

#[test]
fn test_virtualized_history_matches_full_list_geometry() {
    use sotto_desktop::smooth_scroll::{
        chars_per_line_for, compute_history_layout, compute_virtual_window, VIRTUAL_CUSHION_PX,
    };

    let make_items = || -> Vec<HistoryItem> {
        (0..80)
            .map(|i| {
                let text = if i % 5 == 0 {
                    format!("Item #{i}: longer multi-line dictation transcript to test variable heights cleanly across virtualization slices.")
                } else {
                    format!("Item #{i}: short dictation.")
                };
                item(i, &text)
            })
            .collect()
    };

    // Baseline: non-virtualized full list
    let full = studio();
    full.window().set_size(slint::PhysicalSize::new(1040, 840));
    let full_items = make_items();
    let full_cpl = chars_per_line_for(1040, full.window().scale_factor().max(1.0));
    let layout = compute_history_layout(
        full_items.iter().map(|it| it.transcript.as_str()),
        full_cpl,
    );
    let view_h = full.get_history_viewport_height();
    full.set_history_items(ModelRc::from(Rc::new(VecModel::from(full_items))));
    full.set_history_count(80);
    full.set_history_content_height(layout.total_content_h.max(view_h));

    // Virtualized: renders only slice + spacers
    let virt = studio();
    virt.window().set_size(slint::PhysicalSize::new(1040, 840));
    let virt_items = make_items();
    virt.set_history_virtualized(true);
    virt.set_history_count(80);
    virt.set_history_content_height(layout.total_content_h.max(view_h));

    let max_scroll = (layout.total_content_h - view_h).max(0.0);
    for scroll_y in [0.0f32, 1400.0, max_scroll] {
        full.set_history_viewport_y(-scroll_y);
        virt.set_history_viewport_y(-scroll_y);

        let win = compute_virtual_window(&layout, scroll_y, view_h, VIRTUAL_CUSHION_PX)
            .expect("virtual window must exist");
        let slice = virt_items[win.first_idx..=win.last_idx].to_vec();
        virt.set_top_spacer_height(win.top_spacer);
        virt.set_bottom_spacer_height(win.bottom_spacer);
        virt.set_visible_history_items(ModelRc::from(Rc::new(VecModel::from(slice))));

        let full_cards: Vec<(f32, f32)> = ElementHandle::find_by_element_type_name(&full, "HistoryCard")
            .map(|c| (c.absolute_position().y, c.size().height))
            .collect();
        let virt_cards: Vec<(f32, f32)> = ElementHandle::find_by_element_type_name(&virt, "HistoryCard")
            .map(|c| (c.absolute_position().y, c.size().height))
            .collect();

        // Virtualized DOM holds far fewer HistoryCard widgets than 80.
        assert!(
            virt_cards.len() <= 24,
            "at scroll={scroll_y}: virtualized mode must render <= 24 cards, rendered {}",
            virt_cards.len()
        );
        // Every visible card in full_cards inside the viewport must appear at the matching Y in virt_cards.
        let container = find_one(&full, "StudioWindow::history-container");
        let ctop = container.absolute_position().y;
        let cbot = ctop + container.size().height;
        let visible_full: Vec<_> = full_cards
            .iter()
            .filter(|(y, h)| *y + *h >= ctop + 1.0 && *y <= cbot - 1.0)
            .copied()
            .collect();
        assert!(!visible_full.is_empty(), "expected visible cards at scroll={scroll_y}");
        for (fy, fh) in visible_full {
            let found_match = virt_cards
                .iter()
                .any(|(vy, vh)| (vy - fy).abs() <= 2.0 && (vh - fh).abs() <= 1.0);
            assert!(
                found_match,
                "at scroll={scroll_y}: full card at y={fy:.1} h={fh:.1} missing or shifted in virtualized view ({virt_cards:?})"
            );
        }
    }
}

#[test]
fn test_providers_scrollbar_inside_container_when_scrollable() {
    let s = studio();
    s.window().set_size(slint::PhysicalSize::new(760, 600));
    s.set_active_tab(1);
    s.set_providers_content_height(1600.0);
    s.set_providers_can_scroll(true);
    s.set_providers_scroll_progress(0.5);

    let track = find_one(&s, "StudioWindow::providers-scrollbar-track");
    let thumb = find_one(&s, "StudioWindow::providers-scrollbar-thumb");
    let t_pos = track.absolute_position();
    let t_size = track.size();
    let th_pos = thumb.absolute_position();
    let th_size = thumb.size();

    assert!(th_size.height >= 28.0 - 1.0, "providers thumb respects minimum height");
    assert!(
        th_pos.y >= t_pos.y - 1.0 && th_pos.y + th_size.height <= t_pos.y + t_size.height + 1.0,
        "providers thumb must stay inside track"
    );
}

#[test]
fn test_virtualized_vs_full_mixed_transcripts_and_resizes() {
    use sotto_desktop::smooth_scroll::{
        chars_per_line_for, compute_history_layout, compute_virtual_window, ROW_SPACING,
        VIRTUAL_CUSHION_PX,
    };

    let samples = [
        "Ok.",
        "Send that email to Sarah before five.",
        "Can you check why the smooth scrolling in Sotto Studio stopped working after the virtualization update?",
        "https://github.com/criticalstrike18/sotto/actions/runs/1234567890/job/9876543210 — check the release artifact logs.",
        "Line one of a multi-line note.\nLine two with another sentence that might wrap onto another line depending on width.\nLine three short.",
        "WWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWW",
        "iiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiiii",
        "Implementing Per-Monitor V2 DPI awareness, DirectComposition focus shielding (WS_EX_NOACTIVATE), and encrypted SQLite WAL key pools in Rust + Slint.",
        "Mixed emoji 🚀🔥✓ with Japanese 日本語テスト and numbers 1234567890 in one sentence for wrapping accuracy.",
    ];
    let items_vec: Vec<HistoryItem> = (0..120)
        .map(|i| item(i, samples[i as usize % samples.len()]))
        .collect();

    for &(w, h) in &[(760u32, 600u32), (1040u32, 840u32), (1440u32, 900u32)] {
        let full = studio();
        full.window().set_size(slint::PhysicalSize::new(w, h));
        let scale = full.window().scale_factor().max(1.0);
        let cpl = chars_per_line_for(w, scale);
        let layout = compute_history_layout(
            items_vec.iter().map(|it| it.transcript.as_str()),
            cpl,
        );
        let view_h = full.get_history_viewport_height();
        full.set_history_items(ModelRc::from(Rc::new(VecModel::from(items_vec.clone()))));
        full.set_history_count(120);
        full.set_history_content_height(layout.total_content_h.max(view_h));

        let virt = studio();
        virt.window().set_size(slint::PhysicalSize::new(w, h));
        virt.set_history_virtualized(true);
        virt.set_history_count(120);
        virt.set_history_content_height(layout.total_content_h.max(view_h));

        let max_scroll = (layout.total_content_h - view_h).max(0.0);
        let container = find_one(&full, "StudioWindow::history-container");
        let ctop = container.absolute_position().y;
        let cbot = ctop + container.size().height;

        for scroll_y in [0.0f32, max_scroll * 0.25, max_scroll * 0.5, max_scroll] {
            full.set_history_viewport_y(-scroll_y);
            virt.set_history_viewport_y(-scroll_y);

            let win = compute_virtual_window(&layout, scroll_y, view_h, VIRTUAL_CUSHION_PX)
                .expect("virtual window must exist");
            let slice = items_vec[win.first_idx..=win.last_idx].to_vec();
            assert!(
                slice.len() <= 28,
                "at {w}x{h} scroll={scroll_y}: virtual slice should stay bounded, got {}",
                slice.len()
            );
            virt.set_top_spacer_height(win.top_spacer);
            virt.set_bottom_spacer_height(win.bottom_spacer);
            virt.set_visible_history_items(ModelRc::from(Rc::new(VecModel::from(slice))));

            let virt_cards: Vec<(f32, f32)> = ElementHandle::find_by_element_type_name(&virt, "HistoryCard")
                .map(|c| (c.absolute_position().y, c.size().height))
                .collect();
            assert!(
                !virt_cards.is_empty(),
                "at {w}x{h} scroll={scroll_y}: virtual slice must render visible cards"
            );

            // Invariant 1: Zero blank gaps in viewport (allowing up to one ROW_SPACING gap at container clip boundaries).
            let first = virt_cards.first().unwrap();
            assert!(
                first.0 <= ctop + ROW_SPACING + 1.0,
                "at {w}x{h} scroll={scroll_y}: top blank gap above first card ({:.1} > ctop+spacing {:.1})",
                first.0,
                ctop + ROW_SPACING + 1.0
            );
            let last = virt_cards.last().unwrap();
            let last_bot = last.0 + last.1;
            if win.last_idx + 1 == items_vec.len() {
                assert!(
                    last_bot <= cbot + 1.0 && last_bot >= cbot - 140.0,
                    "at {w}x{h} scroll={scroll_y}: tail card edge {last_bot:.1} outside [{:.1}, {:.1}]",
                    cbot - 140.0,
                    cbot + 1.0
                );
            } else {
                assert!(
                    last_bot >= cbot - ROW_SPACING - 1.0,
                    "at {w}x{h} scroll={scroll_y}: bottom blank gap below last card ({last_bot:.1} < cbot-spacing {:.1})",
                    cbot - ROW_SPACING - 1.0
                );
            }
            for pair in virt_cards.windows(2) {
                let gap = pair[1].0 - (pair[0].0 + pair[0].1);
                assert!(
                    (gap - ROW_SPACING).abs() <= 1.0,
                    "at {w}x{h} scroll={scroll_y}: card gap {gap:.1} != {ROW_SPACING}"
                );
            }

            // Invariant 2: At desktop & wide sizes (1040, 1440), full & virtualized line up closely.
            if w >= 1040 {
                let full_vis: Vec<(f32, f32)> = ElementHandle::find_by_element_type_name(&full, "HistoryCard")
                    .map(|c| (c.absolute_position().y, c.size().height))
                    .filter(|(y, ch)| *y + *ch >= ctop + 1.0 && *y <= cbot - 1.0)
                    .collect();
                for (fy, fh) in &full_vis {
                    let matched = virt_cards
                        .iter()
                        .any(|(vy, vh)| (vy - fy).abs() <= 30.0 && (vh - fh).abs() <= 1.0);
                    assert!(
                        matched,
                        "at {w}x{h} scroll={scroll_y}: full card y={fy:.1} h={fh:.1} missing in virt ({virt_cards:?})"
                    );
                }
            }
        }
    }
}

#[test]
fn test_tabs_viewport_heights_and_dynamic_providers_reflow() {
    use sotto_desktop::{ApiKeyItem, LanguageItem};

    let s = studio();
    for &(w, h) in &[(760u32, 600u32), (1040u32, 840u32)] {
        s.window().set_size(slint::PhysicalSize::new(w, h));
        s.set_active_tab(1);
        let prov_c = find_one(&s, "StudioWindow::providers_container");
        approx(
            prov_c.size().height,
            s.get_providers_viewport_height(),
            &format!("providers_container matches providers-viewport-height at {w}x{h}"),
        );

        s.set_active_tab(2);
        let cust_c = find_one(&s, "StudioWindow::custom-container");
        approx(
            cust_c.size().height,
            s.get_custom_viewport_height(),
            &format!("custom-container matches custom-viewport-height at {w}x{h}"),
        );
    }

    // Dynamic Tab 1 height changes (Smart Polish + Language dropdown + Key Pool) and scrollbar bounds
    s.window().set_size(slint::PhysicalSize::new(1040, 840));
    s.set_active_tab(1);
    s.set_other_languages_col1(ModelRc::from(Rc::new(VecModel::from(vec![
        LanguageItem { code: "es".into(), name: "Spanish (es)".into() },
        LanguageItem { code: "fr".into(), name: "French (fr)".into() },
        LanguageItem { code: "de".into(), name: "German (de)".into() },
        LanguageItem { code: "ja".into(), name: "Japanese (ja)".into() },
        LanguageItem { code: "hi".into(), name: "Hindi (hi)".into() },
    ]))));
    s.set_other_languages_col2(ModelRc::from(Rc::new(VecModel::from(vec![
        LanguageItem { code: "pt".into(), name: "Portuguese (pt)".into() },
        LanguageItem { code: "it".into(), name: "Italian (it)".into() },
        LanguageItem { code: "ko".into(), name: "Korean (ko)".into() },
        LanguageItem { code: "zh".into(), name: "Chinese (zh)".into() },
        LanguageItem { code: "ar".into(), name: "Arabic (ar)".into() },
    ]))));
    s.set_groq_key_pool(ModelRc::from(Rc::new(VecModel::from(vec![
        ApiKeyItem { id: 0, key: "gsk_1".into(), masked: "gsk_...1".into(), status: "Active".into(), is_rate_limited: false },
        ApiKeyItem { id: 1, key: "gsk_2".into(), masked: "gsk_...2".into(), status: "Ready".into(), is_rate_limited: false },
        ApiKeyItem { id: 2, key: "gsk_3".into(), masked: "gsk_...3".into(), status: "Ready".into(), is_rate_limited: false },
    ]))));
    s.set_post_process_llm_enabled(false);
    s.set_language_dropdown_open(false);
    let _ = find_one(&s, "StudioWindow::providers_container");
    let base_h = s.get_providers_content_height();

    s.set_post_process_llm_enabled(true);
    let _ = find_one(&s, "StudioWindow::providers_container");
    let with_polish_h = s.get_providers_content_height();
    assert!(
        with_polish_h > base_h + 20.0,
        "enabling Smart Polish must grow providers-content-height ({base_h:.1} -> {with_polish_h:.1})"
    );

    s.set_model_supports_multilingual(true);
    s.set_language_dropdown_open(true);
    let _ = find_one(&s, "StudioWindow::providers_container");
    let with_lang_h = s.get_providers_content_height();
    assert!(
        with_lang_h > with_polish_h + 40.0,
        "opening Language dropdown must grow providers-content-height ({with_polish_h:.1} -> {with_lang_h:.1})"
    );
    assert!(
        with_lang_h > s.get_providers_viewport_height(),
        "expanded Providers content ({with_lang_h:.1}) must exceed viewport so scrolling activates"
    );

    s.set_providers_can_scroll(true);
    for p in [0.0f32, 0.5, 1.0] {
        s.set_providers_scroll_progress(p);
        let track = find_one(&s, "StudioWindow::providers-scrollbar-track");
        let thumb = find_one(&s, "StudioWindow::providers-scrollbar-thumb");
        let t_pos = track.absolute_position();
        let t_size = track.size();
        let th_pos = thumb.absolute_position();
        let th_size = thumb.size();
        assert!(
            th_pos.y >= t_pos.y - 1.0 && th_pos.y + th_size.height <= t_pos.y + t_size.height + 1.0,
            "at progress={p}: providers thumb must stay inside track"
        );
    }
}

#[test]
fn test_reopen_scroll_reset_and_unshown_viewport_recovery() {
    use sotto_desktop::smooth_scroll::{
        reset_all_studio_scrollers, CUSTOM_SCROLLER, PROVIDERS_SCROLLER, STUDIO_SCROLLER,
    };

    // Testing backend starts at 800x600 (while winit pre-show starts at 0x0 and
    // clamps to MIN_VISIBLE_PX 100px); resizing to 1040x840 must recover exact
    // 684px Dictations viewport & history-container height.
    let unshown = studio();
    approx(
        unshown.get_history_viewport_height(),
        unshown.window().size().height as f32 - 156.0,
        "initial viewport formula matches chrome reservation",
    );
    unshown.window().set_size(slint::PhysicalSize::new(1040, 840));
    approx(unshown.get_history_viewport_height(), 684.0, "post-size Dictations viewport");
    approx(
        find_one(&unshown, "StudioWindow::history-container").size().height,
        684.0,
        "post-size history-container height",
    );

    // Scrollers moved away from 0.0 must reset completely on teardown/reopen.
    STUDIO_SCROLLER.with(|sc| {
        let mut eng = sc.borrow_mut();
        eng.set_dimensions(4000.0, 684.0);
        eng.jump_to(-1850.0);
        assert!((eng.current_y() - -1850.0).abs() < 0.01);
    });
    PROVIDERS_SCROLLER.with(|sc| {
        let mut eng = sc.borrow_mut();
        eng.set_dimensions(1800.0, 740.0);
        eng.jump_to(-600.0);
    });
    CUSTOM_SCROLLER.with(|sc| {
        let mut eng = sc.borrow_mut();
        eng.set_dimensions(1800.0, 740.0);
        eng.jump_to(-500.0);
    });

    reset_all_studio_scrollers();

    STUDIO_SCROLLER.with(|sc| assert_eq!(sc.borrow().current_y(), 0.0));
    PROVIDERS_SCROLLER.with(|sc| assert_eq!(sc.borrow().current_y(), 0.0));
    CUSTOM_SCROLLER.with(|sc| assert_eq!(sc.borrow().current_y(), 0.0));
}

#[test]
fn test_empty_history_hides_purge_and_long_cards_stay_within_width() {
    let s = studio();
    s.window().set_size(slint::PhysicalSize::new(760, 600));
    s.set_active_tab(0);
    s.set_history_count(0);

    // Empty history must not show/size the purge button.
    let purge_visible = ElementHandle::find_by_element_id(&s, "StudioWindow::purge-btn")
        .next()
        .is_some_and(|h| h.size().width > 0.0 && h.size().height > 0.0);
    assert!(!purge_visible, "empty history must hide purge-btn");

    // Long multi-sentence transcript at minimum window width (760px) must wrap inside container width.
    let long_body = "Long transcript wrapping check across narrow desktop viewports to guarantee zero horizontal clipping or overflowing card geometry inside the Studio history container even when dictation exceeds several sentences cleanly. ".repeat(2);
    s.set_history_items(ModelRc::from(Rc::new(VecModel::from(vec![item(1, &long_body)]))));
    s.set_history_count(1);

    let container = find_one(&s, "StudioWindow::history-container");
    let cx = container.absolute_position().x;
    let cw = container.size().width;
    let card = ElementHandle::find_by_element_type_name(&s, "HistoryCard")
        .next()
        .expect("HistoryCard must render for non-empty history");
    let cpos = card.absolute_position();
    let csize = card.size();
    assert!(csize.height > 100.0, "long wrapped card must grow vertically (got {:.1})", csize.height);
    assert!(cpos.x >= cx && cpos.x + csize.width <= cx + cw, "card clipped outside container bounds");
}

#[test]
fn test_customization_cards_stack_and_align_across_viewport_sizes() {
    for (w, h) in [(760, 600), (1040, 840)] {
        let s = studio();
        s.window().set_size(slint::PhysicalSize::new(w, h));
        s.set_active_tab(2);

        let container = find_one(&s, "StudioWindow::custom_container");
        let cx = container.absolute_position().x;
        let cw = container.size().width;
        let ch = container.size().height.max(200.0);
        let max_scroll = (s.get_custom_content_height() - ch).max(0.0);

        let ids = [
            "StudioWindow::stage-box",
            "StudioWindow::custom_card",
            "StudioWindow::trigger_card",
            "StudioWindow::cancel_card",
            "StudioWindow::startup_card",
            "StudioWindow::feedback_card",
        ];
        let mut prev_bottom = -1.0f32;
        let mut ref_w: Option<f32> = None;

        for id in ids {
            // Bring card inside custom_flick's visible clip so ElementHandle traverses it,
            // then compare positions in content space (screen_y - custom_viewport_y).
            let mut found = None;
            let mut offset = 0.0f32;
            loop {
                s.set_custom_viewport_y(-offset);
                if let Some(el) = ElementHandle::find_by_element_id(&s, id).next() {
                    found = Some(el);
                    break;
                }
                if offset >= max_scroll {
                    break;
                }
                offset = (offset + ch * 0.45).min(max_scroll);
            }
            let el = found.unwrap_or_else(|| panic!("{w}x{h}: element must be findable while scrolling: {id}"));
            let pos = el.absolute_position();
            let size = el.size();
            let content_y = pos.y - s.get_custom_viewport_y();
            assert!(size.width > 400.0 && size.height > 40.0, "{w}x{h} {id}: unexpectedly small {size:?}");
            assert!(pos.x >= cx && pos.x + size.width <= cx + cw + 1.0, "{w}x{h} {id}: overflows custom_container");
            assert!(content_y >= prev_bottom - 0.5, "{w}x{h} {id}: overlaps previous card ({:.1} < {:.1})", content_y, prev_bottom);
            prev_bottom = content_y + size.height;
            match ref_w {
                None => ref_w = Some(size.width),
                Some(rw) => assert!((size.width - rw).abs() <= 1.5, "{w}x{h} {id}: card width {:.1} misaligned from {:.1}", size.width, rw),
            }
        }
    }
}
