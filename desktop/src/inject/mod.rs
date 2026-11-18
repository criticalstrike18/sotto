// desktop/src/inject/mod.rs

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
pub use windows::*;

#[cfg(not(target_os = "windows"))]
pub fn attach_input_desktop() {}

#[cfg(not(target_os = "windows"))]
pub fn transfer_foreground_focus(_target_hwnd: isize) {}

#[cfg(not(target_os = "windows"))]
pub fn should_use_direct_typing(_text: &str) -> bool {
    false
}

#[cfg(not(target_os = "windows"))]
pub fn inject_text_with_target(text: &str, _target_hwnd: isize) -> Result<(), String> {
    inject_text(text)
}

#[cfg(not(target_os = "windows"))]
pub fn inject_text(_text: &str) -> Result<(), String> {
    Err("Text injection not yet implemented on non-Windows platforms".to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn copy_text_to_clipboard(_text: &str) -> Result<(), String> {
    Err("Clipboard copy is not implemented on non-Windows platforms".to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn inject_clipboard(_text: &str) -> Result<(), String> {
    Err("Clipboard injection not yet implemented on non-Windows platforms".to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn inject_type(_text: &str) -> Result<(), String> {
    Err("Direct typing not yet implemented on non-Windows platforms".to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn send_ctrl_v() -> Result<(), String> {
    Err("Send Ctrl+V not yet implemented on non-Windows platforms".to_string())
}
