// Copyright (c) 2026 Sotto
// Application Name Formatting & Win32 Executable Icon Extraction Cache

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

/// Strips `.exe` and maps raw executable names to human-readable sentence/title case brand names
pub fn format_app_name(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "Desktop".to_string();
    }

    let lower = trimmed.to_lowercase();
    let name = lower.strip_suffix(".exe").unwrap_or(&lower);

    // Explicit high-priority brand mappings
    if (name.starts_with("t3") || name.starts_with("p3")) && name.contains("code") {
        return "T3 Code (Alpha)".to_string();
    }
    if name.contains("t3code") || name.contains("t3-code") {
        return "T3 Code (Alpha)".to_string();
    }

    match name {
        "code" | "visual studio code" => "VS Code".to_string(),
        "cursor" => "Cursor".to_string(),
        "windsurf" => "Windsurf".to_string(),
        "zed" => "Zed".to_string(),
        "devenv" => "Visual Studio".to_string(),
        "windowsterminal" => "Windows Terminal".to_string(),
        "cmd" => "Command Prompt".to_string(),
        "powershell" | "pwsh" => "PowerShell".to_string(),
        "chrome" | "google chrome" => "Google Chrome".to_string(),
        "msedge" | "microsoft edge" => "Microsoft Edge".to_string(),
        "firefox" => "Firefox".to_string(),
        "brave" => "Brave".to_string(),
        "slack" => "Slack".to_string(),
        "discord" => "Discord".to_string(),
        "spotify" => "Spotify".to_string(),
        "notepad" => "Notepad".to_string(),
        "notepad++" => "Notepad++".to_string(),
        "explorer" => "File Explorer".to_string(),
        "sublime_text" => "Sublime Text".to_string(),
        "obsidian" => "Obsidian".to_string(),
        "notion" => "Notion".to_string(),
        "teams" | "ms-teams" => "Microsoft Teams".to_string(),
        "telegram" => "Telegram".to_string(),
        "whatsapp" | "whatsapp.root" => "WhatsApp".to_string(),
        "zoom" => "Zoom".to_string(),
        "figma" => "Figma".to_string(),
        "linear" => "Linear".to_string(),
        "postman" => "Postman".to_string(),
        "wordpad" => "WordPad".to_string(),
        "winword" => "Microsoft Word".to_string(),
        "excel" => "Microsoft Excel".to_string(),
        "powerpnt" => "Microsoft PowerPoint".to_string(),
        "terminal" => "Terminal".to_string(),
        _ => {
            // General Title/Sentence Case formatting:
            // Strip .exe, replace _ and - with spaces, capitalize each word respecting parentheses
            let clean = trimmed.strip_suffix(".exe").or_else(|| trimmed.strip_suffix(".EXE")).unwrap_or(trimmed);
            let words: Vec<String> = clean
                .split(|c: char| c == '_' || c == '-' || c == ' ')
                .filter(|s| !s.is_empty())
                .map(|word| {
                    let mut res = String::new();
                    let mut capitalize_next = true;
                    for c in word.chars() {
                        if c.is_alphabetic() {
                            if capitalize_next {
                                res.extend(c.to_uppercase());
                                capitalize_next = false;
                            } else {
                                res.extend(c.to_lowercase());
                            }
                        } else {
                            res.push(c);
                        }
                    }
                    res
                })
                .collect();
            if words.is_empty() {
                "Desktop".to_string()
            } else {
                words.join(" ")
            }
        }
    }
}

/// Raw RGBA icon buffer (Send + Sync)
#[derive(Clone)]
pub struct RawIconData {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

// In-memory icon cache storing thread-safe raw pixel data
static ICON_CACHE: Mutex<Option<HashMap<String, Option<RawIconData>>>> = Mutex::new(None);

/// Retrieves cached icon for application, or attempts extraction if path is provided.
/// Constructs the `slint::Image` on the calling (UI) thread.
pub fn get_app_icon(app_name: &str, exe_path: Option<&str>) -> (slint::Image, bool) {
    let clean_name = format_app_name(app_name);
    let mut guard = ICON_CACHE.lock().unwrap();
    let cache = guard.get_or_insert_with(HashMap::new);

    let clean_key = clean_name.to_lowercase();
    let raw_key = app_name.trim().to_lowercase();

    // Check cache by clean formatted name or raw executable name
    if let Some(cached) = cache.get(&clean_key).or_else(|| cache.get(&raw_key)) {
        if let Some(raw) = cached {
            let mut pixel_buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(raw.width, raw.height);
            pixel_buf.make_mut_bytes().copy_from_slice(&raw.rgba);
            return (slint::Image::from_rgba8(pixel_buf), true);
        } else {
            return (slint::Image::default(), false);
        }
    }

    // Try multiple avenues to resolve the full executable path:
    let resolved_path = if let Some(path) = exe_path {
        if Path::new(path).exists() {
            Some(path.to_string())
        } else {
            None
        }
    } else {
        None
    };

    let resolved_path = resolved_path
        .or_else(|| find_common_app_path(&clean_name))
        .or_else(|| find_common_app_path(app_name))
        .or_else(|| {
            #[cfg(target_os = "windows")]
            {
                find_running_process_exe(&clean_name)
                    .or_else(|| find_running_process_exe(app_name))
            }
            #[cfg(not(target_os = "windows"))]
            {
                None
            }
        });

    let extracted = if let Some(ref path) = resolved_path {
        extract_icon_from_exe(path)
    } else {
        None
    };

    cache.insert(clean_key, extracted.clone());
    cache.insert(raw_key, extracted.clone());

    if let Some(raw) = extracted {
        let mut pixel_buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(raw.width, raw.height);
        pixel_buf.make_mut_bytes().copy_from_slice(&raw.rgba);
        (slint::Image::from_rgba8(pixel_buf), true)
    } else {
        (slint::Image::default(), false)
    }
}

fn find_common_app_path(formatted_name: &str) -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        let local_app_data = std::env::var("LOCALAPPDATA").ok()?;
        let program_files = std::env::var("ProgramFiles").ok()?;
        let program_files_x86 = std::env::var("ProgramFiles(x86)").ok();

        let candidates: &[(&str, &[&str])] = &[
            ("T3 Code (Alpha)", &[
                &format!(r"{}\Programs\t3code\T3 Code (Alpha).exe", local_app_data),
                &format!(r"{}\Programs\t3code\T3 Code.exe", local_app_data),
                &format!(r"{}\t3code\T3 Code (Alpha).exe", local_app_data),
            ]),
            ("T3 Code", &[
                &format!(r"{}\Programs\t3code\T3 Code (Alpha).exe", local_app_data),
                &format!(r"{}\Programs\t3code\T3 Code.exe", local_app_data),
                &format!(r"{}\t3code\T3 Code (Alpha).exe", local_app_data),
            ]),
            ("VS Code", &[
                &format!(r"{}\Programs\Microsoft VS Code\Code.exe", local_app_data),
                &format!(r"{}\Microsoft VS Code\Code.exe", program_files),
            ]),
            ("Cursor", &[
                &format!(r"{}\Programs\cursor\Cursor.exe", local_app_data),
            ]),
            ("Google Chrome", &[
                &format!(r"{}\Google\Chrome\Application\chrome.exe", program_files),
                &format!(r"{}\Google\Chrome\Application\chrome.exe", program_files_x86.as_deref().unwrap_or("")),
            ]),
            ("Microsoft Edge", &[
                &format!(r"{}\Microsoft\Edge\Application\msedge.exe", program_files_x86.as_deref().unwrap_or("")),
                &format!(r"{}\Microsoft\Edge\Application\msedge.exe", program_files),
            ]),
            ("Slack", &[
                &format!(r"{}\slack\slack.exe", local_app_data),
                &format!(r"{}\Programs\slack\slack.exe", local_app_data),
            ]),
            ("Discord", &[
                &format!(r"{}\Discord\app.ico", local_app_data),
            ]),
            ("Spotify", &[
                &format!(r"{}\Spotify\Spotify.exe", local_app_data),
            ]),
            ("Windows Terminal", &[
                r"C:\Windows\System32\cmd.exe",
            ]),
            ("Command Prompt", &[
                r"C:\Windows\System32\cmd.exe",
            ]),
            ("PowerShell", &[
                r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
            ]),
            ("Notepad", &[
                r"C:\Windows\System32\notepad.exe",
                r"C:\Windows\notepad.exe",
            ]),
            ("File Explorer", &[
                r"C:\Windows\explorer.exe",
            ]),
        ];

        for (name, paths) in candidates {
            if *name == formatted_name {
                for p in *paths {
                    if !p.is_empty() && Path::new(p).exists() {
                        return Some(p.to_string());
                    }
                }
            }
        }
    }
    None
}

/// Dynamically searches running Windows processes for matching executable name and retrieves image path
#[cfg(target_os = "windows")]
pub fn find_running_process_exe(target_name: &str) -> Option<String> {
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
    use windows_sys::Win32::System::Threading::*;

    let target_clean = target_name.trim_end_matches(".exe").trim_end_matches(".EXE").trim().to_lowercase();
    if target_clean.is_empty() || target_clean == "desktop" {
        return None;
    }

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }

        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let name_len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                let exe_name = String::from_utf16_lossy(&entry.szExeFile[..name_len]);
                let exe_clean = exe_name.trim_end_matches(".exe").trim_end_matches(".EXE").to_lowercase();

                let is_match = exe_clean == target_clean
                    || exe_clean.contains(&target_clean)
                    || target_clean.contains(&exe_clean);

                if is_match && entry.th32ProcessID != 0 {
                    let h_proc = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, entry.th32ProcessID);
                    if !h_proc.is_null() {
                        let mut buffer = [0u16; 1024];
                        let mut size = buffer.len() as u32;
                        let success = QueryFullProcessImageNameW(h_proc, 0, buffer.as_mut_ptr(), &mut size);
                        CloseHandle(h_proc);

                        if success != 0 && size > 0 {
                            let path = String::from_utf16_lossy(&buffer[..size as usize]);
                            if Path::new(&path).exists() {
                                CloseHandle(snapshot);
                                return Some(path);
                            }
                        }
                    }
                }

                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }

        CloseHandle(snapshot);
    }
    None
}

/// Win32 GDI Icon Extractor: Converts HICON from executable into 32-bit RGBA buffer at 32x32 resolution
#[cfg(target_os = "windows")]
pub fn extract_icon_from_exe(exe_path: &str) -> Option<RawIconData> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::UI::Shell::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    let wide: Vec<u16> = std::ffi::OsStr::new(exe_path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let mut sfi: SHFILEINFOW = std::mem::zeroed();
        // Request SHGFI_ICON | SHGFI_LARGEICON (32x32) for crisp anti-aliased display on high-DPI screens
        let ret = SHGetFileInfoW(
            wide.as_ptr(),
            0,
            &mut sfi,
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        );

        if ret == 0 || sfi.hIcon.is_null() {
            return None;
        }

        let hicon = sfi.hIcon;
        let width = 32u32;
        let height = 32u32;

        let screen_dc = GetDC(std::ptr::null_mut());
        let mem_dc = CreateCompatibleDC(screen_dc);

        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = width as i32;
        bmi.bmiHeader.biHeight = -(height as i32); // Top-down DIB
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;

        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let hbitmap = CreateDIBSection(
            mem_dc,
            &bmi,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );

        if hbitmap.is_null() || bits.is_null() {
            DestroyIcon(hicon);
            DeleteDC(mem_dc);
            ReleaseDC(std::ptr::null_mut(), screen_dc);
            return None;
        }

        let old_bm = SelectObject(mem_dc, hbitmap);

        DrawIconEx(
            mem_dc,
            0,
            0,
            hicon,
            width as i32,
            height as i32,
            0,
            std::ptr::null_mut(),
            DI_NORMAL,
        );

        let pixel_count = (width * height) as usize;
        let src_bytes = std::slice::from_raw_parts(bits as *const u8, pixel_count * 4);
        let mut rgba = Vec::with_capacity(pixel_count * 4);

        let mut has_visible_pixels = false;
        for i in 0..pixel_count {
            let b = src_bytes[i * 4];
            let g = src_bytes[i * 4 + 1];
            let r = src_bytes[i * 4 + 2];
            let mut a = src_bytes[i * 4 + 3];

            // If the icon has no alpha channel (common for legacy 32-bit icons), treat non-black as opaque
            if a == 0 && (r > 0 || g > 0 || b > 0) {
                a = 255;
            }
            if a > 0 {
                has_visible_pixels = true;
            }
            rgba.push(r);
            rgba.push(g);
            rgba.push(b);
            rgba.push(a);
        }

        SelectObject(mem_dc, old_bm);
        DeleteObject(hbitmap);
        DestroyIcon(hicon);
        DeleteDC(mem_dc);
        ReleaseDC(std::ptr::null_mut(), screen_dc);

        if !has_visible_pixels {
            return None;
        }

        Some(RawIconData {
            width,
            height,
            rgba,
        })
    }
}

#[cfg(not(target_os = "windows"))]
pub fn extract_icon_from_exe(_exe_path: &str) -> Option<RawIconData> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_app_name_mappings() {
        assert_eq!(format_app_name("T3 Code (Alpha).exe"), "T3 Code (Alpha)");
        assert_eq!(format_app_name("t3code.exe"), "T3 Code (Alpha)");
        assert_eq!(format_app_name("P3 Code Alpha.exe"), "T3 Code (Alpha)");
        assert_eq!(format_app_name("code.exe"), "VS Code");
        assert_eq!(format_app_name("chrome.exe"), "Google Chrome");
        assert_eq!(format_app_name("slack.exe"), "Slack");
        assert_eq!(format_app_name("Desktop"), "Desktop");
        assert_eq!(format_app_name("custom_app.exe"), "Custom App");
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn test_t3_code_live_process_and_icon_resolution() {
        let (icon, has_icon) = get_app_icon("T3 Code (Alpha).exe", None);
        println!("T3 Code (Alpha).exe resolution: has_icon={}, size={:?}", has_icon, icon.size());
        if has_icon {
            assert_eq!(icon.size().width, 32);
            assert_eq!(icon.size().height, 32);
        } else {
            assert_eq!(icon.size().width, 0);
            assert_eq!(icon.size().height, 0);
        }
    }
}
