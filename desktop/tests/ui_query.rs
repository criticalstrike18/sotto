// Overall-UI geometry suite: ElementHandle queries against the live
// StudioWindow (production composition, not isolated components).
// No event loop needed — geometry queries run headless. Requires the
// SLINT_EMIT_DEBUG_INFO build flag (set in build.rs).
//
// Backend: headless Slint testing backend (no windows, CI-safe).

use i_slint_backend_testing::ElementHandle;
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

    let mut imgs = ElementHandle::find_by_element_type_name(&s, "Image");
    let marker_img = imgs
        .next()
        .expect("tail marker image must be reachable at max scroll");
    assert!(
        imgs.next().is_none(),
        "only the tail card carries an image"
    );
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
                (h - 77.0).abs() <= 1.0,
                "at {width}px: single-line card must measure 77px, got {h:.0}"
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
        // Viewport formula must match CHROME_RESERVED_PX (132): a stale 156
        // parks max scroll ~26px past content = permanent end void.
        approx(
            s.get_history_viewport_height(),
            h as f32 - sotto_desktop::smooth_scroll::CHROME_RESERVED_PX,
            &format!("viewport-height formula at {w}x{h}"),
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
