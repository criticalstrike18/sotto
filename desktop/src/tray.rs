// Copyright (c) 2026 Sotto
// System Tray implementation for Windows (Step 3.2)

#[cfg(target_os = "windows")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "windows")]
use std::sync::Arc;
#[cfg(target_os = "windows")]
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
#[cfg(target_os = "windows")]
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, REG_DWORD,
};
#[cfg(target_os = "windows")]
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
#[cfg(target_os = "windows")]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconFromResourceEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    GetSystemMetrics, LoadIconW, PostQuitMessage, RegisterClassW, SetForegroundWindow,
    TrackPopupMenu, HICON, IDI_APPLICATION, MF_SEPARATOR, MF_STRING, MSG, SM_CXSMICON,
    SM_CYSMICON, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTBUTTON, WM_COMMAND, WM_DESTROY,
    WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_USER, WNDCLASSW,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    OpenStudio,
    TogglePill,
    Exit,
}

#[cfg(target_os = "windows")]
const WM_TRAY_CALLBACK: u32 = WM_USER + 101;
#[cfg(target_os = "windows")]
const ID_TRAY_ICON: u32 = 1001;

#[cfg(target_os = "windows")]
const ID_MENU_STUDIO: usize = 2001;
#[cfg(target_os = "windows")]
const ID_MENU_TOGGLE_PILL: usize = 2002;
#[cfg(target_os = "windows")]
const ID_MENU_EXIT: usize = 2003;

#[cfg(target_os = "windows")]
const TRAY_DARK_16: &[u8] = include_bytes!("../assets/icons/sotto-tray-dark-16.png");
#[cfg(target_os = "windows")]
const TRAY_DARK_20: &[u8] = include_bytes!("../assets/icons/sotto-tray-dark-20.png");
#[cfg(target_os = "windows")]
const TRAY_DARK_24: &[u8] = include_bytes!("../assets/icons/sotto-tray-dark-24.png");
#[cfg(target_os = "windows")]
const TRAY_DARK_32: &[u8] = include_bytes!("../assets/icons/sotto-tray-dark-32.png");

#[cfg(target_os = "windows")]
const TRAY_LIGHT_16: &[u8] = include_bytes!("../assets/icons/sotto-tray-light-16.png");
#[cfg(target_os = "windows")]
const TRAY_LIGHT_20: &[u8] = include_bytes!("../assets/icons/sotto-tray-light-20.png");
#[cfg(target_os = "windows")]
const TRAY_LIGHT_24: &[u8] = include_bytes!("../assets/icons/sotto-tray-light-24.png");
#[cfg(target_os = "windows")]
const TRAY_LIGHT_32: &[u8] = include_bytes!("../assets/icons/sotto-tray-light-32.png");

#[cfg(target_os = "windows")]
fn select_tray_icon_bytes(light_taskbar: bool, desired_px: i32) -> (&'static [u8], i32) {
    match (light_taskbar, desired_px) {
        (false, ..=16) => (TRAY_DARK_16, 16),
        (false, 17..=20) => (TRAY_DARK_20, 20),
        (false, 21..=24) => (TRAY_DARK_24, 24),
        (false, _) => (TRAY_DARK_32, 32),
        (true, ..=16) => (TRAY_LIGHT_16, 16),
        (true, 17..=20) => (TRAY_LIGHT_20, 20),
        (true, 21..=24) => (TRAY_LIGHT_24, 24),
        (true, _) => (TRAY_LIGHT_32, 32),
    }
}

#[cfg(target_os = "windows")]
unsafe fn is_system_light_taskbar() -> bool {
    let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\0"
        .encode_utf16()
        .collect();
    let val_name: Vec<u16> = "SystemUsesLightTheme\0".encode_utf16().collect();
    let mut hkey: HKEY = std::ptr::null_mut();
    if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey) != 0 {
        return false;
    }
    let mut val_type: u32 = 0;
    let mut data: u32 = 0;
    let mut data_len: u32 = std::mem::size_of::<u32>() as u32;
    let status = RegQueryValueExW(
        hkey,
        val_name.as_ptr(),
        std::ptr::null_mut(),
        &mut val_type,
        &mut data as *mut u32 as *mut u8,
        &mut data_len,
    );
    RegCloseKey(hkey);
    status == 0 && val_type == REG_DWORD && data == 1
}

#[cfg(target_os = "windows")]
unsafe fn load_tray_hicon(light_taskbar: bool) -> (HICON, bool) {
    let desired = GetSystemMetrics(SM_CXSMICON)
        .max(GetSystemMetrics(SM_CYSMICON))
        .max(16);
    let (bytes, bucket_px) = select_tray_icon_bytes(light_taskbar, desired);
    let hicon = CreateIconFromResourceEx(
        bytes.as_ptr(),
        bytes.len() as u32,
        1,
        0x0003_0000,
        bucket_px,
        bucket_px,
        0,
    );
    if !hicon.is_null() {
        return (hicon, true);
    }
    (LoadIconW(std::ptr::null_mut(), IDI_APPLICATION), false)
}

#[cfg(target_os = "windows")]
struct TrayWndState<F> {
    handler: Arc<F>,
    is_light_taskbar: bool,
    h_icon: HICON,
    h_icon_owned: bool,
}

#[cfg(target_os = "windows")]
pub struct SystemTray {
    _thread_handle: Option<std::thread::JoinHandle<()>>,
    is_running: Arc<AtomicBool>,
}

#[cfg(target_os = "windows")]
impl SystemTray {
    pub fn new<F>(action_handler: F) -> Result<Self, String>
    where
        F: Fn(TrayAction) + Send + Sync + 'static,
    {
        let is_running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&is_running);
        let handler = Arc::new(action_handler);

        let thread_handle = std::thread::Builder::new()
            .name("sotto-tray-daemon".into())
            .spawn(move || {
                unsafe {
                    Self::run_tray_thread(running_clone, handler);
                }
            })
            .map_err(|e| format!("Failed to spawn tray thread: {}", e))?;

        Ok(Self {
            _thread_handle: Some(thread_handle),
            is_running,
        })
    }

    unsafe fn run_tray_thread<F>(is_running: Arc<AtomicBool>, handler: Arc<F>)
    where
        F: Fn(TrayAction) + Send + Sync + 'static,
    {
        // 1. Register hidden message window class
        let class_name: Vec<u16> = "SottoTrayMessageWndClass\0".encode_utf16().collect();
        let wnd_class = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(tray_window_proc::<F>),
            cbClsExtra: 0,
            cbWndExtra: std::mem::size_of::<*const Arc<F>>() as i32,
            hInstance: std::ptr::null_mut(),
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };

        RegisterClassW(&wnd_class);

        // 2. Create message-only / hidden window
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );

        if hwnd.is_null() {
            eprintln!("[System Tray] Failed to create tray message window");
            return;
        }

        // 3. Register Notify Icon in Windows Taskbar Notification Area
        let is_light_taskbar = is_system_light_taskbar();
        let (h_icon, h_icon_owned) = load_tray_hicon(is_light_taskbar);

        // Store pointer to tray state in window user data
        let state_box = Box::into_raw(Box::new(TrayWndState {
            handler: handler.clone(),
            is_light_taskbar,
            h_icon,
            h_icon_owned,
        }));
        windows_sys::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(
            hwnd,
            windows_sys::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
            state_box as isize,
        );

        let mut tip: [u16; 128] = [0; 128];
        let tip_str: Vec<u16> = "Sotto — Voice Dictation\0".encode_utf16().collect();
        let copy_len = tip_str.len().min(127);
        tip[..copy_len].copy_from_slice(&tip_str[..copy_len]);

        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = ID_TRAY_ICON;
        nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY_CALLBACK;
        nid.hIcon = h_icon;
        nid.szTip = tip;

        if Shell_NotifyIconW(NIM_ADD, &nid) == 0 {
            eprintln!("[System Tray] Failed to add tray icon to notification area");
        } else {
            println!("[System Tray] Native Windows Notification Area icon active");
        }

        // 4. Message loop
        let mut msg: MSG = std::mem::zeroed();
        while is_running.load(Ordering::SeqCst) && GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            DispatchMessageW(&msg);
        }

        // 5. Cleanup icon and GDI handle when thread exits
        Shell_NotifyIconW(NIM_DELETE, &nid);
        DestroyWindow(hwnd);
        let state = Box::from_raw(state_box);
        if state.h_icon_owned && !state.h_icon.is_null() {
            DestroyIcon(state.h_icon);
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for SystemTray {
    fn drop(&mut self) {
        self.is_running.store(false, Ordering::SeqCst);
    }
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn tray_window_proc<F>(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT
where
    F: Fn(TrayAction) + Send + Sync + 'static,
{
    let state_ptr = windows_sys::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
        hwnd,
        windows_sys::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
    ) as *mut TrayWndState<F>;

    match msg {
        WM_TRAY_CALLBACK => {
            let event = lparam as u32;
            if state_ptr.is_null() {
                return 0;
            }
            let handler = &(*state_ptr).handler;

            if event == WM_LBUTTONUP || event == WM_LBUTTONDBLCLK {
                // Left click on tray icon -> Toggle / Open Sotto Studio
                handler(TrayAction::OpenStudio);
            } else if event == WM_RBUTTONUP {
                // Right click -> Context popup menu
                let mut pt: POINT = std::mem::zeroed();
                GetCursorPos(&mut pt);

                let hmenu = CreatePopupMenu();
                let m_studio: Vec<u16> = "Open Sotto Studio\0".encode_utf16().collect();
                let m_toggle: Vec<u16> = "Toggle Floating Pill\0".encode_utf16().collect();
                let m_exit: Vec<u16> = "Exit Sotto\0".encode_utf16().collect();

                AppendMenuW(hmenu, MF_STRING, ID_MENU_STUDIO, m_studio.as_ptr());
                AppendMenuW(hmenu, MF_STRING, ID_MENU_TOGGLE_PILL, m_toggle.as_ptr());
                AppendMenuW(hmenu, MF_SEPARATOR, 0, std::ptr::null());
                AppendMenuW(hmenu, MF_STRING, ID_MENU_EXIT, m_exit.as_ptr());

                SetForegroundWindow(hwnd);
                TrackPopupMenu(
                    hmenu,
                    TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_BOTTOMALIGN,
                    pt.x,
                    pt.y,
                    0,
                    hwnd,
                    std::ptr::null_mut(),
                );
                DestroyMenu(hmenu);
            }
            0
        }
        WM_SETTINGCHANGE => {
            if !state_ptr.is_null() {
                let state = &mut *state_ptr;
                let now_light = is_system_light_taskbar();
                if now_light != state.is_light_taskbar {
                    let (new_icon, new_owned) = load_tray_hicon(now_light);
                    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
                    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
                    nid.hWnd = hwnd;
                    nid.uID = ID_TRAY_ICON;
                    nid.uFlags = NIF_ICON;
                    nid.hIcon = new_icon;
                    if Shell_NotifyIconW(NIM_MODIFY, &nid) != 0 {
                        if state.h_icon_owned && !state.h_icon.is_null() {
                            DestroyIcon(state.h_icon);
                        }
                        state.h_icon = new_icon;
                        state.h_icon_owned = new_owned;
                        state.is_light_taskbar = now_light;
                    } else if new_owned && !new_icon.is_null() {
                        DestroyIcon(new_icon);
                    }
                }
            }
            0
        }
        WM_COMMAND => {
            let cmd_id = wparam as usize;
            if !state_ptr.is_null() {
                let handler = &(*state_ptr).handler;
                match cmd_id {
                    ID_MENU_STUDIO => handler(TrayAction::OpenStudio),
                    ID_MENU_TOGGLE_PILL => handler(TrayAction::TogglePill),
                    ID_MENU_EXIT => {
                        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
                        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
                        nid.hWnd = hwnd;
                        nid.uID = ID_TRAY_ICON;
                        Shell_NotifyIconW(NIM_DELETE, &nid);
                        handler(TrayAction::Exit);
                    }
                    _ => {}
                }
            }
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(not(target_os = "windows"))]
pub struct SystemTray;

#[cfg(not(target_os = "windows"))]
impl SystemTray {
    pub fn new<F>(_action_handler: F) -> Result<Self, String>
    where
        F: FnMut(TrayAction) + 'static,
    {
        Ok(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tray_manager_initialization() {
        let tray_res = SystemTray::new(|_| {});
        assert!(tray_res.is_ok(), "SystemTray initialization should succeed");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_tray_hicon_loads_for_light_and_dark_themes() {
        assert_eq!(select_tray_icon_bytes(false, 16).1, 16);
        assert_eq!(select_tray_icon_bytes(true, 20).1, 20);
        assert_eq!(select_tray_icon_bytes(false, 24).1, 24);
        assert_eq!(select_tray_icon_bytes(true, 32).1, 32);

        for light in [false, true] {
            unsafe {
                let (hicon, owned) = load_tray_hicon(light);
                assert!(!hicon.is_null(), "tray HICON must load for light={light}");
                assert!(owned, "tray HICON should come from embedded PNG resource, not generic IDI_APPLICATION");
                if owned {
                    DestroyIcon(hicon);
                }
            }
        }
    }
}
