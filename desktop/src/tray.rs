// Copyright (c) 2026 Sotto
// System Tray implementation for Windows (Step 3.2)

#[cfg(target_os = "windows")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "windows")]
use std::sync::Arc;
#[cfg(target_os = "windows")]
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
#[cfg(target_os = "windows")]
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
#[cfg(target_os = "windows")]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, LoadIconW, PostQuitMessage, RegisterClassW,
    SetForegroundWindow, TrackPopupMenu, IDI_APPLICATION, MF_SEPARATOR, MF_STRING, MSG,
    TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTBUTTON, WM_COMMAND, WM_DESTROY, WM_LBUTTONDBLCLK,
    WM_LBUTTONUP, WM_RBUTTONUP, WM_USER, WNDCLASSW,
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

        // Store pointer to handler in window user data
        let handler_box = Box::into_raw(Box::new(handler.clone()));
        windows_sys::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(
            hwnd,
            windows_sys::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
            handler_box as isize,
        );

        // 3. Register Notify Icon in Windows Taskbar Notification Area
        let h_icon = LoadIconW(std::ptr::null_mut(), IDI_APPLICATION);
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

        // 5. Cleanup icon when thread exits
        Shell_NotifyIconW(NIM_DELETE, &nid);
        DestroyWindow(hwnd);
        let _ = Box::from_raw(handler_box);
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
    match msg {
        WM_TRAY_CALLBACK => {
            let event = lparam as u32;
            let handler_ptr = windows_sys::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
                hwnd,
                windows_sys::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
            ) as *const Arc<F>;

            if handler_ptr.is_null() {
                return 0;
            }
            let handler = &*handler_ptr;

            if event == WM_LBUTTONUP || event == WM_LBUTTONDBLCLK {
                // Left click on tray icon -> Toggle / Open Studio Suite
                handler(TrayAction::OpenStudio);
            } else if event == WM_RBUTTONUP {
                // Right click -> Context popup menu
                let mut pt: POINT = std::mem::zeroed();
                GetCursorPos(&mut pt);

                let hmenu = CreatePopupMenu();
                let m_studio: Vec<u16> = "Open Studio Suite\0".encode_utf16().collect();
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
        WM_COMMAND => {
            let cmd_id = wparam as usize;
            let handler_ptr = windows_sys::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
                hwnd,
                windows_sys::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
            ) as *const Arc<F>;

            if !handler_ptr.is_null() {
                let handler = &*handler_ptr;
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
    pub fn new<F>(_action_handler: F) -> Result<Self, String> {
        Ok(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tray_action_variants() {
        let action = TrayAction::OpenStudio;
        assert_eq!(action, TrayAction::OpenStudio);
        assert_ne!(action, TrayAction::TogglePill);
        assert_ne!(action, TrayAction::Exit);
    }

    #[test]
    fn test_tray_manager_initialization() {
        let tray_res = SystemTray::new(|_| {});
        assert!(tray_res.is_ok(), "SystemTray initialization should succeed");
    }
}
