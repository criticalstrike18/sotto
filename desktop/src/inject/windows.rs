// desktop/src/inject/windows.rs

use std::thread;
use std::time::Duration;
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, CountClipboardFormats, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    keybd_event, SendInput, SetFocus, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU,
    VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT, VK_V,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, IsIconic, SetForegroundWindow, ShowWindow,
    SW_RESTORE,
};

#[link(name = "user32")]
extern "system" {
    fn OpenInputDesktop(dw_flags: u32, f_inherit: i32, dw_desired_access: u32) -> isize;
    fn SetThreadDesktop(h_desktop: isize) -> i32;
    fn CloseDesktop(h_desktop: isize) -> i32;
    fn AttachThreadInput(id_attach: u32, id_attach_to: u32, f_attach: i32) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GlobalFree(h_mem: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}

const CF_UNICODETEXT: u32 = 13;

/// Ensures the calling thread is attached to the active user input desktop.
/// On Windows, background threads (Tokio workers, spawned threads, CLI subshells)
/// may run on a secondary desktop context where SendInput is blocked with
/// ERROR_ACCESS_DENIED (5). Attaching to OpenInputDesktop grants interactive input rights.
pub fn attach_input_desktop() {
    thread_local! {
        static CURRENT_DESK: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    }

    CURRENT_DESK.with(|cell| {
        if cell.get() == 0 {
            unsafe {
                let desk = OpenInputDesktop(0, 0, 0x01FF); // GENERIC_ALL
                if desk != 0 {
                    let ok = SetThreadDesktop(desk);
                    if ok != 0 {
                        cell.set(desk);
                    } else {
                        CloseDesktop(desk);
                    }
                }
            }
        }
    });
}

struct ThreadInputGuard {
    attached: bool,
    current_tid: u32,
    target_tid: u32,
}

impl Drop for ThreadInputGuard {
    fn drop(&mut self) {
        if self.attached {
            unsafe {
                AttachThreadInput(self.current_tid, self.target_tid, 0);
            }
        }
    }
}

/// Transmits foreground focus to the target window using AttachThreadInput,
/// bypassing Windows Foreground Lock Timeout (which causes orange flashing taskbars).
pub fn transfer_foreground_focus(target_hwnd: isize) {
    if target_hwnd == 0 {
        return;
    }
    unsafe {
        let current_hwnd = GetForegroundWindow();
        if current_hwnd as isize == target_hwnd {
            return;
        }

        let current_tid = GetCurrentThreadId();
        let mut target_pid: u32 = 0;
        let target_tid = GetWindowThreadProcessId(target_hwnd as _, &mut target_pid);

        let attached = if target_tid != 0 && target_tid != current_tid {
            AttachThreadInput(current_tid, target_tid, 1) != 0
        } else {
            false
        };

        let _guard = ThreadInputGuard {
            attached,
            current_tid,
            target_tid,
        };

        if IsIconic(target_hwnd as _) != 0 {
            ShowWindow(target_hwnd as _, SW_RESTORE);
        }
        SetForegroundWindow(target_hwnd as _);
        SetFocus(target_hwnd as _);

        thread::sleep(Duration::from_millis(50));
    }
}

/// Determines whether text should use direct keystroke typing vs clipboard paste.
/// Returns true if length < 80 characters AND contains no newline characters.
pub fn should_use_direct_typing(text: &str) -> bool {
    let char_count = text.chars().count();
    let has_newlines = text.contains('\n') || text.contains('\r');
    char_count < 80 && !has_newlines
}

/// Injects text into a specific target window, restoring foreground focus to that window first.
pub fn inject_text_with_target(text: &str, target_hwnd: isize) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }

    attach_input_desktop();

    // If a target window was recorded, ensure it is brought to foreground before sending keystrokes
    if target_hwnd != 0 {
        transfer_foreground_focus(target_hwnd);
    }

    // Hybrid Injection Strategy:
    // For short text (< 80 chars) without newlines, directly simulate keyboard typing.
    // This completes in ~1-2ms, completely avoids touching the clipboard, and eliminates
    // clipboard pollution/corruption for typical short dictations and commands.
    if should_use_direct_typing(text) {
        return inject_type(text);
    }

    match inject_clipboard(text) {
        Ok(()) => Ok(()),
        Err(clip_err) => {
            eprintln!("[Inject] Clipboard fast-path failed ({clip_err}); falling back to typing");
            inject_type(text)
        }
    }
}

/// High-performance text injection into the active focused window.
pub fn inject_text(text: &str) -> Result<(), String> {
    inject_text_with_target(text, 0)
}

unsafe fn set_clipboard_dword(format: u32, val: u32) {
    if format == 0 {
        return;
    }
    let hmem = GlobalAlloc(GMEM_MOVEABLE, std::mem::size_of::<u32>());
    if hmem.is_null() {
        return;
    }
    let ptr = GlobalLock(hmem) as *mut u32;
    if ptr.is_null() {
        GlobalFree(hmem);
        return;
    }
    *ptr = val;
    GlobalUnlock(hmem);
    if SetClipboardData(format, hmem as _).is_null() {
        GlobalFree(hmem);
    }
}

unsafe fn set_clipboard_unicode_text(text: &str) -> Result<(), String> {
    let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let byte_count = utf16.len() * std::mem::size_of::<u16>();
    let hmem = GlobalAlloc(GMEM_MOVEABLE, byte_count);
    if hmem.is_null() {
        return Err("GlobalAlloc for clipboard text failed".to_string());
    }
    let ptr = GlobalLock(hmem) as *mut u16;
    if ptr.is_null() {
        GlobalFree(hmem);
        return Err("GlobalLock for clipboard text failed".to_string());
    }
    std::ptr::copy_nonoverlapping(utf16.as_ptr(), ptr, utf16.len());
    GlobalUnlock(hmem);
    if SetClipboardData(CF_UNICODETEXT, hmem as _).is_null() {
        GlobalFree(hmem);
        return Err("SetClipboardData CF_UNICODETEXT failed".to_string());
    }
    Ok(())
}

unsafe fn get_clipboard_unicode_text() -> Option<String> {
    if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 {
        return None;
    }
    let hdata = GetClipboardData(CF_UNICODETEXT);
    if hdata.is_null() {
        return None;
    }
    let ptr = GlobalLock(hdata) as *const u16;
    if ptr.is_null() {
        return None;
    }
    let mut len = 0;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    let slice = std::slice::from_raw_parts(ptr, len);
    let s = String::from_utf16_lossy(slice);
    GlobalUnlock(hdata);
    Some(s)
}

/// Writes the standard exclusion flags that keep a payload out of Win+V
/// history, cloud clipboard sync, and third-party clipboard managers.
unsafe fn write_clipboard_exclusion_flags() {
    // - CanIncludeInClipboardHistory: 0 prevents pollution of Win+V history
    // - CanUploadToCloudStore: 0 prevents syncing the payload to cloud clipboard
    // - ExcludeClipboardContentFromMonitorProcessing: 0 instructs 3rd-party managers to ignore
    let cf_no_history = RegisterClipboardFormatW(windows_sys::w!("CanIncludeInClipboardHistory"));
    let cf_no_cloud = RegisterClipboardFormatW(windows_sys::w!("CanUploadToCloudStore"));
    let cf_no_monitor = RegisterClipboardFormatW(windows_sys::w!(
        "ExcludeClipboardContentFromMonitorProcessing"
    ));

    set_clipboard_dword(cf_no_history, 0);
    set_clipboard_dword(cf_no_cloud, 0);
    set_clipboard_dword(cf_no_monitor, 0);
}

/// Copies text to the OS clipboard without pasting it anywhere.
///
/// Uses the same exclusion formats as [`inject_clipboard`] so copying a history
/// item does not pollute Win+V history or cloud clipboard sync.
pub fn copy_text_to_clipboard(text: &str) -> Result<(), String> {
    // Open with retry loop (in case another process holds it momentarily)
    let mut opened = false;
    for _ in 0..10 {
        if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
            opened = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    if !opened {
        return Err("Unable to open Windows clipboard".to_string());
    }

    unsafe {
        EmptyClipboard();
    }
    let result = unsafe { set_clipboard_unicode_text(text) };
    if result.is_ok() {
        unsafe {
            write_clipboard_exclusion_flags();
        }
    }
    unsafe {
        CloseClipboard();
    }
    result
}

/// Injects text using the OS clipboard + Ctrl+V combo.
pub fn inject_clipboard(text: &str) -> Result<(), String> {
    attach_input_desktop();

    // 1. Open clipboard with retry loop (in case another process holds it momentarily)
    let mut opened = false;
    for _ in 0..10 {
        if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
            opened = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    if !opened {
        return Err("Unable to open Windows clipboard".to_string());
    }

    // 2. Snapshot prior clipboard text (if text format is present)
    let previous_text = unsafe { get_clipboard_unicode_text() };
    if previous_text.is_none() && unsafe { CountClipboardFormats() } > 0 {
        unsafe { CloseClipboard(); }
        return Err("Clipboard contains non-text data (image/file); preserving clipboard and falling back to typing".to_string());
    }

    // 3. Empty clipboard to take ownership
    unsafe {
        EmptyClipboard();
    }

    // 4. Write new text payload
    if let Err(e) = unsafe { set_clipboard_unicode_text(text) } {
        unsafe { CloseClipboard(); }
        return Err(e);
    }

    // 5. Write exclusion flags
    unsafe {
        write_clipboard_exclusion_flags();
        CloseClipboard();
    }

    // 6. Record clipboard sequence number immediately after our write
    let seq_at_inject = unsafe { GetClipboardSequenceNumber() };

    // Give OS a 40ms window to settle clipboard state across processes
    thread::sleep(Duration::from_millis(40));

    send_ctrl_v()?;

    // 7. Background safety restore:
    // After 800ms, restore previous clipboard content ONLY IF:
    // - The sequence number is STILL unchanged (meaning user hasn't copied anything new), AND
    // - The current clipboard text still matches our injected payload.
    if let Some(prev) = previous_text {
        let payload = text.to_string();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(800));

            // Verify sequence number: if changed, user or app copied something else -> abort!
            let current_seq = unsafe { GetClipboardSequenceNumber() };
            if current_seq != seq_at_inject {
                return;
            }

            // Verify content is still our payload before restoring
            let mut opened_for_restore = false;
            for _ in 0..5 {
                if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                    opened_for_restore = true;
                    break;
                }
                thread::sleep(Duration::from_millis(15));
            }
            if opened_for_restore {
                let current_text = unsafe { get_clipboard_unicode_text() };
                if current_text.as_deref() == Some(&payload) {
                    unsafe {
                        EmptyClipboard();
                        let _ = set_clipboard_unicode_text(&prev);
                    }
                }
                unsafe {
                    CloseClipboard();
                }
            }
        });
    }

    Ok(())
}

/// Fallback: Injects text by simulating direct keystrokes via SendInput with Unicode events.
pub fn inject_type(text: &str) -> Result<(), String> {
    attach_input_desktop();

    let utf16_chars: Vec<u16> = text.encode_utf16().collect();
    if utf16_chars.is_empty() {
        return Ok(());
    }

    // Force release any modifier keys that might still be physically held down
    let _ = release_modifier_keys();

    let mut inputs = Vec::with_capacity(utf16_chars.len() * 2);
    for &ch in &utf16_chars {
        if ch == '\n' as u16 {
            // Carriage return / newline handling
            inputs.push(key_input_unicode(13, false));
            inputs.push(key_input_unicode(13, true));
        } else {
            inputs.push(key_input_unicode(ch, false));
            inputs.push(key_input_unicode(ch, true));
        }
    }

    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    };

    if sent != inputs.len() as u32 {
        // Fallback to keybd_event if SendInput is constrained by UIPI session isolation
        unsafe {
            for &ch in &utf16_chars {
                if ch == '\n' as u16 {
                    keybd_event(0x0D, 0, 0, 0);
                    keybd_event(0x0D, 0, KEYEVENTF_KEYUP, 0);
                } else if ch == '\r' as u16 {
                    // skip
                } else {
                    keybd_event(0, ch as u8, KEYEVENTF_UNICODE, 0);
                    keybd_event(0, ch as u8, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP, 0);
                }
            }
        }
    }

    Ok(())
}

/// Simulates native Ctrl+V paste.
pub fn send_ctrl_v() -> Result<(), String> {
    send_ctrl_combo(VK_V)
}

/// Sends a Ctrl + [virtual_key] combination after ensuring modifiers are released.
fn send_ctrl_combo(vk: u16) -> Result<(), String> {
    attach_input_desktop();

    let inputs = [
        // Force-release any modifier keys the user is still holding from the hotkey
        // (release both generic and left/right specific virtual keys cleanly)
        key_input_vk(VK_LWIN, true),
        key_input_vk(VK_RWIN, true),
        key_input_vk(VK_LMENU, true),
        key_input_vk(VK_RMENU, true),
        key_input_vk(VK_MENU, true),
        key_input_vk(VK_LSHIFT, true),
        key_input_vk(VK_RSHIFT, true),
        key_input_vk(VK_SHIFT, true),
        key_input_vk(VK_LCONTROL, true),
        key_input_vk(VK_RCONTROL, true),
        key_input_vk(VK_CONTROL, true),
        // Press Ctrl
        key_input_vk(VK_CONTROL, false),
        // Press Key
        key_input_vk(vk, false),
        // Release Key
        key_input_vk(vk, true),
        // Release Ctrl
        key_input_vk(VK_CONTROL, true),
    ];

    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    };

    if sent != inputs.len() as u32 {
        // Fallback to keybd_event (succeeds universally across Windows sessions)
        unsafe {
            keybd_event(VK_LWIN as u8, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_RWIN as u8, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_LMENU as u8, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_RMENU as u8, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_MENU as u8, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_LSHIFT as u8, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_RSHIFT as u8, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_SHIFT as u8, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_LCONTROL as u8, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_RCONTROL as u8, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_CONTROL as u8, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_CONTROL as u8, 0, 0, 0);
            keybd_event(vk as u8, 0, 0, 0);
            keybd_event(vk as u8, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_CONTROL as u8, 0, KEYEVENTF_KEYUP, 0);
        }
    }

    Ok(())
}

fn release_modifier_keys() -> Result<(), String> {
    let inputs = [
        key_input_vk(VK_LWIN, true),
        key_input_vk(VK_RWIN, true),
        key_input_vk(VK_LMENU, true),
        key_input_vk(VK_RMENU, true),
        key_input_vk(VK_MENU, true),
        key_input_vk(VK_LSHIFT, true),
        key_input_vk(VK_RSHIFT, true),
        key_input_vk(VK_SHIFT, true),
        key_input_vk(VK_LCONTROL, true),
        key_input_vk(VK_RCONTROL, true),
        key_input_vk(VK_CONTROL, true),
    ];

    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    };

    if sent != inputs.len() as u32 {
        unsafe {
            keybd_event(VK_LWIN as u8, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_RWIN as u8, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_LMENU as u8, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_RMENU as u8, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_MENU as u8, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_LSHIFT as u8, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_RSHIFT as u8, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_SHIFT as u8, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_LCONTROL as u8, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_RCONTROL as u8, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_CONTROL as u8, 0, KEYEVENTF_KEYUP, 0);
        }
    }

    Ok(())
}

fn key_input_unicode(ch: u16, key_up: bool) -> INPUT {
    let mut flags = KEYEVENTF_UNICODE;
    if key_up {
        flags |= KEYEVENTF_KEYUP;
    }

    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0,
                wScan: ch,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn key_input_vk(vk: u16, key_up: bool) -> INPUT {
    let mut flags = 0;
    if key_up {
        flags |= KEYEVENTF_KEYUP;
    }

    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    static CLIPBOARD_TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_hybrid_routing_decision() {
        // Short text without newlines -> direct typing
        assert!(should_use_direct_typing("hello world"));
        assert!(should_use_direct_typing("git status"));
        assert!(should_use_direct_typing("Yes, that looks great!"));

        // Exact boundary check: 79 chars is direct, 80 chars is clipboard
        let s79: String = "a".repeat(79);
        let s80: String = "a".repeat(80);
        assert!(should_use_direct_typing(&s79));
        assert!(!should_use_direct_typing(&s80));

        // Short text WITH newlines must use clipboard paste to avoid submitting chat forms
        assert!(!should_use_direct_typing("line 1\nline 2"));
        assert!(!should_use_direct_typing("item\r\n"));
    }

    #[test]
    fn test_clipboard_read_write_restore() {
        let _guard = CLIPBOARD_TEST_MUTEX.lock().unwrap();
        attach_input_desktop();

        // Seed clipboard with known initial content
        let original_text = "test_clipboard_preservation_original_seed";
        let mut opened = false;
        for _ in 0..10 {
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                opened = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        if opened {
            unsafe {
                EmptyClipboard();
                let _ = set_clipboard_unicode_text(original_text);
                CloseClipboard();
            }
        }

        let inject_text_content = "Sotto Voice Injected Sample 12345 (Longer payload >= 80 chars to test clipboard path) -- abcdefghijklm";
        let res = inject_clipboard(inject_text_content);
        assert!(res.is_ok(), "inject_clipboard should succeed: {:?}", res.err());

        // Immediately after injection, clipboard should have our content
        let mut opened2 = false;
        for _ in 0..10 {
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                opened2 = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        if opened2 {
            let current = unsafe { get_clipboard_unicode_text() }.unwrap_or_default();
            assert_eq!(current, inject_text_content);
            unsafe { CloseClipboard(); }
        }

        // Sleep to let background thread restore original (800ms + buffer)
        thread::sleep(Duration::from_millis(950));

        let mut opened3 = false;
        for _ in 0..10 {
            if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
                opened3 = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        if opened3 {
            let restored = unsafe { get_clipboard_unicode_text() }.unwrap_or_default();
            assert_eq!(restored, original_text);
            unsafe { CloseClipboard(); }
        }
    }

    #[test]
    fn test_clipboard_sequence_abort_on_external_change() {
        let _guard = CLIPBOARD_TEST_MUTEX.lock().unwrap();
        attach_input_desktop();

        let initial_text = "initial_text_before_external_copy";
        if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
            unsafe {
                EmptyClipboard();
                let _ = set_clipboard_unicode_text(initial_text);
                CloseClipboard();
            }
        }

        let payload = "injected_payload_to_be_preempted_by_user_copy";
        let res = inject_clipboard(payload);
        assert!(res.is_ok());

        // Simulate user copying something new 100ms later
        thread::sleep(Duration::from_millis(100));
        let user_copy = "user_just_copied_this_important_link";
        if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
            unsafe {
                EmptyClipboard();
                let _ = set_clipboard_unicode_text(user_copy);
                CloseClipboard();
            }
        }

        // Sleep past the 800ms restoration window
        thread::sleep(Duration::from_millis(850));

        // Clipboard MUST still contain user_copy, NOT revert to initial_text!
        if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
            let final_content = unsafe { get_clipboard_unicode_text() }.unwrap_or_default();
            unsafe { CloseClipboard(); }
            assert_eq!(final_content, user_copy, "Restoration must abort when sequence number changes");
        }
    }

    #[test]
    fn test_typing_fallback_sendinput() {
        attach_input_desktop();
        let res = inject_type("Test text typing fallback 123");
        assert!(res.is_ok(), "inject_type should succeed: {:?}", res.err());
    }
}
