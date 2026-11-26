// Interaction suite: REAL TouchArea clicks through the event loop
// (ElementHandle::single_click), proving Slint-side wiring end to end across
// isolated components, live StudioWindow tabs (Dictations, Providers,
// Customization), and FloatingPillWindow (left + right clicks).
//
// Slint's event-loop backend supports exactly ONE #[test] per process, so all
// click phases run sequentially inside a single event-loop run. Headless, CI-safe.
//
// Backend: i_slint_backend_testing with system time (event loop runs).

use i_slint_backend_testing::ElementHandle;
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use sotto_desktop::components::{HistoryCard, KeyField, ModelRow, ProviderCard, TabBar};
use sotto_desktop::{ApiKeyItem, FloatingPillWindow, HistoryItem, LanguageItem, StudioWindow};
use std::cell::RefCell;
use std::rc::Rc;

async fn click(path_owner: &str, found: Option<ElementHandle>) {
    let el = found.unwrap_or_else(|| panic!("click target must exist: {path_owner}"));
    el.single_click(PointerEventButton::Left).await;
}

async fn click_in_flickable(
    studio: &StudioWindow,
    container_id: &str,
    content_h: f32,
    set_viewport_y: fn(&StudioWindow, f32),
    id: &str,
) {
    let (ctop, ch) = ElementHandle::find_by_element_id(studio, container_id)
        .next()
        .map(|el| (el.absolute_position().y, el.size().height))
        .unwrap_or((156.0, 684.0));
    let ch = ch.max(200.0);
    let cbot = ctop + ch;
    let max_scroll = (content_h - ch).max(0.0);
    let mut offset = 0.0f32;
    let found = loop {
        set_viewport_y(studio, -offset);
        if let Some(el) = ElementHandle::find_by_element_id(studio, id).next() {
            let cy = el.absolute_position().y + el.size().height * 0.5;
            if cy >= ctop + 2.0 && cy <= cbot - 2.0 {
                break Some(el);
            }
        }
        if offset >= max_scroll {
            break ElementHandle::find_by_element_id(studio, id).next();
        }
        offset = (offset + ch * 0.30).min(max_scroll);
    };
    click(id, found).await;
}

async fn click_customization(studio: &StudioWindow, id: &str) {
    click_in_flickable(
        studio,
        "StudioWindow::custom-container",
        studio.get_custom_content_height(),
        |s, y| s.set_custom_viewport_y(y),
        id,
    )
    .await;
}

async fn click_providers(studio: &StudioWindow, id: &str) {
    click_in_flickable(
        studio,
        "StudioWindow::providers_container",
        studio.get_providers_content_height(),
        |s, y| s.set_providers_viewport_y(y),
        id,
    )
    .await;
}

async fn right_click(path_owner: &str, found: Option<ElementHandle>) {
    let el = found.unwrap_or_else(|| panic!("right-click target must exist: {path_owner}"));
    el.single_click(PointerEventButton::Right).await;
}

#[test]
fn test_all_click_paths() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let tabs: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
    let row_fired: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
    let copied: Rc<RefCell<(i32, String)>> = Rc::new(RefCell::new((0, String::new())));
    let revealed: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
    let card_fired: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
    let purge_count: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
    let purge_states: Rc<RefCell<Vec<bool>>> = Rc::new(RefCell::new(Vec::new()));

    let studio_tabs: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
    let studio_copied: Rc<RefCell<Option<(i32, String)>>> = Rc::new(RefCell::new(None));
    let prov_selected: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let lang_selected: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let key_added: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let key_removed: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
    let model_picked: Rc<RefCell<Option<(String, i32)>>> = Rc::new(RefCell::new(None));
    let prov_saved: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
    let prov_tested: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
    let polish_toggled: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));

    let trigger_modes: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
    let custom_toggles: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
    let rec_slots: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
    let reset_slots: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
    let stage_cycles: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
    let custom_saved: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));

    let pill_left_clicks: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
    let pill_right_clicks: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));

    let tabs_c = Rc::clone(&tabs);
    let row_c = Rc::clone(&row_fired);
    let copied_c = Rc::clone(&copied);
    let revealed_c = Rc::clone(&revealed);
    let card_c = Rc::clone(&card_fired);
    let purge_count_c = Rc::clone(&purge_count);
    let purge_states_c = Rc::clone(&purge_states);

    let studio_tabs_c = Rc::clone(&studio_tabs);
    let studio_copied_c = Rc::clone(&studio_copied);
    let prov_selected_c = Rc::clone(&prov_selected);
    let lang_selected_c = Rc::clone(&lang_selected);
    let key_added_c = Rc::clone(&key_added);
    let key_removed_c = Rc::clone(&key_removed);
    let model_picked_c = Rc::clone(&model_picked);
    let prov_saved_c = Rc::clone(&prov_saved);
    let prov_tested_c = Rc::clone(&prov_tested);
    let polish_toggled_c = Rc::clone(&polish_toggled);

    let trigger_modes_c = Rc::clone(&trigger_modes);
    let custom_toggles_c = Rc::clone(&custom_toggles);
    let rec_slots_c = Rc::clone(&rec_slots);
    let reset_slots_c = Rc::clone(&reset_slots);
    let stage_cycles_c = Rc::clone(&stage_cycles);
    let custom_saved_c = Rc::clone(&custom_saved);

    let pill_left_c = Rc::clone(&pill_left_clicks);
    let pill_right_c = Rc::clone(&pill_right_clicks);

    slint::spawn_local(async move {
        // Phase 1: isolated TabBar routes clicks to exact tab indices.
        let bar = TabBar::new().expect("TabBar must instantiate");
        let tc = Rc::clone(&tabs_c);
        bar.on_select_tab(move |i| tc.borrow_mut().push(i));
        let touches: Vec<_> =
            ElementHandle::find_by_element_id(&bar, "TabBar::tab-touch").collect();
        tabs_c.borrow_mut().push(touches.len() as i32);
        if touches.len() == 3 {
            touches[1].single_click(PointerEventButton::Left).await;
            touches[2].single_click(PointerEventButton::Left).await;
        }

        // Phase 2: ModelRow click.
        let row = ModelRow::new().expect("ModelRow must instantiate");
        row.set_name("whisper-large-v3-turbo".into());
        let rc = Rc::clone(&row_c);
        row.on_clicked(move || *rc.borrow_mut() = true);
        click("ModelRow::row-touch", ElementHandle::find_by_element_id(&row, "ModelRow::row-touch").next()).await;

        // Phase 3: HistoryCard copy click delivers (id, transcript).
        let card = HistoryCard::new().expect("HistoryCard must instantiate");
        card.set_rec_id(42);
        card.set_rec_transcript("Copy me please.".into());
        let cc = Rc::clone(&copied_c);
        card.on_copy_item(move |id, text| *cc.borrow_mut() = (id, text.to_string()));
        click("HistoryCard::copy-touch", ElementHandle::find_by_element_id(&card, "HistoryCard::copy-touch").next()).await;

        // Phase 4: KeyField reveal click.
        let key = KeyField::new().expect("KeyField must instantiate");
        let kc = Rc::clone(&revealed_c);
        key.on_toggle_visible(move || *kc.borrow_mut() = true);
        click("KeyField::eye-touch", ElementHandle::find_by_element_id(&key, "KeyField::eye-touch").next()).await;

        // Phase 5: ProviderCard click.
        let prov = ProviderCard::new().expect("ProviderCard must instantiate");
        let pc = Rc::clone(&card_c);
        prov.on_clicked(move || *pc.borrow_mut() = true);
        click("ProviderCard::card-touch", ElementHandle::find_by_element_id(&prov, "ProviderCard::card-touch").next()).await;

        // Phase 6: StudioWindow Dictations purge two-step confirmation flow.
        let studio = StudioWindow::new().expect("StudioWindow must instantiate");
        studio.window().set_size(slint::PhysicalSize::new(1040, 840));
        studio.set_history_count(5);
        let pcc = Rc::clone(&purge_count_c);
        studio.on_clear_all_history(move || *pcc.borrow_mut() += 1);
        click("StudioWindow::clear-touch (arm)", ElementHandle::find_by_element_id(&studio, "StudioWindow::clear-touch").next()).await;
        purge_states_c.borrow_mut().push(studio.get_purge_confirm_armed());
        click("StudioWindow::cancel-purge-touch", ElementHandle::find_by_element_id(&studio, "StudioWindow::cancel-purge-touch").next()).await;
        purge_states_c.borrow_mut().push(studio.get_purge_confirm_armed());
        click("StudioWindow::clear-touch (re-arm)", ElementHandle::find_by_element_id(&studio, "StudioWindow::clear-touch").next()).await;
        purge_states_c.borrow_mut().push(studio.get_purge_confirm_armed());
        click("StudioWindow::clear-touch (confirm)", ElementHandle::find_by_element_id(&studio, "StudioWindow::clear-touch").next()).await;
        purge_states_c.borrow_mut().push(studio.get_purge_confirm_armed());

        // Phase 6B: Live StudioWindow virtualized HistoryCard Copy click routes (id, transcript).
        studio.set_history_virtualized(true);
        studio.set_visible_history_items(ModelRc::from(Rc::new(VecModel::from(vec![
            HistoryItem {
                id: 101,
                timestamp_str: "10:00 AM".into(),
                transcript: "First virtualized item".into(),
                duration_ms: 1200,
                duration_str: "1s".into(),
                target_app: "Desktop".into(),
                app_icon: slint::Image::default(),
                has_icon: false,
            },
            HistoryItem {
                id: 102,
                timestamp_str: "10:05 AM".into(),
                transcript: "Second virtualized item".into(),
                duration_ms: 2100,
                duration_str: "2s".into(),
                target_app: "VS Code".into(),
                app_icon: slint::Image::default(),
                has_icon: false,
            },
        ]))));
        let sc_c = Rc::clone(&studio_copied_c);
        studio.on_copy_history_item(move |id, text| *sc_c.borrow_mut() = Some((id, text.to_string())));
        let live_copy_touches: Vec<_> =
            ElementHandle::find_by_element_id(&studio, "HistoryCard::copy-touch").collect();
        if live_copy_touches.len() >= 2 {
            live_copy_touches[1].single_click(PointerEventButton::Left).await;
        }

        // Phase 7: StudioWindow live TabBar switches tabs cleanly.
        let st_c = Rc::clone(&studio_tabs_c);
        let studio_for_tab = studio.as_weak();
        studio.on_select_tab(move |idx| {
            st_c.borrow_mut().push(idx);
            if let Some(s) = studio_for_tab.upgrade() {
                s.set_active_tab(idx);
            }
        });
        let s_tab_touches: Vec<_> =
            ElementHandle::find_by_element_id(&studio, "TabBar::tab-touch").collect();
        if s_tab_touches.len() == 3 {
            s_tab_touches[1].single_click(PointerEventButton::Left).await;
            s_tab_touches[2].single_click(PointerEventButton::Left).await;
            s_tab_touches[0].single_click(PointerEventButton::Left).await;
        }

        // Phase 8: StudioWindow Providers tab interactions (select provider, key pool add/del, model pick, save, test).
        studio.set_active_tab(1);
        studio.set_provider_id("gemini".into());
        studio.set_gemini_new_key("AIza_pool_added_99".into());
        studio.set_gemini_key_pool(ModelRc::from(Rc::new(VecModel::from(vec![ApiKeyItem {
            id: 0,
            key: "AIza_existing_pool_1".into(),
            masked: "AIza...ol_1".into(),
            status: "Ready".into(),
            is_rate_limited: false,
        }]))));
        studio.set_gemini_models(ModelRc::from(Rc::new(VecModel::from(vec![
            SharedString::from("gemini-3.5-transcribe"),
            SharedString::from("gemini-2.5-flash"),
        ]))));
        let ps_c = Rc::clone(&prov_selected_c);
        studio.on_select_provider(move |p| ps_c.borrow_mut().push(p.to_string()));
        let ka_c = Rc::clone(&key_added_c);
        studio.on_add_provider_key(move |p, k| ka_c.borrow_mut().push((p.to_string(), k.to_string())));
        let kr_c = Rc::clone(&key_removed_c);
        studio.on_remove_provider_key(move |p, k| kr_c.borrow_mut().push((p.to_string(), k.to_string())));
        let mp_c = Rc::clone(&model_picked_c);
        studio.on_model_changed(move |p, idx| *mp_c.borrow_mut() = Some((p.to_string(), idx)));
        let psv_c = Rc::clone(&prov_saved_c);
        studio.on_save_provider(move || *psv_c.borrow_mut() = true);
        let pts_c = Rc::clone(&prov_tested_c);
        studio.on_test_provider(move || *pts_c.borrow_mut() = true);
        let polish_c = Rc::clone(&polish_toggled_c);
        studio.on_toggle_post_process_llm(move || *polish_c.borrow_mut() += 1);

        let cards: Vec<_> =
            ElementHandle::find_by_element_id(&studio, "ProviderCard::card-touch").collect();
        if !cards.is_empty() {
            cards[0].single_click(PointerEventButton::Left).await;
        }
        click("KeyPoolManager::add-touch", ElementHandle::find_by_element_id(&studio, "KeyPoolManager::add-touch").next()).await;
        click("KeyPoolManager::del-touch", ElementHandle::find_by_element_id(&studio, "KeyPoolManager::del-touch").next()).await;
        let models: Vec<_> =
            ElementHandle::find_by_element_id(&studio, "ModelRow::row-touch").collect();
        if models.len() >= 2 {
            models[1].single_click(PointerEventButton::Left).await;
        }
        click_providers(&studio, "StudioWindow::save-prov-touch").await;
        click_providers(&studio, "StudioWindow::test-prov-touch").await;
        click_providers(&studio, "StudioWindow::smart_polish_touch").await;

        // Phase 8B: Live 2-column Other Languages dropdown selection clicks.
        studio.set_language_dropdown_open(true);
        studio.set_other_languages_col1(ModelRc::from(Rc::new(VecModel::from(vec![LanguageItem {
            code: "es".into(),
            name: "Spanish".into(),
        }]))));
        studio.set_other_languages_col2(ModelRc::from(Rc::new(VecModel::from(vec![LanguageItem {
            code: "ja".into(),
            name: "Japanese".into(),
        }]))));
        let ls_c = Rc::clone(&lang_selected_c);
        studio.on_select_language(move |code| ls_c.borrow_mut().push(code.to_string()));
        click_providers(&studio, "StudioWindow::c1-touch").await;
        click_providers(&studio, "StudioWindow::c2-touch").await;

        // Phase 9: StudioWindow Customization tab interactions (triggers, toggles, hotkey slot actions, save, stage preview pill).
        studio.set_active_tab(2);
        let st_cyc_c = Rc::clone(&stage_cycles_c);
        studio.on_cycle_state(move || *st_cyc_c.borrow_mut() += 1);
        click_customization(&studio, "VoicePill::touch").await;
        let tm_c = Rc::clone(&trigger_modes_c);
        studio.on_set_trigger_mode(move |m| tm_c.borrow_mut().push(m));
        let ct_c1 = Rc::clone(&custom_toggles_c);
        studio.on_toggle_launch_on_startup(move || ct_c1.borrow_mut().push("startup"));
        let ct_c2 = Rc::clone(&custom_toggles_c);
        studio.on_toggle_chimes(move || ct_c2.borrow_mut().push("chimes"));
        let ct_c3 = Rc::clone(&custom_toggles_c);
        studio.on_toggle_media_pause(move || ct_c3.borrow_mut().push("media"));
        let rec_c = Rc::clone(&rec_slots_c);
        studio.on_start_recording_key(move |slot| rec_c.borrow_mut().push(slot));
        let rst_c = Rc::clone(&reset_slots_c);
        studio.on_reset_trigger(move |slot| rst_c.borrow_mut().push(slot));
        let csv_c = Rc::clone(&custom_saved_c);
        studio.on_save_customization(move || *csv_c.borrow_mut() = true);

        click_customization(&studio, "StudioWindow::ptt_touch").await;
        click_customization(&studio, "StudioWindow::toggle_touch").await;
        click_customization(&studio, "StudioWindow::startup_touch").await;
        click_customization(&studio, "StudioWindow::chimes_touch").await;
        click_customization(&studio, "StudioWindow::media_pause_touch").await;
        click_customization(&studio, "StudioWindow::rec_p_touch").await;
        click_customization(&studio, "StudioWindow::reset_p_touch").await;
        click_customization(&studio, "StudioWindow::rec_s_touch").await;
        click_customization(&studio, "StudioWindow::clear_s_touch").await;
        click_customization(&studio, "StudioWindow::rec_cp_touch").await;
        click_customization(&studio, "StudioWindow::reset_cp_touch").await;
        click_customization(&studio, "StudioWindow::rec_cs_touch").await;
        click_customization(&studio, "StudioWindow::clear_cs_touch").await;
        click_customization(&studio, "StudioWindow::save_touch").await;

        // Phase 10: FloatingPillWindow real pointer left & right click wiring.
        let pill = FloatingPillWindow::new().expect("FloatingPillWindow must instantiate");
        let pl_c = Rc::clone(&pill_left_c);
        pill.on_cycle_state(move || *pl_c.borrow_mut() += 1);
        let pr_c = Rc::clone(&pill_right_c);
        pill.on_toggle_suite(move || *pr_c.borrow_mut() += 1);
        click("VoicePill::touch (left)", ElementHandle::find_by_element_id(&pill, "VoicePill::touch").next()).await;
        right_click("VoicePill::touch (right)", ElementHandle::find_by_element_id(&pill, "VoicePill::touch").next()).await;

        slint::quit_event_loop().unwrap();
    })
    .unwrap();
    slint::run_event_loop().unwrap();

    assert_eq!(*tabs.borrow(), vec![3, 1, 2], "isolated TabBar clicks route 1 then 2");
    assert!(*row_fired.borrow(), "row click must fire clicked()");
    let (id, text) = copied.borrow().clone();
    assert_eq!(id, 42, "copy click must deliver id");
    assert_eq!(text, "Copy me please.", "copy click must deliver transcript");
    assert!(*revealed.borrow(), "eye click must fire toggle-visible()");
    assert!(*card_fired.borrow(), "card click must fire clicked()");
    assert_eq!(*purge_states.borrow(), vec![true, false, true, false], "two-stage purge arm/cancel/confirm states");
    assert_eq!(*purge_count.borrow(), 1, "clear_all_history must fire only after second-stage confirm click");

    assert_eq!(*studio_tabs.borrow(), vec![1, 2, 0], "StudioWindow live TabBar clicks must switch tabs 1->2->0");
    assert_eq!(
        *studio_copied.borrow(),
        Some((102, "Second virtualized item".to_string())),
        "live virtualized HistoryCard copy click must forward exact (id, transcript)"
    );
    assert_eq!(*prov_selected.borrow(), vec!["groq".to_string()], "clicking first ProviderCard selects groq");
    assert_eq!(*lang_selected.borrow(), vec!["es".to_string(), "ja".to_string()], "2-column dropdown clicks must fire select_language");
    assert_eq!(*key_added.borrow(), vec![("gemini".to_string(), "AIza_pool_added_99".to_string())]);
    assert_eq!(*key_removed.borrow(), vec![("gemini".to_string(), "AIza_existing_pool_1".to_string())]);
    assert_eq!(*model_picked.borrow(), Some(("gemini".to_string(), 1)));
    assert!(*prov_saved.borrow(), "Save Providers button must invoke save_provider()");
    assert!(*prov_tested.borrow(), "Test Connection button must invoke test_provider()");
    assert_eq!(*polish_toggled.borrow(), 1, "Smart Polish SottoSwitch must invoke toggle_post_process_llm()");

    assert_eq!(*stage_cycles.borrow(), 1, "Studio stage preview VoicePill click must fire cycle_state");
    assert_eq!(*trigger_modes.borrow(), vec![1, 0], "PTT then Toggle card clicks must fire set_trigger_mode(1), set_trigger_mode(0)");
    assert_eq!(*custom_toggles.borrow(), vec!["startup", "chimes", "media"]);
    assert_eq!(*rec_slots.borrow(), vec![0, 1, 2, 3], "record buttons for slots 0..=3 must route exact slot indices");
    assert_eq!(*reset_slots.borrow(), vec![0, 1, 2, 3], "reset/clear buttons for slots 0..=3 must route exact slot indices");
    assert!(*custom_saved.borrow(), "Save Preset button must invoke save_customization()");

    assert_eq!(*pill_left_clicks.borrow(), 1, "FloatingPill left click must fire cycle_state");
    assert_eq!(*pill_right_clicks.borrow(), 1, "FloatingPill right click must fire toggle_suite");
}
