// desktop/src/startup.rs
//! Windows Startup Registry integration for Sotto.
//!
//! Automatically registers or unregisters Sotto in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
//! with the `--startup` flag so it boots cleanly into background tray without popping the studio suite.

#[cfg(target_os = "windows")]
fn encode_wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Checks if Sotto is registered to launch on Windows startup in HKCU Run.
pub fn is_startup_enabled() -> bool {
    #[cfg(target_os = "windows")]
    unsafe {
        use windows_sys::Win32::System::Registry::{
            RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
        };

        let subkey = encode_wide(r"Software\Microsoft\Windows\CurrentVersion\Run");
        let val_name = encode_wide("Sotto");
        let mut hkey: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey) != 0 {
            return false;
        }

        let mut val_type: u32 = 0;
        let mut data_len: u32 = 0;
        let query_res = RegQueryValueExW(
            hkey,
            val_name.as_ptr(),
            std::ptr::null_mut(),
            &mut val_type,
            std::ptr::null_mut(),
            &mut data_len,
        );
        RegCloseKey(hkey);
        query_res == 0 && data_len > 0
    }
    #[cfg(not(target_os = "windows"))]
    false
}

/// Sets or removes Sotto's launch-on-startup registry entry in HKCU Run.
pub fn set_startup_enabled(enabled: bool) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    unsafe {
        use windows_sys::Win32::System::Registry::{
            RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
            KEY_WRITE, REG_SZ,
        };

        let subkey = encode_wide(r"Software\Microsoft\Windows\CurrentVersion\Run");
        let val_name = encode_wide("Sotto");
        let mut hkey: HKEY = std::ptr::null_mut();
        let open_res = RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_WRITE, &mut hkey);
        if open_res != 0 {
            return Err(format!("Failed to open Run registry key: error code {}", open_res));
        }

        let res = if enabled {
            let exe_path = std::env::current_exe()
                .map_err(|e| format!("Failed to get current executable path: {}", e))?;
            let cmd = format!("\"{}\" --startup", exe_path.to_string_lossy());
            let wide_cmd = encode_wide(&cmd);
            let bytes_len = (wide_cmd.len() * std::mem::size_of::<u16>()) as u32;
            let set_res = RegSetValueExW(
                hkey,
                val_name.as_ptr(),
                0,
                REG_SZ,
                wide_cmd.as_ptr() as *const u8,
                bytes_len,
            );
            if set_res == 0 {
                println!("[Startup] Successfully registered in HKCU Run: {}", cmd);
                Ok(())
            } else {
                Err(format!("Failed to write registry value: error code {}", set_res))
            }
        } else {
            let del_res = RegDeleteValueW(hkey, val_name.as_ptr());
            // Error code 2 = ERROR_FILE_NOT_FOUND (already unregistered)
            if del_res == 0 || del_res == 2 {
                println!("[Startup] Successfully unregistered from HKCU Run");
                Ok(())
            } else {
                Err(format!("Failed to delete registry value: error code {}", del_res))
            }
        };

        RegCloseKey(hkey);
        res
    }
    #[cfg(not(target_os = "windows"))]
    Ok(())
}
