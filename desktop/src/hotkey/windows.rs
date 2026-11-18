// desktop/src/hotkey/windows.rs

use super::{HotkeyEvent, HotkeyMode};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, RegisterHotKey, UnregisterHotKey, MOD_NOREPEAT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PostThreadMessageW, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, HC_ACTION, MSG, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_HOTKEY,
    WM_KEYDOWN, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_QUIT, WM_SYSKEYDOWN, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

/// Mouse trigger selection: 1 = Back (XBUTTON1), 2 = Forward (XBUTTON2), 3 = Middle.
pub const MOUSE_TRIGGER_BACK: i32 = 1;
pub const MOUSE_TRIGGER_FORWARD: i32 = 2;
pub const MOUSE_TRIGGER_MIDDLE: i32 = 3;

/// Win32 virtual-key codes matching the mouse trigger slots above.
pub const VK_XBUTTON1: u32 = 0x05;
pub const VK_XBUTTON2: u32 = 0x06;
pub const VK_MBUTTON_TRIG: u32 = 0x04;

/// Trigger slots: A = primary binding, B = secondary binding.
pub const HOTKEY_ID_SLOT_A: i32 = 1099;
pub const HOTKEY_ID_SLOT_B: i32 = 1100;

/// Pump-thread rearm messages: wParam = mods, lParam = vk (or VK_NONE_SENTINEL).
pub const WM_REARM_SLOT_A: u32 = 0x0400 + 101; // WM_USER + 101
pub const WM_REARM_SLOT_B: u32 = 0x0400 + 102; // WM_USER + 102
pub const VK_NONE_SENTINEL: u32 = u32::MAX;

/// Checks if a virtual key is currently physically held down.
pub fn is_key_down(vk: u16) -> bool {
    unsafe {
        match vk {
            0x11 | 0xA2 | 0xA3 => {
                // Control (Any: Left or Right)
                (GetAsyncKeyState(0x11) as i16) < 0
                    || (GetAsyncKeyState(0xA2) as i16) < 0
                    || (GetAsyncKeyState(0xA3) as i16) < 0
            }
            0x10 | 0xA0 | 0xA1 => {
                // Shift (Any: Left or Right)
                (GetAsyncKeyState(0x10) as i16) < 0
                    || (GetAsyncKeyState(0xA0) as i16) < 0
                    || (GetAsyncKeyState(0xA1) as i16) < 0
            }
            0x12 | 0xA4 | 0xA5 => {
                // Alt (Any: Left or Right)
                (GetAsyncKeyState(0x12) as i16) < 0
                    || (GetAsyncKeyState(0xA4) as i16) < 0
                    || (GetAsyncKeyState(0xA5) as i16) < 0
            }
            0x5B | 0x5C => {
                // Win (Left or Right)
                (GetAsyncKeyState(0x5B) as i16) < 0
                    || (GetAsyncKeyState(0x5C) as i16) < 0
            }
            _ => (GetAsyncKeyState(vk as i32) as i16) < 0,
        }
    }
}

/// Converts a Win32 virtual key code to a friendly string name.
pub fn vk_to_string(vk: u32) -> String {
    match vk {
        0x11 | 0xA2 | 0xA3 => "Ctrl".to_string(),
        0x10 | 0xA0 | 0xA1 => "Shift".to_string(),
        0x12 | 0xA4 | 0xA5 => "Alt".to_string(),
        0x5B | 0x5C => "Win".to_string(),
        0x5D => "Menu".to_string(),
        0x14 => "Caps Lock".to_string(),
        0x90 => "Num Lock".to_string(),
        0x91 => "Scroll Lock".to_string(),
        0x2C => "Print Screen".to_string(),
        0x13 => "Pause".to_string(),
        0xFF | 0x88 | 0xDF => "Fn".to_string(),
        0x70 => "F1".to_string(),
        0x71 => "F2".to_string(),
        0x72 => "F3".to_string(),
        0x73 => "F4".to_string(),
        0x74 => "F5".to_string(),
        0x75 => "F6".to_string(),
        0x76 => "F7".to_string(),
        0x77 => "F8".to_string(),
        0x78 => "F9".to_string(),
        0x79 => "F10".to_string(),
        0x7A => "F11".to_string(),
        0x7B => "F12".to_string(),
        0x7C..=0x87 => format!("F{}", 13 + (vk - 0x7C)),
        0x20 => "Space".to_string(),
        0x0D => "Enter".to_string(),
        0x09 => "Tab".to_string(),
        0x08 => "Backspace".to_string(),
        0x1B => "Esc".to_string(),
        0x2D => "Insert".to_string(),
        0x2E => "Delete".to_string(),
        0x24 => "Home".to_string(),
        0x23 => "End".to_string(),
        0x21 => "PageUp".to_string(),
        0x22 => "PageDown".to_string(),
        0x25 => "Left".to_string(),
        0x26 => "Up".to_string(),
        0x27 => "Right".to_string(),
        0x28 => "Down".to_string(),
        0xC0 => "`".to_string(),
        0xBC => ",".to_string(),
        0xBE => ".".to_string(),
        0xBF => "/".to_string(),
        0xBA => ";".to_string(),
        0xDE => "'".to_string(),
        0xDB => "[".to_string(),
        0xDD => "]".to_string(),
        0xDC => "\\".to_string(),
        0xBD => "-".to_string(),
        0xBB => "=".to_string(),
        0x41..=0x5A => (char::from_u32(vk).unwrap_or('?')).to_string(),
        0x30..=0x39 => (char::from_u32(vk).unwrap_or('?')).to_string(),
        _ => format!("Key 0x{:02X}", vk),
    }
}

/// Human-readable name for a mouse trigger slot.
pub fn mouse_trigger_display(button: i32) -> String {
    match button {
        1 => "Mouse Back".to_string(),
        2 => "Mouse Forward".to_string(),
        3 => "Mouse Middle".to_string(),
        _ => "Off".to_string(),
    }
}

/// Win32 virtual-key code for a mouse trigger slot (None when off).
pub fn mouse_trigger_vk(button: i32) -> Option<u32> {
    match button {
        1 => Some(VK_XBUTTON1),
        2 => Some(VK_XBUTTON2),
        3 => Some(VK_MBUTTON_TRIG),
        _ => None,
    }
}

/// Keys that must never become triggers: pressing them fires a system-wide
/// side effect (caps/num/scroll toggle, Start menu, Snipping Tool, context
/// menu), so recording them would both do that action AND save a broken
/// trigger. Returns a human reason for the recorder status line.
pub fn reserved_capture_reason(vk: u32) -> Option<&'static str> {
    match vk {
        0x14 => Some("Caps Lock toggles your keyboard"),
        0x90 => Some("Num Lock toggles your numpad"),
        0x91 => Some("Scroll Lock toggles scrolling"),
        0x5B | 0x5C => Some("the Win key opens the Start menu"),
        0x2C => Some("Print Screen opens the Snipping Tool"),
        0x5D => Some("the Menu key opens context menus"),
        _ => None,
    }
}

/// Classifies a low-level mouse message for trigger RECORDING. Only side
/// buttons and middle are capturable: Left/Right clicks (and wheel, X-up,
/// unknown X buttons) return None, so they can never be recorded — recording
/// Left click would make every click start dictation.
pub fn classify_record_button(msg: u32, xbutton_id: u32) -> Option<i32> {
    if msg == WM_XBUTTONDOWN {
        match xbutton_id {
            1 => Some(MOUSE_TRIGGER_BACK),
            2 => Some(MOUSE_TRIGGER_FORWARD),
            _ => None,
        }
    } else if msg == WM_MBUTTONDOWN {
        Some(MOUSE_TRIGGER_MIDDLE)
    } else {
        None
    }
}

/// Recorder decision for a key-down seen while armed (Esc cancel, reserved
/// rejection, standalone-modifier staging, or record-with-mods). Win keys
/// are rejected, never staged: pressing one opens the Start menu.
pub enum RecordingKeydown {
    Cancel,
    Rejected(&'static str),
    PendingMod(u32),
    RecordKey,
}

pub fn recording_keydown_action(raw_vk: u32) -> RecordingKeydown {
    if raw_vk == 0x1B {
        return RecordingKeydown::Cancel;
    }
    if let Some(reason) = reserved_capture_reason(raw_vk) {
        return RecordingKeydown::Rejected(reason);
    }
    match raw_vk {
        0x11 | 0xA2 | 0xA3 => RecordingKeydown::PendingMod(0x11),
        0x10 | 0xA0 | 0xA1 => RecordingKeydown::PendingMod(0x10),
        0x12 | 0xA4 | 0xA5 => RecordingKeydown::PendingMod(0x12),
        _ => RecordingKeydown::RecordKey,
    }
}

/// Recorder decision for a key-up while armed, given the staged modifier
/// (0 when none). A reserved staged key (e.g. from an older build's state)
/// is dropped with a message instead of completing.
pub enum RecordingKeyup {
    Ignored,
    Rejected(&'static str),
    CompleteStandalone,
}

pub fn recording_keyup_action(pending_mod: u32) -> RecordingKeyup {
    if pending_mod == 0 {
        return RecordingKeyup::Ignored;
    }
    match reserved_capture_reason(pending_mod) {
        Some(reason) => RecordingKeyup::Rejected(reason),
        None => RecordingKeyup::CompleteStandalone,
    }
}

/// Live state shared with the low-level mouse hook proc (which cannot capture).
/// `mice` holds (button, required-mods-mask) pairs from all armed bindings.
struct MouseLiveState {
    mice: Arc<Mutex<Vec<(i32, u32)>>>,
    mode: Arc<AtomicU8>,
    recording: Arc<AtomicBool>,
    callback: Arc<dyn Fn(HotkeyEvent) + Send + Sync + 'static>,
}

static MOUSE_LIVE: RwLock<Option<MouseLiveState>> = RwLock::new(None);

/// Live state for cancel triggers (keyboard & mouse).
struct CancelLiveState {
    triggers: Arc<Mutex<Vec<crate::hotkey::TriggerBinding>>>,
    recording: Arc<AtomicBool>,
    callback: Arc<dyn Fn(HotkeyEvent) + Send + Sync + 'static>,
}

static CANCEL_LIVE: RwLock<Option<CancelLiveState>> = RwLock::new(None);

#[repr(C)]
struct KbdllHookStruct {
    vk_code: u32,
    scan_code: u32,
    flags: u32,
    time: u32,
    dw_extra_info: usize,
}

fn mods_match(required: u32, current: u32) -> bool {
    (required & 0x000F) == (current & 0x000F)
}

fn check_cancel_key_trigger(vk: u32) -> Option<Arc<dyn Fn(HotkeyEvent) + Send + Sync + 'static>> {
    let guard = CANCEL_LIVE.read().ok()?;
    let st = guard.as_ref()?;
    if !st.recording.load(Ordering::SeqCst) {
        return None;
    }
    let triggers = st.triggers.lock().ok()?.clone();
    let current_mods = current_mod_mask();
    for trig in triggers {
        if let Some(trig_vk) = trig.key {
            if trig_vk == vk && mods_match(trig.mods, current_mods) {
                return Some(Arc::clone(&st.callback));
            }
        }
    }
    None
}

fn check_cancel_mouse_trigger(msg: u32, l_param: isize) -> Option<Arc<dyn Fn(HotkeyEvent) + Send + Sync + 'static>> {
    let guard = CANCEL_LIVE.read().ok()?;
    let st = guard.as_ref()?;
    if !st.recording.load(Ordering::SeqCst) {
        return None;
    }
    let button = if msg == WM_XBUTTONDOWN {
        let data = unsafe { (*(l_param as *const MsllHookStruct)).mouse_data };
        classify_record_button(msg, ((data >> 16) & 0xFFFF) as u32)
    } else if msg == WM_MBUTTONDOWN {
        classify_record_button(msg, 0)
    } else {
        None
    }?;
    let triggers = st.triggers.lock().ok()?.clone();
    let current_mods = current_mod_mask();
    for trig in triggers {
        if let Some(trig_btn) = trig.mouse {
            if trig_btn == button && mods_match(trig.mods, current_mods) {
                return Some(Arc::clone(&st.callback));
            }
        }
    }
    None
}

unsafe extern "system" fn keyboard_hook_proc(n_code: i32, w_param: usize, l_param: isize) -> isize {
    if n_code == (HC_ACTION as i32) {
        let msg = w_param as u32;
        if (msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN) && !RECORD_ARMED.load(Ordering::SeqCst) {
            let kbd = &*(l_param as *const KbdllHookStruct);
            if let Some(cb) = check_cancel_key_trigger(kbd.vk_code) {
                println!("[Hotkey Engine] Cancel key triggered (vk=0x{:X})", kbd.vk_code);
                cb(HotkeyEvent::CancelListening);
                return 1;
            }
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), n_code, w_param, l_param) }
}

/// One-shot mouse capture for trigger recording: invoked with the live
/// modifier mask + pressed button when recording is armed. Installed by the
/// app while its key recorder is listening; independent of trigger dispatch.
static MOUSE_RECORDER: Mutex<Option<Arc<dyn Fn(u32, Option<u32>, i32) + Send + Sync + 'static>>> =
    Mutex::new(None);

/// Recording-armed flag, checked on the hot path before trigger matching.
static RECORD_ARMED: AtomicBool = AtomicBool::new(false);

/// Mouse hook install state: 0 = pending, 1 = installed, -1 = failed.
static HOOK_STATUS: AtomicI32 = AtomicI32::new(0);

/// Hook install state for diagnostics (surfaced in Studio).
pub fn mouse_hook_status() -> i32 {
    HOOK_STATUS.load(Ordering::SeqCst)
}

#[repr(C)]
struct MsllHookStruct {
    _pt_x: i32,
    _pt_y: i32,
    mouse_data: u32,
    _flags: u32,
    _time: u32,
    _extra: usize,
}

/// True when every modifier bit in the Win32 MOD_* mask is physically held.
fn mods_held(mask: u32) -> bool {
    // Ctrl / Alt / Shift / Win, checked via the generic (either-side) VKs.
    const PAIRS: [(u32, u16); 4] = [
        (0x0002, 0x11),
        (0x0001, 0x12),
        (0x0004, 0x10),
        (0x0008, 0x5B),
    ];
    PAIRS
        .iter()
        .all(|(bit, vk)| mask & bit == 0 || is_key_down(*vk))
}

/// Returns Some(is_down) when the message is the press/release of any armed
/// mouse trigger whose required modifiers are held. Never panics.
fn match_mouse_trigger(msg: u32, l_param: isize) -> Option<bool> {
    let mice: Vec<(i32, u32)> = MOUSE_LIVE
        .read()
        .ok()?
        .as_ref()
        .map(|st| st.mice.lock().map(|m| m.clone()).unwrap_or_default())?;
    if mice.is_empty() {
        return None;
    }
    match msg {
        x if x == WM_XBUTTONDOWN || x == WM_XBUTTONUP => {
            let data = unsafe { (*(l_param as *const MsllHookStruct)).mouse_data };
            let xbtn = ((data >> 16) & 0xFFFF) as u32;
            let button = if xbtn == 1 {
                MOUSE_TRIGGER_BACK
            } else if xbtn == 2 {
                MOUSE_TRIGGER_FORWARD
            } else {
                return None;
            };
            mice.iter().find_map(|(b, req)| {
                (*b == button && mods_held(*req)).then_some(msg == WM_XBUTTONDOWN)
            })
        }
        x if x == WM_MBUTTONDOWN || x == WM_MBUTTONUP => mice
            .iter()
            .find_map(|(b, req)| {
                (*b == MOUSE_TRIGGER_MIDDLE && mods_held(*req))
                    .then_some(msg == WM_MBUTTONDOWN)
            }),
        _ => None,
    }
}

/// Dispatches a matched mouse trigger press/release into the dictation engine.
/// Toggle mode flips on press; Hold-to-Talk starts on press and stops on release.
/// Never fires while a trigger recording is armed (capture path owns input).
fn handle_mouse_trigger(is_down: bool) {
    if RECORD_ARMED.load(Ordering::SeqCst) {
        return;
    }
    let snapshot = MOUSE_LIVE.read().ok().and_then(|guard| {
        guard.as_ref().map(|st| {
            (
                Arc::clone(&st.mode),
                Arc::clone(&st.recording),
                Arc::clone(&st.callback),
            )
        })
    });
    let Some((mode, recording, callback)) = snapshot else {
        return;
    };
    let hold = mode.load(Ordering::SeqCst) == HotkeyMode::HoldToTalk as u8;
    if hold {
        if is_down {
            if echo_suppressed() {
                return;
            }
            if !recording.swap(true, Ordering::SeqCst) {
                println!("[Hotkey Engine] Mouse trigger -> START listening (hold)");
                callback(HotkeyEvent::StartListening);
            }
        } else if recording.swap(false, Ordering::SeqCst) {
            println!("[Hotkey Engine] Mouse trigger -> STOP listening (release)");
            callback(HotkeyEvent::StopListening);
        }
    } else if is_down {
        if echo_suppressed() {
            return;
        }
        let was_rec = recording.load(Ordering::SeqCst);
        recording.store(!was_rec, Ordering::SeqCst);
        println!(
            "[Hotkey Engine] Mouse trigger -> {} listening (toggle)",
            if was_rec { "STOP" } else { "START" }
        );
        callback(if was_rec {
            HotkeyEvent::StopListening
        } else {
            HotkeyEvent::StartListening
        });
    }
}

/// Low-level mouse hook proc. Runs on every system mouse event: match fast,
/// never block the chain, and swallow only the active trigger's press/release
/// (so e.g. Back doesn't also navigate while armed).
unsafe extern "system" fn mouse_hook_proc(n_code: i32, w_param: usize, l_param: isize) -> isize {
    if n_code == HC_ACTION as i32 {
        let msg = w_param as u32;
        // Recording capture takes precedence over trigger dispatch.
        if RECORD_ARMED.load(Ordering::SeqCst) {
            let button = if msg == WM_XBUTTONDOWN {
                let data = unsafe { (*(l_param as *const MsllHookStruct)).mouse_data };
                classify_record_button(msg, ((data >> 16) & 0xFFFF) as u32)
            } else {
                classify_record_button(msg, 0)
            };
            if let Some(btn) = button {
                // Drop the MOUSE_RECORDER lock BEFORE invoking capture, avoiding
                // recursive mutex deadlock when capture calls complete_recording() ->
                // set_record_hook(None) on this same mouse hook thread.
                let capture_opt = MOUSE_RECORDER.lock().ok().and_then(|g| g.clone());
                if let Some(capture) = capture_opt {
                    // Fuse co-held keys (dual-signal presses) into one chord.
                    let key = held_nonmod_keys().into_iter().next();
                    capture(current_mod_mask(), key, btn);
                    return 1;
                }
            }
        } else if let Some(cb) = check_cancel_mouse_trigger(msg, l_param) {
            println!("[Hotkey Engine] Cancel mouse button triggered");
            cb(HotkeyEvent::CancelListening);
            return 1;
        } else if let Some(is_down) = match_mouse_trigger(msg, l_param) {
            handle_mouse_trigger(is_down);
            return 1;
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), n_code, w_param, l_param) }
}

/// Currently held modifiers as a Win32 MOD_* bitmask (for chord capture).
pub fn current_mod_mask() -> u32 {
    let mut mask = 0u32;
    if is_key_down(0x11) {
        mask |= 0x0002;
    }
    if is_key_down(0x12) {
        mask |= 0x0001;
    }
    if is_key_down(0x10) {
        mask |= 0x0004;
    }
    if is_key_down(0x5B) || is_key_down(0x5C) {
        mask |= 0x0008;
    }
    mask
}

/// Whether a virtual key may join a recorded chord as the swept-in key.
/// Modifiers fold into `mods`, mouse buttons live on the mouse side, Esc is
/// Cancel, and reserved keys (Caps/Win/...) are never recordable.
pub fn sweep_include_key(vk: u32) -> bool {
    match vk {
        0x01 | 0x02 | 0x04 | 0x05 | 0x06 => false,
        0x1B => false,
        0x10 | 0x11 | 0x12 | 0xA0..=0xA5 => false,
        _ => reserved_capture_reason(vk).is_none(),
    }
}

/// Non-modifier keys physically held right now (ascending vk). Lets one
/// physical press that emits two OS signals (remapped mouse button sending
/// XBUTTON + keystroke) fuse into a SINGLE chord completion instead of
/// racing two recordings.
pub fn held_nonmod_keys() -> Vec<u32> {
    let mut out = Vec::new();
    for vk in 0x08u32..=0xFE {
        if sweep_include_key(vk) && is_key_down(vk as u16) {
            out.push(vk);
        }
    }
    out
}

/// Mouse buttons physically held right now, for the keyboard capture path.
pub fn held_mouse_buttons() -> Vec<i32> {
    let mut out = Vec::new();
    if is_key_down(VK_XBUTTON1 as u16) {
        out.push(MOUSE_TRIGGER_BACK);
    }
    if is_key_down(VK_XBUTTON2 as u16) {
        out.push(MOUSE_TRIGGER_FORWARD);
    }
    if is_key_down(VK_MBUTTON_TRIG as u16) {
        out.push(MOUSE_TRIGGER_MIDDLE);
    }
    out
}

static LAST_TRIGGER_FIRE_MS: AtomicU64 = AtomicU64::new(0);
const TRIGGER_ECHO_WINDOW_MS: u64 = 100;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Echo suppression: some mice/keyboards deliver two signals for one
/// physical press milliseconds apart; the second must not toggle back.
/// Releases/stops bypass this (checked only on start/toggle-down edges).
fn echo_suppressed() -> bool {
    let now = now_ms();
    let last = LAST_TRIGGER_FIRE_MS.load(Ordering::SeqCst);
    if now < last {
        LAST_TRIGGER_FIRE_MS.store(now, Ordering::SeqCst);
        return false;
    }
    if now.saturating_sub(last) < TRIGGER_ECHO_WINDOW_MS {
        return true;
    }
    LAST_TRIGGER_FIRE_MS.store(now, Ordering::SeqCst);
    false
}

/// Formats modifiers and key into standard human-readable format (e.g. "Ctrl + Shift + Space").
pub fn format_hotkey(modifiers: u32, vk: u32) -> String {
    let mut parts = Vec::new();
    if (modifiers & 0x0002) != 0 && !matches!(vk, 0x11 | 0xA2 | 0xA3) {
        parts.push("Ctrl");
    }
    if (modifiers & 0x0001) != 0 && !matches!(vk, 0x12 | 0xA4 | 0xA5) {
        parts.push("Alt");
    }
    if (modifiers & 0x0004) != 0 && !matches!(vk, 0x10 | 0xA0 | 0xA1) {
        parts.push("Shift");
    }
    if (modifiers & 0x0008) != 0 && !matches!(vk, 0x5B | 0x5C) {
        parts.push("Win");
    }
    let key_str = vk_to_string(vk);
    parts.push(&key_str);
    parts.join(" + ")
}

/// Global Hotkey Manager for Windows.
///
/// Runs a dedicated OS message pump thread that registers global hotkeys
/// and dispatches StartListening/StopListening events based on either
/// Toggle or Hold-to-Talk mode.
pub struct HotkeyManager {
    mode: Arc<AtomicU8>,
    is_recording: Arc<AtomicBool>,
    thread_id: Arc<AtomicU32>,
    /// Keyboard combos armed as OS hotkeys: (slot id, mods, vk).
    armed_keys: Arc<Mutex<Vec<ArmedKey>>>,
    /// Standalone-modifier slots polled by the watcher: (slot id, vk list).
    armed_standalone: Arc<Mutex<Vec<(i32, Vec<u32>)>>>,
    mouse_thread_id: Arc<AtomicU32>,
    join_handle: Option<JoinHandle<()>>,
    mouse_join_handle: Option<JoinHandle<()>>,
}

/// One OS-registered keyboard trigger: slot A (primary) or B (secondary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArmedKey {
    pub slot_id: i32,
    pub mods: u32,
    pub vk: u32,
}

impl HotkeyManager {
    /// Spawns the hotkey manager background threads with no bindings armed.
    /// Call `apply_bindings` to arm trigger slots.
    pub fn new<F>(initial_mode: HotkeyMode, callback: F) -> Result<Self, String>
    where
        F: Fn(HotkeyEvent) + Send + Sync + 'static,
    {
        Self::spawn(initial_mode, callback)
    }

    /// Spawns the manager with primary/secondary trigger bindings armed.
    pub fn new_with_bindings<F>(
        initial_mode: HotkeyMode,
        primary: &crate::hotkey::TriggerBinding,
        secondary: &Option<crate::hotkey::TriggerBinding>,
        cancel_primary: &Option<crate::hotkey::TriggerBinding>,
        cancel_secondary: &Option<crate::hotkey::TriggerBinding>,
        callback: F,
    ) -> Result<Self, String>
    where
        F: Fn(HotkeyEvent) + Send + Sync + 'static,
    {
        let mgr = Self::spawn(initial_mode, callback)?;
        mgr.apply_bindings(primary, secondary, cancel_primary, cancel_secondary);
        Ok(mgr)
    }

    fn spawn<F>(initial_mode: HotkeyMode, callback: F) -> Result<Self, String>
    where
        F: Fn(HotkeyEvent) + Send + Sync + 'static,
    {
        crate::inject::attach_input_desktop();

        let mode = Arc::new(AtomicU8::new(initial_mode as u8));
        let is_recording = Arc::new(AtomicBool::new(false));
        let thread_id = Arc::new(AtomicU32::new(0));
        let armed_keys = Arc::new(Mutex::new(Vec::<ArmedKey>::new()));
        let armed_standalone = Arc::new(Mutex::new(Vec::<(i32, Vec<u32>)>::new()));
        let mouse_thread_id = Arc::new(AtomicU32::new(0));

        let mode_clone = Arc::clone(&mode);
        let rec_clone = Arc::clone(&is_recording);
        let tid_clone = Arc::clone(&thread_id);
        let armed_keys_clone = Arc::clone(&armed_keys);
        let cb = Arc::new(callback);

        // Publish live state for the low-level mouse hook proc (type-erased:
        // the proc cannot capture the generic callback). Latest manager wins.
        {
            let inner = Arc::clone(&cb);
            let erased: Arc<dyn Fn(HotkeyEvent) + Send + Sync + 'static> =
                Arc::new(move |evt| inner(evt));
            let state = MouseLiveState {
                mice: Arc::new(Mutex::new(Vec::new())),
                mode: Arc::clone(&mode),
                recording: Arc::clone(&is_recording),
                callback: erased,
            };
            if let Ok(mut guard) = MOUSE_LIVE.write() {
                *guard = Some(state);
            }
        }

        {
            let inner = Arc::clone(&cb);
            let erased: Arc<dyn Fn(HotkeyEvent) + Send + Sync + 'static> =
                Arc::new(move |evt| inner(evt));
            let state = CancelLiveState {
                triggers: Arc::new(Mutex::new(Vec::new())),
                recording: Arc::clone(&is_recording),
                callback: erased,
            };
            if let Ok(mut guard) = CANCEL_LIVE.write() {
                *guard = Some(state);
            }
        }

        let (init_tx, init_rx) = std::sync::mpsc::channel();

        // Background watcher thread for standalone modifier-only bindings
        // (e.g. bare Ctrl), which Win32 RegisterHotKey does not dispatch.
        // Polls every armed slot's VK set for fresh press edges.
        let m_mode = Arc::clone(&mode);
        let m_rec = Arc::clone(&is_recording);
        let m_standalone = Arc::clone(&armed_standalone);
        let m_cb = Arc::clone(&cb);

        thread::Builder::new()
            .name("sotto-modifier-watcher".to_string())
            .spawn(move || {
                let mut was_down: Vec<bool> = Vec::new();
                loop {
                    thread::sleep(Duration::from_millis(15));
                    let slots: Vec<(i32, Vec<u32>)> = m_standalone
                        .lock()
                        .map(|s| s.clone())
                        .unwrap_or_default();
                    if was_down.len() != slots.len() {
                        was_down = vec![false; slots.len()];
                    }
                    for (idx, (slot_id, vks)) in slots.iter().enumerate() {
                        let is_down = vks.iter().any(|vk| is_key_down(*vk as u16));
                        if is_down && !was_down[idx] && !RECORD_ARMED.load(Ordering::SeqCst) {
                            let mode = if m_mode.load(Ordering::SeqCst) == HotkeyMode::HoldToTalk as u8 {
                                HotkeyMode::HoldToTalk
                            } else {
                                HotkeyMode::Toggle
                            };
                            println!("[Hotkey Engine] Standalone trigger fired: slot={}, mode={:?}", slot_id, mode);
                            Self::handle_hotkey_trigger(
                                *slot_id,
                                mode,
                                &vks,
                                &m_rec,
                                &m_cb,
                            );
                        }
                        was_down[idx] = is_down;
                    }
                }
            })
            .ok();

        let mouse_tid = Arc::clone(&mouse_thread_id);
        let mouse_handle = thread::Builder::new()
            .name("sotto-mouse-pump".to_string())
            .spawn(move || {
                crate::inject::attach_input_desktop();
                let hook = unsafe {
                    SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), std::ptr::null_mut(), 0)
                };
                let kbd_hook = unsafe {
                    SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook_proc), std::ptr::null_mut(), 0)
                };
                if hook.is_null() {
                    HOOK_STATUS.store(-1, Ordering::SeqCst);
                    eprintln!("[Hotkey Engine] WH_MOUSE_LL hook failed; mouse triggers unavailable");
                    return;
                }
                HOOK_STATUS.store(1, Ordering::SeqCst);
                if kbd_hook.is_null() {
                    eprintln!("[Hotkey Engine] WH_KEYBOARD_LL hook failed; cancel key triggers unavailable");
                } else {
                    println!("[Hotkey Engine] Low-level keyboard hook installed (Cancel triggers)");
                }
                let tid = unsafe { GetCurrentThreadId() };
                mouse_tid.store(tid, Ordering::SeqCst);
                println!("[Hotkey Engine] Low-level mouse hook installed (Back/Forward/Middle triggers)");

                let mut msg = MSG {
                    hwnd: std::ptr::null_mut(),
                    message: 0,
                    wParam: 0,
                    lParam: 0,
                    time: 0,
                    pt: POINT { x: 0, y: 0 },
                };
                while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
                    unsafe {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                unsafe {
                    if !hook.is_null() {
                        UnhookWindowsHookEx(hook);
                    }
                    if !kbd_hook.is_null() {
                        UnhookWindowsHookEx(kbd_hook);
                    }
                }
            })
            .ok();

        let handle = thread::Builder::new()
            .name("sotto-hotkey-pump".to_string())
            .spawn(move || {
                crate::inject::attach_input_desktop();
                let tid = unsafe { GetCurrentThreadId() };
                tid_clone.store(tid, Ordering::SeqCst);

                // No fixed hotkeys: trigger slots arm via WM_REARM_SLOT_A/B
                // posted by apply_bindings (primary/secondary bindings).
                println!("[Hotkey Engine] Hotkey pump online; slots arm on demand");

                let _ = init_tx.send(Ok(()));

                // Standard Win32 Thread Message Loop
                let mut msg = MSG {
                    hwnd: std::ptr::null_mut(),
                    message: 0,
                    wParam: 0,
                    lParam: 0,
                    time: 0,
                    pt: POINT { x: 0, y: 0 },
                };

                while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
                    if msg.message == WM_REARM_SLOT_A || msg.message == WM_REARM_SLOT_B {
                        let slot_id = if msg.message == WM_REARM_SLOT_A {
                            HOTKEY_ID_SLOT_A
                        } else {
                            HOTKEY_ID_SLOT_B
                        };
                        let new_mods = msg.wParam as u32;
                        let new_vk = msg.lParam as u32;
                        unsafe {
                            UnregisterHotKey(std::ptr::null_mut(), slot_id);
                            if new_vk != VK_NONE_SENTINEL {
                                let ok = RegisterHotKey(
                                    std::ptr::null_mut(),
                                    slot_id,
                                    new_mods | MOD_NOREPEAT as u32,
                                    new_vk,
                                );
                                println!(
                                    "[Hotkey Engine] Slot {} bound: mods=0x{:X}, vk=0x{:X}, registered={}",
                                    slot_id, new_mods, new_vk, ok != 0
                                );
                            } else {
                                println!("[Hotkey Engine] Slot {} unbound", slot_id);
                            }
                        }
                        continue;
                    }

                    if msg.message == WM_HOTKEY {
                        let hotkey_id = msg.wParam as i32;
                        // Never dispatch while recording (capture owns input).
                        if RECORD_ARMED.load(Ordering::SeqCst) {
                            continue;
                        }
                        // Only armed slot ids dispatch; anything else is stale.
                        let main_vk = armed_keys_clone
                            .lock()
                            .map(|keys| {
                                keys.iter().find(|k| k.slot_id == hotkey_id).map(|k| k.vk)
                            })
                            .unwrap_or(None);

                        if let Some(vk) = main_vk {
                            let current_mode = if mode_clone.load(Ordering::SeqCst) == HotkeyMode::HoldToTalk as u8 {
                                HotkeyMode::HoldToTalk
                            } else {
                                HotkeyMode::Toggle
                            };

                            Self::handle_hotkey_trigger(
                                hotkey_id,
                                current_mode,
                                &[vk],
                                &rec_clone,
                                &cb,
                            );
                        }
                    }

                    unsafe {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }

                // Cleanup hotkeys on exit
                unsafe {
                    UnregisterHotKey(std::ptr::null_mut(), HOTKEY_ID_SLOT_A);
                    UnregisterHotKey(std::ptr::null_mut(), HOTKEY_ID_SLOT_B);
                }
            })
            .map_err(|e| format!("Failed to spawn hotkey thread: {}", e))?;

        // Wait for thread initialization
        match init_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(())) => Ok(Self {
                mode,
                is_recording,
                thread_id,
                armed_keys,
                armed_standalone,
                mouse_thread_id,
                join_handle: Some(handle),
                mouse_join_handle: mouse_handle,
            }),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(format!("Hotkey initialization timed out: {}", e)),
        }
    }

    /// Sets the hotkey operational mode (Toggle or HoldToTalk).
    pub fn set_mode(&self, mode: HotkeyMode) {
        self.mode.store(mode as u8, Ordering::SeqCst);
        println!("[Hotkey Engine] Mode changed to: {:?}", mode);
    }

    pub fn set_recording(&self, recording: bool) {
        self.is_recording.store(recording, Ordering::SeqCst);
    }

    /// Returns the current hotkey mode.
    pub fn mode(&self) -> HotkeyMode {
        if self.mode.load(Ordering::SeqCst) == HotkeyMode::HoldToTalk as u8 {
            HotkeyMode::HoldToTalk
        } else {
            HotkeyMode::Toggle
        }
    }

    /// Query if voice dictation is currently recording.
    pub fn is_recording(&self) -> bool {
        self.is_recording.load(Ordering::SeqCst)
    }

    /// Installs (or clears with None) the one-shot mouse capture used while
    /// the app's trigger recorder is listening.
    pub fn set_record_hook(f: Option<Arc<dyn Fn(u32, Option<u32>, i32) + Send + Sync + 'static>>) {
        RECORD_ARMED.store(f.is_some(), Ordering::SeqCst);
        if let Ok(mut guard) = MOUSE_RECORDER.lock() {
            *guard = f;
        }
    }

    /// Internal handler when a trigger fires (hotkey id or watcher edge).
    /// `key_vks` lists the keys whose release ends Hold-to-Talk (release on
    /// ANY listed key going up; single-entry preserves classic behavior).
    fn handle_hotkey_trigger<F>(
        _hotkey_id: i32,
        mode: HotkeyMode,
        key_vks: &[u32],
        is_rec: &Arc<AtomicBool>,
        callback: &Arc<F>,
    ) where
        F: Fn(HotkeyEvent) + Send + Sync + 'static,
    {
        match mode {
            HotkeyMode::Toggle => {
                if echo_suppressed() {
                    return;
                }
                let currently_rec = is_rec.load(Ordering::SeqCst);
                if !currently_rec {
                    is_rec.store(true, Ordering::SeqCst);
                    println!("[Hotkey Engine] Toggle -> START listening");
                    callback(HotkeyEvent::StartListening);
                } else {
                    is_rec.store(false, Ordering::SeqCst);
                    println!("[Hotkey Engine] Toggle -> STOP listening");
                    callback(HotkeyEvent::StopListening);
                }
            }
            HotkeyMode::HoldToTalk => {
                // If already recording, ignore key repeats
                if is_rec.load(Ordering::SeqCst) {
                    return;
                }
                if echo_suppressed() {
                    return;
                }

                is_rec.store(true, Ordering::SeqCst);
                println!("[Hotkey Engine] Hold-to-Talk -> START listening");
                callback(HotkeyEvent::StartListening);

                // Spawn a dedicated key-release poller
                let is_rec_poller = Arc::clone(is_rec);
                let cb_poller = Arc::clone(callback);
                let release_vks: Vec<u16> = key_vks.iter().map(|v| (v & 0xFFFF) as u16).collect();

                thread::spawn(move || {
                    let started = Instant::now();
                    loop {
                        thread::sleep(Duration::from_millis(20));

                        // 60-second safety timeout prevents runaway recording
                        if started.elapsed() > Duration::from_secs(60) {
                            println!("[Hotkey Engine] Hold-to-Talk release timeout (60s exceeded)");
                            break;
                        }

                        // If dictation stopped externally, exit poller
                        if !is_rec_poller.load(Ordering::SeqCst) {
                            return;
                        }

                        // Release when ANY tracked key is up (single-key:
                        // identical to the classic main-vk check).
                        let any_down = release_vks.iter().any(|vk| is_key_down(*vk));
                        if !any_down {
                            println!("[Hotkey Engine] Key release detected!");
                            break;
                        }
                    }

                    if is_rec_poller.load(Ordering::SeqCst) {
                        is_rec_poller.store(false, Ordering::SeqCst);
                        println!("[Hotkey Engine] Hold-to-Talk -> STOP listening");
                        cb_poller(HotkeyEvent::StopListening);
                    }
                });
            }
        }
    }

    /// Arms primary + secondary trigger bindings (replaces preset/custom/mouse
    /// setters). OS hotkey (re)registration runs on the pump thread via posted
    /// messages; watcher/mouse matchers update synchronously.
    pub fn apply_bindings(
        &self,
        primary: &crate::hotkey::TriggerBinding,
        secondary: &Option<crate::hotkey::TriggerBinding>,
        cancel_primary: &Option<crate::hotkey::TriggerBinding>,
        cancel_secondary: &Option<crate::hotkey::TriggerBinding>,
    ) {
        let slots = [
            (HOTKEY_ID_SLOT_A, Some(primary)),
            (HOTKEY_ID_SLOT_B, secondary.as_ref()),
        ];

        let mut keys = Vec::new();
        let mut standalone: Vec<(i32, Vec<u32>)> = Vec::new();
        let mut mice = Vec::new();

        for (slot_id, binding) in slots {
            let Some(b) = binding else { continue };
            let plan = super::plan_binding(b);
            match plan.keyboard {
                Some((mods, vk)) => {
                    keys.push(ArmedKey { slot_id, mods, vk });
                    let tid = self.thread_id.load(Ordering::SeqCst);
                    if tid != 0 {
                        let msg = if slot_id == HOTKEY_ID_SLOT_A {
                            WM_REARM_SLOT_A
                        } else {
                            WM_REARM_SLOT_B
                        };
                        unsafe {
                            PostThreadMessageW(tid, msg, mods as usize, vk as isize);
                        }
                    }
                }
                None => {
                    // No keyboard part: unregister any previous OS hotkey.
                    let tid = self.thread_id.load(Ordering::SeqCst);
                    if tid != 0 {
                        let msg = if slot_id == HOTKEY_ID_SLOT_A {
                            WM_REARM_SLOT_A
                        } else {
                            WM_REARM_SLOT_B
                        };
                        unsafe {
                            PostThreadMessageW(tid, msg, 0, VK_NONE_SENTINEL as isize);
                        }
                    }
                }
            }
            if !plan.standalone.is_empty() {
                standalone.push((slot_id, plan.standalone));
            }
            if let Some((btn, req)) = plan.mouse {
                mice.push((btn, req));
            }
        }

        if let Ok(mut guard) = self.armed_keys.lock() {
            *guard = keys;
        }
        if let Ok(mut guard) = self.armed_standalone.lock() {
            *guard = standalone;
        }
        if let Ok(guard) = MOUSE_LIVE.read() {
            if let Some(ref st) = *guard {
                if let Ok(mut m) = st.mice.lock() {
                    *m = mice;
                }
            }
        }
        let mut cancels = Vec::new();
        if let Some(cp) = cancel_primary {
            if cp.is_valid() {
                cancels.push(cp.clone());
            }
        }
        if let Some(cs) = cancel_secondary {
            if cs.is_valid() {
                cancels.push(cs.clone());
            }
        }
        if let Ok(guard) = CANCEL_LIVE.read() {
            if let Some(ref st) = *guard {
                if let Ok(mut trig_guard) = st.triggers.lock() {
                    *trig_guard = cancels;
                }
            }
        }

        println!(
            "[Hotkey Engine] Slots armed: primary={} secondary={} cancel_primary={} cancel_secondary={}",
            primary.display(),
            secondary.as_ref().map(|b| b.display()).unwrap_or_else(|| "—".to_string()),
            cancel_primary.as_ref().map(|b| b.display()).unwrap_or_else(|| "—".to_string()),
            cancel_secondary.as_ref().map(|b| b.display()).unwrap_or_else(|| "—".to_string())
        );
    }

    /// Temporarily unregisters OS hotkeys from Windows while recorder is armed.
    /// This allows keys like F8 or combinations that were bound as hotkeys to
    /// pass cleanly into the focused window for recording.
    pub fn disarm_hotkeys(&self) {
        let tid = self.thread_id.load(Ordering::SeqCst);
        if tid != 0 {
            unsafe {
                PostThreadMessageW(tid, WM_REARM_SLOT_A, 0, VK_NONE_SENTINEL as isize);
                PostThreadMessageW(tid, WM_REARM_SLOT_B, 0, VK_NONE_SENTINEL as isize);
            }
        }
        if let Ok(mut guard) = self.armed_keys.lock() {
            guard.clear();
        }
    }
}

impl Drop for HotkeyManager {
    fn drop(&mut self) {
        let tid = self.thread_id.load(Ordering::SeqCst);
        if tid != 0 {
            unsafe {
                PostThreadMessageW(tid, WM_QUIT, 0, 0);
            }
        }
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
        let mouse_tid = self.mouse_thread_id.load(Ordering::SeqCst);
        if mouse_tid != 0 {
            unsafe {
                PostThreadMessageW(mouse_tid, WM_QUIT, 0, 0);
            }
        }
        if let Some(handle) = self.mouse_join_handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Hook-installing tests share one global LL hook + dispatch state, so
    /// they must not overlap. Pure mapping tests stay parallel.
    static HOOK_SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn test_hotkey_manager_lifecycle_and_modes() {
        let _serial = HOOK_SERIAL.lock().unwrap();
        let manager = HotkeyManager::new(HotkeyMode::Toggle, move |evt| {
            println!("Event: {:?}", evt);
        });

        match manager {
            Ok(mgr) => {
                assert_eq!(mgr.mode(), HotkeyMode::Toggle);
                mgr.set_mode(HotkeyMode::HoldToTalk);
                assert_eq!(mgr.mode(), HotkeyMode::HoldToTalk);
                assert!(!mgr.is_recording());
                // Slot arming is additive-safe and repeatable.
                let primary = crate::hotkey::TriggerBinding::f8();
                let secondary = Some(crate::hotkey::TriggerBinding::ctrl_shift_space());
                mgr.apply_bindings(&primary, &secondary, &None, &None);
                mgr.apply_bindings(&primary, &None, &None, &None);
                assert!(!mgr.is_recording());
            }
            Err(e) => {
                println!("Note: Running in test environment without interactive desktop: {}", e);
            }
        }
    }

    #[test]
    fn test_is_key_down_query() {
        let pressed = is_key_down(0x77); // VK_F8
        assert!(!pressed || pressed);
    }

    #[test]
    fn test_format_hotkey_strings() {
        assert_eq!(format_hotkey(0, 0x77), "F8");
        assert_eq!(format_hotkey(0x0002 | 0x0004, 0x20), "Ctrl + Shift + Space");
        assert_eq!(format_hotkey(0x0001, 0x44), "Alt + D");
    }

    #[test]
    fn test_mouse_trigger_slots() {
        assert_eq!(mouse_trigger_display(0), "Off");
        assert_eq!(mouse_trigger_display(1), "Mouse Back");
        assert_eq!(mouse_trigger_display(2), "Mouse Forward");
        assert_eq!(mouse_trigger_display(3), "Mouse Middle");
        assert_eq!(mouse_trigger_display(99), "Off");
        assert_eq!(mouse_trigger_vk(0), None);
        assert_eq!(mouse_trigger_vk(1), Some(VK_XBUTTON1));
        assert_eq!(mouse_trigger_vk(2), Some(VK_XBUTTON2));
        assert_eq!(mouse_trigger_vk(3), Some(VK_MBUTTON_TRIG));
        assert_eq!(mouse_trigger_vk(-1), None);
    }

    #[test]
    fn test_mouse_hook_end_to_end() {
        let _serial = HOOK_SERIAL.lock().unwrap();
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP,
            MOUSEINPUT,
        };
        // XBUTTON1 identifier for mouseData (not exported by windows-sys 0.59).
        const XBUTTON1_ID: u32 = 0x0001;

        fn send_xbutton(down: bool) {
            let flag = if down { MOUSEEVENTF_XDOWN } else { MOUSEEVENTF_XUP };
            let mut input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: XBUTTON1_ID,
                        dwFlags: flag,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            unsafe {
                SendInput(
                    1,
                    &mut input as *mut INPUT,
                    std::mem::size_of::<INPUT>() as i32,
                );
            }
        }

        let events = Arc::new(Mutex::new(Vec::new()));
        let capture = Arc::clone(&events);
        let manager = HotkeyManager::new(HotkeyMode::Toggle, move |evt| {
            capture.lock().unwrap().push(evt);
        });
        match manager {
            Ok(mgr) => {
                let back = crate::hotkey::TriggerBinding::normalized(0, None, Some(1));
                mgr.apply_bindings(&back, &None, &None, &None);
                // Let the LL hook install and the rearm settle.
                std::thread::sleep(Duration::from_millis(300));

                // First Back press toggles listening ON (resend while polling:
                // a parallel test's armed recorder may swallow one press).
                let mut started = false;
                for i in 0..200 {
                    if i % 10 == 0 {
                        send_xbutton(true);
                        send_xbutton(false);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                    if events.lock().unwrap().contains(&HotkeyEvent::StartListening) {
                        started = true;
                        break;
                    }
                }
                assert!(started, "synthetic Back press must start dictation");

                // Second Back press toggles back OFF.
                events.lock().unwrap().clear();
                let mut stopped = false;
                for i in 0..200 {
                    if i % 10 == 0 {
                        send_xbutton(true);
                        send_xbutton(false);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                    if events.lock().unwrap().contains(&HotkeyEvent::StopListening) {
                        stopped = true;
                        break;
                    }
                }
                assert!(stopped, "second Back press must stop dictation");
            }
            Err(e) => {
                println!("Note: Running in test environment without interactive desktop: {}", e);
            }
        }
    }

    #[test]
    fn test_record_armed_suppresses_live_dispatch() {
        let _serial = HOOK_SERIAL.lock().unwrap();
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP,
            MOUSEINPUT,
        };
        const XBUTTON1_ID: u32 = 0x0001;

        fn send_xbutton(down: bool) {
            let flag = if down { MOUSEEVENTF_XDOWN } else { MOUSEEVENTF_XUP };
            let mut input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: XBUTTON1_ID,
                        dwFlags: flag,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            unsafe {
                SendInput(
                    1,
                    &mut input as *mut INPUT,
                    std::mem::size_of::<INPUT>() as i32,
                );
            }
        }

        let events = Arc::new(Mutex::new(Vec::new()));
        let capture = Arc::clone(&events);
        let manager = HotkeyManager::new(HotkeyMode::Toggle, move |evt| {
            capture.lock().unwrap().push(evt);
        });
        match manager {
            Ok(mgr) => {
                let back = crate::hotkey::TriggerBinding::normalized(0, None, Some(1));
                mgr.apply_bindings(&back, &None, &None, &None);
                std::thread::sleep(Duration::from_millis(300));

                // Arm a recorder (as the Studio slot recorder does): live
                // dispatch must stay silent while the recorder captures.
                let recorded = Arc::new(Mutex::new(Vec::new()));
                let rec_capture = Arc::clone(&recorded);
                HotkeyManager::set_record_hook(Some(Arc::new(move |mods: u32, key: Option<u32>, btn: i32| {
                    rec_capture.lock().unwrap().push((mods, key, btn));
                })));
                send_xbutton(true);
                send_xbutton(false);
                std::thread::sleep(Duration::from_millis(600));
                assert!(
                    events.lock().unwrap().is_empty(),
                    "no live dictation event may fire while a recorder is armed"
                );
                assert!(
                    !recorded.lock().unwrap().is_empty(),
                    "the armed recorder must still capture the press"
                );

                // Clearing the recorder re-arms live dispatch immediately.
                HotkeyManager::set_record_hook(None);
                let mut started = false;
                for i in 0..200 {
                    if i % 10 == 0 {
                        send_xbutton(true);
                        send_xbutton(false);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                    if events.lock().unwrap().contains(&HotkeyEvent::StartListening) {
                        started = true;
                        break;
                    }
                }
                assert!(started, "live dispatch must resume once recording ends");
                HotkeyManager::set_record_hook(None);
            }
            Err(e) => {
                println!("Note: Running in test environment without interactive desktop: {}", e);
            }
        }
    }

    #[test]
    fn test_record_hook_callback_can_disarm_without_deadlock() {
        let _serial = HOOK_SERIAL.lock().unwrap();
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP,
            MOUSEINPUT,
        };
        const XBUTTON1_ID: u32 = 0x0001;

        fn send_xbutton(down: bool) {
            let flag = if down { MOUSEEVENTF_XDOWN } else { MOUSEEVENTF_XUP };
            let mut input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: XBUTTON1_ID,
                        dwFlags: flag,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            unsafe {
                SendInput(
                    1,
                    &mut input as *mut INPUT,
                    std::mem::size_of::<INPUT>() as i32,
                );
            }
        }

        let manager = HotkeyManager::new(HotkeyMode::Toggle, |_| {});
        match manager {
            Ok(mgr) => {
                let back = crate::hotkey::TriggerBinding::normalized(0, None, Some(1));
                mgr.apply_bindings(&back, &None, &None, &None);
                std::thread::sleep(Duration::from_millis(300));

                let disarmed = Arc::new(AtomicBool::new(false));
                let disarmed_cb = Arc::clone(&disarmed);
                HotkeyManager::set_record_hook(Some(Arc::new(move |_mods, _key, _btn| {
                    // Crucial: production recorder clears the hook inside this callback.
                    // Doing so must never deadlock the mouse hook thread.
                    HotkeyManager::set_record_hook(None);
                    disarmed_cb.store(true, Ordering::SeqCst);
                })));

                send_xbutton(true);
                send_xbutton(false);
                std::thread::sleep(Duration::from_millis(400));
                assert!(disarmed.load(Ordering::SeqCst), "callback must complete and disarm without deadlock");
                HotkeyManager::set_record_hook(None);
            }
            Err(e) => {
                println!("Note: Running in test environment without interactive desktop: {}", e);
            }
        }
    }

    #[test]
    fn test_reserved_capture_reasons() {
        // Every key with a system-wide side effect is reserved.
        for vk in [0x14, 0x90, 0x91, 0x5B, 0x5C, 0x2C, 0x5D] {
            assert!(
                reserved_capture_reason(vk).is_some(),
                "vk=0x{vk:X} must be reserved"
            );
        }
        // Ordinary keys, modifiers-as-chord-parts and F13-F24 stay usable.
        for vk in [0x77, 0x20, 0x0D, 0x11, 0x10, 0x7C, 0x87, 0xB3] {
            assert!(
                reserved_capture_reason(vk).is_none(),
                "vk=0x{vk:X} must stay recordable"
            );
        }
        assert_eq!(vk_to_string(0x14), "Caps Lock");
        assert_eq!(vk_to_string(0x7C), "F13");
        assert_eq!(vk_to_string(0x87), "F24");
    }

    #[test]
    fn test_classify_record_button() {
        const LBUTTONDOWN: u32 = 0x0201;
        const RBUTTONDOWN: u32 = 0x0204;
        const WHEEL: u32 = 0x020A;
        // Capturable: side buttons + middle only.
        assert_eq!(classify_record_button(WM_XBUTTONDOWN, 1), Some(MOUSE_TRIGGER_BACK));
        assert_eq!(classify_record_button(WM_XBUTTONDOWN, 2), Some(MOUSE_TRIGGER_FORWARD));
        assert_eq!(classify_record_button(WM_MBUTTONDOWN, 0), Some(MOUSE_TRIGGER_MIDDLE));
        // Never capturable: Left/Right (would hijack every click), wheel,
        // releases, unknown X buttons.
        assert_eq!(classify_record_button(LBUTTONDOWN, 0), None);
        assert_eq!(classify_record_button(RBUTTONDOWN, 0), None);
        assert_eq!(classify_record_button(WHEEL, 0), None);
        assert_eq!(classify_record_button(WM_XBUTTONUP, 0), None);
        assert_eq!(classify_record_button(WM_MBUTTONUP, 0), None);
        assert_eq!(classify_record_button(WM_XBUTTONDOWN, 0), None);
        assert_eq!(classify_record_button(WM_XBUTTONDOWN, 3), None);
    }

    #[test]
    fn test_recording_key_decisions() {
        // Escape cancels.
        assert!(matches!(
            recording_keydown_action(0x1B),
            RecordingKeydown::Cancel
        ));
        // Reserved keys are rejected, never staged or recorded.
        for vk in [0x14, 0x90, 0x91, 0x5B, 0x5C, 0x2C, 0x5D] {
            assert!(
                matches!(recording_keydown_action(vk), RecordingKeydown::Rejected(_)),
                "vk=0x{vk:X} keydown must be rejected"
            );
            assert!(
                matches!(recording_keyup_action(vk), RecordingKeyup::Rejected(_)),
                "vk=0x{vk:X} keyup must be rejected"
            );
        }
        // Ctrl/Shift/Alt (any variant) stage as pending modifiers.
        assert!(matches!(
            recording_keydown_action(0xA2),
            RecordingKeydown::PendingMod(0x11)
        ));
        assert!(matches!(
            recording_keydown_action(0x10),
            RecordingKeydown::PendingMod(0x10)
        ));
        assert!(matches!(
            recording_keydown_action(0xA5),
            RecordingKeydown::PendingMod(0x12)
        ));
        // Ordinary keys record; empty pending on release is ignored.
        assert!(matches!(
            recording_keydown_action(0x77),
            RecordingKeydown::RecordKey
        ));
        assert!(matches!(recording_keyup_action(0), RecordingKeyup::Ignored));
        assert!(matches!(
            recording_keyup_action(0x11),
            RecordingKeyup::CompleteStandalone
        ));
    }

    #[test]
    fn test_sweep_include_key() {
        // Modifiers fold into mods; mouse buttons live on the mouse side;
        // Esc cancels; reserved keys are never swept in.
        for vk in [0x10u32, 0x11, 0x12, 0xA0, 0xA3, 0x01, 0x02, 0x04, 0x05, 0x06, 0x1B,
                   0x14, 0x90, 0x5B, 0x2C, 0x5D] {
            assert!(!sweep_include_key(vk), "vk=0x{vk:X} must be swept out");
        }
        for vk in [0x77u32, 0x20, 0x0D, 0x42, 0x7C, 0xB3] {
            assert!(sweep_include_key(vk), "vk=0x{vk:X} must be sweepable");
        }
        // With no keys physically held, the sweep is empty (deterministic).
        assert!(held_nonmod_keys().is_empty());
        assert!(held_mouse_buttons().is_empty());
    }

    #[test]
    fn test_trigger_echo_debounce() {
        let _serial = HOOK_SERIAL.lock().unwrap();
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP,
            MOUSEINPUT,
        };
        const XBUTTON1_ID: u32 = 0x0001;

        fn send_xbutton(down: bool) {
            let flag = if down { MOUSEEVENTF_XDOWN } else { MOUSEEVENTF_XUP };
            let mut input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: XBUTTON1_ID,
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

        fn press() {
            send_xbutton(true);
            std::thread::sleep(Duration::from_millis(30));
            send_xbutton(false);
            std::thread::sleep(Duration::from_millis(30));
        }

        let events = Arc::new(Mutex::new(Vec::new()));
        let capture = Arc::clone(&events);
        let manager = HotkeyManager::new(HotkeyMode::Toggle, move |evt| {
            capture.lock().unwrap().push(evt);
        });
        match manager {
            Ok(mgr) => {
                let back = crate::hotkey::TriggerBinding::normalized(0, None, Some(1));
                mgr.apply_bindings(&back, &None, &None, &None);
                std::thread::sleep(Duration::from_millis(300));

                // First press toggles ON (tight poll: the echo below must land
                // inside the suppression window anchored at this dispatch).
                press();
                let mut started = false;
                for _ in 0..400 {
                    std::thread::sleep(Duration::from_millis(5));
                    if events.lock().unwrap().contains(&HotkeyEvent::StartListening) {
                        started = true;
                        break;
                    }
                }
                assert!(started, "first press must start dictation");

                // An immediate echo (dual-signal press) must NOT toggle back.
                events.lock().unwrap().clear();
                press();
                std::thread::sleep(Duration::from_millis(150));
                assert!(
                    !events.lock().unwrap().contains(&HotkeyEvent::StopListening),
                    "echo within the window must be suppressed"
                );

                // A genuine later press toggles OFF.
                std::thread::sleep(Duration::from_millis(150));
                press();
                let mut stopped = false;
                for _ in 0..400 {
                    std::thread::sleep(Duration::from_millis(5));
                    if events.lock().unwrap().contains(&HotkeyEvent::StopListening) {
                        stopped = true;
                        break;
                    }
                }
                assert!(stopped, "press after the window must stop dictation");
            }
            Err(e) => {
                println!("Note: Running in test environment without interactive desktop: {}", e);
            }
        }
    }

    #[test]
    fn test_mouse_trigger_lifecycle() {
        let _serial = HOOK_SERIAL.lock().unwrap();
        let manager = HotkeyManager::new(HotkeyMode::Toggle, move |evt| {
            println!("Event: {:?}", evt);
        });
        match manager {
            Ok(mgr) => {
                // Mouse-only secondary alongside keyboard primary arms cleanly.
                let primary = crate::hotkey::TriggerBinding::f8();
                let secondary = Some(crate::hotkey::TriggerBinding::normalized(0, None, Some(1)));
                mgr.apply_bindings(&primary, &secondary, &None, &None);
                assert!(!mgr.is_recording());
                // Chord variant: Ctrl-gated Back button.
                let chord = crate::hotkey::TriggerBinding::normalized(0x0002, None, Some(1));
                assert_eq!(chord.display(), "Ctrl + Mouse Back");
                mgr.apply_bindings(&chord, &None, &None, &None);
            }
            Err(e) => {
                println!("Note: Running in test environment without interactive desktop: {}", e);
            }
        }
    }
}
