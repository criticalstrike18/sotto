// desktop/src/inject/mod.rs

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
pub use windows::*;

#[cfg(not(target_os = "windows"))]
pub fn inject_text(_text: &str) -> Result<(), String> {
    Err("Text injection not yet implemented on non-Windows platforms".to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn copy_text_to_clipboard(_text: &str) -> Result<(), String> {
    Err("Clipboard copy is not implemented on non-Windows platforms".to_string())
}
