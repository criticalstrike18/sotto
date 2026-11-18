// Computer-use suite: map a trigger the way a user does — open Studio,
// switch to Customization, scroll to the trigger card, press the primary
// Record button — then inject REAL OS mouse input through the REAL global
// hook and observe what the armed recorder captures.
//
// Uses Mouse Forward (XBUTTON2); the armed recorder swallows it, so the
// press has no side effects (no navigation, no clicks landing on windows).
//
// Deliberately NOT sent live here (each would act on the real machine):
// Left/Right clicks (they would click whatever sits under the cursor) and
// Caps Lock et al (they would toggle real keyboard state). Those paths are
// pinned by test_classify_record_button and test_reserved_capture_reasons,
// which cover the exact guards the live capture proc calls.
//
// Harness notes: the query walker only yields TouchAreas with real geometry
// (hence show() + scroll), and only elements inside the visible viewport
// (hence scrolling the rows into view before clicking).
//
// Backend: i_slint_backend_testing with system time (event loop runs).
// Slint's event-loop backend supports exactly ONE #[test] per process, so
// all phases run sequentially inside a single event-loop run.
// Windows-only: needs the real low-level hook + SendInput.

#![cfg(target_os = "windows")]

use i_slint_backend_testing::ElementHandle;
use slint::platform::PointerEventButton;
use slint::ComponentHandle;
use sotto_desktop::hotkey::{HotkeyManager, HotkeyMode, TriggerBinding};
use sotto_desktop::StudioWindow;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
};

const XBUTTON2_ID: u32 = 0x0002;

fn send_forward(down: bool) {
    let flag = if down { MOUSEEVENTF_XDOWN } else { MOUSEEVENTF_XUP };
    let mut input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: XBUTTON2_ID,
                dwFlags: flag,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(1, &mut input as *mut INPUT, std::mem::size_of::<INPUT>() as i32);
    }
}

#[derive(Default)]
struct Outcomes {
    record_seen_slot: i32,
    first_slot: i32,
    clicked: bool,
    captured: Vec<(u32, Option<u32>, i32)>,
    live_events_while_armed: usize,
    manager_ready: bool,
}

#[test]
fn test_computer_use_record_button_maps_forward() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    let outcomes: Rc<RefCell<Outcomes>> = Rc::new(RefCell::new(Outcomes {
        record_seen_slot: -99,
        first_slot: -99,
        ..Default::default()
    }));

    let out = Rc::clone(&outcomes);
    slint::spawn_local(async move {
        // A user opens Studio on the Customization tab and scrolls the
        // trigger card into view.
        let studio = StudioWindow::new().expect("StudioWindow must instantiate");
        studio.window().set_size(slint::PhysicalSize::new(1040, 840));
        studio.show().expect("Studio must show headless");
        studio.set_active_tab(2);
        studio.set_custom_viewport_y(-600.0);

        // Wire the Record callback the way the app does: remember the slot
        // and arm the REAL global record hook. Capture lands in a test-local
        // vec, so no config files are touched.
        let captured: Arc<Mutex<Vec<(u32, Option<u32>, i32)>>> = Arc::new(Mutex::new(Vec::new()));
        let cap_hook = Arc::clone(&captured);
        let out_arm = Rc::clone(&out);
        studio.on_start_recording_key(move |slot| {
            out_arm.borrow_mut().record_seen_slot = slot;
            let cap_call = Arc::clone(&cap_hook);
            HotkeyManager::set_record_hook(Some(Arc::new(move |mods: u32, key: Option<u32>, btn: i32| {
                cap_call.lock().unwrap().push((mods, key, btn));
            })));
        });

        // Phase 1: press Record on the PRIMARY slot row, like a user.
        let mut phase_ok = true;
        match ElementHandle::find_by_element_id(&studio, "StudioWindow::rec_p_touch").next() {
            Some(el) => {
                el.single_click(PointerEventButton::Left).await;
                out.borrow_mut().clicked = true;
                // Mirror the app wiring: armed flag + armed slot.
                studio.set_is_recording_key(true);
                studio.set_recording_slot(0);
                let s = out.borrow().record_seen_slot;
                out.borrow_mut().first_slot = s;
            }
            None => phase_ok = false,
        }

        // Phase 1b: pressing the OTHER slot's Record retargets (slot 1) —
        // it must not cancel the armed session. Fails on global-flag logic.
        if phase_ok {
            match ElementHandle::find_by_element_id(&studio, "StudioWindow::rec_s_touch").next() {
                Some(el) => {
                    el.single_click(PointerEventButton::Left).await;
                    studio.set_recording_slot(out.borrow().record_seen_slot);
                }
                None => phase_ok = false,
            }
        }

        // Phase 2: live manager + REAL Forward presses. Forward is NOT armed
        // as a live trigger here (only the recorder is), so any live event
        // would prove dispatch leaked through arming. Resend while polling:
        // a parallel test's armed recorder may swallow one press.
        if phase_ok {
            let live: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
            let live_c = Arc::clone(&live);
            match HotkeyManager::new(HotkeyMode::Toggle, move |evt| {
                live_c.lock().unwrap().push(format!("{:?}", evt));
            }) {
                Ok(mgr) => {
                    out.borrow_mut().manager_ready = true;
                    let fwd = TriggerBinding::normalized(0, None, Some(2));
                    mgr.apply_bindings(&fwd, &None, &None, &None);
                    std::thread::sleep(Duration::from_millis(300));

                    for i in 0..30 {
                        if !captured.lock().unwrap().is_empty() {
                            break;
                        }
                        if i % 5 == 0 {
                            send_forward(true);
                            std::thread::sleep(Duration::from_millis(60));
                            send_forward(false);
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    out.borrow_mut().captured = captured.lock().unwrap().clone();
                    out.borrow_mut().live_events_while_armed = live.lock().unwrap().len();
                    HotkeyManager::set_record_hook(None);
                }
                Err(e) => {
                    HotkeyManager::set_record_hook(None);
                    println!("Note: no interactive desktop for computer-use input: {}", e);
                }
            }
        }

        HotkeyManager::set_record_hook(None);
        slint::quit_event_loop().unwrap();
    })
    .unwrap();
    slint::run_event_loop().unwrap();

    let o = outcomes.borrow();
    assert!(o.clicked, "primary Record touch must be found (scrolled into view) and clicked");
    assert_eq!(o.first_slot, 0, "clicking primary Record must arm slot 0");
    assert_eq!(
        o.record_seen_slot, 1,
        "clicking secondary Record while armed must retarget to slot 1, not cancel"
    );
    if o.manager_ready {
        assert!(!o.captured.is_empty(), "real Forward press must be captured while armed");
        assert!(
            o.captured.iter().any(|&(_, k, b)| b == 2 && k.is_none()),
            "lone Forward press must capture button 2 with no fused key, got {:?}",
            o.captured
        );
        assert_eq!(
            o.live_events_while_armed, 0,
            "no live dictation event may fire while the recorder is armed"
        );
    }
    HotkeyManager::set_record_hook(None);
}
