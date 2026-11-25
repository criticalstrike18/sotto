// Interaction suite: REAL TouchArea clicks through the event loop
// (ElementHandle::single_click), proving Slint-side wiring end to end.
//
// Slint's event-loop backend supports exactly ONE #[test] per process
// ("integration tests with only one #[test] function"), so all click phases
// run sequentially inside a single event-loop run, each with a labeled
// assertion. Headless, CI-safe.
//
// Backend: i_slint_backend_testing with system time (event loop runs).

use i_slint_backend_testing::ElementHandle;
use slint::platform::PointerEventButton;
use slint::ComponentHandle;
use sotto_desktop::components::{HistoryCard, KeyField, ModelRow, ProviderCard, TabBar};
use sotto_desktop::StudioWindow;
use std::cell::RefCell;
use std::rc::Rc;

async fn click(path_owner: &str, found: Option<ElementHandle>) {
    let el = found.unwrap_or_else(|| panic!("click target must exist: {path_owner}"));
    el.single_click(PointerEventButton::Left).await;
}

#[test]
fn test_all_click_paths() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    // Recorded outcomes, asserted after the loop returns (a failure inside
    // the async block must still reach quit_event_loop).
    let tabs: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
    let row_fired: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
    let copied: Rc<RefCell<(i32, String)>> = Rc::new(RefCell::new((0, String::new())));
    let revealed: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
    let card_fired: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
    let purge_count: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
    let purge_states: Rc<RefCell<Vec<bool>>> = Rc::new(RefCell::new(Vec::new()));

    let tabs_c = Rc::clone(&tabs);
    let row_c = Rc::clone(&row_fired);
    let copied_c = Rc::clone(&copied);
    let revealed_c = Rc::clone(&revealed);
    let card_c = Rc::clone(&card_fired);
    let purge_count_c = Rc::clone(&purge_count);
    let purge_states_c = Rc::clone(&purge_states);

    slint::spawn_local(async move {
        // Phase 1: tab clicks route to select-tab with the right index.
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

        // Phase 2: model row click fires clicked().
        let row = ModelRow::new().expect("ModelRow must instantiate");
        row.set_name("whisper-large-v3-turbo".into());
        let rc = Rc::clone(&row_c);
        row.on_clicked(move || *rc.borrow_mut() = true);
        click("ModelRow::row-touch", ElementHandle::find_by_element_id(&row, "ModelRow::row-touch").next()).await;

        // Phase 3: history copy click delivers (id, transcript).
        let card = HistoryCard::new().expect("HistoryCard must instantiate");
        card.set_rec_id(42);
        card.set_rec_transcript("Copy me please.".into());
        let cc = Rc::clone(&copied_c);
        card.on_copy_item(move |id, text| *cc.borrow_mut() = (id, text.to_string()));
        click("HistoryCard::copy-touch", ElementHandle::find_by_element_id(&card, "HistoryCard::copy-touch").next()).await;

        // Phase 4: key reveal click fires toggle-visible().
        let key = KeyField::new().expect("KeyField must instantiate");
        let kc = Rc::clone(&revealed_c);
        key.on_toggle_visible(move || *kc.borrow_mut() = true);
        click("KeyField::eye-touch", ElementHandle::find_by_element_id(&key, "KeyField::eye-touch").next()).await;

        // Phase 5: provider card click fires clicked().
        let prov = ProviderCard::new().expect("ProviderCard must instantiate");
        let pc = Rc::clone(&card_c);
        prov.on_clicked(move || *pc.borrow_mut() = true);
        click("ProviderCard::card-touch", ElementHandle::find_by_element_id(&prov, "ProviderCard::card-touch").next()).await;

        // Phase 6: two-stage purge confirmation arms -> cancels -> arms -> confirms.
        let studio = StudioWindow::new().expect("StudioWindow must instantiate");
        studio.window().set_size(slint::PhysicalSize::new(1040, 840));
        studio.set_history_count(5);
        let pcc = Rc::clone(&purge_count_c);
        studio.on_clear_all_history(move || *pcc.borrow_mut() += 1);
        // Click 1: arms confirmation, does NOT purge yet.
        click("StudioWindow::clear-touch (arm)", ElementHandle::find_by_element_id(&studio, "StudioWindow::clear-touch").next()).await;
        purge_states_c.borrow_mut().push(studio.get_purge_confirm_armed());
        // Click Cancel: disarms confirmation, still does NOT purge.
        click("StudioWindow::cancel-purge-touch", ElementHandle::find_by_element_id(&studio, "StudioWindow::cancel-purge-touch").next()).await;
        purge_states_c.borrow_mut().push(studio.get_purge_confirm_armed());
        // Click 2 (arm again) then Click 3 (Confirm Purge): invokes clear_all_history and disarms.
        click("StudioWindow::clear-touch (re-arm)", ElementHandle::find_by_element_id(&studio, "StudioWindow::clear-touch").next()).await;
        purge_states_c.borrow_mut().push(studio.get_purge_confirm_armed());
        click("StudioWindow::clear-touch (confirm)", ElementHandle::find_by_element_id(&studio, "StudioWindow::clear-touch").next()).await;
        purge_states_c.borrow_mut().push(studio.get_purge_confirm_armed());

        slint::quit_event_loop().unwrap();
    })
    .unwrap();
    slint::run_event_loop().unwrap();

    assert_eq!(*tabs.borrow(), vec![3, 1, 2], "tab clicks route 1 then 2");
    assert!(*row_fired.borrow(), "row click must fire clicked()");
    let (id, text) = copied.borrow().clone();
    assert_eq!(id, 42, "copy click must deliver id");
    assert_eq!(text, "Copy me please.", "copy click must deliver transcript");
    assert!(*revealed.borrow(), "eye click must fire toggle-visible()");
    assert!(*card_fired.borrow(), "card click must fire clicked()");
    assert_eq!(*purge_states.borrow(), vec![true, false, true, false], "two-stage purge arm/cancel/confirm states");
    assert_eq!(*purge_count.borrow(), 1, "clear_all_history must fire only after second-stage confirm click");
}
