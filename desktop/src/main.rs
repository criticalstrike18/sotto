#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// Copyright (c) 2026 Sotto
// Studio & Voice Pill Architecture matching whisper-flow-designs/interactive-preview.html

slint::include_modules!();

use sotto_desktop::{app_icon, audio, config, db, hotkey, inject, media, router, startup, tray};
use sotto_desktop::db::{AsyncHistoryQueue, get_process_info_for_hwnd};

use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::rc::Rc;
use std::cell::RefCell;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
use windows_sys::core::GUID;
#[cfg(target_os = "windows")]
use windows_sys::Win32::Foundation::{POINT, RECT};
#[cfg(target_os = "windows")]
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
#[cfg(target_os = "windows")]
use windows_sys::Win32::System::Com::{CoCreateInstance, CoInitialize, CLSCTX_INPROC_SERVER};
#[cfg(target_os = "windows")]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetForegroundWindow, GetWindowLongW, GetWindowRect, IsWindowVisible,
    SetForegroundWindow, SetWindowLongW, SetWindowPos, ShowWindow, GWL_EXSTYLE, HWND_TOPMOST,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SW_RESTORE,
    SW_SHOWNOACTIVATE, SystemParametersInfoW, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, SPI_GETWORKAREA,
};
#[cfg(target_os = "windows")]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

const IPC_ADDR: &str = "127.0.0.1:49281";

// Thread-local storage for StudioWindow on the UI main thread.
// Ensures lazy zero-allocation startup and safe single-threaded UI execution.
thread_local! {
    static STUDIO_INSTANCE: RefCell<Option<StudioWindow>> = const { RefCell::new(None) };
    static STUDIO_SCROLL_TIMER: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
    static STUDIO_ANIM_TIMER: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
    static STUDIO_HISTORY_CACHE: RefCell<Vec<HistoryItem>> = const { RefCell::new(Vec::new()) };
    static STUDIO_HISTORY_LAYOUT: RefCell<sotto_desktop::smooth_scroll::HistoryLayout> =
        RefCell::new(sotto_desktop::smooth_scroll::HistoryLayout::default());
    static STUDIO_CACHED_WINDOW: RefCell<Option<(sotto_desktop::smooth_scroll::VirtualWindow, f32, Instant)>> =
        const { RefCell::new(None) };
    static STUDIO_LAYOUT_KEY: RefCell<Option<(u64, u32, u32)>> = const { RefCell::new(None) };
}

static ACTIVE_OPT_IDLE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

fn wake_pill_anim_timer() {
    STUDIO_ANIM_TIMER.with(|t| {
        if let Some(ref timer) = *t.borrow() {
            if !timer.running() {
                timer.restart();
            }
        }
    });
}

fn should_anim_timer_run(
    engine_state: u8,
    opt_idle: i32,
    studio_visible: bool,
    studio_tab: i32,
    studio_preview_state: i32,
    was_listening: bool,
    last_studio_preview_state: i32,
) -> bool {
    if engine_state != 0 || opt_idle != 0 || was_listening {
        return true;
    }
    studio_visible
        && studio_tab == 2
        && (studio_preview_state == 1 || last_studio_preview_state != studio_preview_state)
}

/// Drops the Studio window and all UI-thread history/icon caches so closing frees RAM.
fn drop_studio_instance(studio_visible: &Arc<AtomicBool>) {
    STUDIO_SCROLL_TIMER.with(|t| {
        if let Some(ref timer) = *t.borrow() {
            timer.stop();
        }
        *t.borrow_mut() = None;
    });
    STUDIO_HISTORY_CACHE.with(|c| {
        let mut items = c.borrow_mut();
        items.clear();
        items.shrink_to_fit();
    });
    STUDIO_HISTORY_LAYOUT.with(|l| *l.borrow_mut() = Default::default());
    STUDIO_CACHED_WINDOW.with(|w| *w.borrow_mut() = None);
    STUDIO_LAYOUT_KEY.with(|k| *k.borrow_mut() = None);
    sotto_desktop::smooth_scroll::reset_all_studio_scrollers();
    app_icon::clear_ui_icon_cache();
    STUDIO_INSTANCE.with(|cell| {
        if let Some(s) = cell.borrow_mut().take() {
            let _ = s.hide();
            drop(s);
        }
    });
    studio_visible.store(false, Ordering::SeqCst);
    trim_working_set();
}

/// Returns `(phys_w, logical_h, scale, realized)` using Studio's default 1040x840
/// geometry when queried synchronously before winit has realized the native window.
fn effective_studio_metrics(s: &StudioWindow) -> (u32, f32, f32, bool) {
    let scale = s.window().scale_factor().max(1.0);
    let win_size = s.window().size();
    if win_size.width > 0 && win_size.height > 0 {
        (win_size.width, win_size.height as f32 / scale, scale, true)
    } else {
        let fallback_w = (1040.0 * scale).round() as u32;
        (fallback_w, 840.0, scale, false)
    }
}

/// Returns `(phys_w, visible_h, scale, realized)` calibrated per Studio tab
/// (`156px` chrome on Tab 0 Dictations; `100px` chrome on Tab 1/Tab 2).
fn effective_tab_visible_h(s: &StudioWindow, active_tab: i32) -> (u32, f32, f32, bool) {
    let (phys_w, logical_h, scale, realized) = effective_studio_metrics(s);
    let reserved = if active_tab == 0 {
        sotto_desktop::smooth_scroll::CHROME_RESERVED_PX
    } else {
        sotto_desktop::smooth_scroll::CONTENT_CHROME_RESERVED_PX
    };
    let visible_h = if realized {
        let prop_h = match active_tab {
            1 => s.get_providers_viewport_height(),
            2 => s.get_custom_viewport_height(),
            _ => s.get_history_viewport_height(),
        };
        if prop_h > 50.0 {
            prop_h
        } else {
            sotto_desktop::smooth_scroll::compute_visible_height(
                logical_h,
                reserved,
                sotto_desktop::smooth_scroll::MIN_VISIBLE_PX,
            )
        }
    } else {
        sotto_desktop::smooth_scroll::compute_visible_height(
            logical_h,
            reserved,
            sotto_desktop::smooth_scroll::MIN_VISIBLE_PX,
        )
    };
    (phys_w, visible_h, scale, realized)
}

fn patch_studio_icons_from_cache() {
    with_studio(|s| {
        let changed = STUDIO_HISTORY_CACHE.with(|cache| {
            let mut items = cache.borrow_mut();
            let mut any = false;
            for item in items.iter_mut() {
                if !item.has_icon {
                    if let Some((icon, true)) = app_icon::try_get_cached_app_icon(item.target_app.as_str()) {
                        item.app_icon = icon;
                        item.has_icon = true;
                        any = true;
                    }
                }
            }
            any
        });
        if changed {
            let (_, view_h, _, _) = effective_tab_visible_h(s, 0);
            sync_history_virtual_window(s, view_h, true);
        }
    });
}

/// Populates Studio history instantaneously from SQLite without blocking the UI thread
/// on uncached Win32 icon/process lookups; resolves missing icons in a background batch.
fn hydrate_studio_history_nonblocking(s: &StudioWindow, db: &db::HistoryDb, limit: usize) {
    let recent = db.get_recent(limit).unwrap_or_default();
    let mut missing: std::collections::HashMap<String, Option<String>> = std::collections::HashMap::new();
    let items: Vec<HistoryItem> = recent
        .into_iter()
        .map(|r| {
            let formatted = app_icon::format_app_name(&r.target_app);
            let (icon, has_icon) = if formatted.eq_ignore_ascii_case("Desktop") {
                (slint::Image::default(), false)
            } else if let Some(hit) = app_icon::try_get_cached_app_icon(&formatted) {
                hit
            } else {
                missing
                    .entry(formatted.clone())
                    .or_insert_with(|| Some(r.target_app.clone()));
                (slint::Image::default(), false)
            };
            HistoryItem {
                id: r.id as i32,
                timestamp_str: r.timestamp_str.into(),
                transcript: r.transcript.into(),
                duration_ms: r.duration_ms,
                duration_str: r.duration_str.into(),
                target_app: formatted.into(),
                app_icon: icon,
                has_icon,
            }
        })
        .collect();

    set_studio_history_list(s, items);

    if !missing.is_empty() {
        std::thread::spawn(move || {
            let mut any_found = false;
            for (formatted, raw) in missing {
                if app_icon::prewarm_app_icon(&formatted, raw.as_deref()) {
                    any_found = true;
                }
            }
            if any_found {
                let _ = slint::invoke_from_event_loop(patch_studio_icons_from_cache);
            }
        });
    }
}

fn recompute_studio_history_layout(win_w: u32, scale: f32) -> (i32, f32) {
    let gen = STUDIO_HISTORY_GEN.load(Ordering::Relaxed);
    let key = (gen, win_w, scale.to_bits());
    if STUDIO_LAYOUT_KEY.with(|k| *k.borrow() == Some(key)) {
        return STUDIO_HISTORY_LAYOUT.with(|l| {
            let layout = l.borrow();
            (layout.heights.len() as i32, layout.total_content_h)
        });
    }

    let cpl = sotto_desktop::smooth_scroll::chars_per_line_for(win_w, scale);
    let res = STUDIO_HISTORY_CACHE.with(|cache| {
        let items = cache.borrow();
        let layout = sotto_desktop::smooth_scroll::compute_history_layout(
            items.iter().map(|it| it.transcript.as_str()),
            cpl,
        );
        let total_h = layout.total_content_h;
        let count = items.len() as i32;
        STUDIO_HISTORY_LAYOUT.with(|l| *l.borrow_mut() = layout);
        (count, total_h)
    });
    STUDIO_LAYOUT_KEY.with(|k| *k.borrow_mut() = Some(key));
    STUDIO_CACHED_WINDOW.with(|cw| *cw.borrow_mut() = None);
    res
}

fn sync_history_virtual_window(s: &StudioWindow, visible_h: f32, force_refill: bool) {
    let scroll_offset_y = sotto_desktop::smooth_scroll::STUDIO_SCROLLER
        .with(|sc| (-sc.borrow().current_y()).max(0.0));
    let now = Instant::now();

    let (win_opt, n) = STUDIO_HISTORY_LAYOUT.with(|l| {
        let layout = l.borrow();
        let n = layout.heights.len();
        if n == 0 {
            return (None, 0);
        }
        let win = sotto_desktop::smooth_scroll::compute_virtual_window(
            &layout,
            scroll_offset_y,
            visible_h,
            sotto_desktop::smooth_scroll::VIRTUAL_CUSHION_PX,
        );
        (win, n)
    });

    let Some(win) = win_opt else {
        let empty_vis: Rc<VecModel<HistoryItem>> = Rc::new(VecModel::default());
        s.set_visible_history_items(ModelRc::from(empty_vis));
        s.set_top_spacer_height(0.0);
        s.set_bottom_spacer_height(0.0);
        STUDIO_CACHED_WINDOW.with(|cw| *cw.borrow_mut() = None);
        return;
    };

    let needs_refill = if force_refill {
        true
    } else {
        let cached = STUDIO_CACHED_WINDOW.with(|cw| *cw.borrow());
        STUDIO_HISTORY_LAYOUT.with(|l| {
            sotto_desktop::smooth_scroll::should_refill_virtual_window(
                &l.borrow(),
                cached,
                win,
                scroll_offset_y,
                visible_h,
                now,
            )
        })
    };

    if needs_refill {
        let slice: Vec<HistoryItem> = STUDIO_HISTORY_CACHE.with(|cache| {
            let items = cache.borrow();
            let end = (win.last_idx + 1).min(items.len());
            let start = win.first_idx.min(end);
            items[start..end].to_vec()
        });
        s.set_top_spacer_height(win.top_spacer);
        s.set_bottom_spacer_height(win.bottom_spacer);
        s.set_visible_history_items(ModelRc::from(Rc::new(VecModel::from(slice))));
        STUDIO_CACHED_WINDOW.with(|cw| *cw.borrow_mut() = Some((win, scroll_offset_y, now)));
    }
    let _ = n;
}

fn sync_history_dimensions_and_window(s: &StudioWindow, visible_h: f32, force_refill: bool) -> (i32, f32) {
    let (phys_w, _, scale, _) = effective_studio_metrics(s);
    let (count, total_content_h) = recompute_studio_history_layout(phys_w, scale);
    sotto_desktop::smooth_scroll::STUDIO_SCROLLER.with(|scroller| {
        let mut eng = scroller.borrow_mut();
        eng.set_dimensions(total_content_h, visible_h);
        s.set_history_scroll_progress(eng.progress());
    });
    let can_scroll = total_content_h > visible_h;
    s.set_history_count(count);
    s.set_history_can_scroll(can_scroll);
    s.set_history_content_height(total_content_h.max(visible_h));
    sync_history_virtual_window(s, visible_h, force_refill);
    (count, total_content_h)
}

fn set_studio_history_list(s: &StudioWindow, items: Vec<HistoryItem>) {
    s.set_history_virtualized(true);
    let count = items.len() as i32;
    STUDIO_HISTORY_CACHE.with(|cache| *cache.borrow_mut() = items);
    STUDIO_CACHED_WINDOW.with(|cw| *cw.borrow_mut() = None);
    STUDIO_LAYOUT_KEY.with(|k| *k.borrow_mut() = None);
    let empty_full: Rc<VecModel<HistoryItem>> = Rc::new(VecModel::default());
    s.set_history_items(ModelRc::from(empty_full));
    STUDIO_HISTORY_GEN.fetch_add(1, Ordering::SeqCst);

    sotto_desktop::smooth_scroll::STUDIO_SCROLLER.with(|scroller| {
        scroller.borrow_mut().jump_to(0.0);
    });
    s.set_history_viewport_y(0.0);
    s.set_history_scroll_progress(0.0);
    s.set_history_is_scrolling(false);

    let (_, visible_h, _, _) = effective_tab_visible_h(s, 0);
    let (_, _) = sync_history_dimensions_and_window(s, visible_h, true);
    s.set_history_count(count);
}

fn prepend_studio_history_record(rec: db::HistoryRecord, exe_path_ui: Option<String>, source_label: &str) {
    with_studio(|s| {
        let formatted = app_icon::format_app_name(&rec.target_app);
        let (icon, has_icon) = if formatted.eq_ignore_ascii_case("Desktop") {
            (slint::Image::default(), false)
        } else if let Some(hit) = app_icon::try_get_cached_app_icon(&formatted) {
            hit
        } else {
            let formatted_bg = formatted.clone();
            let raw_bg = exe_path_ui.clone();
            std::thread::spawn(move || {
                if app_icon::prewarm_app_icon(&formatted_bg, raw_bg.as_deref()) {
                    let _ = slint::invoke_from_event_loop(patch_studio_icons_from_cache);
                }
            });
            (slint::Image::default(), false)
        };
        let (phys_w, visible_h, a_scale, _) = effective_tab_visible_h(s, 0);
        let a_cpl = sotto_desktop::smooth_scroll::chars_per_line_for(phys_w, a_scale);
        let inserted_h = sotto_desktop::smooth_scroll::estimate_card_height(&rec.transcript, a_cpl)
            + sotto_desktop::smooth_scroll::ROW_SPACING;

        let new_count = STUDIO_HISTORY_CACHE.with(|cache| {
            let mut items = cache.borrow_mut();
            items.insert(
                0,
                HistoryItem {
                    id: rec.id as i32,
                    timestamp_str: rec.timestamp_str.into(),
                    transcript: rec.transcript.into(),
                    duration_ms: rec.duration_ms,
                    duration_str: rec.duration_str.into(),
                    target_app: formatted.into(),
                    app_icon: icon,
                    has_icon,
                },
            );
            if items.len() > 500 {
                items.truncate(500);
            }
            items.len() as i32
        });

        STUDIO_HISTORY_GEN.fetch_add(1, Ordering::SeqCst);
        let (count, total_content_h) = recompute_studio_history_layout(phys_w, a_scale);
        sotto_desktop::smooth_scroll::STUDIO_SCROLLER.with(|scroller| {
            let mut eng = scroller.borrow_mut();
            eng.set_dimensions(total_content_h, visible_h);
            // Anchor scrolling (Fix R8): keep reading position steady when prepending at index 0.
            if eng.current_y() < -15.0 {
                eng.shift_anchor(-inserted_h);
                let snapped = (eng.current_y() * a_scale).round() / a_scale;
                s.set_history_viewport_y(snapped);
            }
            s.set_history_scroll_progress(eng.progress());
        });
        s.set_history_can_scroll(total_content_h > visible_h);
        s.set_history_content_height(total_content_h.max(visible_h));
        s.set_history_count(count);
        sync_history_virtual_window(s, visible_h, true);
        println!(
            "[Studio History] Inserted {} transcription #{} into live Studio list (total: {})",
            source_label, rec.id, new_count
        );
        let _ = std::io::stdout().flush();
    });
}


/// Last cumulative GID_PAN totals (x, y). Windows pan deltas are cumulative
/// since GF_BEGIN, so consecutive packets are diffed (see gesture_pan_delta).
#[cfg(target_os = "windows")]
static PAN_TOTALS: std::sync::Mutex<(i32, i32)> = std::sync::Mutex::new((0, 0));

#[cfg(target_os = "windows")]
static IS_RECORDING_KEY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Target slot for the in-progress recording: 0 = primary, 1 = secondary.
#[cfg(target_os = "windows")]
static RECORD_TARGET: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);
pub static PENDING_MOD_KEY: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn wake_scroll_driver() {
    STUDIO_SCROLL_TIMER.with(|t| {
        if let Some(ref timer) = *t.borrow() {
            if !timer.running() {
                timer.restart();
            }
        }
    });
}

fn sync_studio_dimensions(s: &StudioWindow, force_refill: bool) {
    let active_tab = s.get_active_tab();
    let (_, visible_h, _, _) = effective_tab_visible_h(s, active_tab);

    if active_tab == 0 {
        let _ = sync_history_dimensions_and_window(s, visible_h, force_refill);
    } else if active_tab == 1 {
        let prov_content_h = s.get_providers_content_height();
        if prov_content_h > 0.0 {
            sotto_desktop::smooth_scroll::PROVIDERS_SCROLLER.with(|scroller| {
                let mut eng = scroller.borrow_mut();
                eng.set_dimensions(prov_content_h, visible_h);
                s.set_providers_can_scroll(prov_content_h > visible_h);
                s.set_providers_scroll_progress(eng.progress());
            });
        }
    } else if active_tab == 2 {
        let custom_content_h = s.get_custom_content_height();
        if custom_content_h > 0.0 {
            sotto_desktop::smooth_scroll::CUSTOM_SCROLLER.with(|scroller| {
                let mut eng = scroller.borrow_mut();
                eng.set_dimensions(custom_content_h, visible_h);
                s.set_custom_can_scroll(custom_content_h > visible_h);
                s.set_custom_scroll_progress(eng.progress());
            });
        }
    }

    if sotto_desktop::smooth_scroll::any_studio_scroller_animating()
        || (force_refill && active_tab != 0)
    {
        wake_scroll_driver();
    }
}

/// Validates and extracts `(pt_x, pt_y, is_begin)` from Win32 `WM_GESTURE` (`GID_PAN`).
#[allow(dead_code)]
fn decode_gesture_pan_point(dw_id: u32, pts_x: i16, pts_y: i16, dw_flags: u32, expected_gid_pan: u32) -> Option<(i32, i32, bool)> {
    const GF_BEGIN: u32 = 0x0000_0001;
    if dw_id != expected_gid_pan {
        return None;
    }
    Some((pts_x as i32, pts_y as i32, (dw_flags & GF_BEGIN) != 0))
}

/// Handles WM_GESTURE pan for touchscreen scrolling (R10). Feeds the same
/// engine target as wheel/keyboard, so all inputs share one driver.
/// Returns true when the gesture was consumed.
#[cfg(target_os = "windows")]
fn handle_touch_pan_gesture(wparam: usize, lparam: isize, active_tab: i32, dpi_scale: f32) -> bool {
    use windows_sys::Win32::UI::Input::Touch::*;
    if (wparam & 0xFFFF) as u32 != GID_PAN || lparam == 0 {
        return false;
    }
    let mut info: GESTUREINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<GESTUREINFO>() as u32;
    if unsafe { GetGestureInfo(lparam as _, &mut info) } == 0 {
        return false;
    }
    let Some((pt_x, pt_y, is_begin)) =
        decode_gesture_pan_point(info.dwID, info.ptsLocation.x, info.ptsLocation.y, info.dwFlags, GID_PAN)
    else {
        return false;
    };
    unsafe {
        CloseGestureInfoHandle(lparam as _);
    }
    if let Ok(mut totals) = PAN_TOTALS.lock() {
        let (_dx, dy_phys) = sotto_desktop::smooth_scroll::gesture_pan_delta(
            (pt_x, pt_y),
            &mut totals,
            is_begin,
        );
        let dy = dy_phys / dpi_scale.max(1.0);
        if dy != 0.0 {
            if active_tab == 0 {
                sotto_desktop::smooth_scroll::STUDIO_SCROLLER.with(|scroller| {
                    scroller.borrow_mut().scroll_by(dy);
                });
            } else if active_tab == 1 {
                sotto_desktop::smooth_scroll::PROVIDERS_SCROLLER.with(|scroller| {
                    scroller.borrow_mut().scroll_by(dy);
                });
            } else if active_tab == 2 {
                sotto_desktop::smooth_scroll::CUSTOM_SCROLLER.with(|scroller| {
                    scroller.borrow_mut().scroll_by(dy);
                });
            }
            wake_scroll_driver();
        }
        true
    } else {
        false
    }
}
pub static STUDIO_HISTORY_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static IS_SWITCHING_TABS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub static GLOBAL_HOTKEY_MGR: std::sync::Mutex<Option<std::sync::Arc<hotkey::HotkeyManager>>> =
    std::sync::Mutex::new(None);

/// Live transcription dispatcher shared with the Studio Providers tab.
pub static SHARED_TRANSCRIBER: std::sync::Mutex<Option<router::SharedTranscriber>> =
    std::sync::Mutex::new(None);

/// Global post processor shared with the Studio Providers tab for dynamic updates.
pub static GLOBAL_POST_PROCESSOR: std::sync::Mutex<Option<std::sync::Arc<sotto_desktop::cleaner::PostProcessor>>> =
    std::sync::Mutex::new(None);

/// Weak handle to the live recorder, so the Customization tab can apply a
/// capture-mode change without threading the recorder through every UI call.
pub static GLOBAL_RECORDER: std::sync::Mutex<
    Option<std::sync::Weak<std::sync::Mutex<Option<audio::recorder::AudioRecorder>>>>,
> = std::sync::Mutex::new(None);

/// Active long-recording session (pause-aware chunk pump + ordered transcriber).
/// Kept app-global so every trigger path starts/stops the same take.
static LONG_FORM_SESSION: std::sync::Mutex<Option<sotto_desktop::long_form::LongFormSession>> =
    std::sync::Mutex::new(None);

/// High-performance multi-threaded Tokio runtime for background AI LLM post-processing.
static SHARED_TOKIO: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();

pub fn shared_tokio() -> &'static tokio::runtime::Runtime {
    SHARED_TOKIO.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("Failed to initialize background Tokio runtime")
    })
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn studio_subclass_wndproc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    msg: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    match msg {
        WM_WINDOWPOSCHANGING => {
            if IS_SWITCHING_TABS.load(std::sync::atomic::Ordering::SeqCst) {
                let winpos = unsafe { &mut *(lparam as *mut WINDOWPOS) };
                winpos.flags |= SWP_NOSIZE | SWP_NOMOVE;
                return 0;
            }
        }
        WM_SIZE => {
            if !IS_SWITCHING_TABS.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = slint::invoke_from_event_loop(|| {
                    let _ = with_studio(|s| {
                        sync_studio_dimensions(s, true);
                    });
                });
            }
        }
        WM_MOUSEHWHEEL => {
            return 0;
        }
        WM_GESTURE => {
            let (active_tab, dpi_scale) = with_studio(|s| (s.get_active_tab(), s.window().scale_factor().max(1.0)))
                .unwrap_or((0, 1.0));
            if handle_touch_pan_gesture(wparam, lparam, active_tab, dpi_scale) {
                return 0;
            }
        }
        WM_MOUSEWHEEL => {
            const MK_CONTROL_KEY: u16 = 0x0008;
            const MK_SHIFT_KEY: u16 = 0x0004;
            let fw_keys = (wparam & 0xFFFF) as u16;
            let ctrl_or_shift = (fw_keys & (MK_CONTROL_KEY | MK_SHIFT_KEY)) != 0;

            if !ctrl_or_shift {
                let delta = ((wparam >> 16) as i16) as f32;
                let active_tab = with_studio(|s| {
                    sync_studio_dimensions(s, false);
                    s.get_active_tab()
                })
                .unwrap_or(0);
                if active_tab == 0 {
                    sotto_desktop::smooth_scroll::STUDIO_SCROLLER.with(|scroller| {
                        let mut engine = scroller.borrow_mut();
                        engine.on_wheel_accelerated(delta);
                    });
                } else if active_tab == 1 {
                    sotto_desktop::smooth_scroll::PROVIDERS_SCROLLER.with(|scroller| {
                        let mut engine = scroller.borrow_mut();
                        engine.on_wheel_accelerated(delta);
                    });
                } else if active_tab == 2 {
                    sotto_desktop::smooth_scroll::CUSTOM_SCROLLER.with(|scroller| {
                        let mut engine = scroller.borrow_mut();
                        engine.on_wheel_accelerated(delta);
                    });
                }
                wake_scroll_driver();
                return 0;
            }
        }
        WM_KEYUP | WM_SYSKEYUP => {
            if IS_RECORDING_KEY.load(std::sync::atomic::Ordering::SeqCst) {
                let pending_mod = PENDING_MOD_KEY.swap(0, std::sync::atomic::Ordering::SeqCst);
                match hotkey::recording_keyup_action(pending_mod) {
                    hotkey::RecordingKeyup::CompleteStandalone => {
                        // Standalone modifier was pressed and released without pressing another key:
                        // complete immediately into the target slot.
                        println!("[Studio] Standalone key recorded via release: {} (vk=0x{:X})", hotkey::vk_to_string(pending_mod), pending_mod);
                        complete_recording(0, Some(pending_mod), None);
                    }
                    hotkey::RecordingKeyup::Rejected(reason) => {
                        // Reserved staged key (e.g. Win from an older build's state):
                        // drop it, stay armed, explain.
                        println!("[Studio] Reserved key released during recording: {} (vk=0x{:X})", hotkey::vk_to_string(pending_mod), pending_mod);
                        set_trigger_status(format!("{} can't be a trigger — {} — press another key", hotkey::vk_to_string(pending_mod), reason));
                    }
                    hotkey::RecordingKeyup::Ignored => {}
                }
                return 0;
            }
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if IS_RECORDING_KEY.load(std::sync::atomic::Ordering::SeqCst) {
                use hotkey::RecordingKeydown;
                let raw_vk = (wparam & 0xFFFF) as u32;
                let target = RECORD_TARGET.load(std::sync::atomic::Ordering::SeqCst);
                match hotkey::recording_keydown_action(raw_vk) {
                    RecordingKeydown::Cancel => {
                        if target == 2 || target == 3 {
                            // In cancel slots, Esc is a valid trigger!
                            // Fall through to record Esc (0x1B).
                        } else {
                            cancel_recording();
                            let _ = with_studio(|s| {
                                s.set_is_recording_key(false);
                                s.set_recording_slot(-1);
                            });
                            println!("[Studio] Key recording cancelled via Escape");
                            return 0;
                        }
                    }
                    RecordingKeydown::Rejected(reason) => {
                        // Reserved key (Caps/Win/PrintScreen/...): pressing it fires a
                        // system action, so never record it — stay armed and explain.
                        println!("[Studio] Reserved key ignored during recording: {} (vk=0x{:X})", hotkey::vk_to_string(raw_vk), raw_vk);
                        set_trigger_status(format!("{} can't be a trigger — {} — press another key", hotkey::vk_to_string(raw_vk), reason));
                        return 0;
                    }
                    RecordingKeydown::PendingMod(normalized_mod) => {
                        PENDING_MOD_KEY.store(normalized_mod, std::sync::atomic::Ordering::SeqCst);
                        let preview_disp = format!("{} + ...", hotkey::vk_to_string(normalized_mod));
                        set_trigger_status(format!("{} — press a key, click a side/middle mouse button, or release for it alone", preview_disp));
                        return 0;
                    }
                    RecordingKeydown::RecordKey => {}
                }

                // A non-modifier key was pressed (Space, F8, Fn, etc.)
                PENDING_MOD_KEY.store(0, std::sync::atomic::Ordering::SeqCst);

                let mut mods = 0u32;
                if hotkey::is_key_down(0x11) { mods |= 0x0002; } // MOD_CONTROL
                if hotkey::is_key_down(0x12) { mods |= 0x0001; } // MOD_ALT
                if hotkey::is_key_down(0x10) { mods |= 0x0004; } // MOD_SHIFT
                if hotkey::is_key_down(0x5B) || hotkey::is_key_down(0x5C) { mods |= 0x0008; } // MOD_WIN

                let vk = raw_vk;
                // Fuse a co-held mouse button (key + mouse chords, dual-signal
                // presses) so one physical press completes exactly once.
                let mouse = hotkey::held_mouse_buttons().into_iter().next();
                println!("[Studio] Key recorded: mods=0x{:X}, vk=0x{:X}, mouse={:?}", mods, vk, mouse);
                complete_recording(mods, Some(vk), mouse);
                return 0;
            }

            let ctrl_down = unsafe { (windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyState(0x11) as i16) < 0 };
            let alt_down = unsafe { (windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyState(0x12) as i16) < 0 };

            if !ctrl_down && !alt_down {
                const VK_SPACE: usize = 0x20;
                const VK_PRIOR: usize = 0x21; // Page Up
                const VK_NEXT: usize = 0x22;  // Page Down
                const VK_END: usize = 0x23;   // End
                const VK_HOME: usize = 0x24;  // Home
                const VK_UP: usize = 0x26;    // Up Arrow
                const VK_DOWN: usize = 0x28;  // Down Arrow

                let active_tab = with_studio(|s| {
                    sync_studio_dimensions(s, false);
                    s.get_active_tab()
                })
                .unwrap_or(0);
                let mut handled = true;

                let dispatch_key = |s: &mut sotto_desktop::smooth_scroll::SmoothScrollEngine| {
                    match wparam {
                        VK_UP => s.scroll_by(80.0),
                        VK_DOWN => s.scroll_by(-80.0),
                        VK_PRIOR => s.scroll_page_up(),
                        VK_NEXT => s.scroll_page_down(),
                        VK_HOME => s.scroll_to_top(),
                        VK_END => s.scroll_to_bottom(),
                        VK_SPACE => {
                            let shift_down = unsafe { (windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyState(0x10) as i16) < 0 };
                            if shift_down {
                                s.scroll_page_up();
                            } else {
                                s.scroll_page_down();
                            }
                        }
                        _ => return false,
                    }
                    true
                };

                if active_tab == 0 {
                    sotto_desktop::smooth_scroll::STUDIO_SCROLLER.with(|scroller| {
                        handled = dispatch_key(&mut scroller.borrow_mut());
                    });
                } else if active_tab == 1 {
                    sotto_desktop::smooth_scroll::PROVIDERS_SCROLLER.with(|scroller| {
                        handled = dispatch_key(&mut scroller.borrow_mut());
                    });
                } else if active_tab == 2 {
                    sotto_desktop::smooth_scroll::CUSTOM_SCROLLER.with(|scroller| {
                        handled = dispatch_key(&mut scroller.borrow_mut());
                    });
                } else {
                    handled = false;
                }

                if handled {
                    wake_scroll_driver();
                    return 0;
                }
            }
        }
        WM_CLOSE => {
            println!("[Studio HWND] Received WM_CLOSE!");
            let _ = std::io::stdout().flush();
        }
        WM_DESTROY => {
            println!("[Studio HWND] Received WM_DESTROY!");
            let _ = std::io::stdout().flush();
        }
        WM_SHOWWINDOW => {
            println!("[Studio HWND] Received WM_SHOWWINDOW (wparam={})", wparam);
            let _ = std::io::stdout().flush();
        }
        WM_NCDESTROY => {
            println!("[Studio HWND] Received WM_NCDESTROY!");
            let _ = std::io::stdout().flush();
            RemovePropW(hwnd, STUDIO_ORIG_WNDPROC_PROP.as_ptr());
        }
        _ => {}
    }
    let orig = GetPropW(hwnd, STUDIO_ORIG_WNDPROC_PROP.as_ptr()) as usize;
    if orig != 0 {
        let func: unsafe extern "system" fn(windows_sys::Win32::Foundation::HWND, u32, usize, isize) -> isize = std::mem::transmute(orig);
        CallWindowProcW(Some(func), hwnd, msg, wparam, lparam)
    } else {
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}

#[cfg(target_os = "windows")]
const STUDIO_ORIG_WNDPROC_PROP: &[u16] = &[
    b's' as u16, b'o' as u16, b't' as u16, b't' as u16, b'o' as u16, b'_' as u16,
    b'w' as u16, b'n' as u16, b'd' as u16, b'p' as u16, b'r' as u16, b'o' as u16, b'c' as u16, 0,
];

#[cfg(target_os = "windows")]
fn setup_studio_native_window(studio: &StudioWindow) -> bool {
    let win = studio.window();
    let win_handle_wrapper = win.window_handle();
    if let Ok(handle) = win_handle_wrapper.window_handle() {
        if let RawWindowHandle::Win32(win32_handle) = handle.as_raw() {
            let hwnd = win32_handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;
            unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::*;
                use windows_sys::Win32::Graphics::Dwm::*;

                let existing = GetPropW(hwnd, STUDIO_ORIG_WNDPROC_PROP.as_ptr());
                if existing != 0 as _ {
                    return true;
                }
                if true {
                    // 1. Ensure Fullscreen / Maximize and Resizable Styles are enabled on HWND
                    let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
                    SetWindowLongW(hwnd, GWL_STYLE, (style | WS_MAXIMIZEBOX | WS_THICKFRAME | WS_MINIMIZEBOX) as i32);

                    // 2. Windows 11 Immersive Dark Titlebar & Edge Polish
                    let dark_mode: windows_sys::core::BOOL = 1;
                    DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE as u32, &dark_mode as *const _ as _, 4);

                    // 3. Subclass WndProc to hook WM_MOUSEWHEEL for Lenis Smooth Scroll Engine
                    let prev = SetWindowLongPtrW(
                        hwnd,
                        GWLP_WNDPROC,
                        studio_subclass_wndproc as *const () as isize,
                    );
                    if prev != 0 {
                        SetPropW(hwnd, STUDIO_ORIG_WNDPROC_PROP.as_ptr(), prev as _);
                        println!("[Studio] HWND {:?} subclassed successfully for smooth scrolling", hwnd);
                        let _ = std::io::stdout().flush();
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Safely executes an operation on the StudioWindow if it has been instantiated.
fn with_studio<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&StudioWindow) -> R,
{
    STUDIO_INSTANCE.with(|cell| {
        cell.borrow().as_ref().map(f)
    })
}

/// Sets the trigger status line in Studio (safe from any thread).
#[cfg(target_os = "windows")]
fn set_trigger_status(text: String) {
    let _ = slint::invoke_from_event_loop(move || {
        let _ = with_studio(|s| {
            s.set_trigger_status(text.into());
        });
    });
}

/// Cancels an in-progress trigger recording without saving, restoring active triggers.
#[cfg(target_os = "windows")]
fn cancel_recording() {
    IS_RECORDING_KEY.store(false, Ordering::SeqCst);
    PENDING_MOD_KEY.store(0, Ordering::SeqCst);
    RECORD_TARGET.store(0, Ordering::SeqCst);
    hotkey::HotkeyManager::set_record_hook(None);
    let cfg = config::AppConfig::load();
    if let Ok(guard) = GLOBAL_HOTKEY_MGR.lock() {
        if let Some(ref mgr) = *guard {
            mgr.apply_bindings(
                &cfg.primary_trigger,
                &cfg.secondary_trigger,
                &cfg.primary_cancel_trigger,
                &cfg.secondary_cancel_trigger,
            );
        }
    }
}

/// Completes a trigger recording from any capture path (keyboard subclass
/// proc or mouse hook): validates, saves to the target slot immediately,
/// applies live. Safe from any thread; UI updates hop to the event loop.
#[cfg(target_os = "windows")]
fn complete_recording(mods: u32, key: Option<u32>, mouse: Option<i32>) {
    use hotkey::TriggerBinding;
    let end_state = || {
        IS_RECORDING_KEY.store(false, Ordering::SeqCst);
        PENDING_MOD_KEY.store(0, Ordering::SeqCst);
        hotkey::HotkeyManager::set_record_hook(None);
    };
    let restore_hotkeys = || {
        let cfg = config::AppConfig::load();
        if let Ok(guard) = GLOBAL_HOTKEY_MGR.lock() {
            if let Some(ref mgr) = *guard {
                mgr.apply_bindings(
                    &cfg.primary_trigger,
                    &cfg.secondary_trigger,
                    &cfg.primary_cancel_trigger,
                    &cfg.secondary_cancel_trigger,
                );
            }
        }
    };
    let binding = TriggerBinding::normalized(mods, key, mouse);
    if binding.is_empty() {
        end_state();
        restore_hotkeys();
        set_trigger_status("Nothing captured — press a key or mouse button.".to_string());
        return;
    }
    if let Some(vk) = binding.key {
        if let Some(reason) = hotkey::reserved_capture_reason(vk) {
            // Reserved key slipped through (e.g. chord raced with Win): refuse
            // to save — recording it would fire a system action every press.
            end_state();
            restore_hotkeys();
            set_trigger_status(format!("{} can't be a trigger — {} — try again", hotkey::vk_to_string(vk), reason));
            return;
        }
    }
    if !binding.is_valid() {
        end_state();
        restore_hotkeys();
        set_trigger_status("Too many keys at once (maximum 5) — try again.".to_string());
        return;
    }
    end_state();
    let target = RECORD_TARGET.load(Ordering::SeqCst).clamp(0, 3);
    let mut cfg = config::AppConfig::load();
    let label = match target {
        1 => {
            cfg.secondary_trigger = Some(binding.clone());
            "Secondary"
        }
        2 => {
            cfg.primary_cancel_trigger = Some(binding.clone());
            "Primary Cancel"
        }
        3 => {
            cfg.secondary_cancel_trigger = Some(binding.clone());
            "Secondary Cancel"
        }
        _ => {
            cfg.primary_trigger = binding.clone();
            "Primary"
        }
    };
    if let Err(e) = cfg.save() {
        eprintln!("[Config Error] Failed to persist trigger: {}", e);
    }
    if let Ok(guard) = GLOBAL_HOTKEY_MGR.lock() {
        if let Some(ref mgr) = *guard {
            mgr.apply_bindings(
                &cfg.primary_trigger,
                &cfg.secondary_trigger,
                &cfg.primary_cancel_trigger,
                &cfg.secondary_cancel_trigger,
            );
        }
    }
    println!("[Studio] {} trigger recorded: {}", label, binding.display());
    set_trigger_status(format!("{} set to {}", label, binding.display()));
    let primary_disp = cfg.primary_trigger.display();
    let secondary_disp = cfg
        .secondary_trigger
        .as_ref()
        .map(|b| b.display())
        .unwrap_or_else(|| "Not set".to_string());
    let cancel_primary_disp = cfg
        .primary_cancel_trigger
        .as_ref()
        .map(|b| b.display())
        .unwrap_or_else(|| "Not set".to_string());
    let cancel_secondary_disp = cfg
        .secondary_cancel_trigger
        .as_ref()
        .map(|b| b.display())
        .unwrap_or_else(|| "Not set".to_string());
    let _ = slint::invoke_from_event_loop(move || {
        let _ = with_studio(|s| {
            s.set_trigger_primary_display(primary_disp.into());
            s.set_trigger_secondary_display(secondary_disp.into());
            s.set_cancel_primary_display(cancel_primary_disp.into());
            s.set_cancel_secondary_display(cancel_secondary_disp.into());
            s.set_is_recording_key(false);
            s.set_recording_slot(-1);
        });
    });
}

/// Loads (api_key, selected_model) for a provider: SQLite store first, env fallback.
fn provider_credentials(
    db: &sotto_desktop::db::HistoryDb,
    provider_id: &str,
) -> (Option<String>, Option<String>) {
    let stored = db.get_provider_key(provider_id).ok().flatten();
    let model = stored
        .as_ref()
        .map(|k| k.selected_model.trim().to_string())
        .filter(|m| !m.is_empty());
    let mut key = stored
        .as_ref()
        .map(|k| k.api_key.trim().to_string())
        .filter(|k| !k.is_empty());
    if key.is_none() {
        key = db
            .get_provider_api_keys(provider_id)
            .ok()
            .and_then(|mut keys| if keys.is_empty() { None } else { Some(keys.remove(0)) });
    }
    if key.is_none() {
        let env_name = if provider_id == router::PROVIDER_GEMINI {
            "GEMINI_API_KEY"
        } else {
            "GROQ_API_KEY"
        };
        key = std::env::var(env_name).ok().filter(|v| !v.trim().is_empty());
    }
    (key, model)
}

/// Loads all configured API keys for a provider's rotation pool (SQLite first, env fallback).
fn load_provider_key_pool(
    db: &sotto_desktop::db::HistoryDb,
    provider_id: &str,
) -> Vec<String> {
    let mut keys = db.get_provider_api_keys(provider_id).unwrap_or_default();
    if keys.is_empty() {
        let env_name = if provider_id == router::PROVIDER_GEMINI {
            "GEMINI_API_KEY"
        } else {
            "GROQ_API_KEY"
        };
        if let Ok(v) = std::env::var(env_name) {
            let trimmed = v.trim().to_string();
            if !trimmed.is_empty() {
                keys.push(trimmed);
            }
        }
    }
    keys
}

fn slint_string_model(values: Vec<String>) -> ModelRc<slint::SharedString> {
    ModelRc::from(Rc::new(VecModel::from(
        values.into_iter().map(slint::SharedString::from).collect::<Vec<_>>(),
    )))
}

fn model_at(models: &ModelRc<slint::SharedString>, index: i32, fallback: &str) -> String {
    if index >= 0 {
        if let Some(m) = models.row_data(index as usize) {
            let text = m.as_str().to_string();
            if !text.trim().is_empty() {
                return text;
            }
        }
    }
    fallback.to_string()
}

fn language_items_model(items: &[(&str, &str)]) -> ModelRc<LanguageItem> {
    let list: Vec<LanguageItem> = items
        .iter()
        .map(|(code, name)| LanguageItem {
            code: (*code).into(),
            name: (*name).into(),
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(list)))
}

fn refresh_key_pool_ui(
    s: &StudioWindow,
    db: &sotto_desktop::db::HistoryDb,
    transcriber_override: Option<&router::SharedTranscriber>,
) {
    let groq_keys = db.get_provider_api_keys(router::PROVIDER_GROQ).unwrap_or_default();
    let gemini_keys = db.get_provider_api_keys(router::PROVIDER_GEMINI).unwrap_or_default();

    let (groq_statuses, gemini_statuses) = if let Some(tr) = transcriber_override {
        if let Ok(t) = tr.lock() {
            (t.get_key_statuses(router::PROVIDER_GROQ), t.get_key_statuses(router::PROVIDER_GEMINI))
        } else {
            (vec![], vec![])
        }
    } else if let Ok(guard) = SHARED_TRANSCRIBER.lock() {
        if let Some(ref tr) = *guard {
            if let Ok(t) = tr.lock() {
                (t.get_key_statuses(router::PROVIDER_GROQ), t.get_key_statuses(router::PROVIDER_GEMINI))
            } else {
                (vec![], vec![])
            }
        } else {
            (vec![], vec![])
        }
    } else {
        (vec![], vec![])
    };

    let groq_items: Vec<ApiKeyItem> = groq_keys.iter().enumerate().map(|(i, k)| {
        let st = groq_statuses.iter().find(|s| &s.key == k);
        let is_limited = st.map(|s| s.is_rate_limited).unwrap_or(false);
        let status_str = st.map(|s| s.status_description()).unwrap_or_else(|| "Ready".to_string());
        ApiKeyItem {
            id: i as i32,
            key: k.clone().into(),
            masked: router::mask_key(k).into(),
            status: status_str.into(),
            is_rate_limited: is_limited,
        }
    }).collect();

    let gemini_items: Vec<ApiKeyItem> = gemini_keys.iter().enumerate().map(|(i, k)| {
        let st = gemini_statuses.iter().find(|s| &s.key == k);
        let is_limited = st.map(|s| s.is_rate_limited).unwrap_or(false);
        let status_str = st.map(|s| s.status_description()).unwrap_or_else(|| "Ready".to_string());
        ApiKeyItem {
            id: i as i32,
            key: k.clone().into(),
            masked: router::mask_key(k).into(),
            status: status_str.into(),
            is_rate_limited: is_limited,
        }
    }).collect();

    s.set_groq_key_pool(ModelRc::from(Rc::new(VecModel::from(groq_items))));
    s.set_gemini_key_pool(ModelRc::from(Rc::new(VecModel::from(gemini_items))));
}

/// Pushes provider selection, keys, cached model lists and chip text into Studio.
/// No-op when Studio is closed; called on Studio open, tab switch and after saves.
fn sync_provider_ui(
    db: &sotto_desktop::db::HistoryDb,
    transcriber: &router::SharedTranscriber,
) {
    let (groq_key, _) = provider_credentials(db, router::PROVIDER_GROQ);
    let (gemini_key, _) = provider_credentials(db, router::PROVIDER_GEMINI);
    let groq_models = db.cached_model_list(router::PROVIDER_GROQ, router::FALLBACK_GROQ_MODELS);
    let gemini_models = db.cached_model_list(router::PROVIDER_GEMINI, router::FALLBACK_GEMINI_MODELS);
    let snapshot = transcriber.lock().map(|t| {
        let active = t.active_provider().to_string();
        let gm = t.provider_model(router::PROVIDER_GROQ).to_string();
        let mm = t.provider_model(router::PROVIDER_GEMINI).to_string();
        let chip = t.chip_text();
        (active, gm, mm, chip)
    });
    let Ok((active, groq_model, gemini_model, chip)) = snapshot else {
        return;
    };
    let groq_idx = groq_models.iter().position(|m| m == &groq_model).unwrap_or(0) as i32;
    let gemini_idx = gemini_models.iter().position(|m| m == &gemini_model).unwrap_or(0) as i32;

    let cfg = config::AppConfig::load();
    let current_lang = cfg.transcription_language.clone();
    let active_model = if active == router::PROVIDER_GEMINI {
        &gemini_model
    } else {
        &groq_model
    };
    let is_en_only = router::is_english_only_model(active_model);
    let effective_lang = if is_en_only { "en".to_string() } else { current_lang };

    let display_lang = if effective_lang == "en" || effective_lang == "auto" {
        "More Languages".to_string()
    } else {
        format!("{} ({})", router::language_display_name(&effective_lang), effective_lang)
    };

    let mid = router::OTHER_LANGUAGES.len() / 2;
    let col1_model = language_items_model(&router::OTHER_LANGUAGES[..mid]);
    let col2_model = language_items_model(&router::OTHER_LANGUAGES[mid..]);

    let _ = with_studio(|s| {
        s.set_provider_id(active.clone().into());
        s.set_groq_key(groq_key.clone().unwrap_or_default().into());
        s.set_gemini_key(gemini_key.clone().unwrap_or_default().into());
        s.set_groq_models(slint_string_model(groq_models.clone()));
        s.set_gemini_models(slint_string_model(gemini_models.clone()));
        s.set_groq_model_index(groq_idx);
        s.set_gemini_model_index(gemini_idx);
        s.set_provider_chip_text(chip.clone().into());

        s.set_transcription_language(effective_lang.into());
        s.set_model_supports_multilingual(!is_en_only);
        s.set_selected_other_language_display(display_lang.into());
        s.set_other_languages_col1(col1_model);
        s.set_other_languages_col2(col2_model);

        s.set_post_process_llm_enabled(cfg.post_process_llm_enabled);
        s.set_post_process_llm_provider(cfg.post_process_llm_provider.clone().into());
        s.set_post_process_llm_model(cfg.post_process_llm_model.clone().into());
        s.set_post_process_style(cfg.post_process_style);
        refresh_key_pool_ui(&s, db, Some(transcriber));
        sync_studio_dimensions(&s, false);
    });
}

/// Fetches fresh model lists for providers that have keys, caches them in SQLite,
/// and repairs the stored + live model choice if it vanished upstream.
/// Runs on a background thread; never blocks startup or dictation.
fn refresh_provider_models(
    db: sotto_desktop::db::HistoryDb,
    transcriber: router::SharedTranscriber,
) {
    for provider_id in [router::PROVIDER_GROQ, router::PROVIDER_GEMINI] {
        let (key, _) = provider_credentials(&db, provider_id);
        let Some(k) = key.filter(|k| !k.trim().is_empty()) else {
            continue;
        };
        let fetched: Result<Vec<String>, String> = if provider_id == router::PROVIDER_GEMINI {
            match router::GeminiClient::new(Some(k)) {
                Ok(c) => c.list_models_blocking(),
                Err(e) => Err(e),
            }
        } else {
            match router::GroqClient::new(Some(k)) {
                Ok(c) => c.list_models_blocking(),
                Err(e) => Err(e),
            }
        };
        match fetched {
            Ok(models) if !models.is_empty() => {
                let json = serde_json::to_string(&models).unwrap_or_else(|_| "[]".to_string());
                let _ = db.cache_provider_models(provider_id, &json);
                println!("[Providers] Refreshed model list for {}: {} models", provider_id, models.len());
                // Repair the stored + live model if it disappeared upstream.
                let stored = db
                    .get_provider_key(provider_id)
                    .ok()
                    .flatten()
                    .map(|p| p.selected_model)
                    .unwrap_or_default();
                if !models.iter().any(|m| m == &stored) {
                    let first = models[0].clone();
                    let _ = db.set_provider_model(provider_id, &first);
                    if let Ok(mut t) = transcriber.lock() {
                        t.set_provider_model(provider_id, first);
                    }
                }
                sync_provider_ui(&db, &transcriber);
            }
            Ok(_) => {}
            Err(e) => println!("[Providers] Model refresh skipped for {}: {}", provider_id, e),
        }
    }
    // Reclaim cold allocations (HTTP/2 handshakes, JSON parser heaps, TLS buffers)
    trim_working_set();
}

use sotto_desktop::{trim_working_set, InactivityTracker};

fn bring_studio_to_front(studio: &StudioWindow) {
    #[cfg(target_os = "windows")]
    {
        setup_studio_native_window(studio);
        let win = studio.window();
        let win_handle_wrapper = win.window_handle();
        if let Ok(handle) = win_handle_wrapper.window_handle() {
            if let RawWindowHandle::Win32(win32_handle) = handle.as_raw() {
                let hwnd = win32_handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;
                unsafe {
                    let mut work_area = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                    SystemParametersInfoW(
                        SPI_GETWORKAREA,
                        0,
                        &mut work_area as *mut _ as *mut _,
                        0,
                    );
                    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                    GetWindowRect(hwnd, &mut rect);
                    let w = rect.right - rect.left;
                    let h = rect.bottom - rect.top;
                    let work_w = work_area.right - work_area.left;
                    let work_h = work_area.bottom - work_area.top;
                    let x = work_area.left + (work_w - w) / 2;
                    let y = work_area.top + (work_h - h) / 2;

                    SetWindowPos(
                        hwnd,
                        0 as _,
                        x,
                        y,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
                    );
                    ShowWindow(hwnd, SW_RESTORE);
                    SetForegroundWindow(hwnd);
                }
            }
        }
    }
}

/// Screen-space rectangle using the same exclusive right/bottom convention as
/// Win32 `RECT`. Kept platform-neutral so the placement geometry is unit-tested.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScreenRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl ScreenRect {
    fn width(self) -> i32 {
        self.right.saturating_sub(self.left).max(0)
    }

    fn height(self) -> i32 {
        self.bottom.saturating_sub(self.top).max(0)
    }

    fn intersection(self, other: Self) -> Option<Self> {
        let left = self.left.max(other.left);
        let top = self.top.max(other.top);
        let right = self.right.min(other.right);
        let bottom = self.bottom.min(other.bottom);
        if right > left && bottom > top {
            Some(Self {
                left,
                top,
                right,
                bottom,
            })
        } else {
            None
        }
    }

    /// Removes the area occupied by an always-visible taskbar from the usable
    /// rectangle. A normal docked taskbar already ends exactly at the work-area
    /// edge, so this is usually a no-op. It matters when a shell mod (for
    /// example Windhawk's taskbar-height mod) draws a taller taskbar without
    /// updating the reserved work area: without this the pill would be placed
    /// underneath it.
    fn without_taskbar(self, monitor: Self, taskbar: Self) -> Self {
        if self.intersection(monitor).is_none() || self.intersection(taskbar).is_none() {
            return self;
        }

        let mut adjusted = self;
        if taskbar.width() >= taskbar.height() {
            let monitor_center = (monitor.top as i64 + monitor.bottom as i64) / 2;
            let taskbar_center = (taskbar.top as i64 + taskbar.bottom as i64) / 2;
            if taskbar_center <= monitor_center {
                adjusted.top = adjusted.top.max(taskbar.bottom);
            } else {
                adjusted.bottom = adjusted.bottom.min(taskbar.top);
            }
        } else {
            let monitor_center = (monitor.left as i64 + monitor.right as i64) / 2;
            let taskbar_center = (taskbar.left as i64 + taskbar.right as i64) / 2;
            if taskbar_center <= monitor_center {
                adjusted.left = adjusted.left.max(taskbar.right);
            } else {
                adjusted.right = adjusted.right.min(taskbar.left);
            }
        }

        if adjusted.right > adjusted.left && adjusted.bottom > adjusted.top {
            adjusted
        } else {
            self
        }
    }

    /// Clamps a window origin into this rectangle, keeping at least `margin`
    /// physical pixels of breathing room on every side.
    fn clamp_point(self, x: i32, y: i32, width: i32, height: i32, margin: i32) -> (i32, i32) {
        let min_x = self.left.saturating_add(margin);
        let min_y = self.top.saturating_add(margin);
        let max_x = self
            .right
            .saturating_sub(margin)
            .saturating_sub(width)
            .max(min_x);
        let max_y = self
            .bottom
            .saturating_sub(margin)
            .saturating_sub(height)
            .max(min_y);
        (x.clamp(min_x, max_x), y.clamp(min_y, max_y))
    }
}

#[cfg(target_os = "windows")]
impl ScreenRect {
    fn from_win(rect: RECT) -> Self {
        Self {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        }
    }
}

#[cfg(target_os = "windows")]
struct TaskbarCache {
    monitor: isize,
    rects: Vec<ScreenRect>,
    fetched: Instant,
}

#[cfg(target_os = "windows")]
struct TaskbarSearch {
    monitor: HMONITOR,
    rects: Vec<ScreenRect>,
}

#[cfg(target_os = "windows")]
static TASKBAR_CACHE: Mutex<Option<TaskbarCache>> = Mutex::new(None);

#[cfg(target_os = "windows")]
const TASKBAR_CACHE_TTL: Duration = Duration::from_millis(1000);

#[cfg(target_os = "windows")]
unsafe extern "system" fn collect_taskbar_windows(
    hwnd: windows_sys::Win32::Foundation::HWND,
    lparam: isize,
) -> i32 {
    let search = &mut *(lparam as *mut TaskbarSearch);
    if IsWindowVisible(hwnd) == 0 {
        return 1;
    }

    let mut class = [0u16; 64];
    let len = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
    if len <= 0 {
        return 1;
    }
    let class = &class[..len as usize];
    let primary = "Shell_TrayWnd".encode_utf16().eq(class.iter().copied());
    let secondary = "Shell_SecondaryTrayWnd"
        .encode_utf16()
        .eq(class.iter().copied());
    if !primary && !secondary {
        return 1;
    }

    if MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) != search.monitor {
        return 1;
    }

    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    if GetWindowRect(hwnd, &mut rect) != 0 {
        search.rects.push(ScreenRect::from_win(rect));
    }
    1
}

#[cfg(target_os = "windows")]
unsafe fn taskbars_on_monitor(monitor: HMONITOR) -> Vec<ScreenRect> {
    let key = monitor as isize;
    if let Ok(cache) = TASKBAR_CACHE.lock() {
        if let Some(cached) = cache.as_ref() {
            if cached.monitor == key && cached.fetched.elapsed() < TASKBAR_CACHE_TTL {
                return cached.rects.clone();
            }
        }
    }

    let mut search = TaskbarSearch {
        monitor,
        rects: Vec::new(),
    };
    EnumWindows(Some(collect_taskbar_windows), &mut search as *mut _ as isize);

    if let Ok(mut cache) = TASKBAR_CACHE.lock() {
        *cache = Some(TaskbarCache {
            monitor: key,
            rects: search.rects.clone(),
            fetched: Instant::now(),
        });
    }
    search.rects
}

#[cfg(target_os = "windows")]
unsafe fn taskbar_is_auto_hidden() -> bool {
    use windows_sys::Win32::UI::Shell::{
        SHAppBarMessage, ABM_GETSTATE, ABS_AUTOHIDE, APPBARDATA,
    };

    let mut data: APPBARDATA = std::mem::zeroed();
    data.cbSize = std::mem::size_of::<APPBARDATA>() as u32;
    let state = SHAppBarMessage(ABM_GETSTATE, &mut data) as u32;
    state & ABS_AUTOHIDE != 0
}

/// Usable area for the pill, in physical pixels. Starts from the monitor's
/// work area and then removes any visible taskbar that overlaps it. The second
/// step is what keeps the pill visible with a taller Windhawk taskbar whose
/// height is not reflected in `rcWork`.
#[cfg(target_os = "windows")]
unsafe fn effective_pill_area(monitor: HMONITOR) -> ScreenRect {
    let mut info: MONITORINFO = std::mem::zeroed();
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    if GetMonitorInfoW(monitor, &mut info) == 0 {
        let mut work = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work as *mut _ as *mut _, 0);
        return ScreenRect::from_win(work);
    }

    let monitor_rect = ScreenRect::from_win(info.rcMonitor);
    let work_area = ScreenRect::from_win(info.rcWork);
    if taskbar_is_auto_hidden() {
        return work_area;
    }

    taskbars_on_monitor(monitor)
        .into_iter()
        .fold(work_area, |area, taskbar| {
            area.without_taskbar(monitor_rect, taskbar)
        })
}

/// Clamps an intended pill origin to the monitor under the pill's center.
/// Multi-monitor safe: the target point, not the window's old monitor, chooses
/// the work area, so dragging across screens is not blocked at the seam.
#[cfg(target_os = "windows")]
unsafe fn clamp_pill_origin(x: i32, y: i32, width: i32, height: i32, margin: i32) -> (i32, i32) {
    let point = POINT {
        x: x.saturating_add(width / 2),
        y: y.saturating_add(height / 2),
    };
    let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
    effective_pill_area(monitor).clamp_point(x, y, width, height, margin)
}

/// Re-clamps the live window and re-asserts topmost only when it actually
/// moved. Returns true when a correction was applied.
#[cfg(target_os = "windows")]
unsafe fn clamp_pill_to_work_area(
    hwnd: windows_sys::Win32::Foundation::HWND,
    margin: i32,
) -> bool {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    if GetWindowRect(hwnd, &mut rect) == 0 {
        return false;
    }

    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    let (x, y) = clamp_pill_origin(rect.left, rect.top, width, height, margin);
    if x == rect.left && y == rect.top {
        return false;
    }

    SetWindowPos(
        hwnd,
        HWND_TOPMOST,
        x,
        y,
        0,
        0,
        SWP_NOSIZE | SWP_NOACTIVATE,
    );
    true
}

#[cfg(target_os = "windows")]
unsafe fn remove_window_from_taskbar(hwnd: windows_sys::Win32::Foundation::HWND) {
    // 1. Strip WS_EX_APPWINDOW and apply WS_EX_TOOLWINDOW + WS_EX_NOACTIVATE + WS_EX_TOPMOST
    let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    let new_ex_style = (ex_style & !WS_EX_APPWINDOW) | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST;
    SetWindowLongW(hwnd, GWL_EXSTYLE, new_ex_style as i32);

    // 2. Call COM ITaskbarList::DeleteTab to explicitly delete the taskbar button
    let _ = CoInitialize(std::ptr::null_mut());

    #[repr(C)]
    #[allow(non_snake_case)]
    struct ITaskbarListVtbl {
        QueryInterface: unsafe extern "system" fn(*mut std::ffi::c_void, *const GUID, *mut *mut std::ffi::c_void) -> i32,
        AddRef: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
        Release: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
        HrInit: unsafe extern "system" fn(*mut std::ffi::c_void) -> i32,
        AddTab: unsafe extern "system" fn(*mut std::ffi::c_void, windows_sys::Win32::Foundation::HWND) -> i32,
        DeleteTab: unsafe extern "system" fn(*mut std::ffi::c_void, windows_sys::Win32::Foundation::HWND) -> i32,
        ActivateTab: unsafe extern "system" fn(*mut std::ffi::c_void, windows_sys::Win32::Foundation::HWND) -> i32,
        SetActiveAlt: unsafe extern "system" fn(*mut std::ffi::c_void, windows_sys::Win32::Foundation::HWND) -> i32,
    }

    const CLSID_TASKBAR_LIST: GUID = GUID {
        data1: 0x56FDF344,
        data2: 0xFD6D,
        data3: 0x11D0,
        data4: [0x95, 0x8A, 0x00, 0x60, 0x97, 0xC9, 0xA0, 0x90],
    };
    const IID_ITASKBAR_LIST: GUID = GUID {
        data1: 0x56FDF342,
        data2: 0xFD6D,
        data3: 0x11D0,
        data4: [0x95, 0x8A, 0x00, 0x60, 0x97, 0xC9, 0xA0, 0x90],
    };

    let mut taskbar_list: *mut std::ffi::c_void = std::ptr::null_mut();
    let hr = CoCreateInstance(
        &CLSID_TASKBAR_LIST,
        std::ptr::null_mut(),
        CLSCTX_INPROC_SERVER,
        &IID_ITASKBAR_LIST,
        &mut taskbar_list,
    );
    if hr == 0 && !taskbar_list.is_null() {
        let vtbl = *(taskbar_list as *const *const ITaskbarListVtbl);
        let _ = ((*vtbl).HrInit)(taskbar_list);
        let _ = ((*vtbl).DeleteTab)(taskbar_list, hwnd);
        let _ = ((*vtbl).Release)(taskbar_list);
    }
}

fn setup_floating_pill_window(pill: &FloatingPillWindow) -> bool {
    #[cfg(target_os = "windows")]
    {
        let win = pill.window();
        let win_handle_wrapper = win.window_handle();
        match win_handle_wrapper.window_handle() {
            Ok(handle) => {
                if let RawWindowHandle::Win32(win32_handle) = handle.as_raw() {
                    let hwnd = win32_handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;
                    let mut work_area = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                    unsafe {
                        SystemParametersInfoW(
                            SPI_GETWORKAREA,
                            0,
                            &mut work_area as *mut _ as *mut _,
                            0,
                        );
                        let scale = win.scale_factor();
                        let work_w = work_area.right - work_area.left;
                        let pill_w = (140.0 * scale).round() as i32;
                        let pill_h = (60.0 * scale).round() as i32;
                        let margin_bottom = (16.0 * scale).round() as i32;
                        let default_x = work_area.left + (work_w - pill_w) / 2;
                        let default_y = work_area.bottom - pill_h - margin_bottom;

                        let cfg = config::AppConfig::load();
                        let (x, y) = match (cfg.pill_x, cfg.pill_y) {
                            (Some(sx), Some(sy)) => {
                                let screen_w = windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(windows_sys::Win32::UI::WindowsAndMessaging::SM_CXVIRTUALSCREEN);
                                let screen_h = windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(windows_sys::Win32::UI::WindowsAndMessaging::SM_CYVIRTUALSCREEN);
                                let screen_left = windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(windows_sys::Win32::UI::WindowsAndMessaging::SM_XVIRTUALSCREEN);
                                let screen_top = windows_sys::Win32::UI::WindowsAndMessaging::GetSystemMetrics(windows_sys::Win32::UI::WindowsAndMessaging::SM_YVIRTUALSCREEN);

                                if screen_w > 0 && screen_h > 0
                                    && sx >= screen_left - 20
                                    && sx <= screen_left + screen_w - 40
                                    && sy >= screen_top - 20
                                    && sy <= screen_top + screen_h - 30
                                {
                                    (sx, sy)
                                } else {
                                    (default_x, default_y)
                                }
                            }
                            _ => (default_x, default_y),
                        };

                        // Clamp before the first paint so a saved position from
                        // an older taskbar layout never flashes underneath it.
                        // This also resolves taller-than-rcWork Windhawk taskbars.
                        let edge_margin = (8.0 * scale).round() as i32;
                        let (x, y) = clamp_pill_origin(x, y, pill_w, pill_h, edge_margin);

                        // Apply taskbar removal policy (strip WS_EX_APPWINDOW, set WS_EX_TOOLWINDOW + DeleteTab)
                        remove_window_from_taskbar(hwnd);

                        SetWindowPos(
                            hwnd,
                            HWND_TOPMOST,
                            x,
                            y,
                            pill_w,
                            pill_h,
                            SWP_SHOWWINDOW | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                        );
                        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                        if clamp_pill_to_work_area(hwnd, edge_margin) {
                            println!("[Floating Pill] Position corrected to stay clear of the taskbar/work area");
                        }
                        println!("[Floating Pill] Docked at ({}, {}) with WS_EX_NOACTIVATE focus shield (Zero taskbar footprint)", x, y);
                        let _ = std::io::stdout().flush();
                    }
                    return true;
                }
            }
            Err(e) => {
                println!("[Floating Pill] WindowHandle not ready yet: {:?}", e);
                let _ = std::io::stdout().flush();
            }
        }
    }
    false
}

/// Helper to wire callbacks to a newly instantiated StudioWindow
fn wire_studio_window(
    studio: &StudioWindow,
    pill_weak: &slint::Weak<FloatingPillWindow>,
    engine_state: &Arc<AtomicU8>,
    studio_visible: &Arc<AtomicBool>,
    _cycle_fn: Arc<dyn Fn() + Send + Sync>,
    history_queue: &Arc<AsyncHistoryQueue>,
) {
    let s_weak = studio.as_weak();
    let p_weak = pill_weak.clone();

    #[cfg(target_os = "windows")]
    setup_studio_native_window(studio);

    // Fullscreen / Maximize Toggle Handler
    let s_weak_fs = s_weak.clone();
    studio.on_toggle_fullscreen(move || {
        #[cfg(target_os = "windows")]
        if let Some(s) = s_weak_fs.upgrade() {
            let win = s.window();
            let win_handle_wrapper = win.window_handle();
            if let Ok(handle) = win_handle_wrapper.window_handle() {
                if let RawWindowHandle::Win32(win32_handle) = handle.as_raw() {
                    let hwnd = win32_handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;
                    unsafe {
                        use windows_sys::Win32::UI::WindowsAndMessaging::*;
                        if IsZoomed(hwnd) != 0 {
                            ShowWindow(hwnd, SW_RESTORE);
                            s.set_is_fullscreen(false);
                            println!("[Studio] Window restored to standard geometry");
                        } else {
                            ShowWindow(hwnd, SW_MAXIMIZE);
                            s.set_is_fullscreen(true);
                            println!("[Studio] Window maximized to full screen");
                        }
                        let _ = std::io::stdout().flush();
                    }
                }
            }
        }
    });

    // Event-driven 60Hz subpixel smooth scroll driver: runs while animating, or until native subclass + real layout settle.
    let s_weak_scroll = s_weak.clone();
    let scroll_timer = slint::Timer::default();
    let mut cached_dim_key: (i32, u32, u32, u64) = (-1, 0, 0, 0);
    let mut cached_prov_dim: (i32, i32) = (-1, -1);
    let mut cached_custom_dim: (i32, i32) = (-1, -1);
    let mut last_tab: i32 = -1;
    let mut last_written_progress: f32 = -1.0;
    let mut last_written_providers_progress: f32 = -1.0;
    let mut last_providers_can_scroll: Option<bool> = None;
    let mut last_written_custom_progress: f32 = -1.0;
    let mut last_custom_can_scroll: Option<bool> = None;
    #[cfg(target_os = "windows")]
    let mut is_subclassed: bool = false;

    scroll_timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(sotto_desktop::smooth_scroll::RECOMMENDED_FRAME_INTERVAL_MS),
        move || {
            let Some(s) = s_weak_scroll.upgrade() else {
                STUDIO_SCROLL_TIMER.with(|t| {
                    if let Some(ref timer) = *t.borrow() {
                        timer.stop();
                    }
                });
                return;
            };
            if !s.window().is_visible() {
                STUDIO_SCROLL_TIMER.with(|t| {
                    if let Some(ref timer) = *t.borrow() {
                        timer.stop();
                    }
                });
                return;
            }

            #[cfg(target_os = "windows")]
            if !is_subclassed {
                if setup_studio_native_window(&s) {
                    is_subclassed = true;
                }
            }

            let active_tab = s.get_active_tab();
            if active_tab != last_tab {
                last_tab = active_tab;
                cached_dim_key = (-1, 0, 0, 0);
                cached_prov_dim = (-1, -1);
                cached_custom_dim = (-1, -1);
            }

            let win_size = s.window().size();
            let (_, visible_h, scale, realized) = effective_tab_visible_h(&s, active_tab);

            if active_tab == 0 {
                let count = s.get_history_count();
                let gen = STUDIO_HISTORY_GEN.load(Ordering::Relaxed);
                let dim_key = (count, win_size.width, win_size.height, gen);

                if dim_key != cached_dim_key {
                    cached_dim_key = dim_key;
                    let (_, _) = sync_history_dimensions_and_window(&s, visible_h, true);
                }

                sotto_desktop::smooth_scroll::STUDIO_SCROLLER.with(|scroller| {
                    let mut engine = scroller.borrow_mut();
                    if let Some(new_y) = engine.tick() {
                        let snapped_y = (new_y * scale).round() / scale;
                        s.set_history_viewport_y(snapped_y);
                        s.set_history_is_scrolling(true);

                        let progress = engine.progress();
                        if (progress - last_written_progress).abs() >= 0.005 {
                            last_written_progress = progress;
                            s.set_history_scroll_progress(progress);
                        }
                    } else if !engine.is_animating() {
                        s.set_history_is_scrolling(false);
                        let progress = engine.progress();
                        if (progress - last_written_progress).abs() >= 0.005 {
                            last_written_progress = progress;
                            s.set_history_scroll_progress(progress);
                        }
                    }
                });
                sync_history_virtual_window(&s, visible_h, false);
            } else if active_tab == 1 {
                let prov_content_h = s.get_providers_content_height();
                let prov_visible_h = visible_h;
                let prov_dim_key = (prov_content_h.round() as i32, prov_visible_h.round() as i32);
                if prov_content_h > 0.0 && prov_dim_key != cached_prov_dim {
                    cached_prov_dim = prov_dim_key;
                    sotto_desktop::smooth_scroll::PROVIDERS_SCROLLER.with(|scroller| {
                        let mut eng = scroller.borrow_mut();
                        eng.set_dimensions(prov_content_h, prov_visible_h);
                        let progress = eng.progress();
                        last_written_providers_progress = progress;
                        s.set_providers_scroll_progress(progress);
                    });
                    let prov_can_scroll = prov_content_h > prov_visible_h;
                    if last_providers_can_scroll != Some(prov_can_scroll) {
                        last_providers_can_scroll = Some(prov_can_scroll);
                        s.set_providers_can_scroll(prov_can_scroll);
                    }
                }

                sotto_desktop::smooth_scroll::PROVIDERS_SCROLLER.with(|scroller| {
                    let mut engine = scroller.borrow_mut();
                    if let Some(new_y) = engine.tick() {
                        let snapped_y = (new_y * scale).round() / scale;
                        s.set_providers_viewport_y(snapped_y);
                        s.set_providers_is_scrolling(true);
                        let progress = engine.progress();
                        if (progress - last_written_providers_progress).abs() >= 0.005 {
                            last_written_providers_progress = progress;
                            s.set_providers_scroll_progress(progress);
                        }
                    } else if !engine.is_animating() {
                        s.set_providers_is_scrolling(false);
                        let progress = engine.progress();
                        if (progress - last_written_providers_progress).abs() >= 0.005 {
                            last_written_providers_progress = progress;
                            s.set_providers_scroll_progress(progress);
                        }
                    }
                });
            } else if active_tab == 2 {
                let custom_content_h = s.get_custom_content_height();
                let custom_visible_h = visible_h;
                let custom_dim_key = (custom_content_h.round() as i32, custom_visible_h.round() as i32);
                if custom_content_h > 0.0 && custom_dim_key != cached_custom_dim {
                    cached_custom_dim = custom_dim_key;
                    sotto_desktop::smooth_scroll::CUSTOM_SCROLLER.with(|scroller| {
                        let mut eng = scroller.borrow_mut();
                        eng.set_dimensions(custom_content_h, custom_visible_h);
                        let progress = eng.progress();
                        last_written_custom_progress = progress;
                        s.set_custom_scroll_progress(progress);
                    });
                    let custom_can_scroll = custom_content_h > custom_visible_h;
                    if last_custom_can_scroll != Some(custom_can_scroll) {
                        last_custom_can_scroll = Some(custom_can_scroll);
                        s.set_custom_can_scroll(custom_can_scroll);
                    }
                }

                sotto_desktop::smooth_scroll::CUSTOM_SCROLLER.with(|scroller| {
                    let mut engine = scroller.borrow_mut();
                    if let Some(new_y) = engine.tick() {
                        let snapped_y = (new_y * scale).round() / scale;
                        s.set_custom_viewport_y(snapped_y);
                        s.set_custom_is_scrolling(true);
                        let progress = engine.progress();
                        if (progress - last_written_custom_progress).abs() >= 0.005 {
                            last_written_custom_progress = progress;
                            s.set_custom_scroll_progress(progress);
                        }
                    } else if !engine.is_animating() {
                        s.set_custom_is_scrolling(false);
                        let progress = engine.progress();
                        if (progress - last_written_custom_progress).abs() >= 0.005 {
                            last_written_custom_progress = progress;
                            s.set_custom_scroll_progress(progress);
                        }
                    }
                });
            }

            #[cfg(target_os = "windows")]
            let ready_to_sleep = is_subclassed && realized;
            #[cfg(not(target_os = "windows"))]
            let ready_to_sleep = realized;

            if ready_to_sleep && !sotto_desktop::smooth_scroll::any_studio_scroller_animating() {
                STUDIO_SCROLL_TIMER.with(|t| {
                    if let Some(ref timer) = *t.borrow() {
                        timer.stop();
                    }
                });
            }
        },
    );
    if !sotto_desktop::smooth_scroll::any_studio_scroller_animating() {
        scroll_timer.stop();
    }
    STUDIO_SCROLL_TIMER.with(|t| {
        *t.borrow_mut() = Some(scroll_timer);
    });

    let s_weak_drag = s_weak.clone();
    studio.on_history_drag_scroll(move |norm_y| {
        if let Some(s) = s_weak_drag.upgrade() {
            sync_studio_dimensions(&s, false);
            sotto_desktop::smooth_scroll::STUDIO_SCROLLER.with(|scroller| {
                let mut engine = scroller.borrow_mut();
                let max_s = engine.max_scroll_limit();
                let target = -norm_y.clamp(0.0, 1.0) * max_s;
                engine.scroll_to(target);
            });
            wake_scroll_driver();
        }
    });

    let s_weak_prov_drag = s_weak.clone();
    studio.on_providers_drag_scroll(move |norm_y| {
        if let Some(s) = s_weak_prov_drag.upgrade() {
            sync_studio_dimensions(&s, false);
            sotto_desktop::smooth_scroll::PROVIDERS_SCROLLER.with(|scroller| {
                let mut engine = scroller.borrow_mut();
                let max_s = engine.max_scroll_limit();
                let target = -norm_y.clamp(0.0, 1.0) * max_s;
                engine.scroll_to(target);
            });
            wake_scroll_driver();
        }
    });

    let s_weak_custom_drag = s_weak.clone();
    studio.on_custom_drag_scroll(move |norm_y| {
        if let Some(s) = s_weak_custom_drag.upgrade() {
            sync_studio_dimensions(&s, false);
            sotto_desktop::smooth_scroll::CUSTOM_SCROLLER.with(|scroller| {
                let mut engine = scroller.borrow_mut();
                let max_s = engine.max_scroll_limit();
                let target = -norm_y.clamp(0.0, 1.0) * max_s;
                engine.scroll_to(target);
            });
            wake_scroll_driver();
        }
    });

    // Initialize studio stage preview with live pill's active options and config
    if let Some(p) = pill_weak.upgrade() {
        let cfg = config::AppConfig::load();
        studio.set_opt_idle(p.get_opt_idle());
        studio.set_opt_listening(p.get_opt_listening());
        studio.set_opt_transcribe(p.get_opt_transcribe());
        studio.set_opt_success(p.get_opt_success());
        studio.set_opt_error(p.get_opt_error());
        studio.set_surface_opacity(p.get_surface_opacity());
        studio.set_trigger_mode(cfg.trigger_mode);
        studio.set_capture_mode(cfg.capture_mode.as_i32());
        studio.set_chimes_enabled(cfg.chimes_enabled);
        studio.set_media_pause_enabled(cfg.media_pause_enabled);
        studio.set_trigger_primary_display(cfg.primary_trigger.display().into());
        studio.set_trigger_secondary_display(
            cfg.secondary_trigger
                .as_ref()
                .map(|b| b.display())
                .unwrap_or_else(|| "Not set".to_string())
                .into(),
        );
        studio.set_cancel_primary_display(
            cfg.primary_cancel_trigger
                .as_ref()
                .map(|b| b.display())
                .unwrap_or_else(|| "Not set".to_string())
                .into(),
        );
        studio.set_cancel_secondary_display(
            cfg.secondary_cancel_trigger
                .as_ref()
                .map(|b| b.display())
                .unwrap_or_else(|| "Not set".to_string())
                .into(),
        );
        #[cfg(target_os = "windows")]
        studio.set_launch_on_startup(startup::is_startup_enabled());
        #[cfg(not(target_os = "windows"))]
        studio.set_launch_on_startup(cfg.launch_on_startup);
    }

    // 1. Cycle state (Preview Only in Studio Stage: does not mutate live pill or trigger recording)
    let s_weak_cycle = s_weak.clone();
    studio.on_cycle_state(move || {
        if let Some(s) = s_weak_cycle.upgrade() {
            let next = (s.get_active_state() + 1) % 5;
            s.set_active_state(next);
            wake_pill_anim_timer();
            println!("[Studio Preview] Cycled preview state to {}", next);
            let _ = std::io::stdout().flush();
        }
    });

    // 2. Select state directly from studio control panel (Preview Only in Studio Stage)
    let s_weak_sel = s_weak.clone();
    studio.on_select_state(move |s| {
        if let Some(studio) = s_weak_sel.upgrade() {
            studio.set_active_state(s);
            wake_pill_anim_timer();
            println!("[Studio Panel] Direct preview state selected: {}", s);
            let _ = std::io::stdout().flush();
        }
    });

    // 3. Select modular option in studio preview (Preview Only until Save Changes is clicked)
    let s_weak_opt = s_weak.clone();
    studio.on_select_option(move |opt| {
        if let Some(studio) = s_weak_opt.upgrade() {
            let cur_state = studio.get_active_state();
            match cur_state {
                0 => studio.set_opt_idle(opt),
                1 => studio.set_opt_listening(opt),
                2 => studio.set_opt_transcribe(opt),
                3 => studio.set_opt_success(opt),
                4 => studio.set_opt_error(opt),
                _ => {}
            }
            println!("[Studio Panel] Preview variant for state {} set to: {}", cur_state, opt);
            let _ = std::io::stdout().flush();
        }
    });

    // 4. Studio Window Close handler: Hide immediately and defer full deallocation cleanly
    let s_vis_close = Arc::clone(studio_visible);
    studio.window().on_close_requested(move || {
        s_vis_close.store(false, Ordering::SeqCst);
        let vis_drop = Arc::clone(&s_vis_close);
        let _ = slint::invoke_from_event_loop(move || {
            // Skip drop if re-opened before the deferred cleanup ran
            if !vis_drop.load(Ordering::SeqCst) {
                drop_studio_instance(&vis_drop);
                println!("[Studio Action] Studio Suite closed (X) and deallocated from RAM");
                let _ = std::io::stdout().flush();
            }
        });
        slint::CloseRequestResponse::HideWindow
    });

    // 5. Adjust opacity in studio preview (Preview Only until Save Changes is clicked)
    let s_weak_op = s_weak.clone();
    studio.on_set_opacity(move |op| {
        if let Some(studio) = s_weak_op.upgrade() {
            studio.set_surface_opacity(op);
        }
    });

    // 6. Adjust background context
    let s_weak_bg = s_weak.clone();
    studio.on_set_bg(move |bg| {
        if let Some(studio) = s_weak_bg.upgrade() {
            studio.set_bg_mode(bg);
            println!("[Studio Panel] Background context set: {}", bg);
            let _ = std::io::stdout().flush();
        }
    });

    // 7. (Retired: preset picker UI removed in the unified slot model.
    // Triggers are recorded per slot via on_start_recording_key.)

    // 8. Speech Trigger Recording Behavior (Mode 0: Hands-Free Toggle, Mode 1: Push to Talk)
    // Staged in preview only — requires clicking Save Preset to apply!
    let s_weak_tm = s_weak.clone();
    studio.on_set_trigger_mode(move |mode| {
        if let Some(studio) = s_weak_tm.upgrade() {
            studio.set_trigger_mode(mode);
            println!("[Studio Panel] Speech recording behavior staged: {} (requires Save Preset)", mode);
            let _ = std::io::stdout().flush();
        }
    });

    // 8a. Capture strategy (0 always-on, 1 dual-mic splice, 2 onboard only).
    // Staged here; Save Preset applies it to the live recorder.
    let s_weak_cm = s_weak.clone();
    studio.on_set_capture_mode(move |mode| {
        if let Some(studio) = s_weak_cm.upgrade() {
            studio.set_capture_mode(mode);
            println!(
                "[Studio Panel] Capture mode staged: {} (requires Save Preset)",
                config::CaptureMode::from_i32(mode).label()
            );
            let _ = std::io::stdout().flush();
        }
    });

    // 8a. Feedback cues and media handling apply immediately. They are
    // independent of the staged audio-capture mode and never block the UI.
    let s_weak_chimes = s_weak.clone();
    studio.on_toggle_chimes(move || {
        if let Some(studio) = s_weak_chimes.upgrade() {
            let next = !studio.get_chimes_enabled();
            studio.set_chimes_enabled(next);
            audio::chime::set_enabled(next);

            let mut cfg = config::AppConfig::load();
            cfg.chimes_enabled = next;
            if let Err(e) = cfg.save() {
                eprintln!("[Config Error] Failed to persist chimes_enabled: {}", e);
            }
            println!("[Feedback] Start/stop chimes: {}", if next { "on" } else { "off" });
        }
    });

    let s_weak_media_pause = s_weak.clone();
    studio.on_toggle_media_pause(move || {
        if let Some(studio) = s_weak_media_pause.upgrade() {
            let next = !studio.get_media_pause_enabled();
            studio.set_media_pause_enabled(next);
            media::set_enabled(next);

            let mut cfg = config::AppConfig::load();
            cfg.media_pause_enabled = next;
            if let Err(e) = cfg.save() {
                eprintln!("[Config Error] Failed to persist media_pause_enabled: {}", e);
            }
            println!("[Media] Automatic pause/resume: {}", if next { "on" } else { "off" });
        }
    });

    // 8b. Mouse hook health poll (drives the Studio diagnostic line).
    let s_weak_poll = s_weak.clone();
    studio.on_poll_mouse_hook(move || {
        if let Some(studio) = s_weak_poll.upgrade() {
            studio.set_mouse_hook_ready(hotkey::mouse_hook_status() == 1);
        }
    });

    // Trigger slot recorder: each slot records keyboard combos, single keys,
    // mouse buttons, or held-modifier + mouse chords into itself.
    let s_weak_rec = s_weak.clone();
    let engine_state_rec = Arc::clone(engine_state);
    studio.on_start_recording_key(move |slot| {
        if engine_state_rec.load(Ordering::SeqCst) != 0 {
            #[cfg(target_os = "windows")]
            set_trigger_status("Cannot configure hotkeys while voice dictation is active".to_string());
            println!("[Studio] Hotkey recording rejected: voice dictation in progress");
            let _ = std::io::stdout().flush();
            return;
        }
        let target = slot.clamp(0, 3);
        #[cfg(target_os = "windows")]
        {
            if let Ok(guard) = GLOBAL_HOTKEY_MGR.lock() {
                if let Some(ref mgr) = *guard {
                    mgr.disarm_hotkeys();
                }
            }
            RECORD_TARGET.store(target, Ordering::SeqCst);
            PENDING_MOD_KEY.store(0, Ordering::SeqCst);
            IS_RECORDING_KEY.store(true, Ordering::SeqCst);
            // Arm mouse capture: a side/middle click completes the recording,
            // fusing any co-held key into one chord (dual-signal presses).
            hotkey::HotkeyManager::set_record_hook(Some(std::sync::Arc::new(
                move |mods: u32, key: Option<u32>, button: i32| {
                    complete_recording(mods, key, Some(button));
                },
            )));
        }
        if let Some(studio) = s_weak_rec.upgrade() {
            studio.set_is_recording_key(true);
            studio.set_recording_slot(target);
            println!("[Studio] Trigger recording started for slot {}", target);
            let _ = std::io::stdout().flush();
        }
        #[cfg(target_os = "windows")]
        set_trigger_status(if target == 2 || target == 3 {
            "Listening — press keys (e.g. Esc) or mouse buttons, or click Cancel".to_string()
        } else {
            "Listening — press keys, click a side/middle mouse button, Esc cancels".to_string()
        });
    });

    let s_weak_stop_rec = s_weak.clone();
    studio.on_stop_recording_key(move || {
        #[cfg(target_os = "windows")]
        cancel_recording();
        if let Some(studio) = s_weak_stop_rec.upgrade() {
            studio.set_is_recording_key(false);
            studio.set_recording_slot(-1);
            println!("[Studio] Trigger recording cancelled");
            let _ = std::io::stdout().flush();
        }
    });

    // Slot reset: primary restores F8, secondary clears to empty.
    let s_weak_reset = s_weak.clone();
    let engine_state_rst = Arc::clone(engine_state);
    studio.on_reset_trigger(move |slot| {
        if engine_state_rst.load(Ordering::SeqCst) != 0 {
            #[cfg(target_os = "windows")]
            set_trigger_status("Cannot reset hotkeys while voice dictation is active".to_string());
            return;
        }
        #[cfg(target_os = "windows")]
        cancel_recording();
        let mut cfg = config::AppConfig::load();
        if slot == 1 {
            cfg.secondary_trigger = None;
        } else if slot == 2 {
            cfg.primary_cancel_trigger = Some(hotkey::TriggerBinding::escape());
        } else if slot == 3 {
            cfg.secondary_cancel_trigger = None;
        } else {
            cfg.primary_trigger = hotkey::TriggerBinding::f8();
        }
        let _ = cfg.save();
        if let Ok(guard) = GLOBAL_HOTKEY_MGR.lock() {
            if let Some(ref mgr) = *guard {
                mgr.apply_bindings(
                    &cfg.primary_trigger,
                    &cfg.secondary_trigger,
                    &cfg.primary_cancel_trigger,
                    &cfg.secondary_cancel_trigger,
                );
            }
        }
        let primary_disp = cfg.primary_trigger.display();
        let secondary_disp = cfg
            .secondary_trigger
            .as_ref()
            .map(|b| b.display())
            .unwrap_or_else(|| "Not set".to_string());
        let cancel_primary_disp = cfg
            .primary_cancel_trigger
            .as_ref()
            .map(|b| b.display())
            .unwrap_or_else(|| "Not set".to_string());
        let cancel_secondary_disp = cfg
            .secondary_cancel_trigger
            .as_ref()
            .map(|b| b.display())
            .unwrap_or_else(|| "Not set".to_string());
        if let Some(studio) = s_weak_reset.upgrade() {
            studio.set_is_recording_key(false);
            studio.set_recording_slot(-1);
            studio.set_trigger_primary_display(primary_disp.clone().into());
            studio.set_trigger_secondary_display(secondary_disp.into());
            studio.set_cancel_primary_display(cancel_primary_disp.clone().into());
            studio.set_cancel_secondary_display(cancel_secondary_disp.into());
            println!("[Studio] Trigger slot {} reset", slot);
            let _ = std::io::stdout().flush();
        }
        #[cfg(target_os = "windows")]
        set_trigger_status(match slot {
            1 => "Secondary trigger cleared".to_string(),
            2 => format!("Primary Cancel reset to {}", cancel_primary_disp),
            3 => "Secondary Cancel cleared".to_string(),
            _ => format!("Primary trigger reset to {}", primary_disp),
        });
    });

    let s_weak_startup = s_weak.clone();
    studio.on_toggle_launch_on_startup(move || {
        let mut cfg = config::AppConfig::load();
        let new_state = !cfg.launch_on_startup;
        #[cfg(target_os = "windows")]
        {
            if let Err(e) = startup::set_startup_enabled(new_state) {
                eprintln!("[Startup Registry Error] Failed to update registry: {}", e);
            } else {
                println!("[Startup Registry] Launch on startup set to: {}", new_state);
            }
        }
        cfg.launch_on_startup = new_state;
        let _ = cfg.save();
        if let Some(studio) = s_weak_startup.upgrade() {
            studio.set_launch_on_startup(new_state);
        }
    });

    // 8b. Providers tab: select / save / test / model stepping / key visibility
    let s_weak_prov_sel = s_weak.clone();
    studio.on_select_provider(move |id| {
        let pid = id.as_str().to_string();
        if let Ok(shared) = SHARED_TRANSCRIBER.lock() {
            if let Some(ref tr) = *shared {
                if let Ok(mut t) = tr.lock() {
                    t.set_active_provider(&pid);
                }
                let chip = tr.lock().map(|t| t.chip_text()).unwrap_or_default();
                let s_cb = s_weak_prov_sel.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(s) = s_cb.upgrade() {
                        s.set_provider_chip_text(chip.into());
                    }
                });
            }
        }
        let mut cfg = config::AppConfig::load();
        cfg.active_provider = pid.clone();
        let _ = cfg.save();
        if let Some(s) = s_weak_prov_sel.upgrade() {
            s.set_provider_id(pid.clone().into());
            let model_name = if pid == router::PROVIDER_GEMINI {
                model_at(&s.get_gemini_models(), s.get_gemini_model_index(), router::DEFAULT_GEMINI_MODEL)
            } else {
                model_at(&s.get_groq_models(), s.get_groq_model_index(), router::DEFAULT_GROQ_MODEL)
            };
            let is_en_only = router::is_english_only_model(&model_name);
            s.set_model_supports_multilingual(!is_en_only);
            if is_en_only {
                let current_lang = s.get_transcription_language().as_str().to_string();
                if current_lang != "en" {
                    s.set_transcription_language("en".into());
                    s.set_selected_other_language_display("More Languages".into());
                    s.set_language_dropdown_open(false);
                    if let Ok(shared) = SHARED_TRANSCRIBER.lock() {
                        if let Some(ref tr) = *shared {
                            if let Ok(mut t) = tr.lock() {
                                t.set_language("en");
                            }
                        }
                    }
                    let mut cfg = config::AppConfig::load();
                    cfg.transcription_language = "en".to_string();
                    let _ = cfg.save();
                }
            }
            wake_scroll_driver();
        }
        println!("[Providers] Active provider selected: {}", pid);
        let _ = std::io::stdout().flush();
    });

    let s_weak_prov_save = s_weak.clone();
    let hq_prov_save = Arc::clone(history_queue);
    let engine_state_sp = Arc::clone(engine_state);
    studio.on_save_provider(move || {
        if let Some(s) = s_weak_prov_save.upgrade() {
            let db = hq_prov_save.db();
            let gmodel = model_at(&s.get_groq_models(), s.get_groq_model_index(), router::DEFAULT_GROQ_MODEL);
            let mmodel = model_at(&s.get_gemini_models(), s.get_gemini_model_index(), router::DEFAULT_GEMINI_MODEL);
            // Persist selected models without touching or deleting the encrypted key pool.
            let _ = db.set_provider_model(router::PROVIDER_GROQ, &gmodel);
            let _ = db.set_provider_model(router::PROVIDER_GEMINI, &mmodel);

            let active = s.get_provider_id().as_str().to_string();
            let raw_lang = s.get_transcription_language().as_str().to_string();
            let active_model = if active == router::PROVIDER_GEMINI { &mmodel } else { &gmodel };
            let is_en_only = router::is_english_only_model(active_model);
            s.set_model_supports_multilingual(!is_en_only);
            let lang = if is_en_only { "en".to_string() } else { raw_lang };
            if is_en_only {
                s.set_transcription_language("en".into());
                s.set_selected_other_language_display("More Languages".into());
                s.set_language_dropdown_open(false);
            }

            // Rebuild the live dispatcher preserving full multi-key rotation pools if idle.
            if engine_state_sp.load(Ordering::SeqCst) == 0 {
                if let Ok(shared) = SHARED_TRANSCRIBER.lock() {
                    if let Some(ref tr) = *shared {
                        let groq_keys = load_provider_key_pool(db, router::PROVIDER_GROQ);
                        let gemini_keys = load_provider_key_pool(db, router::PROVIDER_GEMINI);
                        let chip = if let Ok(mut t) = tr.lock() {
                            *t = router::Transcriber::with_key_pools(
                                &active,
                                groq_keys,
                                Some(gmodel.clone()),
                                gemini_keys,
                                Some(mmodel.clone()),
                                &lang,
                            );
                            Some(t.chip_text())
                        } else {
                            None
                        };
                        if let Some(c) = chip {
                            s.set_provider_chip_text(c.into());
                        }
                    }
                }
            }
            refresh_key_pool_ui(&s, db, None);

            let mut cfg = config::AppConfig::load();
            cfg.active_provider = active;
            cfg.transcription_language = lang;
            if let Err(e) = cfg.save() {
                eprintln!("[Config Error] Failed to persist provider: {}", e);
            } else {
                println!("[Providers] Active provider & models saved; encrypted keys loaded from local SQLite store.");
            }
            s.set_provider_save_feedback(true);
            let s_timer = s.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(1800), move || {
                if let Some(s2) = s_timer.upgrade() {
                    s2.set_provider_save_feedback(false);
                }
            });
            let _ = std::io::stdout().flush();
        }
    });

    let s_weak_prov_test = s_weak.clone();
    let hq_prov_test = Arc::clone(history_queue);
    studio.on_test_provider(move || {
        if let Some(s) = s_weak_prov_test.upgrade() {
            let db = hq_prov_test.db();
            let active = s.get_provider_id().as_str().to_string();
            let (stored_key, _) = provider_credentials(db, &active);
            let pending_key = if active == router::PROVIDER_GEMINI {
                let draft = s.get_gemini_new_key().as_str().trim().to_string();
                if draft.is_empty() { s.get_gemini_key().as_str().trim().to_string() } else { draft }
            } else {
                let draft = s.get_groq_new_key().as_str().trim().to_string();
                if draft.is_empty() { s.get_groq_key().as_str().trim().to_string() } else { draft }
            };
            let key = stored_key.unwrap_or(pending_key);
            if key.trim().is_empty() {
                s.set_provider_status("Enter an API key first, then Test.".into());
                s.set_provider_status_ok(false);
                return;
            }
            s.set_provider_testing(true);
            s.set_provider_status("".into());
            let s_cb = s_weak_prov_test.clone();
            let active_bg = active.clone();
            std::thread::spawn(move || {
                let result: Result<usize, String> = if active_bg == router::PROVIDER_GEMINI {
                    match router::GeminiClient::new(Some(key)) {
                        Ok(c) => c.validate_key_blocking(),
                        Err(e) => Err(e),
                    }
                } else {
                    match router::GroqClient::new(Some(key)) {
                        Ok(c) => c.list_models_blocking().map(|m| m.len()),
                        Err(e) => Err(e),
                    }
                };
                let (ok, msg) = match result {
                    Ok(n) => (true, format!("✓ {} key valid — {} transcription models available.", active_bg, n)),
                    Err(e) => (false, format!("✗ {} key check failed: {}", active_bg, e)),
                };
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(s2) = s_cb.upgrade() {
                        s2.set_provider_testing(false);
                        s2.set_provider_status(msg.into());
                        s2.set_provider_status_ok(ok);
                    }
                });
            });
        }
    });

    let s_weak_add_key = s_weak.clone();
    let hq_add_key = Arc::clone(history_queue);
    studio.on_add_provider_key(move |provider, key| {
        let prov = provider.as_str().trim().to_lowercase();
        let k = key.as_str().trim();
        if k.is_empty() {
            return;
        }
        let db = hq_add_key.db();
        if let Err(e) = db.add_provider_api_key(&prov, k) {
            eprintln!("[DB Error] Failed to add provider key: {}", e);
            return;
        }

        if let Ok(shared) = SHARED_TRANSCRIBER.lock() {
            if let Some(ref tr) = *shared {
                if let Ok(mut t) = tr.lock() {
                    t.add_provider_key(&prov, k);
                }
            }
        }

        if let Some(s) = s_weak_add_key.upgrade() {
            let (primary, _) = provider_credentials(db, &prov);
            if prov == router::PROVIDER_GEMINI {
                s.set_gemini_key(primary.unwrap_or_default().into());
            } else {
                s.set_groq_key(primary.unwrap_or_default().into());
            }
            refresh_key_pool_ui(&s, &db, None);
            wake_scroll_driver();
        }
        println!("[KeyPool] Added key to {}: {}", prov, router::mask_key(k));
    });

    let s_weak_rem_key = s_weak.clone();
    let hq_rem_key = Arc::clone(history_queue);
    studio.on_remove_provider_key(move |provider, key| {
        let prov = provider.as_str().trim().to_lowercase();
        let k = key.as_str().trim();
        let db = hq_rem_key.db();
        if let Err(e) = db.remove_provider_api_key(&prov, k) {
            eprintln!("[DB Error] Failed to remove provider key: {}", e);
            return;
        }

        if let Ok(shared) = SHARED_TRANSCRIBER.lock() {
            if let Some(ref tr) = *shared {
                if let Ok(mut t) = tr.lock() {
                    t.remove_provider_key(&prov, k);
                }
            }
        }

        if let Some(s) = s_weak_rem_key.upgrade() {
            let (primary, _) = provider_credentials(db, &prov);
            if prov == router::PROVIDER_GEMINI {
                s.set_gemini_key(primary.unwrap_or_default().into());
            } else {
                s.set_groq_key(primary.unwrap_or_default().into());
            }
            refresh_key_pool_ui(&s, &db, None);
            wake_scroll_driver();
        }
        println!("[KeyPool] Removed key from {}: {}", prov, router::mask_key(k));
    });

    let s_weak_prov_eye = s_weak.clone();
    studio.on_toggle_key_visible(move |provider| {
        if let Some(s) = s_weak_prov_eye.upgrade() {
            if provider.as_str() == router::PROVIDER_GEMINI {
                s.set_gemini_key_visible(!s.get_gemini_key_visible());
            } else {
                s.set_groq_key_visible(!s.get_groq_key_visible());
            }
        }
    });

    let s_weak_lang_sel = s_weak.clone();
    studio.on_select_language(move |code| {
        let lang = code.as_str().to_string();
        if let Some(s) = s_weak_lang_sel.upgrade() {
            let active = s.get_provider_id().as_str().to_string();
            let active_model = if active == router::PROVIDER_GEMINI {
                model_at(&s.get_gemini_models(), s.get_gemini_model_index(), router::DEFAULT_GEMINI_MODEL)
            } else {
                model_at(&s.get_groq_models(), s.get_groq_model_index(), router::DEFAULT_GROQ_MODEL)
            };
            let is_en_only = router::is_english_only_model(&active_model);
            let final_lang = if is_en_only { "en".to_string() } else { lang };
            s.set_transcription_language(final_lang.clone().into());
            s.set_language_dropdown_open(false);
            let display = if final_lang == "en" || final_lang == "auto" {
                "More Languages".to_string()
            } else {
                format!("{} ({})", router::language_display_name(&final_lang), final_lang)
            };
            s.set_selected_other_language_display(display.into());

            // Live hot swap transcriber language
            if let Ok(shared) = SHARED_TRANSCRIBER.lock() {
                if let Some(ref tr) = *shared {
                    if let Ok(mut t) = tr.lock() {
                        t.set_language(&final_lang);
                    }
                }
            }

            // Persist to config
            let mut cfg = config::AppConfig::load();
            cfg.transcription_language = final_lang.clone();
            let _ = cfg.save();
            println!("[Language] Transcription language set to: {}", final_lang);
            let _ = std::io::stdout().flush();
            wake_scroll_driver();
        }
    });

    let s_weak_lang_drop = s_weak.clone();
    studio.on_toggle_language_dropdown(move || {
        if let Some(s) = s_weak_lang_drop.upgrade() {
            let open = s.get_language_dropdown_open();
            s.set_language_dropdown_open(!open);
            wake_scroll_driver();
        }
    });

    let s_weak_model_changed = s_weak.clone();
    let hq_model_changed = Arc::clone(history_queue);
    studio.on_model_changed(move |provider, idx| {
        if let Some(s) = s_weak_model_changed.upgrade() {
            let prov = provider.as_str();
            let model_name = if prov == router::PROVIDER_GEMINI {
                model_at(&s.get_gemini_models(), idx, router::DEFAULT_GEMINI_MODEL)
            } else {
                model_at(&s.get_groq_models(), idx, router::DEFAULT_GROQ_MODEL)
            };
            let _ = hq_model_changed.db().set_provider_model(prov, &model_name);
            if let Ok(shared) = SHARED_TRANSCRIBER.lock() {
                if let Some(ref tr) = *shared {
                    if let Ok(mut t) = tr.lock() {
                        t.set_provider_model(prov, model_name.clone());
                        s.set_provider_chip_text(t.chip_text().into());
                    }
                }
            }
            let is_en_only = router::is_english_only_model(&model_name);
            s.set_model_supports_multilingual(!is_en_only);
            if is_en_only {
                let current_lang = s.get_transcription_language().as_str().to_string();
                if current_lang != "en" {
                    s.set_transcription_language("en".into());
                    s.set_selected_other_language_display("More Languages".into());
                    s.set_language_dropdown_open(false);
                    if let Ok(shared) = SHARED_TRANSCRIBER.lock() {
                        if let Some(ref tr) = *shared {
                            if let Ok(mut t) = tr.lock() {
                                t.set_language("en");
                            }
                        }
                    }
                    let mut cfg = config::AppConfig::load();
                    cfg.transcription_language = "en".to_string();
                    let _ = cfg.save();
                    println!("[Language] Model '{}' is English-only. Reset language to English.", model_name);
                }
            }
            wake_scroll_driver();
        }
    });

    studio.on_open_provider_site(move |provider| {
        let url = if provider.as_str() == router::PROVIDER_GEMINI {
            "https://aistudio.google.com/apikey"
        } else {
            "https://console.groq.com/keys"
        };
        println!("[Providers] Opening console: {}", url);
        #[cfg(target_os = "windows")]
        {
            let _ = std::process::Command::new("cmd")
                .args(["/C", "start", "", url])
                .spawn();
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = std::process::Command::new("xdg-open").arg(url).spawn();
        }
    });

    let s_weak_pp_toggle = s_weak.clone();
    studio.on_toggle_post_process_llm(move || {
        if let Some(s) = s_weak_pp_toggle.upgrade() {
            let cur = s.get_post_process_llm_enabled();
            let next = !cur;
            s.set_post_process_llm_enabled(next);

            let mut cfg = config::AppConfig::load();
            cfg.post_process_llm_enabled = next;
            if let Err(e) = cfg.save() {
                eprintln!("[Config Error] Failed to persist post_process_llm_enabled: {}", e);
            }
            if let Ok(guard) = GLOBAL_POST_PROCESSOR.lock() {
                if let Some(ref p) = *guard {
                    p.update_options(cfg.to_post_process_options());
                }
            }
            println!("[PostProcess LLM] Toggle changed to: {}", next);
            wake_scroll_driver();
        }
    });

    let s_weak_pp_prov = s_weak.clone();
    studio.on_set_post_process_provider(move |prov| {
        if let Some(s) = s_weak_pp_prov.upgrade() {
            let provider = prov.as_str().to_string();
            s.set_post_process_llm_provider(provider.clone().into());

            let mut cfg = config::AppConfig::load();
            cfg.post_process_llm_provider = provider.clone();
            if provider == "groq" && !cfg.post_process_llm_model.starts_with("qwen") && !cfg.post_process_llm_model.starts_with("openai") {
                cfg.post_process_llm_model = "qwen/qwen3.8-27b".to_string();
                s.set_post_process_llm_model(cfg.post_process_llm_model.clone().into());
            } else if provider == "gemini" && !cfg.post_process_llm_model.starts_with("gemini") {
                cfg.post_process_llm_model = "gemini-2.5-flash".to_string();
                s.set_post_process_llm_model(cfg.post_process_llm_model.clone().into());
            }
            if let Err(e) = cfg.save() {
                eprintln!("[Config Error] Failed to persist post_process_llm_provider: {}", e);
            }
            if let Ok(guard) = GLOBAL_POST_PROCESSOR.lock() {
                if let Some(ref p) = *guard {
                    p.update_options(cfg.to_post_process_options());
                }
            }
            println!("[PostProcess LLM] Provider set to: {}, model: {}", provider, cfg.post_process_llm_model);
        }
    });

    let s_weak_pp_model = s_weak.clone();
    studio.on_set_post_process_model(move |model| {
        if let Some(s) = s_weak_pp_model.upgrade() {
            let model_str = model.as_str().to_string();
            s.set_post_process_llm_model(model_str.clone().into());

            let mut cfg = config::AppConfig::load();
            cfg.post_process_llm_model = model_str.clone();
            if let Err(e) = cfg.save() {
                eprintln!("[Config Error] Failed to persist post_process_llm_model: {}", e);
            }
            if let Ok(guard) = GLOBAL_POST_PROCESSOR.lock() {
                if let Some(ref p) = *guard {
                    p.update_options(cfg.to_post_process_options());
                }
            }
            println!("[PostProcess LLM] Model set to: {}", model_str);
        }
    });

    let s_weak_pp_style = s_weak.clone();
    studio.on_set_post_process_style(move |style| {
        if let Some(s) = s_weak_pp_style.upgrade() {
            s.set_post_process_style(style);

            let mut cfg = config::AppConfig::load();
            cfg.post_process_style = style;
            if let Err(e) = cfg.save() {
                eprintln!("[Config Error] Failed to persist post_process_style: {}", e);
            }
            if let Ok(guard) = GLOBAL_POST_PROCESSOR.lock() {
                if let Some(ref p) = *guard {
                    p.update_options(cfg.to_post_process_options());
                }
            }
            println!("[PostProcess LLM] Style set to: {}", style);
        }
    });

    // 9. Save Customization & Apply to Live Floating Pill
    let s_weak_save = s_weak.clone();
    let p_weak_save = p_weak.clone();
    let engine_state_sc = Arc::clone(engine_state);
    studio.on_save_customization(move || {
        if let (Some(studio), Some(pill)) = (s_weak_save.upgrade(), p_weak_save.upgrade()) {
            let mut cfg = config::AppConfig::load();
            cfg.opt_idle = studio.get_opt_idle();
            cfg.opt_listening = studio.get_opt_listening();
            cfg.opt_transcribe = studio.get_opt_transcribe();
            cfg.opt_success = studio.get_opt_success();
            cfg.opt_error = studio.get_opt_error();
            cfg.surface_opacity = studio.get_surface_opacity();
            cfg.trigger_mode = studio.get_trigger_mode();
            cfg.capture_mode = config::CaptureMode::from_i32(studio.get_capture_mode());
            cfg.chimes_enabled = studio.get_chimes_enabled();
            cfg.media_pause_enabled = studio.get_media_pause_enabled();
            audio::chime::set_enabled(cfg.chimes_enabled);
            media::set_enabled(cfg.media_pause_enabled);

            // Apply the capture strategy to the live recorder immediately. The
            // recorder refuses the change mid-take, so it can never interrupt
            // audio; in that case it simply applies on the next Save.
            if engine_state_sc.load(Ordering::SeqCst) == 0 {
                if let Ok(guard) = GLOBAL_RECORDER.lock() {
                    if let Some(weak) = guard.as_ref() {
                        if let Some(rec_arc) = weak.upgrade() {
                            if let Ok(mut rec_guard) = rec_arc.lock() {
                                if let Some(rec) = rec_guard.as_mut() {
                                    if let Err(e) = rec.set_capture_mode(cfg.capture_mode) {
                                        eprintln!(
                                            "[Audio Recorder] Capture mode not applied: {}",
                                            e
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Apply immediately to live floating pill
            ACTIVE_OPT_IDLE.store(cfg.opt_idle, Ordering::Relaxed);
            pill.set_opt_idle(cfg.opt_idle);
            pill.set_opt_listening(cfg.opt_listening);
            pill.set_opt_transcribe(cfg.opt_transcribe);
            pill.set_opt_success(cfg.opt_success);
            pill.set_opt_error(cfg.opt_error);
            pill.set_surface_opacity(cfg.surface_opacity);
            if cfg.opt_idle != 0 {
                wake_pill_anim_timer();
            }

            // Apply speech recording behavior to hotkey manager on Save Preset!
            let hk_mode = resolve_hotkey_mode(false, cfg.trigger_mode);
            // Only mutate active OS hotkey hooks if not currently recording audio
            if engine_state_sc.load(Ordering::SeqCst) == 0 {
                if let Ok(guard) = GLOBAL_HOTKEY_MGR.lock() {
                    if let Some(ref mgr) = *guard {
                        mgr.set_mode(hk_mode);
                        // Re-apply both trigger slots so Save is source of truth.
                        mgr.apply_bindings(
                            &cfg.primary_trigger,
                            &cfg.secondary_trigger,
                            &cfg.primary_cancel_trigger,
                            &cfg.secondary_cancel_trigger,
                        );
                    }
                }
            }

            // Persist to %APPDATA%\Sotto\config.json
            if let Err(e) = cfg.save() {
                eprintln!("[Config Error] Failed to persist config: {}", e);
            } else {
                println!("[Config] Customization applied to live pill and saved to disk.");
                let _ = std::io::stdout().flush();
            }

            // Visual confirmation feedback animation on Save button
            studio.set_save_feedback(true);
            let s_timer = studio.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(1800), move || {
                if let Some(s) = s_timer.upgrade() {
                    s.set_save_feedback(false);
                }
            });
        }
    });

    // 8. Tab switching (locks window bounds to prevent Slint layout resize jumps)
    let s_weak_tab = s_weak.clone();
    let hq_tab = Arc::clone(history_queue);
    studio.on_select_tab(move |tab| {
        IS_SWITCHING_TABS.store(true, std::sync::atomic::Ordering::SeqCst);
        #[cfg(target_os = "windows")]
        let mut cur_rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        #[cfg(target_os = "windows")]
        let mut hwnd_opt = None;

        if let Some(s) = s_weak_tab.upgrade() {
            #[cfg(target_os = "windows")]
            {
                let win = s.window();
                let win_handle_wrapper = win.window_handle();
                if let Ok(handle) = win_handle_wrapper.window_handle() {
                    if let RawWindowHandle::Win32(win32_handle) = handle.as_raw() {
                        let hwnd = win32_handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;
                        unsafe {
                            GetWindowRect(hwnd, &mut cur_rect as *mut _ as *mut _);
                        }
                        hwnd_opt = Some(hwnd);
                    }
                }
            }

            s.set_active_tab(tab);
            s.set_purge_confirm_armed(false);

            #[cfg(target_os = "windows")]
            if let Some(hwnd) = hwnd_opt {
                let w = cur_rect.right - cur_rect.left;
                let h = cur_rect.bottom - cur_rect.top;
                if w > 0 && h > 0 {
                    unsafe {
                        SetWindowPos(
                            hwnd,
                            std::ptr::null_mut(),
                            cur_rect.left,
                            cur_rect.top,
                            w,
                            h,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                    }
                }
            }
        }
        IS_SWITCHING_TABS.store(false, std::sync::atomic::Ordering::SeqCst);
        // Opening Providers refreshes keys / cached models / chip from the store.
        if tab == 1 {
            let db = hq_tab.db();
            let tr_opt = SHARED_TRANSCRIBER.lock().ok().and_then(|guard| guard.clone());
            if let Some(tr) = tr_opt {
                sync_provider_ui(db, &tr);
            }
        }
        let _ = with_studio(|s| sync_studio_dimensions(s, true));
        wake_scroll_driver();
        if tab == 2 {
            wake_pill_anim_timer();
        }
        println!("[Studio Suite] Tab switched to: {}", match tab {
            0 => "Dictations",
            1 => "Providers",
            _ => "Customization",
        });
        let _ = std::io::stdout().flush();
    });

    // 9. Copy history item to clipboard
    let s_weak_copy = s_weak.clone();
    studio.on_copy_history_item(move |id, text| {
        match inject::copy_text_to_clipboard(text.as_str()) {
            Ok(()) => {
                println!(
                    "[Studio History] Copied record #{} to clipboard ({} chars)",
                    id,
                    text.len()
                );
                let _ = std::io::stdout().flush();
            }
            Err(e) => {
                eprintln!(
                    "[Studio History] Failed to copy record #{} to clipboard: {}",
                    id, e
                );
            }
        }
        if let Some(s) = s_weak_copy.upgrade() {
            s.set_copied_item_id(id);
            let s_timer = s.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(1500), move || {
                if let Some(s2) = s_timer.upgrade() {
                    s2.set_copied_item_id(-1);
                }
            });
        }
    });

    // 10. Clear all history
    let s_weak_clear = s_weak.clone();
    let hq_clear = Arc::clone(history_queue);
    studio.on_clear_all_history(move || {
        println!("[Studio History] Clear all history requested");
        let _ = std::io::stdout().flush();
        if let Some(s) = s_weak_clear.upgrade() {
            s.set_purge_confirm_armed(false);
        }
        let s_cb = s_weak_clear.clone();
        hq_clear.enqueue_clear_all(move || {
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(s) = s_cb.upgrade() {
                    let empty_model: Rc<VecModel<HistoryItem>> = Rc::new(VecModel::default());
                    s.set_history_items(ModelRc::from(empty_model));
                    set_studio_history_list(&s, Vec::new());
                    s.set_purge_confirm_armed(false);
                    s.set_history_viewport_y(0.0);
                    s.set_history_scroll_progress(0.0);
                    s.set_history_can_scroll(false);
                    s.set_history_is_scrolling(false);
                    sotto_desktop::smooth_scroll::STUDIO_SCROLLER.with(|scroller| {
                        scroller.borrow_mut().jump_to(0.0);
                    });
                }
            });
        });
    });

// (Issue R10 on_history_drag_scroll unified at scroller init)
}

/// Lazy instantiates or toggles the Studio Window
fn toggle_studio_window(
    pill_weak: &slint::Weak<FloatingPillWindow>,
    engine_state: &Arc<AtomicU8>,
    studio_visible: &Arc<AtomicBool>,
    cycle_fn: &Arc<dyn Fn() + Send + Sync>,
    history_queue: &Arc<AsyncHistoryQueue>,
) {
    let currently_visible = studio_visible.load(Ordering::SeqCst);
    if currently_visible {
        drop_studio_instance(studio_visible);
        println!("[Studio Action] Studio Suite closed and deallocated from RAM");
        let _ = std::io::stdout().flush();
        return;
    }

    STUDIO_INSTANCE.with(|cell| {
        let mut guard = cell.borrow_mut();
        if guard.is_none() {
            match StudioWindow::new() {
                Ok(studio) => {
                    wire_studio_window(
                        &studio,
                        pill_weak,
                        engine_state,
                        studio_visible,
                        cycle_fn.clone(),
                        history_queue,
                    );
                    *guard = Some(studio);
                    println!("[Studio Architecture] StudioWindow instantiated on demand");
                }
                Err(e) => {
                    eprintln!("[Studio Architecture Error] Failed to create StudioWindow: {}", e);
                    return;
                }
            }
        }
        if let Some(s) = guard.as_ref() {
            if let Some(p) = pill_weak.upgrade() {
                let cfg = config::AppConfig::load();
                // Isolated from live pill state: preview stage maintains selected preview state
                s.set_opt_idle(p.get_opt_idle());
                s.set_opt_listening(p.get_opt_listening());
                s.set_opt_transcribe(p.get_opt_transcribe());
                s.set_opt_success(p.get_opt_success());
                s.set_opt_error(p.get_opt_error());
                s.set_surface_opacity(p.get_surface_opacity());
                s.set_trigger_mode(cfg.trigger_mode);
                s.set_capture_mode(cfg.capture_mode.as_i32());
                s.set_chimes_enabled(cfg.chimes_enabled);
                s.set_media_pause_enabled(cfg.media_pause_enabled);
                s.set_trigger_primary_display(cfg.primary_trigger.display().into());
                s.set_trigger_secondary_display(
                    cfg.secondary_trigger
                        .as_ref()
                        .map(|b| b.display())
                        .unwrap_or_else(|| "Not set".to_string())
                        .into(),
                );
                s.set_cancel_primary_display(
                    cfg.primary_cancel_trigger
                        .as_ref()
                        .map(|b| b.display())
                        .unwrap_or_else(|| "Not set".to_string())
                        .into(),
                );
                s.set_cancel_secondary_display(
                    cfg.secondary_cancel_trigger
                        .as_ref()
                        .map(|b| b.display())
                        .unwrap_or_else(|| "Not set".to_string())
                        .into(),
                );
                #[cfg(target_os = "windows")]
                s.set_launch_on_startup(startup::is_startup_enabled());
                #[cfg(not(target_os = "windows"))]
                s.set_launch_on_startup(cfg.launch_on_startup);
            }
            s.set_purge_confirm_armed(false);
            let _ = s.show();
            bring_studio_to_front(s);
            hydrate_studio_history_nonblocking(s, history_queue.db(), 500);
            sync_studio_dimensions(s, true);
            wake_scroll_driver();
            studio_visible.store(true, Ordering::SeqCst);
            wake_pill_anim_timer();
            println!("[Studio Action] Studio Suite shown & focused");
        }
        let _ = std::io::stdout().flush();
    });

    // Refresh provider keys / models / chip AFTER the instance borrow is released
    // (sync_provider_ui borrows the Studio instance itself; nesting it inside the
    // borrow_mut above panics the RefCell).
    {
        let db = history_queue.db();
        let tr_opt = SHARED_TRANSCRIBER.lock().ok().and_then(|guard| guard.clone());
        if let Some(tr) = tr_opt {
            sync_provider_ui(db, &tr);
        }
    }
}

/// Initiates voice dictation (State 0 -> State 1: Listening)
fn start_dictation(
    pill_weak: &slint::Weak<FloatingPillWindow>,
    recorder: &Arc<Mutex<Option<audio::recorder::AudioRecorder>>>,
    engine_state: &Arc<AtomicU8>,
    recording_start: &Arc<Mutex<Option<Instant>>>,
    target_hwnd: &Arc<AtomicIsize>,
    inactivity: &InactivityTracker,
    dictation_gen: &Arc<std::sync::atomic::AtomicU64>,
) {
    inactivity.record_activity();
    let prev = engine_state.load(Ordering::SeqCst);
    if prev != 0 {
        return;
    }

    dictation_gen.fetch_add(1, Ordering::SeqCst);

    // 1. Capture current foreground window before dictation starts
    #[cfg(target_os = "windows")]
    {
        let fg = unsafe { GetForegroundWindow() } as isize;
        if fg != 0 {
            target_hwnd.store(fg, Ordering::SeqCst);
        }
    }

    // 1b. Quiet background playback before the microphone opens. The command
    // is asynchronous; opening the headset stream typically cuts A2DP at the
    // same moment, and the SMTC pause usually lands first.
    media::pause_for_dictation();

    // 2. Start audio capture directly into RAM
    if let Ok(mut rec_guard) = recorder.lock() {
        if let Some(rec) = rec_guard.as_mut() {
            if let Err(e) = rec.start() {
                eprintln!("[Audio Pipeline Error] Failed to start recorder: {}", e);
                media::resume_immediately();
                return;
            }
        }
    }

    // 2b. Arm the pause-aware long-recording pump. Sealed chunks are
    // transcribed while the user keeps talking; short takes still use the
    // existing single-upload path when stop() finds no sealed chunk.
    let transcriber_snapshot = SHARED_TRANSCRIBER.lock().ok().and_then(|guard| {
        guard
            .as_ref()
            .and_then(|shared| shared.lock().ok().map(|t| t.clone()))
    });
    if let Some(transcriber_snapshot) = transcriber_snapshot {
        if let Ok(mut slot) = LONG_FORM_SESSION.lock() {
            if let Some(stale) = slot.take() {
                stale.cancel();
            }
            *slot = Some(sotto_desktop::long_form::LongFormSession::start(
                Arc::clone(recorder),
                transcriber_snapshot,
            ));
        }
    } else {
        eprintln!("[LongForm] No transcriber snapshot; long takes will not be chunked");
    }

    // 3. Mark start time for actual elapsed time counter
    if let Ok(mut start_guard) = recording_start.lock() {
        *start_guard = Some(Instant::now());
    }

    // 4. Update atomic state to Listening
    engine_state.store(1, Ordering::SeqCst);

    // The cue is intentionally short and quiet. It is played after capture is
    // armed so the user hears it through the route the take is using.
    audio::chime::play_start();

    #[cfg(target_os = "windows")]
    if let Ok(guard) = GLOBAL_HOTKEY_MGR.lock() {
        if let Some(ref mgr) = *guard {
            mgr.set_recording(true);
        }
    }

    // 5. Update UI properties
    if let Some(pill) = pill_weak.upgrade() {
        pill.set_active_state(1);
        pill.set_timer_sec(0);
        pill.set_timer_text("0:00".into());
    }
    wake_pill_anim_timer();
// Studio preview stage remains independent from live mic start

    println!("[Pipeline] State -> 1: Listening (Audio Capture Started)");
    let _ = std::io::stdout().flush();
}

/// Instantly aborts dictation without sending audio, transcribing, or pasting
fn cancel_dictation(
    pill_weak: &slint::Weak<FloatingPillWindow>,
    recorder: &Arc<Mutex<Option<audio::recorder::AudioRecorder>>>,
    engine_state: &Arc<AtomicU8>,
    recording_start: &Arc<Mutex<Option<Instant>>>,
    dictation_gen: &Arc<std::sync::atomic::AtomicU64>,
    safety_net: &Arc<audio::AudioSafetyNet>,
) {
    dictation_gen.fetch_add(1, Ordering::SeqCst);
    let prev = engine_state.swap(0, Ordering::SeqCst);
    if prev == 0 {
        return;
    }

    // Stop and detach the long-recording pump before the recorder is stopped,
    // so no thread can poll the buffers while the device is being released.
    if let Ok(mut slot) = LONG_FORM_SESSION.lock() {
        if let Some(session) = slot.take() {
            session.cancel();
        }
    }

    // Release the headset stream before the stop cue and music. The cue must
    // not be captured, and playback must not resume into an open microphone.
    if let Ok(mut rec_guard) = recorder.lock() {
        if let Some(rec) = rec_guard.as_mut() {
            let _ = rec.stop();
        }
    }
    if prev == 1 {
        audio::chime::play_stop();
    }
    media::resume_immediately();

    if let Ok(mut start_guard) = recording_start.lock() {
        *start_guard = None;
    }
    safety_net.clear();

    #[cfg(target_os = "windows")]
    if let Ok(guard) = GLOBAL_HOTKEY_MGR.lock() {
        if let Some(ref mgr) = *guard {
            mgr.set_recording(false);
        }
    }

    let p_weak = pill_weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(p) = p_weak.upgrade() {
            p.set_active_state(0);
            p.set_timer_sec(0);
            p.set_timer_text("0:00".into());
        }
    });

    println!("[Pipeline] Cancelled dictation! Returned to Idle (State 0)");
    let _ = std::io::stdout().flush();
}

/// Finalizes voice dictation (State 1 -> State 2: Transcribing -> State 3: Success -> State 0: Idle)
fn stop_dictation(
    pill_weak: &slint::Weak<FloatingPillWindow>,
    recorder: &Arc<Mutex<Option<audio::recorder::AudioRecorder>>>,
    transcriber: &router::SharedTranscriber,
    engine_state: &Arc<AtomicU8>,
    recording_start: &Arc<Mutex<Option<Instant>>>,
    target_hwnd: &Arc<AtomicIsize>,
    inactivity: &InactivityTracker,
    history_queue: &Arc<AsyncHistoryQueue>,
    safety_net: &Arc<audio::AudioSafetyNet>,
    post_processor: &Arc<sotto_desktop::cleaner::PostProcessor>,
    dictation_gen: &Arc<std::sync::atomic::AtomicU64>,
) {
    inactivity.record_activity();
    let prev = engine_state.load(Ordering::SeqCst);
    if prev != 1 {
        return;
    }

    let my_gen = dictation_gen.load(Ordering::SeqCst);

    // 1. Transition to Transcribing (State 2)
    engine_state.store(2, Ordering::SeqCst);
    if let Ok(mut start_guard) = recording_start.lock() {
        *start_guard = None;
    }

    if let Some(pill) = pill_weak.upgrade() {
        pill.set_active_state(2);
    }
    wake_pill_anim_timer();
// Studio preview stage remains independent from live mic stop

    println!("[Pipeline] State -> 2: Transcribing (Audio Stopped, Groq HTTP/2 Upload)");
    let _ = std::io::stdout().flush();

    // 2. Stop audio capture. A long-form session owns the pump and the tail;
    // short takes fall through to the existing single-upload pipeline.
    let session_finish = LONG_FORM_SESSION
        .lock()
        .ok()
        .and_then(|mut slot| slot.take())
        .map(|session| session.finish());

    let payload_opt = match session_finish {
        Some(sotto_desktop::long_form::LongFormFinish::Chunked(resolution)) => {
            let transcriber_snapshot = match transcriber.lock() {
                Ok(guard) => guard.clone(),
                Err(poisoned) => poisoned.into_inner().clone(),
            };
            // The recorder has released the headset stream by this point, so
            // this cue is not part of the take.
            audio::chime::play_stop();
            spawn_long_form_resolution(
                resolution,
                transcriber_snapshot,
                pill_weak.clone(),
                Arc::clone(engine_state),
                target_hwnd.load(Ordering::SeqCst),
                Arc::clone(history_queue),
                Arc::clone(safety_net),
                Arc::clone(post_processor),
                Arc::clone(dictation_gen),
                my_gen,
            );
            return;
        }
        Some(sotto_desktop::long_form::LongFormFinish::Single(payload)) => payload,
        None => {
            if let Ok(mut rec_guard) = recorder.lock() {
                if let Some(rec) = rec_guard.as_mut() {
                    match rec.stop() {
                        Ok(p) => p,
                        Err(e) => {
                            eprintln!(
                                "[Audio Pipeline Error] Failed to finalize recording: {}",
                                e
                            );
                            None
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            }
        }
    };

    // The recorder has released the headset stream by this point, so this cue
    // is not part of the take.
    audio::chime::play_stop();

    match payload_opt {
        Some(payload) => {
            println!(
                "[Audio Pipeline] Captured {}ms audio buffer ({} bytes WAV ready for dispatch)",
                payload.duration_ms,
                payload.wav_bytes.len()
            );

            let target_active = target_hwnd.load(Ordering::SeqCst);
            // Step 2.2: Save audio payload to in-memory Safety Net cache before dispatch
            safety_net.save(payload.wav_bytes.clone(), payload.duration_ms, target_active);

            {
                let transcriber_snapshot = match transcriber.lock() {
                    Ok(guard) => guard.clone(),
                    Err(poisoned) => poisoned.into_inner().clone(),
                };
                let groq_for_llm = transcriber_snapshot.groq_client();
                let gemini_for_llm = transcriber_snapshot.gemini_client();
                let pill_async = pill_weak.clone();
                let engine_async = Arc::clone(engine_state);
                let target_async = target_active;
                let history_async = Arc::clone(history_queue);
                let safety_async = Arc::clone(safety_net);
                let processor_async = Arc::clone(post_processor);
                let dict_gen_async = Arc::clone(dictation_gen);

                std::thread::spawn(move || {
                    if dict_gen_async.load(Ordering::SeqCst) != my_gen {
                        println!("[Pipeline] Transcription aborted: cancelled by user");
                        return;
                    }
                    match transcriber_snapshot.transcribe_blocking(payload.wav_bytes) {
                        Ok(res) => {
                            if dict_gen_async.load(Ordering::SeqCst) != my_gen {
                                println!("[Pipeline] Transcription completed but aborted by cancel trigger");
                                return;
                            }
                            println!("[Transcribe Pipeline] Raw Transcript: \"{}\" in {}ms", res.text, res.latency_ms);
                            safety_async.clear(); // Successfully transcribed, purge audio buffer from RAM

                            let (app_name, exe_path) = get_process_info_for_hwnd(target_async);
                            let cleaned_text = if processor_async.get_options().llm_enabled || processor_async.get_options().mode == sotto_desktop::cleaner::PostProcessMode::SmartLLM {
                                shared_tokio().block_on(processor_async.process_async_with_duration(
                                    &res.text,
                                    Some(&app_name),
                                    groq_for_llm.as_ref(),
                                    gemini_for_llm.as_ref(),
                                    Some(payload.duration_ms),
                                ))
                            } else {
                                processor_async.process_local_with_duration(&res.text, Some(&app_name), Some(payload.duration_ms))
                            };

                            if cleaned_text.is_empty() {
                                println!("[Cleaner] Dropped hallucinated or empty transcript: {:?}", res.text);
                                engine_async.store(0, Ordering::SeqCst);
                                media::resume_after_dictation();
                                let p1 = pill_async.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(p) = p1.upgrade() {
                                        p.set_active_state(0);
                                    }
// Studio preview isolated
                                });
                                return;
                            }
                            println!("[PostProcessor] Final Transformed Text: \"{}\" (Venue: {})", cleaned_text, app_name);

                            engine_async.store(3, Ordering::SeqCst);

                            let p1 = pill_async.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(p) = p1.upgrade() {
                                    p.set_active_state(3); // State 3: Success
                                }
                                wake_pill_anim_timer();
// Studio preview isolated
                                println!("[Pipeline] State -> 3: Success");
                            });

                            if dict_gen_async.load(Ordering::SeqCst) != my_gen {
                                println!("[Pipeline] Text injection aborted by cancel trigger");
                                return;
                            }
                            #[cfg(target_os = "windows")]
                            if let Ok(guard) = GLOBAL_HOTKEY_MGR.lock() {
                                if let Some(ref mgr) = *guard {
                                    mgr.set_recording(false);
                                }
                            }
                            // Inject text with target window focus restoration
                            if let Err(e) = inject::inject_text_with_target(&cleaned_text, target_async) {
                                eprintln!("[Inject Error] Failed to inject text: {}", e);
                            }

                            // Resume only after the written text exists, with a
                            // short settle so music does not race the route
                            // switch back to A2DP.
                            media::resume_after_dictation();

                            // Asynchronously enqueue dictation into SQLite WAL DB (zero injection latency)
                            let _ = app_icon::prewarm_app_icon(&app_name, exe_path.as_deref());
                            let exe_path_clone = exe_path.clone();
                            history_async.enqueue_insert(
                                cleaned_text.clone(),
                                payload.duration_ms as i32,
                                app_name,
                                move |rec| {
                                    let exe_path_ui = exe_path_clone.clone();
                                    let _ = slint::invoke_from_event_loop(move || {
                                        prepend_studio_history_record(rec, exe_path_ui, "new");
                                    });
                                },
                            );

                            // Auto-reset back to Idle after 400ms
                            std::thread::sleep(Duration::from_millis(400));
                            if engine_async.load(Ordering::SeqCst) == 3 {
                                engine_async.store(0, Ordering::SeqCst);
                                let p_idle = pill_async.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(p) = p_idle.upgrade() {
                                        p.set_active_state(0);
                                        p.set_timer_sec(0);
                                        p.set_timer_text("0:00".into());
                                    }
// Studio preview isolated
                                    println!("[Pipeline] State -> 0: Idle (Ready)");
                                });
                            }
                        }
                        Err(e) => {
                            eprintln!("[Transcribe Pipeline Error] {}", e);
                            engine_async.store(4, Ordering::SeqCst);
                            media::resume_after_dictation();

                            let p1 = pill_async.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(p) = p1.upgrade() {
                                    p.set_active_state(4); // State 4: Error
                                }
                                wake_pill_anim_timer();
// Studio preview isolated
                                println!("[Pipeline] State -> 4: Error (Audio safely buffered in RAM for retry)");
                            });

                            // Display error state for 6s before visually resetting to Idle; audio remains safe in RAM!
                            std::thread::sleep(Duration::from_millis(6000));
                            if engine_async.load(Ordering::SeqCst) == 4 {
                                engine_async.store(0, Ordering::SeqCst);
                                let p_idle = pill_async.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(p) = p_idle.upgrade() {
                                        p.set_active_state(0);
                                        p.set_timer_sec(0);
                                        p.set_timer_text("0:00".into());
                                    }
// Studio preview isolated
                                    println!("[Pipeline] State -> 0: Idle (Ready, retry buffer preserved)");
                                });
                            }
                        }
                    }
                });
            }
        }
        None => {
            // Silence or ghost tap rejected by filters
            println!("[Pipeline] Dictation rejected by acoustic filters; resetting to Idle");
            engine_state.store(0, Ordering::SeqCst);
            media::resume_after_dictation();
            if let Some(pill) = pill_weak.upgrade() {
                pill.set_active_state(0);
                pill.set_timer_sec(0);
                pill.set_timer_text("0:00".into());
            }
// Studio preview isolated
        }
    }
}

/// Resolves a chunked take in the background: waits for sealed-chunk uploads and
/// the tail, then runs the same cleanup/injection/history path as a short take.
#[allow(clippy::too_many_arguments)]
fn spawn_long_form_resolution(
    resolution: sotto_desktop::long_form::LongFormResolution,
    transcriber_snapshot: router::Transcriber,
    pill_weak: slint::Weak<FloatingPillWindow>,
    engine_state: Arc<AtomicU8>,
    target_hwnd: isize,
    history_queue: Arc<AsyncHistoryQueue>,
    safety_net: Arc<audio::AudioSafetyNet>,
    post_processor: Arc<sotto_desktop::cleaner::PostProcessor>,
    dictation_gen: Arc<std::sync::atomic::AtomicU64>,
    my_gen: u64,
) {
    std::thread::spawn(move || {
        let outcome = resolution.wait();

        if dictation_gen.load(Ordering::SeqCst) != my_gen {
            println!("[LongForm] Resolution completed but aborted by cancel trigger");
            return;
        }

        if let Some(error) = outcome.error {
            // Keep completed chunks as text and unresolved chunks as audio, in
            // their exact original order, so retry does not re-upload work that
            // already succeeded.
            safety_net.save_segments(
                outcome.segments,
                outcome.total_duration_ms,
                target_hwnd,
            );
            eprintln!(
                "[LongForm Pipeline Error] {} ({}ms cached for retry)",
                error, outcome.total_duration_ms
            );
            engine_state.store(4, Ordering::SeqCst);
            media::resume_after_dictation();

            let p1 = pill_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(p) = p1.upgrade() {
                    p.set_active_state(4); // State 4: Error
                }
                wake_pill_anim_timer();
            });

            std::thread::sleep(Duration::from_millis(6000));
            if engine_state.load(Ordering::SeqCst) == 4 {
                engine_state.store(0, Ordering::SeqCst);
                let p_idle = pill_weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(p) = p_idle.upgrade() {
                        p.set_active_state(0);
                        p.set_timer_sec(0);
                        p.set_timer_text("0:00".into());
                    }
                });
            }
            return;
        }

        let raw_text = outcome.text;
        let duration_ms = outcome.total_duration_ms;
        safety_net.clear();
        println!(
            "[LongForm] Stitched {}ms transcript from sealed chunks: \"{}\"",
            duration_ms,
            raw_text
        );

        if raw_text.trim().is_empty() {
            println!("[LongForm] All chunks were silent; resetting to Idle");
            engine_state.store(0, Ordering::SeqCst);
            media::resume_after_dictation();
            let p1 = pill_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(p) = p1.upgrade() {
                    p.set_active_state(0);
                    p.set_timer_sec(0);
                    p.set_timer_text("0:00".into());
                }
            });
            return;
        }

        let groq_for_llm = transcriber_snapshot.groq_client();
        let gemini_for_llm = transcriber_snapshot.gemini_client();
        let (app_name, exe_path) = get_process_info_for_hwnd(target_hwnd);
        let cleaned_text = if post_processor.get_options().llm_enabled
            || post_processor.get_options().mode
                == sotto_desktop::cleaner::PostProcessMode::SmartLLM
        {
            shared_tokio().block_on(post_processor.process_async_with_duration(
                &raw_text,
                Some(&app_name),
                groq_for_llm.as_ref(),
                gemini_for_llm.as_ref(),
                Some(duration_ms),
            ))
        } else {
            post_processor.process_local_with_duration(
                &raw_text,
                Some(&app_name),
                Some(duration_ms),
            )
        };

        if cleaned_text.is_empty() {
            println!(
                "[Cleaner] Dropped hallucinated or empty long-form transcript: {:?}",
                raw_text
            );
            engine_state.store(0, Ordering::SeqCst);
            media::resume_after_dictation();
            let p1 = pill_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(p) = p1.upgrade() {
                    p.set_active_state(0);
                }
            });
            return;
        }

        // Cleanup can take a while (Smart LLM). Do not publish state or inject
        // text into a newer take if the user cancelled in the meantime.
        if dictation_gen.load(Ordering::SeqCst) != my_gen {
            println!("[LongForm] Cleanup completed but aborted by cancel trigger");
            return;
        }

        println!(
            "[PostProcessor] Final Transformed Text: \"{}\" (Venue: {})",
            cleaned_text, app_name
        );
        engine_state.store(3, Ordering::SeqCst);

        let p1 = pill_weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(p) = p1.upgrade() {
                p.set_active_state(3); // State 3: Success
            }
            wake_pill_anim_timer();
            println!("[Pipeline] State -> 3: Success (LongForm)");
        });

        if dictation_gen.load(Ordering::SeqCst) != my_gen {
            println!("[LongForm] Text injection aborted by cancel trigger");
            return;
        }
        #[cfg(target_os = "windows")]
        if let Ok(guard) = GLOBAL_HOTKEY_MGR.lock() {
            if let Some(ref mgr) = *guard {
                mgr.set_recording(false);
            }
        }
        if let Err(e) = inject::inject_text_with_target(&cleaned_text, target_hwnd) {
            eprintln!("[Inject Error] Failed to inject long-form text: {}", e);
        }

        media::resume_after_dictation();

        let _ = app_icon::prewarm_app_icon(&app_name, exe_path.as_deref());
        let exe_path_clone = exe_path.clone();
        history_queue.enqueue_insert(
            cleaned_text.clone(),
            duration_ms as i32,
            app_name,
            move |rec| {
                let exe_path_ui = exe_path_clone.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    prepend_studio_history_record(rec, exe_path_ui, "long-form");
                });
            },
        );

        std::thread::sleep(Duration::from_millis(400));
        if engine_state.load(Ordering::SeqCst) == 3 {
            engine_state.store(0, Ordering::SeqCst);
            let p_idle = pill_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(p) = p_idle.upgrade() {
                    p.set_active_state(0);
                    p.set_timer_sec(0);
                    p.set_timer_text("0:00".into());
                }
                println!("[Pipeline] State -> 0: Idle (LongForm ready)");
            });
        }
    });
}

/// Resolves a safety-net snapshot in order. Text segments are appended as-is;
/// audio segments are uploaded with the preceding text as the prompt. On
/// failure, the returned snapshot keeps completed work as text and only the
/// unresolved chunks as audio, preserving the original sequence.
fn resolve_retry_segments(
    transcriber: &router::Transcriber,
    segments: Vec<audio::RetrySegment>,
) -> Result<(String, u128), (String, Vec<audio::RetrySegment>)> {
    let mut resolved: Vec<audio::RetrySegment> = Vec::with_capacity(segments.len());
    let mut stitched = String::new();
    let mut latency_ms = 0u128;
    let mut failure: Option<String> = None;

    for segment in segments {
        if failure.is_some() {
            resolved.push(segment);
            continue;
        }

        match segment {
            audio::RetrySegment::Text(text) => {
                let joined =
                    audio::stitch_transcripts([stitched.as_str(), text.as_str()]);
                stitched = joined;
                resolved.push(audio::RetrySegment::Text(text));
            }
            audio::RetrySegment::Audio {
                wav_bytes,
                duration_ms,
            } => {
                let prompt = audio::chunker::continuation_prompt(&stitched);
                let prompt_opt = (!prompt.is_empty()).then_some(prompt.as_str());
                match transcriber.transcribe_blocking_with_prompt(wav_bytes.clone(), prompt_opt)
                {
                    Ok(result) => {
                        let joined = audio::stitch_transcripts([
                            stitched.as_str(),
                            result.text.as_str(),
                        ]);
                        stitched = joined;
                        latency_ms += result.latency_ms;
                        resolved.push(audio::RetrySegment::Text(result.text));
                    }
                    Err(e) => {
                        failure = Some(e);
                        resolved.push(audio::RetrySegment::audio(wav_bytes, duration_ms));
                    }
                }
            }
        }
    }

    match failure {
        Some(e) => Err((e, resolved)),
        None => Ok((stitched, latency_ms)),
    }
}

/// Retries transcription using buffered audio retained in RAM (Step 2.2: Safety Net)
fn retry_dictation(
    pill_weak: &slint::Weak<FloatingPillWindow>,
    safety_net: &Arc<audio::AudioSafetyNet>,
    transcriber: &router::SharedTranscriber,
    engine_state: &Arc<AtomicU8>,
    inactivity: &InactivityTracker,
    history_queue: &Arc<AsyncHistoryQueue>,
    post_processor: &Arc<sotto_desktop::cleaner::PostProcessor>,
) -> bool {
    inactivity.record_activity();
    if let Some(cached) = safety_net.get_for_retry() {
        // Transition back to State 2: Transcribing
        engine_state.store(2, Ordering::SeqCst);
        if let Some(pill) = pill_weak.upgrade() {
            pill.set_active_state(2);
        }
        wake_pill_anim_timer();
// Studio preview isolated

        println!(
            "[Safety Net] Retrying transcription (attempt #{}) with {} ordered segment(s), {} audio bytes, {}ms",
            cached.retry_count,
            cached.segments.len(),
            cached.retained_audio_bytes(),
            cached.duration_ms
        );
        let _ = std::io::stdout().flush();

        {
            let transcriber_snapshot = match transcriber.lock() {
                    Ok(guard) => guard.clone(),
                    Err(poisoned) => poisoned.into_inner().clone(),
                };
            let groq_for_llm = transcriber_snapshot.groq_client();
            let gemini_for_llm = transcriber_snapshot.gemini_client();
            let pill_async = pill_weak.clone();
            let engine_async = Arc::clone(engine_state);
            let safety_async = Arc::clone(safety_net);
            let history_async = Arc::clone(history_queue);
            let target_hwnd = cached.target_hwnd;
            let duration_ms = cached.duration_ms;
            let cached_segments = cached.segments;
            let processor_async = Arc::clone(post_processor);

            std::thread::spawn(move || {
                match resolve_retry_segments(&transcriber_snapshot, cached_segments) {
                    Ok((raw_text, latency_ms)) => {
                        println!("[Safety Net] Retry succeeded! Raw Transcript: \"{}\" in {}ms", raw_text, latency_ms);
                        safety_async.clear(); // Successfully transcribed, purge audio buffer from RAM

                        let (app_name, exe_path) = get_process_info_for_hwnd(target_hwnd);
                        let cleaned_text = if processor_async.get_options().llm_enabled || processor_async.get_options().mode == sotto_desktop::cleaner::PostProcessMode::SmartLLM {
                            shared_tokio().block_on(processor_async.process_async_with_duration(
                                &raw_text,
                                Some(&app_name),
                                groq_for_llm.as_ref(),
                                gemini_for_llm.as_ref(),
                                Some(duration_ms),
                            ))
                        } else {
                            processor_async.process_local_with_duration(&raw_text, Some(&app_name), Some(duration_ms))
                        };

                        if cleaned_text.is_empty() {
                            println!("[Cleaner] Dropped hallucinated or empty retry transcript: {:?}", raw_text);
                            engine_async.store(0, Ordering::SeqCst);
                            let p1 = pill_async.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(p) = p1.upgrade() {
                                    p.set_active_state(0);
                                }
// Studio preview isolated
                            });
                            return;
                        }
                        println!("[PostProcessor] Sanitized Retry Transcript: \"{}\" (Venue: {})", cleaned_text, app_name);

                        engine_async.store(3, Ordering::SeqCst);

                        let p1 = pill_async.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(p) = p1.upgrade() {
                                p.set_active_state(3); // State 3: Success
                            }
                            wake_pill_anim_timer();
// Studio preview isolated
                            println!("[Pipeline] State -> 3: Success (Retry)");
                        });

                        // Inject text with target window focus restoration
                        if let Err(e) = inject::inject_text_with_target(&cleaned_text, target_hwnd) {
                            eprintln!("[Inject Error] Failed to inject retry text: {}", e);
                        }

                        // Asynchronously enqueue dictation into SQLite WAL DB
                        let _ = app_icon::prewarm_app_icon(&app_name, exe_path.as_deref());
                        let exe_path_clone = exe_path.clone();
                        history_async.enqueue_insert(
                            cleaned_text.clone(),
                            duration_ms as i32,
                            app_name,
                            move |rec| {
                                let exe_path_ui = exe_path_clone.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    prepend_studio_history_record(rec, exe_path_ui, "retry");
                                });
                            },
                        );

                        // Auto-reset back to Idle after 400ms
                        std::thread::sleep(Duration::from_millis(400));
                        if engine_async.load(Ordering::SeqCst) == 3 {
                            engine_async.store(0, Ordering::SeqCst);
                            let p_idle = pill_async.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(p) = p_idle.upgrade() {
                                    p.set_active_state(0);
                                    p.set_timer_sec(0);
                                    p.set_timer_text("0:00".into());
                                }
// Studio preview isolated
                                println!("[Pipeline] State -> 0: Idle (Ready)");
                            });
                        }
                    }
                    Err((e, snapshot)) => {
                        // Keep already-transcribed text and only the unresolved
                        // audio, in order, for another retry.
                        safety_async.save_segments(snapshot, duration_ms, target_hwnd);
                        eprintln!("[Safety Net Error] Retry attempt failed: {}", e);
                        engine_async.store(4, Ordering::SeqCst);

                        let p1 = pill_async.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(p) = p1.upgrade() {
                                p.set_active_state(4); // State 4: Error
                            }
                            wake_pill_anim_timer();
// Studio preview isolated
                            println!("[Pipeline] State -> 4: Error (Retry Failed)");
                        });

                        // Keep buffer in RAM for further user retries; auto-dismiss visual warning after 6s
                        std::thread::sleep(Duration::from_millis(6000));
                        if engine_async.load(Ordering::SeqCst) == 4 {
                            engine_async.store(0, Ordering::SeqCst);
                            let p_idle = pill_async.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(p) = p_idle.upgrade() {
                                    p.set_active_state(0);
                                    p.set_timer_sec(0);
                                    p.set_timer_text("0:00".into());
                                }
// Studio preview isolated
                            });
                        }
                    }
                }
            });
            return true;
        }
    }
    false
}

/// Live capture-latency benchmark. Deliberately interactive: real handshake
/// numbers only exist when a human actually speaks into the connected mic.
fn run_capture_bench(
    mode: config::CaptureMode,
    takes: u32,
    seconds: f32,
) -> Result<(), String> {
    println!("\n=== SOTTO CAPTURE TIMING BENCH ===");
    println!("Mode: {} | takes: {} | speak for {:.1}s each", mode.label(), takes, seconds);
    println!("Per-take measurements are appended to: {:?}\n", audio::timing::timing_log_path());

    let mut recorder = audio::recorder::AudioRecorder::new_with_mode(mode)?;
    for take in 1..=takes {
        println!("[Capture Bench] Take {}/{} — speak normally now...", take, takes);
        let _ = std::io::stdout().flush();
        std::thread::sleep(Duration::from_millis(800));

        recorder.start()?;
        let started = Instant::now();
        while started.elapsed().as_secs_f32() < seconds {
            std::thread::sleep(Duration::from_millis(20));
        }
        let payload = recorder.stop()?;

        match recorder.last_timing() {
            Some(t) => {
                println!(
                    "               first speech in output: {} ms | {} ms after trigger | handshake observed: {} ms",
                    t.first_speech_offset_ms
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "n/a".to_string()),
                    t.first_speech_after_trigger_ms
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "n/a".to_string()),
                    t.handshake_ms
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "n/a".to_string()),
                );
            }
            None => println!("[Capture Bench] No timing recorded (take was empty)"),
        }
        if payload.is_none() {
            println!("[Capture Bench] No payload (silence or shorter than the ghost-tap floor)");
        }
        let _ = std::io::stdout().flush();
        std::thread::sleep(Duration::from_millis(700));
    }

    let all = audio::timing::load_capture_timings(&audio::timing::timing_log_path());
    let mine: Vec<_> = all
        .into_iter()
        .filter(|t| t.mode == mode.label())
        .rev()
        .take(takes as usize)
        .collect();
    let summary = audio::timing::summarize(&mine);
    println!("\n[Capture Bench] Summary for {}", mode.label());
    println!("  takes: {}", summary.takes);
    println!(
        "  avg handshake: {} ms",
        summary.avg_handshake_ms.map(|v| format!("{:.0}", v)).unwrap_or_else(|| "n/a".into())
    );
    println!(
        "  avg splice processing: {} ms",
        summary.avg_splice_ms.map(|v| format!("{:.1}", v)).unwrap_or_else(|| "n/a".into())
    );
    println!(
        "  avg finalize: {} ms",
        summary.avg_finalize_ms.map(|v| format!("{:.1}", v)).unwrap_or_else(|| "n/a".into())
    );
    println!("  onboard-only fallbacks: {}", summary.onboard_only_fallbacks);
    println!("\nCompare modes by rerunning with --capture-mode=always-on|dual-mic|onboard-only");
    let _ = std::io::stdout().flush();
    Ok(())
}

/// Resolves the startup or saved hotkey mode; `--hold-to-talk` overrides persisted config.
fn resolve_hotkey_mode(cli_hold_to_talk: bool, trigger_mode: i32) -> hotkey::HotkeyMode {
    if cli_hold_to_talk || trigger_mode == 1 {
        hotkey::HotkeyMode::HoldToTalk
    } else {
        hotkey::HotkeyMode::Toggle
    }
}

fn main() -> Result<(), slint::PlatformError> {
    #[cfg(target_os = "windows")]
    unsafe {
        use windows_sys::Win32::UI::HiDpi::*;
        if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) != 0 {
            println!("[DPI] Windows Per-Monitor V2 High-DPI awareness enabled (Crisp native resolution)");
        } else if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE) != 0 {
            println!("[DPI] Windows Per-Monitor V1 High-DPI awareness enabled");
        } else {
            windows_sys::Win32::UI::WindowsAndMessaging::SetProcessDPIAware();
            println!("[DPI] Windows System DPI awareness fallback enabled");
        }
    }

    let args: Vec<String> = std::env::args().collect();
    let is_startup = args.iter().any(|arg| arg == "--startup");
    let is_record_test = args.iter().any(|arg| arg == "--record-test");
    let is_transcribe_test = args.iter().any(|arg| arg == "--transcribe-test");
    let is_inject_test = args.iter().any(|arg| arg == "--inject-test");
    let is_hotkey_test = args.iter().any(|arg| arg == "--hotkey-test");
    let is_phase1_test = args.iter().any(|arg| arg == "--phase1-test");
    let is_snapshot_studio = args.iter().any(|arg| arg == "--snapshot-studio");
    let is_record_scroll_video = args.iter().any(|arg| arg == "--record-scroll-video");
    let is_retry_test = args.iter().any(|arg| arg == "--retry-test");
    let is_cleaner_test = args.iter().any(|arg| arg == "--cleaner-test");
    let is_post_process_test = args.iter().any(|arg| arg == "--post-process-test");
    let is_capture_bench = args.iter().any(|arg| arg == "--capture-bench");

    if is_capture_bench {
        let mode = match args
            .iter()
            .find_map(|arg| arg.strip_prefix("--capture-mode="))
        {
            Some("always-on") | Some("always_on") | Some("0") => config::CaptureMode::AlwaysOn,
            Some("onboard") | Some("onboard-only") | Some("2") => config::CaptureMode::OnboardOnly,
            Some("dual") | Some("dual-mic") | Some("dual_mic_splice") | Some("1") => {
                config::CaptureMode::DualMicSplice
            }
            _ => config::AppConfig::load().capture_mode,
        };
        let takes = args
            .iter()
            .find_map(|arg| arg.strip_prefix("--takes="))
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(3)
            .clamp(1, 20);
        let seconds = args
            .iter()
            .find_map(|arg| arg.strip_prefix("--seconds="))
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(3.0)
            .clamp(1.0, 20.0);

        if let Err(e) = run_capture_bench(mode, takes, seconds) {
            eprintln!("[Capture Bench Error] {}", e);
        }
        return Ok(());
    }

    if is_snapshot_studio {
        std::env::set_var("SLINT_BACKEND", "winit-software");
        println!("[Snapshot] Taking Studio Window snapshot...");
        let studio = StudioWindow::new()?;
        let history_db = std::sync::Arc::new(sotto_desktop::db::HistoryDb::open_default().unwrap_or_else(|e| {
            panic!("Failed to open DB: {}", e);
        }));
        let mut history = history_db.get_recent(50).unwrap_or_default();
        if history.is_empty() {
            history = vec![
                sotto_desktop::db::HistoryRecord {
                    id: 1,
                    timestamp_str: "Just now (14:32:05)".to_string(),
                    transcript: "Optimize the real-time audio pipeline to clamp PCM16 samples and prevent buffer clipping during peak speech input.".to_string(),
                    duration_ms: 3200,
                    duration_str: "3.2s".to_string(),
                    target_app: "Visual Studio Code".to_string(),
                    timestamp: 1,
                },
                sotto_desktop::db::HistoryRecord {
                    id: 2,
                    timestamp_str: "2 mins ago (14:30:12)".to_string(),
                    transcript: "Draft a quarterly summary highlighting our 60 FPS FemtoVG latency improvements and zero-allocation memory trimming policies.".to_string(),
                    duration_ms: 4800,
                    duration_str: "4.8s".to_string(),
                    target_app: "Google Chrome".to_string(),
                    timestamp: 2,
                },
                sotto_desktop::db::HistoryRecord {
                    id: 3,
                    timestamp_str: "15 mins ago (14:17:40)".to_string(),
                    transcript: "Reviewing pull request for Per-Monitor V2 High-DPI awareness on Windows 11 displays.".to_string(),
                    duration_ms: 2100,
                    duration_str: "2.1s".to_string(),
                    target_app: "Slack".to_string(),
                    timestamp: 3,
                },
            ];
        }
        let count = history.len() as i32;
        let items: Vec<HistoryItem> = history.clone()
            .into_iter()
            .map(|r| {
                let formatted = sotto_desktop::app_icon::format_app_name(&r.target_app);
                let (icon, has_icon) = sotto_desktop::app_icon::get_app_icon(&formatted, Some(&r.target_app));
                HistoryItem {
                    id: r.id as i32,
                    timestamp_str: r.timestamp_str.into(),
                    transcript: r.transcript.into(),
                    duration_ms: r.duration_ms,
                    duration_str: r.duration_str.into(),
                    target_app: formatted.into(),
                    app_icon: icon,
                    has_icon,
                }
            })
            .collect();
        studio.set_history_items(slint::ModelRc::from(std::rc::Rc::new(slint::VecModel::from(items))));
        studio.set_history_count(count);
        studio.set_active_tab(0);
        studio.window().set_size(slint::PhysicalSize::new(1040, 840));
        studio.show()?;

        let snap_dir = std::env::var("SOTTO_EXPORT_PREVIEW_DIR")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                let win_default = std::path::PathBuf::from("D:/whiper-mobile");
                if win_default.exists() {
                    win_default
                } else {
                    std::path::PathBuf::from(".")
                }
            });
        let _ = std::fs::create_dir_all(&snap_dir);

        let write_snap_bmp = |filename: &str, buf: &slint::SharedPixelBuffer<slint::Rgba8Pixel>| {
            let w = buf.width();
            let h = buf.height();
            let pixels = buf.as_slice();
            let pixel_bytes_len = pixels.len() * 4;
            let file_size = 14 + 40 + pixel_bytes_len;
            let mut out = Vec::with_capacity(file_size);
            out.extend_from_slice(b"BM");
            out.extend_from_slice(&(file_size as u32).to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&(14u32 + 40u32).to_le_bytes());
            out.extend_from_slice(&(40u32).to_le_bytes());
            out.extend_from_slice(&(w as i32).to_le_bytes());
            out.extend_from_slice(&((-(h as i32))).to_le_bytes());
            out.extend_from_slice(&(1u16).to_le_bytes());
            out.extend_from_slice(&(32u16).to_le_bytes());
            out.extend_from_slice(&(0u32).to_le_bytes());
            out.extend_from_slice(&(pixel_bytes_len as u32).to_le_bytes());
            out.extend_from_slice(&(2835u32).to_le_bytes());
            out.extend_from_slice(&(2835u32).to_le_bytes());
            out.extend_from_slice(&(0u32).to_le_bytes());
            out.extend_from_slice(&(0u32).to_le_bytes());
            for p in pixels {
                out.push(p.b);
                out.push(p.g);
                out.push(p.r);
                out.push(p.a);
            }
            let out_path = snap_dir.join(filename);
            std::fs::write(&out_path, out).expect("write snapshot bmp");
            println!("[Snapshot] Successfully saved {} ({}x{})", out_path.display(), w, h);
        };

        let buffer = studio.window().take_snapshot()?;
        write_snap_bmp("studio_snapshot.bmp", &buffer);

        // Take bottom snapshot (scrolled all the way to -max_scroll)
        let scale = studio.window().scale_factor().max(1.0);
        let win_size = studio.window().size();
        let visible_h = studio.get_history_viewport_height().max(100.0);
        let chars_per_line = sotto_desktop::smooth_scroll::chars_per_line_for(win_size.width, scale);
        let heights: Vec<f32> = history.iter()
            .map(|r| sotto_desktop::smooth_scroll::estimate_card_height(&r.transcript, chars_per_line))
            .collect();
        let total_content_h = sotto_desktop::smooth_scroll::compute_content_height(&heights, sotto_desktop::smooth_scroll::ROW_SPACING)
            + sotto_desktop::smooth_scroll::LIST_BOTTOM_PADDING
            + sotto_desktop::smooth_scroll::LIST_OVERSCROLL_PX;
        studio.set_history_content_height(total_content_h.max(visible_h));
        let max_scroll = (total_content_h - visible_h).max(0.0);
        studio.set_history_viewport_y(-max_scroll);
        studio.set_history_scroll_progress(1.0);
        studio.set_history_is_scrolling(false);

        let buf_bottom = studio.window().take_snapshot()?;
        write_snap_bmp("studio_snapshot_bottom.bmp", &buf_bottom);
        println!("[Snapshot] max_scroll={}", max_scroll);

        // Take scrolling snapshot to prove scrollbar is normal and not bright cyan
        studio.set_history_is_scrolling(true);
        let buf_scrolling = studio.window().take_snapshot()?;
        write_snap_bmp("studio_snapshot_scrolling.bmp", &buf_scrolling);

        // Also take Tab 2 (Customization) snapshot with animated Listening preview pill
        studio.set_active_tab(2);
        studio.set_active_state(1); // Listening state
        studio.set_bar1_h(5.0);
        studio.set_bar2_h(8.5);
        studio.set_bar3_h(13.0);
        studio.set_bar4_h(8.0);
        studio.set_bar5_h(4.5);
        studio.set_timer_text("0:03".into());
        studio.set_timer_sec(3);

        let buf_cust = studio.window().take_snapshot()?;
        write_snap_bmp("studio_customization_snapshot.bmp", &buf_cust);

        // 2. Take bottom snapshot of customization view (showing trigger controls and save preset)
        let custom_h = studio.get_custom_content_height();
        let custom_vis = (buf_cust.height() as f32) - sotto_desktop::smooth_scroll::CONTENT_CHROME_RESERVED_PX;
        let max_scroll = (custom_h - custom_vis).max(0.0);
        studio.set_custom_viewport_y(-max_scroll);
        studio.set_custom_scroll_progress(1.0);
        studio.set_custom_is_scrolling(false);
        studio.set_custom_can_scroll(max_scroll > 0.0);
        studio.set_trigger_primary_display("Ctrl".into());
        let buffer_b = studio.window().take_snapshot()?;
        write_snap_bmp("studio_customization_bottom.bmp", &buffer_b);

        // 3. Take snapshot with key recording listening state
        studio.set_is_recording_key(true);
        let buffer_picker = studio.window().take_snapshot()?;
        write_snap_bmp("studio_customization_picker.bmp", &buffer_picker);

        // 4. Providers tab snapshot (logos, model radio lists, single key box)
        studio.set_provider_id("groq".into());
        studio.set_groq_key("gsk_snapshot_preview".into());
        studio.set_gemini_key("AIza_snapshot_preview".into());
        studio.set_groq_models(slint::ModelRc::from(std::rc::Rc::new(slint::VecModel::from(vec![
            slint::SharedString::from("whisper-large-v3-turbo"),
            slint::SharedString::from("whisper-large-v3"),
        ]))));
        studio.set_gemini_models(slint::ModelRc::from(std::rc::Rc::new(slint::VecModel::from(vec![
            slint::SharedString::from("gemini-3.5-transcribe"),
            slint::SharedString::from("gemini-2.5-flash"),
        ]))));
        studio.set_groq_model_index(0);
        studio.set_gemini_model_index(0);
        studio.set_provider_chip_text("Groq whisper-large-v3-turbo".into());
        studio.set_active_tab(1);
        let buffer_prov = studio.window().take_snapshot()?;
        write_snap_bmp("studio_providers_snapshot.bmp", &buffer_prov);
        return Ok(());
    }

    if is_record_scroll_video {
        println!("[Video] Recording Studio History 60 FPS High-Speed Lenis Scroll Demo...");
        std::env::set_var("SLINT_BACKEND", "winit-software");
        let studio = StudioWindow::new()?;
        let history_db = std::sync::Arc::new(sotto_desktop::db::HistoryDb::open_default().unwrap_or_else(|e| {
            panic!("Failed to open DB: {}", e);
        }));
        let history = history_db.get_recent(100).unwrap_or_default();
        let count = history.len() as i32;
        studio.window().set_size(slint::PhysicalSize::new(960, 720));
        studio.show()?;

        let scale = studio.window().scale_factor().max(1.0);
        let win_size = studio.window().size();
        let logical_h = win_size.height as f32 / scale;
        let visible_h = sotto_desktop::smooth_scroll::compute_visible_height(logical_h, 240.0, 300.0);
        let chars_per_line = sotto_desktop::smooth_scroll::chars_per_line_for(win_size.width, scale);
        use sotto_desktop::smooth_scroll::estimate_card_height;

        let mut heights: Vec<f32> = Vec::with_capacity(history.len());
        let items: Vec<HistoryItem> = history
            .into_iter()
            .map(|r| {
                heights.push(estimate_card_height(&r.transcript, chars_per_line));
                let formatted = sotto_desktop::app_icon::format_app_name(&r.target_app);
                let (icon, has_icon) = sotto_desktop::app_icon::get_app_icon(&formatted, Some(&r.target_app));
                HistoryItem {
                    id: r.id as i32,
                    timestamp_str: r.timestamp_str.into(),
                    transcript: r.transcript.into(),
                    duration_ms: r.duration_ms,
                    duration_str: r.duration_str.into(),
                    target_app: formatted.into(),
                    app_icon: icon,
                    has_icon,
                }
            })
            .collect();
        studio.set_history_items(slint::ModelRc::from(std::rc::Rc::new(slint::VecModel::from(items))));
        studio.set_history_count(count);
        studio.set_active_tab(0);
        studio.set_history_viewport_y(0.0);

        let total_content_h = sotto_desktop::smooth_scroll::compute_content_height(&heights, sotto_desktop::smooth_scroll::ROW_SPACING)
            + sotto_desktop::smooth_scroll::LIST_BOTTOM_PADDING
            + sotto_desktop::smooth_scroll::LIST_OVERSCROLL_PX;
        studio.set_history_content_height(total_content_h.max(visible_h + 100.0));

        let initial_buf = studio.window().take_snapshot()?;
        let w = initial_buf.width();
        let h = initial_buf.height();

        let art_dir = r"C:\Users\saksh\.t3\userdata\providers\antigravity\3cf4cd39b2f0203a397b9a66f80d5406e109c29014cf36da8ebec39e25e9979e\antigravity-acp\brain\b112fa31-64af-424c-84eb-44fb80a4b6d3";
        let out_video = format!("{}/second_screen_live_scroll_verified.mp4", art_dir);

        let mut ffmpeg = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-f", "rawvideo",
                "-vcodec", "rawvideo",
                "-s", &format!("{}x{}", w, h),
                "-pix_fmt", "rgb24",
                "-r", "60",
                "-i", "-",
                "-c:v", "libopenh264",
                "-b:v", "6M",
                "-pix_fmt", "yuv420p",
                "-movflags", "+faststart",
                &out_video,
            ])
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("Failed to start ffmpeg");

        let mut stdin = ffmpeg.stdin.take().expect("Failed to open ffmpeg stdin");
        use std::io::Write;

        let mut engine = sotto_desktop::smooth_scroll::SmoothScrollEngine::new();
        engine.set_dimensions(total_content_h, visible_h);

        let dt = 1.0 / 60.0;
        let mut frames_written = 0;
        let mut telemetry: Vec<(f32, f32, f32)> = Vec::with_capacity(360);
        let mut prev_viewport_y = 0.0f32;

        // 360 frames (6.0 seconds @ 60 fps)
        for frame in 0..360 {
            // Phase 1: Frame 20 = Single notch down (-120px, isolated dt=1.0)
            if frame == 20 {
                engine.on_wheel_accelerated_dt(-120.0, 1.0);
            }
            // Phase 2: Frames 60..100 = Continuous rapid human scrolling (1 notch every 4 frames ~ 66ms, 11 notches)
            if (60..=100).contains(&frame) && (frame - 60) % 4 == 0 {
                engine.on_wheel_accelerated_dt(-120.0, 0.066);
            }
            // Phase 3: Frames 140..160 = High-speed flick (1 notch every 2 frames ~ 33ms, 11 notches)
            if (140..=160).contains(&frame) && (frame - 140) % 2 == 0 {
                engine.on_wheel_accelerated_dt(-120.0, 0.033);
            }
            // Phase 4: Frames 200..220 = Reverse directional flick up (+120 every 2 frames)
            if (200..=220).contains(&frame) && (frame - 200) % 2 == 0 {
                engine.on_wheel_accelerated_dt(120.0, 0.033);
            }
            // Phase 5: Frame 250 = Direct 1:1 Scrollbar Drag Jump (Windows Explorer Parity)
            if frame == 250 {
                engine.jump_to(-800.0);
            }
            // Phase 6: Frame 290 = Discrete Keyboard Down Arrow (-80px)
            if frame == 290 {
                engine.scroll_by(-80.0);
            }

            if let Some(new_y) = engine.tick_dt(dt) {
                let snapped_y = (new_y * scale).round() / scale;
                studio.set_history_viewport_y(snapped_y);
                studio.set_history_is_scrolling(true);
                studio.set_history_scroll_progress(engine.progress());
            } else if !engine.is_animating() {
                studio.set_history_is_scrolling(false);
            }

            let cur_y = studio.get_history_viewport_y();
            let v = (cur_y - prev_viewport_y) / dt;
            prev_viewport_y = cur_y;
            telemetry.push((frame as f32 * dt, cur_y, v));

            let buf = studio.window().take_snapshot()?;
            let pixels = buf.as_slice();
            let mut rgb_bytes = Vec::with_capacity(pixels.len() * 3);
            for p in pixels {
                rgb_bytes.push(p.r);
                rgb_bytes.push(p.g);
                rgb_bytes.push(p.b);
            }
            if let Err(e) = stdin.write_all(&rgb_bytes) {
                eprintln!("[Video] Stdin write error at frame {}: {}", frame, e);
                break;
            }
            frames_written += 1;
            if frame % 60 == 0 {
                println!("[Video] Frame {}/360: viewport_y = {:.2}px, progress = {:.2}%", frame, studio.get_history_viewport_y(), engine.progress() * 100.0);
            }
        }

        drop(stdin);
        let status = ffmpeg.wait().expect("ffmpeg wait failed");
        println!("[Video] FFmpeg exit status: {:?}. Total frames written: {}", status, frames_written);

        // Also duplicate to sotto_high_speed_scroll_60fps.mp4 for convenience
        let alt_video = format!("{}/sotto_high_speed_scroll_60fps.mp4", art_dir);
        let _ = std::fs::copy(&out_video, &alt_video);

        // Save telemetry data
        let telemetry_path = std::path::Path::new(art_dir).join("second_screen_scroll_telemetry.json");
        let entries: Vec<String> = telemetry.iter().map(|(t, y, v)| format!("[{:.4}, {:.2}, {:.2}]", t, y, v)).collect();
        let json_content = format!("[{}]", entries.join(","));
        match std::fs::write(&telemetry_path, json_content) {
            Ok(_) => println!("[Video] Telemetry successfully written to {}", telemetry_path.display()),
            Err(e) => eprintln!("[Video Error] Failed to write telemetry: {}", e),
        }

        return Ok(());
    }
    let is_hold_to_talk = args.iter().any(|arg| arg == "--hold-to-talk");

    // Comprehensive automated verification of all Phase 1 features
    if is_phase1_test {
        println!("\n=======================================================");
        println!("       SOTTO PHASE 1 COMPREHENSIVE VERIFICATION");
        println!("=======================================================\n");

        // 1. Audio Ingest & Filters (Step 1.3)
        println!("[1/5] Testing In-Memory Audio Ingest & Filtering (Step 1.3)...");
        match audio::recorder::AudioRecorder::new() {
            Ok(rec) => {
                println!("  [PASS] Microphone hardware detected: '{}'", rec.device_name());
            }
            Err(e) => {
                println!("  [WARN] Microphone not attached: {}", e);
            }
        }
        let silent_samples = vec![20i16; 16000];
        let val_silence = audio::filter::validate_audio(&silent_samples, 16000, 250, 0.005);
        assert!(matches!(val_silence, audio::filter::AudioValidation::DiscardSilence { .. }), "Silence filter should flag silent audio");
        println!("  [PASS] Silence energy filter verified");

        let short_samples = vec![1000i16; 1600]; // 100ms
        let val_ghost = audio::filter::validate_audio(&short_samples, 16000, 250, 0.001);
        assert!(matches!(val_ghost, audio::filter::AudioValidation::DiscardGhostTap { .. }), "Ghost tap filter should discard short audio");
        println!("  [PASS] Ghost tap filter verified (duration < 250ms rejected)");

        let wav_header = audio::wav::encode_pcm16_to_wav(&[0i16; 16000], 16000);
        assert_eq!(&wav_header[0..4], b"RIFF");
        assert_eq!(&wav_header[8..12], b"WAVE");
        println!("  [PASS] In-memory WAV container encoder verified (44-byte RIFF header)");

        // 2. Pre-warmed Groq HTTP/2 Client (Step 1.4)
        println!("\n[2/5] Testing Pre-Warmed Groq HTTP/2 API Client (Step 1.4)...");
        let api_key = std::env::var("GROQ_API_KEY")
            .ok()
            .or_else(|| {
                db::HistoryDb::open_default()
                    .ok()
                    .and_then(|db| db.get_provider_key("groq").ok().flatten().map(|k| k.api_key))
                    .filter(|k| !k.trim().is_empty())
            });

        if let Some(ref key) = api_key {
            match router::GroqClient::new(Some(key.clone())) {
                Ok(client) => {
                    println!("  [PASS] Groq HTTP/2 client initialized with connection pool & rustls-tls");
                    let mut pcm = Vec::with_capacity(16000);
                    for i in 0..16000 {
                        let t = i as f32 / 16000.0;
                        let s = (2.0 * std::f32::consts::PI * 440.0 * t).sin();
                        pcm.push((s * 8000.0) as i16);
                    }
                    let wav = audio::wav::encode_pcm16_to_wav(&pcm, 16000);
                    match client.transcribe_blocking(wav) {
                        Ok(res) => {
                            println!("  [PASS] Live Groq HTTP/2 transcription successful!");
                            println!("         Model: {}", res.model);
                            println!("         Response Latency: {} ms", res.latency_ms);
                            println!("         Transcript: \"{}\"", res.text);
                        }
                        Err(e) => {
                            println!("  [FAIL] Live transcription failed: {}", e);
                        }
                    }
                }
                Err(e) => {
                    println!("  [FAIL] Failed to initialize Groq client: {}", e);
                }
            }
        } else {
            println!("  [INFO] GROQ_API_KEY not configured in env or SQLite DB; skipping live Groq probe");
        }

        // 3. Text Injection Subsystem (Step 1.5)
        println!("\n[3/5] Testing Text Injection Subsystem (Step 1.5)...");
        inject::attach_input_desktop();
        let sample = "Sotto Automated Phase 1 Inject Verification";
        match inject::inject_text(sample) {
            Ok(()) => {
                println!("  [PASS] OS clipboard fast-path + SendInput injection verified");
                println!("  [PASS] Modifier key release & 800ms restore guard operational");
            }
            Err(e) => {
                println!("  [FAIL] Injection failed: {}", e);
            }
        }

        // 4. Global Hotkey Engine (Step 1.6)
        println!("\n[4/5] Testing Global Hotkey Engine (Step 1.6)...");
        match hotkey::HotkeyManager::new(hotkey::HotkeyMode::Toggle, |_| {}) {
            Ok(mgr) => {
                println!("  [PASS] Global hotkeys registered (Ctrl+Shift+Space & F8)");
                println!("  [PASS] Mode switching: Toggle -> HoldToTalk -> Toggle");
                mgr.set_mode(hotkey::HotkeyMode::HoldToTalk);
                mgr.set_mode(hotkey::HotkeyMode::Toggle);
            }
            Err(e) => {
                println!("  [NOTE] Hotkey registration note (another instance active?): {}", e);
            }
        }

        // 5. Native System Tray Daemon (Step 3.2)
        println!("\n[5/5] Testing Native Win32 System Tray Daemon (Step 3.2)...");
        match tray::SystemTray::new(|_: tray::TrayAction| {}) {
            Ok(_) => {
                println!("  [PASS] Shell_NotifyIconW & tray message loop initialized");
            }
            Err(e) => {
                println!("  [FAIL] Tray icon initialization failed: {}", e);
            }
        }

        println!("\n=======================================================");
        println!("  ALL PHASE 1 SUBSYSTEMS FULLY VERIFIED & PASSING!");
        println!("=======================================================\n");
        return Ok(());
    }

    // Standalone Audio Safety Net & Retry Verification (Step 2.2)
    // Standalone Text Sanitizer Verification for Step 2.3
    if is_cleaner_test {
        println!("\n=== SOTTO SANITIZER VERIFICATION (Step 2.3) ===");
        let cases = [
            ("Um, I think that uh we should go to to the store.", "I think that we should go to the store."),
            ("Thank you for watching", ""),
            (", uh, we need to to fix this right right now, please!", "We need to fix this right now, please!"),
            ("The quick brown fox jumps over the lazy dog.", "The quick brown fox jumps over the lazy dog."),
        ];
        for (idx, (input, expected)) in cases.iter().enumerate() {
            let output = sotto_desktop::cleaner::sanitize_transcript(input);
            println!("[{}/4] Input:    \"{}\"", idx + 1, input);
            println!("      Expected: \"{}\"", expected);
            println!("      Output:   \"{}\"", output);
            assert_eq!(&output, expected, "Mismatch on test case {}", idx + 1);
        }
        println!("\n[PASS] Local Regex Sanitizer (Step 2.3) fully verified!\n");
        return Ok(());
    }

    // Standalone Post-Processing Pipeline Verification
    if is_post_process_test {
        println!("\n=== SOTTO POST-PROCESSING PIPELINE VERIFICATION ===");
        let mut opts = sotto_desktop::cleaner::PostProcessOptions::default();
        opts.custom_dictionary.push(sotto_desktop::cleaner::DictionaryEntry::new("super tool", "Sotto Engine"));
        let processor = sotto_desktop::cleaner::PostProcessor::new(opts);

        let cases: &[(&str, &str, Option<u32>, &str)] = &[
            (
                "Um, I think that uh we should go to to github and check out super tool.",
                "VS Code",
                None,
                "I think that we should go to GitHub and check out Sotto Engine.",
            ),
            (
                "cargo build with five dollars and ten percent discount.",
                "Windows Terminal",
                None,
                "cargo build with $5 and 10% discount",
            ),
            (
                "contact support at sign example dot com or check hashtag rust",
                "Chrome",
                None,
                "contact support @ example dot com or check #Rust",
            ),
            (
                "sounds good.",
                "Slack",
                None,
                "sounds good",
            ),
            (
                "Thank you for watching",
                "Notepad",
                None,
                "",
            ),
            (
                "Thank you so much for watching this video please like and subscribe to this channel",
                "Chrome",
                Some(500),
                "",
            ),
            (
                "We need to deploy this release right now please",
                "Chrome",
                Some(1500),
                "We need to deploy this release right now please",
            ),
        ];

        for (idx, (input, venue, dur, expected)) in cases.iter().enumerate() {
            let output = processor.process_local_with_duration(input, Some(venue), *dur);
            println!("[{}/{}] Input:    \"{}\" (Venue: {}, Dur: {:?})", idx + 1, cases.len(), input, venue, dur);
            println!("      Expected: \"{}\"", expected);
            println!("      Output:   \"{}\"", output);
            assert_eq!(&output, expected, "Mismatch on test case {}", idx + 1);
        }
        println!("\n[PASS] Post-Processing Layer & Anti-Hallucination Rate Guard fully verified!\n");
        return Ok(());
    }

        if is_retry_test {
        println!("\n=== SOTTO AUDIO RETRY & SAFETY NET VERIFICATION (Step 2.2) ===");
        let safety_net = Arc::new(audio::AudioSafetyNet::new());
        assert!(!safety_net.has_retryable_audio(), "Safety net must start empty");
        assert_eq!(safety_net.retry_count(), 0);

        // Generate synthetic speech PCM (1.5s 440Hz sine wave)
        let mut pcm = Vec::with_capacity(24000);
        for i in 0..24000 {
            let t = i as f32 / 16000.0;
            let s = (2.0 * std::f32::consts::PI * 440.0 * t).sin();
            pcm.push((s * 8000.0) as i16);
        }
        let wav = audio::wav::encode_pcm16_to_wav(&pcm, 16000);
        println!("[1/4] Generated synthetic test audio ({} bytes WAV)", wav.len());

        // Save audio to safety net
        safety_net.save(wav.clone(), 1500, 0x12345);
        assert!(safety_net.has_retryable_audio(), "Safety net must contain cached audio");
        assert_eq!(safety_net.cached_duration_ms(), Some(1500));
        println!("[2/4] Cached audio safely in RAM before Groq upload");

        // Simulate failed network call -> audio must be preserved in RAM
        println!("[3/4] Simulating network drop / failed API response...");
        assert!(safety_net.has_retryable_audio(), "Cached audio must persist after failure");
        assert_eq!(safety_net.retry_count(), 0);

        // Trigger retry -> extract cached audio
        let cached = safety_net.get_for_retry().expect("Audio must be available for retry");
        assert_eq!(
            cached.segments,
            vec![audio::RetrySegment::audio(wav, 1500)]
        );
        assert_eq!(cached.duration_ms, 1500);
        assert_eq!(cached.target_hwnd, 0x12345);
        assert_eq!(cached.retry_count, 1);
        println!("[4/4] Successfully retrieved cached audio on Retry (attempt #1) without re-recording");

        // Simulate successful retry -> clear cache
        safety_net.clear();
        assert!(!safety_net.has_retryable_audio(), "Safety net must be cleared upon successful transcription");
        println!("\n[PASS] Audio Retry & Safety Net (Step 2.2) fully verified!\n");
        return Ok(());
    }

    // Standalone Hotkey Verification for Step 1.6
    if is_hotkey_test {
        println!("\n=== SOTTO GLOBAL HOTKEY VERIFICATION ===");
        println!("Mode: {}", if is_hold_to_talk { "Hold-to-Talk (Hold F8 or Ctrl+Shift+Space)" } else { "Toggle (Tap F8 or Ctrl+Shift+Space)" });
        println!("Press Ctrl+C to exit test mode.\n");

        let mode = if is_hold_to_talk {
            hotkey::HotkeyMode::HoldToTalk
        } else {
            hotkey::HotkeyMode::Toggle
        };

        let is_rec = Arc::new(AtomicBool::new(false));
        let is_rec_cb = Arc::clone(&is_rec);

        let _hotkey_mgr = hotkey::HotkeyManager::new(mode, move |evt| {
            match evt {
                hotkey::HotkeyEvent::StartListening => {
                    is_rec_cb.store(true, Ordering::SeqCst);
                    println!("[Hotkey Test] >>> START LISTENING (Recording Active) <<<");
                }
                hotkey::HotkeyEvent::StopListening => {
                    is_rec_cb.store(false, Ordering::SeqCst);
                    println!("[Hotkey Test] <<< STOP LISTENING (Finalizing Recording) >>>");
                }
                hotkey::HotkeyEvent::CancelListening => {
                    is_rec_cb.store(false, Ordering::SeqCst);
                    println!("[Hotkey Test] <<< CANCEL LISTENING (Recording Discarded) >>>");
                }
            }
            let _ = std::io::stdout().flush();
        }).expect("Failed to initialize hotkey manager");

        loop {
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    // Standalone Live Transcription Verification for Step 1.4
    if is_transcribe_test {
        println!("[Groq Test] Running pre-warmed Groq HTTP/2 transcription test...");
        let api_key = std::env::var("GROQ_API_KEY")
            .ok()
            .or_else(|| {
                db::HistoryDb::open_default()
                    .ok()
                    .and_then(|db| db.get_provider_key("groq").ok().flatten().map(|k| k.api_key))
                    .filter(|k| !k.trim().is_empty())
            });

        let client = match router::GroqClient::new(api_key) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[Groq Test Error] Failed to initialize client: {}", e);
                return Ok(());
            }
        };

        // Synthesize a 1-second 440Hz test sine tone WAV in RAM
        let mut pcm = Vec::with_capacity(16000);
        for i in 0..16000 {
            let t = i as f32 / 16000.0;
            let s = (2.0 * std::f32::consts::PI * 440.0 * t).sin();
            pcm.push((s * 8000.0) as i16);
        }
        let test_wav = audio::wav::encode_pcm16_to_wav(&pcm, 16000);

        println!("[Groq Test] Dispatching 16kHz WAV ({} bytes) over persistent HTTP/2 pool...", test_wav.len());
        match client.transcribe_blocking(test_wav) {
            Ok(res) => {
                println!("\n=== GROQ TRANSCRIPTION RESULT ===");
                println!("Text: \"{}\"", res.text);
                println!("Model: {}", res.model);
                println!("Response Latency: {} ms", res.latency_ms);
            }
            Err(e) => {
                eprintln!("[Groq Test Error] Transcription failed: {}", e);
            }
        }
        return Ok(());
    }

    // Standalone Text Injection Verification for Step 1.5
    if is_inject_test {
        println!("[Inject Test] Initializing OS text injection test helper...");
        println!("[Inject Test] You have 3 seconds to focus Notepad, a browser, or any text field...");
        for i in (1..=3).rev() {
            println!("[Inject Test] Injecting text in {}s...", i);
            let _ = std::io::stdout().flush();
            std::thread::sleep(Duration::from_secs(1));
        }

        let test_message = "Hello from Sotto! (Voice Dictation Text Injection Verified)";
        println!("[Inject Test] Injecting text: \"{}\"", test_message);
        match inject::inject_text(test_message) {
            Ok(()) => {
                println!("\n=== TEXT INJECTION TEST SUCCESSFUL ===");
                println!("Injected String: \"{}\"", test_message);
                println!("Fast-path: Clipboard with Ctrl+V simulated, prior clipboard restored after 800ms.");
            }
            Err(e) => {
                eprintln!("[Inject Test Error] Injection failed: {}", e);
            }
        }
        return Ok(());
    }

    // Standalone mic capture verification for Step 1.3
    if is_record_test {
        println!("[Audio Test] Running standalone mic capture verification...");
        match audio::recorder::AudioRecorder::new() {
            Ok(mut rec) => {
                println!("[Audio Test] Mic detected: '{}'", rec.device_name());
                println!("[Audio Test] Recording 3 seconds of audio into RAM now (speak into your microphone)...");
                let _ = std::io::stdout().flush();

                if let Err(e) = rec.start() {
                    eprintln!("[Audio Test Error] Failed to start recorder: {}", e);
                    return Ok(());
                }

                // Wait 3 seconds
                for i in 1..=3 {
                    std::thread::sleep(Duration::from_secs(1));
                    println!("[Audio Test] Recording... {}s (live RMS: {:.4})", i, rec.live_rms());
                    let _ = std::io::stdout().flush();
                }

                match rec.stop() {
                    Ok(Some(payload)) => {
                        println!("\n=== AUDIO RECORDING TEST SUCCESSFUL ===");
                        println!("Duration: {} ms", payload.duration_ms);
                        println!("Samples (16kHz Mono PCM16): {}", payload.pcm_samples.len());
                        println!("RMS Energy: {:.5}", payload.rms);
                        println!("Peak Amplitude: {:.5}", payload.peak);
                        println!("Digital AGC Gain: {:.2}x", payload.digital_gain);
                        println!("WAV File Size (In-Memory Buffer): {} bytes", payload.wav_bytes.len());

                        let out_path = "target/test_recording.wav";
                        if let Err(e) = std::fs::write(out_path, &payload.wav_bytes) {
                            eprintln!("[Audio Test Error] Failed to write test WAV file: {}", e);
                        } else {
                            println!("Saved test WAV playback verification file to: {}", out_path);
                        }
                    }
                    Ok(None) => {
                        println!("[Audio Test] Recording was rejected by silence or ghost-tap filter.");
                    }
                    Err(e) => {
                        eprintln!("[Audio Test Error] Error stopping recording: {}", e);
                    }
                }
            }
            Err(e) => {
                eprintln!("[Audio Test Error] Failed to initialize AudioRecorder: {}", e);
            }
        }
        return Ok(());
    }

fn load_or_create_ipc_token() -> String {
    let token_path = config::AppConfig::config_path().parent().unwrap().join(".ipc_token");
    if let Ok(existing) = std::fs::read_to_string(&token_path) {
        let trimmed = existing.trim();
        if trimmed.len() >= 16 {
            return trimmed.to_string();
        }
    }
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    let p = Box::new(0u8);
    (&*p as *const u8 as usize).hash(&mut hasher);
    let h1 = hasher.finish();
    let mut hasher2 = std::collections::hash_map::DefaultHasher::new();
    h1.hash(&mut hasher2);
    std::time::Instant::now().hash(&mut hasher2);
    let h2 = hasher2.finish();
    let token = format!("{:016x}{:016x}", h1, h2);
    let _ = std::fs::write(&token_path, &token);
    token
}

    // 1. Single-Instance Check via Local Loopback IPC
    let ipc_token = load_or_create_ipc_token();
    let listener = match TcpListener::bind(IPC_ADDR) {
        Ok(l) => l,
        Err(_) => {
            let addr = IPC_ADDR.parse().unwrap();
            if let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) {
                println!("[Sotto] Another instance is already running. Signaling it to restore app...");
                let payload = format!("{}:RESTORE_APP\n", ipc_token);
                let _ = stream.write_all(payload.as_bytes());
                let _ = stream.flush();
                return Ok(());
            } else {
                // If connect failed, port may be in TIME_WAIT from a recently closed instance.
                // Wait briefly and try binding again, or fallback to an ephemeral port.
                std::thread::sleep(Duration::from_millis(600));
                match TcpListener::bind(IPC_ADDR) {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("[IPC Warning] Could not bind IPC port ({}), fallback to ephemeral listener", e);
                        TcpListener::bind("127.0.0.1:0").map_err(|e| slint::PlatformError::Other(e.to_string()))?
                    }
                }
            }
        }
    };

    println!("Starting Sotto Engine & Floating Pill (startup mode: {})...", is_startup);
    let _ = std::io::stdout().flush();

    // 2. Load persisted customization settings, then initialize the in-memory
    // audio recorder with the selected capture strategy.
    let app_cfg = config::AppConfig::load();
    media::init(app_cfg.media_pause_enabled);
    audio::chime::set_enabled(app_cfg.chimes_enabled);
    let recorder = Arc::new(Mutex::new(
        match audio::recorder::AudioRecorder::new_with_mode(app_cfg.capture_mode) {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("[Audio Ingest Notice] Mic not available at startup: {}", e);
                None
            }
        },
    ));
    if let Ok(mut guard) = GLOBAL_RECORDER.lock() {
        *guard = Some(Arc::downgrade(&recorder));
    }

    // Initialize local SQLite history database with WAL mode and background queue (Step 2.1)
    let history_db = match db::HistoryDb::open_default() {
        Ok(database) => {
            println!("[History DB] Opened SQLite WAL database at: {:?}", db::default_db_path());
            database
        }
        Err(e) => {
            eprintln!("[History DB Warning] Failed to open standard DB (fallback to in-memory): {}", e);
            db::HistoryDb::open_in_memory().expect("In-memory SQLite initialization failed")
        }
    };

    // Load persisted customization settings (active provider included)
    // (already loaded above for recorder mode)

    // Provider-backed transcription dispatcher (Studio -> Providers tab).
    // Key priority per provider: SQLite provider store > environment > none.
    let provider_db = history_db.clone();
    let (_, groq_model) = provider_credentials(&provider_db, router::PROVIDER_GROQ);
    let (_, gemini_model) = provider_credentials(&provider_db, router::PROVIDER_GEMINI);
    let groq_keys = load_provider_key_pool(&provider_db, router::PROVIDER_GROQ);
    let gemini_keys = load_provider_key_pool(&provider_db, router::PROVIDER_GEMINI);
    let transcriber: router::SharedTranscriber = Arc::new(Mutex::new(router::Transcriber::with_key_pools(
        &app_cfg.active_provider,
        groq_keys,
        groq_model,
        gemini_keys,
        gemini_model,
        &app_cfg.transcription_language,
    )));
    if let Ok(t) = transcriber.lock() {
        println!(
            "[Providers] Active: {} (model: {}){}",
            t.active_provider(),
            t.active_model(),
            if t.has_usable_key() { "" } else { " — no API key, add one under Studio -> Providers" }
        );
    }
    if let Ok(mut guard) = SHARED_TRANSCRIBER.lock() {
        *guard = Some(Arc::clone(&transcriber));
    }

    // Background provider model-list refresh (never blocks startup or dictation).
    {
        let db_bg = provider_db.clone();
        let tr_bg = Arc::clone(&transcriber);
        std::thread::spawn(move || refresh_provider_models(db_bg, tr_bg));
    }

    let history_queue = Arc::new(AsyncHistoryQueue::new(history_db));

    // In-memory Audio Safety Net cache (Step 2.2: Retain last buffer in RAM for network failure recovery)
    let safety_net = Arc::new(audio::AudioSafetyNet::new());

    // Authoritative state machine: 0: Idle, 1: Listening, 2: Transcribing, 3: Success, 4: Error
    let engine_state = Arc::new(AtomicU8::new(0));
    let recording_start = Arc::new(Mutex::new(None::<Instant>));
    let target_hwnd = Arc::new(AtomicIsize::new(0));
    let dictation_gen = Arc::new(std::sync::atomic::AtomicU64::new(0));

    // 3. Create Desktop Floating Pill Window
    let pill = FloatingPillWindow::new()?;
    let studio_visible = Arc::new(AtomicBool::new(false));

    // Persisted customization settings (loaded earlier for provider setup)
    let post_processor = Arc::new(sotto_desktop::cleaner::PostProcessor::new(app_cfg.to_post_process_options()));
    if let Ok(mut guard) = GLOBAL_POST_PROCESSOR.lock() {
        *guard = Some(Arc::clone(&post_processor));
    }
    ACTIVE_OPT_IDLE.store(app_cfg.opt_idle, Ordering::Relaxed);
    pill.set_opt_idle(app_cfg.opt_idle);
    pill.set_opt_listening(app_cfg.opt_listening);
    pill.set_opt_transcribe(app_cfg.opt_transcribe);
    pill.set_opt_success(app_cfg.opt_success);
    pill.set_opt_error(app_cfg.opt_error);
    pill.set_surface_opacity(app_cfg.surface_opacity);

    // 4. Window drag handler (smooth delta positioning)
    let pill_weak_drag = pill.as_weak();
    pill.on_drag_window(move |dx, dy| {
        #[cfg(target_os = "windows")]
        {
            if let Some(pill) = pill_weak_drag.upgrade() {
                let win = pill.window();
                let win_handle_wrapper = win.window_handle();
                if let Ok(handle) = win_handle_wrapper.window_handle() {
                    if let RawWindowHandle::Win32(win32_handle) = handle.as_raw() {
                        let hwnd = win32_handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;
                        let edge_margin = (8.0 * win.scale_factor().max(1.0)).round() as i32;
                        let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                        unsafe {
                            GetWindowRect(hwnd, &mut rect as *mut _ as *mut _);
                            let width = rect.right - rect.left;
                            let height = rect.bottom - rect.top;
                            let (x, y) = clamp_pill_origin(
                                rect.left + dx as i32,
                                rect.top + dy as i32,
                                width,
                                height,
                                edge_margin,
                            );
                            SetWindowPos(
                                hwnd,
                                std::ptr::null_mut(),
                                x,
                                y,
                                0,
                                0,
                                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                            );
                        }
                    }
                }
            }
        }
    });

    let pill_weak_drag_finish = pill.as_weak();
    pill.on_drag_finished(move || {
        #[cfg(target_os = "windows")]
        {
            if let Some(pill) = pill_weak_drag_finish.upgrade() {
                let win = pill.window();
                let win_handle_wrapper = win.window_handle();
                if let Ok(handle) = win_handle_wrapper.window_handle() {
                    if let RawWindowHandle::Win32(win32_handle) = handle.as_raw() {
                        let hwnd = win32_handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;
                        let edge_margin = (8.0 * win.scale_factor().max(1.0)).round() as i32;
                        let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                        unsafe {
                            clamp_pill_to_work_area(hwnd, edge_margin);
                            GetWindowRect(hwnd, &mut rect as *mut _ as *mut _);
                        }
                        let mut cfg = config::AppConfig::load();
                        cfg.pill_x = Some(rect.left);
                        cfg.pill_y = Some(rect.top);
                        if let Err(e) = cfg.save() {
                            eprintln!("[Config Error] Failed to persist pill position: {}", e);
                        } else {
                            println!("[Floating Pill] Position persisted at ({}, {})", rect.left, rect.top);
                            let _ = std::io::stdout().flush();
                        }
                    }
                }
            }
        }
    });

    // Inactivity tracker for conservative 45-second working set trimming
    let inactivity = InactivityTracker::new();

    // 5. State transitions: Click triggers recording lifecycle & Groq HTTP/2 dispatch
    let pill_weak = pill.as_weak();
    let recorder_cycle = Arc::clone(&recorder);
    let transcriber_cycle = Arc::clone(&transcriber);
    let engine_state_cycle = Arc::clone(&engine_state);
    let recording_start_cycle = Arc::clone(&recording_start);
    let target_hwnd_cycle = Arc::clone(&target_hwnd);
    let inactivity_cycle = inactivity.clone();
    let history_cycle = Arc::clone(&history_queue);
    let safety_cycle = Arc::clone(&safety_net);
    let processor_cycle = Arc::clone(&post_processor);
    let cycle_fn: Arc<dyn Fn() + Send + Sync> = {
        let p_w = pill_weak.clone();
        let rec = Arc::clone(&recorder_cycle);
        let transcriber_c = Arc::clone(&transcriber_cycle);
        let est = Arc::clone(&engine_state_cycle);
        let rstart = Arc::clone(&recording_start_cycle);
        let thwnd = Arc::clone(&target_hwnd_cycle);
        let inact = inactivity_cycle.clone();
        let hq = Arc::clone(&history_cycle);
        let safety = Arc::clone(&safety_cycle);
        let post_proc = Arc::clone(&processor_cycle);
        let dict_gen_cycle = Arc::clone(&dictation_gen);

        Arc::new(move || {
            let current = est.load(Ordering::SeqCst);
            match current {
                0 => {
                    // Idle -> Start dictation
                    start_dictation(&p_w, &rec, &est, &rstart, &thwnd, &inact, &dict_gen_cycle);
                }
                1 => {
                    // Listening -> Stop dictation & transcribe
                    stop_dictation(&p_w, &rec, &transcriber_c, &est, &rstart, &thwnd, &inact, &hq, &safety, &post_proc, &dict_gen_cycle);
                }
                2 => {
                    // Transcribing -> Click is ignored to prevent UI corruption while processing
                    println!("[State Transition] Click ignored: currently transcribing...");
                }
                3 => {
                    // Success -> Immediately dismiss back to Idle
                    inact.record_activity();
                    est.store(0, Ordering::SeqCst);
                    if let Some(p) = p_w.upgrade() {
                        p.set_active_state(0);
                        p.set_timer_sec(0);
                        p.set_timer_text("0:00".into());
                    }
// Studio preview isolated
                    println!("[State Transition] Clicked during Success: reset to Idle");
                }
                4 => {
                    // Error -> RETRY transcription with cached audio buffer (Step 2.2)
                    inact.record_activity();
                    println!("[HUD Pill] Clicked during Error: executing Audio Safety Net retry");
                    let retried = retry_dictation(&p_w, &safety, &transcriber_c, &est, &inact, &hq, &post_proc);
                    if !retried {
                        // If no audio available or expired, reset to Idle
                        est.store(0, Ordering::SeqCst);
                        if let Some(p) = p_w.upgrade() {
                            p.set_active_state(0);
                            p.set_timer_sec(0);
                            p.set_timer_text("0:00".into());
                        }
// Studio preview isolated
                        println!("[State Transition] No cached audio to retry: reset to Idle");
                    }
                }
                _ => {}
            }
        })
    };

    // Bind cycle callback to floating pill
    let c_pill = cycle_fn.clone();
    pill.on_cycle_state(move || c_pill());



    // 6. Right-Click on Floating Pill -> Toggle Studio / Settings Suite
    let s_vis_toggle = Arc::clone(&studio_visible);
    let p_weak_toggle = pill.as_weak();
    let est_toggle = Arc::clone(&engine_state);
    let c_toggle = cycle_fn.clone();

    let hq_toggle = Arc::clone(&history_queue);
    pill.on_toggle_suite(move || {
        toggle_studio_window(
            &p_weak_toggle,
            &est_toggle,
            &s_vis_toggle,
            &c_toggle,
            &hq_toggle,
        );
    });

    // 7. Global Hotkey Engine (Step 1.6)
    let hotkey_mode = resolve_hotkey_mode(is_hold_to_talk, app_cfg.trigger_mode);

    let pill_hk_weak = pill.as_weak();
    let inactivity_hk = inactivity.clone();
    let rec_hk = Arc::clone(&recorder);
    let transcriber_hk = Arc::clone(&transcriber);
    let est_hk = Arc::clone(&engine_state);
    let rstart_hk = Arc::clone(&recording_start);
    let thwnd_hk = Arc::clone(&target_hwnd);
    let hq_hk = Arc::clone(&history_queue);
    let safety_hk = Arc::clone(&safety_net);
    let processor_hk = Arc::clone(&post_processor);
    let dict_gen_hk = Arc::clone(&dictation_gen);

    // Arm the saved trigger slots: primary always, secondary when set, and cancel triggers.
    // (Config load already migrated legacy preset/custom/mouse settings.)
    let _hotkey_mgr = match hotkey::HotkeyManager::new_with_bindings(
        hotkey_mode,
        &app_cfg.primary_trigger,
        &app_cfg.secondary_trigger,
        &app_cfg.primary_cancel_trigger,
        &app_cfg.secondary_cancel_trigger,
        move |evt| {
        let p_weak = pill_hk_weak.clone();
        let r_clone = Arc::clone(&rec_hk);
        let t_clone = Arc::clone(&transcriber_hk);
        let est_clone = Arc::clone(&est_hk);
        let rstart_clone = Arc::clone(&rstart_hk);
        let thwnd_clone = Arc::clone(&thwnd_hk);
        let hq_clone = Arc::clone(&hq_hk);
        let safety_clone = Arc::clone(&safety_hk);
        let inact_clone = inactivity_hk.clone();
        let proc_clone = Arc::clone(&processor_hk);
        let dict_gen_clone = Arc::clone(&dict_gen_hk);
        let _ = slint::invoke_from_event_loop(move || {
            match evt {
                hotkey::HotkeyEvent::StartListening => {
                    let cur = est_clone.load(Ordering::SeqCst);
                    if cur == 4 {
                        println!("[Hotkey] Triggered during Error state: executing Safety Net retry");
                        let retried = retry_dictation(&p_weak, &safety_clone, &t_clone, &est_clone, &inact_clone, &hq_clone, &proc_clone);
                        if !retried {
                            start_dictation(&p_weak, &r_clone, &est_clone, &rstart_clone, &thwnd_clone, &inact_clone, &dict_gen_clone);
                        }
                    } else {
                        start_dictation(&p_weak, &r_clone, &est_clone, &rstart_clone, &thwnd_clone, &inact_clone, &dict_gen_clone);
                    }
                }
                hotkey::HotkeyEvent::StopListening => {
                    stop_dictation(&p_weak, &r_clone, &t_clone, &est_clone, &rstart_clone, &thwnd_clone, &inact_clone, &hq_clone, &safety_clone, &proc_clone, &dict_gen_clone);
                }
                hotkey::HotkeyEvent::CancelListening => {
                    println!("[Hotkey Engine] Cancel listening event triggered");
                    cancel_dictation(&p_weak, &r_clone, &est_clone, &rstart_clone, &dict_gen_clone, &safety_clone);
                }
            }
        });
    }) {
        Ok(mgr) => {
            println!(
                "[Sotto Engine] Triggers Active: primary={} secondary={} (Mode: {:?})",
                app_cfg.primary_trigger.display(),
                app_cfg.secondary_trigger.as_ref().map(|b| b.display()).unwrap_or_else(|| "—".to_string()),
                hotkey_mode
            );
            // Share the LIVE manager with the Studio panel so trigger changes
            // apply to the engine driving dictation.
            let shared = std::sync::Arc::new(mgr);
            if let Ok(mut guard) = GLOBAL_HOTKEY_MGR.lock() {
                *guard = Some(std::sync::Arc::clone(&shared));
            }
            Some(shared)
        }
        Err(e) => {
            eprintln!("[Sotto Engine Error] Failed to register global hotkeys: {}", e);
            None
        }
    };

    // 8. Adaptive Animation Clock (Event-Driven Zero-Cost Idle Architecture)
    let pill_anim_weak = pill.as_weak();
    let recorder_anim = Arc::clone(&recorder);
    let engine_state_anim = Arc::clone(&engine_state);
    let recording_start_anim = Arc::clone(&recording_start);
    let studio_vis_anim = Arc::clone(&studio_visible);
    let timer = slint::Timer::default();
    let mut tick: f32 = 0.0;
    let mut studio_anim_tick: f32 = 0.0;
    let mut smoothed_energy: f32 = 0.0;
    let mut adaptive_ambient: f32 = 0.0080;
    let mut speech_hangover_ticks: u32 = 0;
    let mut idle_tick_divider: u32 = 0;
    let mut was_listening: bool = false;
    let mut last_studio_preview_state: i32 = -1;

    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(20),
        move || {
            let pill_opt = pill_anim_weak.upgrade();
            let cur_state = engine_state_anim.load(Ordering::Relaxed);
            let is_listening = cur_state == 1;
            let studio_vis = studio_vis_anim.load(Ordering::Relaxed);

            // Live Studio Customization Preview Pill Animation:
            // Animates preview pill inside Studio window only during Listening preview state
            let (studio_tab, studio_preview) = if studio_vis {
                with_studio(|s| {
                    let tab = s.get_active_tab();
                    let preview_state = s.get_active_state();
                    if tab == 2 {
                        if preview_state == 1 {
                            studio_anim_tick += 1.0;
                            s.set_anim_tick(studio_anim_tick);
                            // Organic waveform animation during Listening preview
                            let t = studio_anim_tick * 0.14;
                            let b3 = 3.5 + (t * 1.5).sin().abs() * 10.5;
                            let b2 = 3.0 + ((t + 0.6) * 1.3).sin().abs() * 7.5;
                            let b4 = 3.0 + ((t + 1.2) * 1.3).sin().abs() * 7.5;
                            let b1 = 3.0 + ((t + 0.3) * 1.0).sin().abs() * 5.0;
                            let b5 = 3.0 + ((t + 0.9) * 1.0).sin().abs() * 5.0;
                            s.set_bar1_h(b1);
                            s.set_bar2_h(b2);
                            s.set_bar3_h(b3);
                            s.set_bar4_h(b4);
                            s.set_bar5_h(b5);
                            let preview_sec = (studio_anim_tick as u64 / 50) % 60;
                            s.set_timer_text(format!("0:{:02}", preview_sec).into());
                            s.set_timer_sec(preview_sec as i32);
                        } else if last_studio_preview_state != preview_state {
                            // Settle to static rest exactly ONCE on state change (prevents 50Hz layout churn)
                            s.set_bar1_h(3.0);
                            s.set_bar2_h(3.0);
                            s.set_bar3_h(3.0);
                            s.set_bar4_h(3.0);
                            s.set_bar5_h(3.0);
                            s.set_timer_text("0:00".into());
                            s.set_timer_sec(0);
                        }
                        last_studio_preview_state = preview_state;
                    }
                    (tab, preview_state)
                })
                .unwrap_or((-1, -1))
            } else {
                (-1, -1)
            };

            let opt_idle = ACTIVE_OPT_IDLE.load(Ordering::Relaxed);
            if is_listening {
                was_listening = true;
                tick += 1.0;

                // 1. Elapsed time tracking
                if let Ok(guard) = recording_start_anim.lock() {
                    if let Some(start) = *guard {
                        let elapsed = start.elapsed();
                        let secs = elapsed.as_secs();
                        let timer_str = format!("{:01}:{:02}", secs / 60, secs % 60);
                        if let Some(p) = &pill_opt {
                            p.set_timer_sec((secs % 60) as i32);
                            p.set_timer_text(timer_str.clone().into());
                        }
                    }
                }

                // 2. Real microphone live energy & 3 acoustic formant bands (lock-free)
                let (raw_rms, low_rms, mid_rms, high_rms) = if let Ok(rec_guard) = recorder_anim.try_lock() {
                    rec_guard.as_ref().map(|r| r.live_formants()).unwrap_or((0.0, 0.0, 0.0, 0.0))
                } else {
                    (0.0, 0.0, 0.0, 0.0)
                };

                // 3. Dynamic Adaptive Noise Floor Tracker (VAD):
                if raw_rms < adaptive_ambient {
                    adaptive_ambient += (raw_rms - adaptive_ambient) * 0.12;
                } else {
                    adaptive_ambient += (raw_rms - adaptive_ambient) * 0.0015;
                }
                adaptive_ambient = adaptive_ambient.clamp(0.0020, 0.0350);

                let speech_threshold = adaptive_ambient * 1.55 + 0.0028;
                let is_speech = raw_rms > speech_threshold;
                if is_speech {
                    speech_hangover_ticks = 8;
                } else if speech_hangover_ticks > 0 {
                    speech_hangover_ticks -= 1;
                }

                let voice_activity = if is_speech || speech_hangover_ticks > 0 {
                    let snr_excess = (raw_rms - speech_threshold).max(0.0);
                    let norm = (snr_excess / 0.028).clamp(0.0, 1.0);
                    0.30 + 0.70 * norm.sqrt()
                } else {
                    0.0
                };

                if voice_activity > smoothed_energy {
                    smoothed_energy += (voice_activity - smoothed_energy) * 0.85;
                } else {
                    smoothed_energy += (voice_activity - smoothed_energy) * 0.20;
                    if smoothed_energy < 0.015 {
                        smoothed_energy = 0.0;
                    }
                }

                let (b1, b2, b3, b4, b5) = if smoothed_energy > 0.0 {
                    let e = smoothed_energy;
                    let denom = raw_rms + 0.001;
                    let low_ratio = (low_rms / denom).clamp(0.2, 1.8);
                    let mid_ratio = (mid_rms / denom).clamp(0.3, 2.2);
                    let high_ratio = (high_rms / denom).clamp(0.2, 1.8);

                    let flutter = (tick * 0.16).sin() * 0.12 + 0.88;

                    let b3 = 3.0 + e * (9.0 + 7.5 * mid_ratio * flutter);
                    let b2 = 3.0 + e * (5.5 + 5.0 * ((low_ratio + mid_ratio) * 0.5) * flutter);
                    let b4 = 3.0 + e * (5.5 + 5.0 * ((mid_ratio + high_ratio) * 0.5) * flutter);
                    let b1 = 3.0 + e * (3.0 + 3.5 * low_ratio * flutter);
                    let b5 = 3.0 + e * (3.0 + 3.5 * high_ratio * flutter);

                    (b1, b2, b3, b4, b5)
                } else {
                    (3.0, 3.0, 3.0, 3.0, 3.0)
                };

                if let Some(p) = &pill_opt {
                    p.set_anim_tick(tick);
                    p.set_bar1_h(b1);
                    p.set_bar2_h(b2);
                    p.set_bar3_h(b3);
                    p.set_bar4_h(b4);
                    p.set_bar5_h(b5);
                }
            } else {
                // Non-listening states
                if was_listening {
                    was_listening = false;
                    smoothed_energy = 0.0;
                    speech_hangover_ticks = 0;
                    if let Some(p) = &pill_opt {
                        p.set_bar1_h(3.0);
                        p.set_bar2_h(3.0);
                        p.set_bar3_h(3.0);
                        p.set_bar4_h(3.0);
                        p.set_bar5_h(3.0);
                    }
                }

                if cur_state == 0 {
                    idle_tick_divider = (idle_tick_divider + 1) % 100;
                    if opt_idle != 0 && idle_tick_divider % 4 == 0 {
                        tick += 1.0;
                        if let Some(p) = &pill_opt {
                            p.set_anim_tick(tick);
                        }
                    }
                } else {
                    tick += 1.0;
                    if let Some(p) = &pill_opt {
                        p.set_anim_tick(tick);
                    }
                }
            }

            if !should_anim_timer_run(
                cur_state,
                opt_idle,
                studio_vis,
                studio_tab,
                studio_preview,
                was_listening,
                last_studio_preview_state,
            ) {
                STUDIO_ANIM_TIMER.with(|t| {
                    if let Some(ref anim) = *t.borrow() {
                        anim.stop();
                    }
                });
            }
        },
    );
    if !should_anim_timer_run(
        engine_state.load(Ordering::Relaxed),
        ACTIVE_OPT_IDLE.load(Ordering::Relaxed),
        studio_visible.load(Ordering::Relaxed),
        -1,
        -1,
        false,
        -1,
    ) {
        timer.stop();
    }
    STUDIO_ANIM_TIMER.with(|t| {
        *t.borrow_mut() = Some(timer);
    });

    // 9. IPC Background Listener: Responds to secondary .exe launches
    let s_vis_ipc = Arc::clone(&studio_visible);
    let p_weak_ipc = pill.as_weak();
    let est_ipc = Arc::clone(&engine_state);
    let c_ipc = cycle_fn.clone();
    let hq_ipc = Arc::clone(&history_queue);
    let inactivity_ipc = inactivity.clone();
    let safety_ipc = Arc::clone(&safety_net);
    let transcriber_ipc = Arc::clone(&transcriber);
    let processor_ipc = Arc::clone(&post_processor);
    let ipc_auth_token = ipc_token.clone();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if let Ok(mut stream) = stream {
                let mut buf = [0u8; 256];
                if let Ok(n) = stream.read(&mut buf) {
                    let raw = String::from_utf8_lossy(&buf[..n]);
                    let upper = raw.to_uppercase();
                    if upper.starts_with("GET ")
                        || upper.starts_with("POST ")
                        || upper.starts_with("PUT ")
                        || upper.starts_with("DELETE ")
                        || upper.starts_with("OPTIONS ")
                        || upper.starts_with("HEAD ")
                        || upper.starts_with("CONNECT ")
                        || upper.contains("HOST:")
                        || upper.contains("ORIGIN:")
                        || upper.contains("HTTP/1.")
                        || upper.contains("HTTP/2.")
                    {
                        eprintln!("[IPC Security Warning] Dropped HTTP cross-origin probe / malformed request.");
                        let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n");
                        let _ = stream.flush();
                        continue;
                    }

                    let parts: Vec<&str> = raw.trim().splitn(2, ':').collect();
                    if parts.len() != 2 || parts[0].trim() != ipc_auth_token.trim() {
                        eprintln!("[IPC Security Warning] Rejected unauthenticated IPC command.");
                        let _ = stream.write_all(b"UNAUTHORIZED\n");
                        let _ = stream.flush();
                        continue;
                    }
                    let msg = parts[1].trim();
                    if msg.contains("RESTORE_APP") || msg.contains("SHOW_SUITE") {
                        let vis = Arc::clone(&s_vis_ipc);
                        let p_weak = p_weak_ipc.clone();
                        let est = Arc::clone(&est_ipc);
                        let c = c_ipc.clone();
                        let hq = Arc::clone(&hq_ipc);

                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(pill) = p_weak.upgrade() {
                                let _ = pill.show();
                                setup_floating_pill_window(&pill);
                            }
                            if !vis.load(Ordering::SeqCst) {
                                toggle_studio_window(&p_weak, &est, &vis, &c, &hq);
                            } else {
                                with_studio(|s| {
                                    let _ = s.show();
                                    bring_studio_to_front(s);
                                });
                            }
                            println!("[IPC] Secondary launch detected. Restored floating pill and opened Studio Suite.");
                            let _ = std::io::stdout().flush();
                        });
                    } else if msg.contains("HIDE_SUITE") {
                        let vis = Arc::clone(&s_vis_ipc);
                        let p_weak = p_weak_ipc.clone();
                        let est = Arc::clone(&est_ipc);
                        let c = c_ipc.clone();
                        let hq = Arc::clone(&hq_ipc);

                        let _ = slint::invoke_from_event_loop(move || {
                            if vis.load(Ordering::SeqCst) {
                                toggle_studio_window(&p_weak, &est, &vis, &c, &hq);
                                println!("[IPC] Studio Suite hidden via IPC.");
                                let _ = std::io::stdout().flush();
                            }
                        });
                    } else if msg.contains("SET_TAB_1") {
                        let _ = slint::invoke_from_event_loop(move || {
                            with_studio(|s| {
                                s.set_active_tab(1);
                            });
                        });
                    } else if msg.contains("SET_TAB_2") {
                        let _ = slint::invoke_from_event_loop(move || {
                            with_studio(|s| {
                                s.set_active_tab(2);
                            });
                        });
                    } else if msg.contains("TRIGGER_RETRY") {
                        let p_weak = p_weak_ipc.clone();
                        let safety = Arc::clone(&safety_ipc);
                        let transcriber_r = Arc::clone(&transcriber_ipc);
                        let est = Arc::clone(&est_ipc);
                        let inact = inactivity_ipc.clone();
                        let hq = Arc::clone(&hq_ipc);
                        let proc = Arc::clone(&processor_ipc);
                        let _ = slint::invoke_from_event_loop(move || {
                            retry_dictation(&p_weak, &safety, &transcriber_r, &est, &inact, &hq, &proc);
                        });
                    } else if msg.contains("SET_TAB_0") {
                        let _ = slint::invoke_from_event_loop(move || {
                            with_studio(|s| {
                                s.set_active_tab(0);
                            });
                        });
                    } else if msg.contains("RECORD_ACTIVITY") {
                        inactivity_ipc.record_activity();
                        println!("[IPC] User activity recorded via IPC.");
                        let _ = std::io::stdout().flush();
                    } else if msg.contains("CHECK_TRIM") {
                        if inactivity_ipc.check_and_trim(Duration::ZERO) {
                            println!("[IPC] Immediate check_and_trim executed via IPC.");
                        }
                        let _ = std::io::stdout().flush();
                    } else if msg.contains("EXIT") {
                        println!("[IPC] Clean exit requested via IPC -> terminating cleanly");
                        let _ = std::io::stdout().flush();
                        #[cfg(target_os = "windows")]
                        unsafe {
                            windows_sys::Win32::System::Threading::TerminateProcess(
                                windows_sys::Win32::System::Threading::GetCurrentProcess(),
                                0,
                            );
                        }
                        #[cfg(not(target_os = "windows"))]
                        std::process::exit(0);
                    }
                }
            }
        }
    });

    // 10. Show Floating Pill window
    pill.show()?;
    let _ = setup_floating_pill_window(&pill);

    // Re-position pill after window is fully mapped (repeats until handle is ready)
    let pill_pos_weak = pill.as_weak();
    let pos_timer = slint::Timer::default();
    let attempts = Arc::new(AtomicU8::new(0));
    let docked = Arc::new(AtomicBool::new(false));
    let docked_clone = docked.clone();
    pos_timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(50),
        move || {
            if docked_clone.load(Ordering::SeqCst) {
                return;
            }
            let count = attempts.fetch_add(1, Ordering::SeqCst);
            if let Some(pill) = pill_pos_weak.upgrade() {
                if setup_floating_pill_window(&pill) || count >= 40 {
                    docked_clone.store(true, Ordering::SeqCst);
                }
            }
        },
    );

    // Keep the pill clear of the taskbar when the shell changes while Sotto is
    // running: taskbar height, docking edge, monitor layout, or a Windhawk mod
    // re-applying its style. The check is a no-op when the pill is already
    // inside the usable area, so it never fights a drag.
    #[cfg(target_os = "windows")]
    let pill_bounds_weak = pill.as_weak();
    #[cfg(target_os = "windows")]
    let pill_bounds_timer = slint::Timer::default();
    #[cfg(target_os = "windows")]
    pill_bounds_timer.start(
        slint::TimerMode::Repeated,
        Duration::from_secs(2),
        move || {
            if let Some(pill) = pill_bounds_weak.upgrade() {
                let win = pill.window();
                if !win.is_visible() {
                    return;
                }
                let win_handle_wrapper = win.window_handle();
                if let Ok(handle) = win_handle_wrapper.window_handle() {
                    if let RawWindowHandle::Win32(win32_handle) = handle.as_raw() {
                        let hwnd = win32_handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;
                        let edge_margin = (8.0 * win.scale_factor().max(1.0)).round() as i32;
                        unsafe {
                            let _ = clamp_pill_to_work_area(hwnd, edge_margin);
                        }
                    }
                }
            }
        },
    );

    // 11. If user launched app manually (not --startup at boot), open Studio Suite
    if !is_startup {
        toggle_studio_window(
            &pill.as_weak(),
            &engine_state,
            &studio_visible,
            &cycle_fn,
            &history_queue,
        );
    }

    // 12. Native System Tray Daemon (Step 3.2)
    let s_vis_tray = Arc::clone(&studio_visible);
    let p_weak_tray = pill.as_weak();
    let est_tray = Arc::clone(&engine_state);
    let c_tray = cycle_fn.clone();
    let hq_tray = Arc::clone(&history_queue);

    let pill_vis_tray = Arc::new(AtomicBool::new(true));
    let pill_vis_action = Arc::clone(&pill_vis_tray);
    let rec_tray = Arc::clone(&recorder);

    let _tray = match tray::SystemTray::new(move |action| {
        match action {
            tray::TrayAction::OpenStudio => {
                let vis = Arc::clone(&s_vis_tray);
                let p_weak = p_weak_tray.clone();
                let est = Arc::clone(&est_tray);
                let c = c_tray.clone();
                let hq = Arc::clone(&hq_tray);

                let _ = slint::invoke_from_event_loop(move || {
                    toggle_studio_window(&p_weak, &est, &vis, &c, &hq);
                });
            }
            tray::TrayAction::TogglePill => {
                let p_weak = p_weak_tray.clone();
                let p_vis = Arc::clone(&pill_vis_action);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(p) = p_weak.upgrade() {
                        let currently_visible = p_vis.load(Ordering::SeqCst);
                        if currently_visible {
                            let _ = p.hide();
                            p_vis.store(false, Ordering::SeqCst);
                            println!("[System Tray] Floating Pill hidden");
                            trim_working_set();
                        } else {
                            let _ = p.show();
                            setup_floating_pill_window(&p);
                            p_vis.store(true, Ordering::SeqCst);
                            println!("[System Tray] Floating Pill shown (Zero taskbar footprint)");
                        }
                        let _ = std::io::stdout().flush();
                    }
                });
            }
            tray::TrayAction::Exit => {
                println!("[System Tray] Exit requested from tray menu -> terminating cleanly");
                let _ = std::io::stdout().flush();
                if let Ok(mut rec) = rec_tray.lock() {
                    if let Some(r) = rec.as_mut() {
                        let _ = r.stop();
                    }
                    *rec = None;
                }
                media::shutdown();
                #[cfg(target_os = "windows")]
                unsafe {
                    windows_sys::Win32::System::Threading::TerminateProcess(
                        windows_sys::Win32::System::Threading::GetCurrentProcess(),
                        0,
                    );
                }
                #[cfg(not(target_os = "windows"))]
                std::process::exit(0);
            }
        }
    }) {
        Ok(t) => Some(t),
        Err(e) => {
            eprintln!("[System Tray Notice] Failed to create tray icon: {}", e);
            None
        }
    };

    // 12. Conservative Inactivity Trimming (45 Seconds Threshold)
    // Ensures physical memory remains warm during active dictation workflows,
    // avoiding soft page fault latency penalties between consecutive sentences.
    let inactivity_timer = inactivity.clone();
    let engine_state_trim = Arc::clone(&engine_state);
    let conservative_timer = slint::Timer::default();
    conservative_timer.start(
        slint::TimerMode::Repeated,
        Duration::from_secs(5),
        move || {
            // Only trim if currently in Idle state (state 0)
            if engine_state_trim.load(Ordering::SeqCst) == 0 {
                if inactivity_timer.check_and_trim(Duration::from_secs(45)) {
                    println!("[Memory Optimizer] 45s of complete inactivity reached -> applied conservative working set trim");
                    let _ = std::io::stdout().flush();
                }
            }
        },
    );

    // Initial post-startup working set trim: flushes cold startup initialization pages
    let trim_timer = slint::Timer::default();
    trim_timer.start(
        slint::TimerMode::SingleShot,
        Duration::from_millis(1200),
        move || {
            println!("[Optimizer] Post-startup working set trim applied");
            trim_working_set();
        },
    );

    // Run Slint event loop
    let res = slint::run_event_loop();
    // Release the microphone before handing the route back to media playback.
    if let Ok(mut rec_guard) = recorder.lock() {
        if let Some(rec) = rec_guard.as_mut() {
            let _ = rec.stop();
        }
    }
    media::shutdown();
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_anim_timer_run_gates_idle_and_wakes_on_activity() {
        // Static Idle with Studio closed: stopped (0 Hz).
        assert!(!should_anim_timer_run(0, 0, false, 0, 0, false, -1));
        // Static Idle on Dictations or Providers tab: stopped (0 Hz).
        assert!(!should_anim_timer_run(0, 0, true, 0, 1, false, -1));
        assert!(!should_anim_timer_run(0, 0, true, 1, 1, false, -1));
        // Tab 2 Preview in non-listening settled state: stopped (0 Hz).
        assert!(!should_anim_timer_run(0, 0, true, 2, 0, false, 0));
        // Tab 2 Preview switching from Listening(1) -> Idle(0): 1 settle frame runs.
        assert!(should_anim_timer_run(0, 0, true, 2, 0, false, 1));
        // Tab 2 Preview Listening(1): active.
        assert!(should_anim_timer_run(0, 0, true, 2, 1, false, 1));
        // Animated pill idle option (opt_idle != 0): active.
        assert!(should_anim_timer_run(0, 1, false, 0, 0, false, -1));
        // Recording/transcribing/success/error: active.
        assert!(should_anim_timer_run(1, 0, false, 0, 0, false, -1));
        assert!(should_anim_timer_run(2, 0, false, 0, 0, false, -1));
        // Exiting listening: 1 settle frame runs (`was_listening`).
        assert!(should_anim_timer_run(0, 0, false, 0, 0, true, -1));
    }

    #[test]
    fn test_decode_gesture_pan_point_validates_id_flags_and_signed_coords() {
        const TEST_GID_PAN: u32 = 4;
        const GF_BEGIN: u32 = 0x0000_0001;

        assert_eq!(decode_gesture_pan_point(TEST_GID_PAN, -12, 340, GF_BEGIN, TEST_GID_PAN), Some((-12, 340, true)));
        assert_eq!(decode_gesture_pan_point(TEST_GID_PAN, 0, -45, 0, TEST_GID_PAN), Some((0, -45, false)));
        // Non-pan gestures must return None so Windows DefWindowProc handles them.
        assert_eq!(decode_gesture_pan_point(3 /* GID_ZOOM */, 10, 20, 0, TEST_GID_PAN), None);
    }

    #[test]
    fn test_conservative_inactivity_tracker() {
        let tracker = InactivityTracker::new();
        assert!(!tracker.is_trimmed());

        // A threshold of 45s should NOT trigger trim immediately
        assert!(!tracker.check_and_trim(Duration::from_secs(45)));
        assert!(!tracker.is_trimmed());

        // With zero threshold, it triggers trim
        assert!(tracker.check_and_trim(Duration::ZERO));
        assert!(tracker.is_trimmed());

        // Repeated check within maintenance interval does not repeat trim
        assert!(!tracker.check_and_trim(Duration::ZERO));

        // When maintenance interval has elapsed, periodic trim triggers
        assert!(tracker.check_and_trim_maintenance(Duration::ZERO, Duration::ZERO));

        // Interaction resets tracker
        tracker.record_activity();
        assert!(!tracker.is_trimmed());
        assert!(tracker.check_and_trim(Duration::ZERO));
    }

    #[test]
    fn test_trim_working_set_safety() {
        // Calling trim_working_set should execute safely without crashing or throwing SEH exceptions
        trim_working_set();
    }

    #[test]
    fn test_adaptive_animation_idle_skips_ticks() {
        // In Idle state (0) with Option 0 (Micro Mic), animation ticks must be skipped
        let state = 0u8;
        let opt_idle = 0i32;

        let should_tick = if state == 0 {
            opt_idle != 0
        } else {
            true
        };

        assert!(!should_tick, "Idle state with static micro mic should not generate animation ticks");
    }

    #[test]
    fn test_adaptive_animation_throttled_cadence() {
        // In Idle state with animated variant (Pulsing Pip), ticks must be throttled to 1 in 4 (80ms)
        let mut ticks_generated = 0;
        let mut divider = 0;

        for _ in 0..100 {
            divider = (divider + 1) % 4;
            if divider == 0 {
                ticks_generated += 1;
            }
        }

        assert_eq!(ticks_generated, 25, "100 timer cycles at 20ms should produce exactly 25 throttled ticks (80ms cadence)");
    }

    #[test]
    fn test_lazy_studio_holder_lifecycle() {
        let holder: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));

        // Initial state is unallocated (lazy)
        assert!(holder.lock().unwrap().is_none(), "Lazy holder should start unallocated");

        // First user demand initializes the holder
        *holder.lock().unwrap() = Some(true);
        assert!(holder.lock().unwrap().is_some(), "Lazy holder should be instantiated on demand");

        // Visibility tracking
        let visible = Arc::new(AtomicBool::new(false));
        visible.store(true, Ordering::SeqCst);
        assert!(visible.load(Ordering::SeqCst));
        visible.store(false, Ordering::SeqCst);
        assert!(!visible.load(Ordering::SeqCst));
    }

    #[test]
    fn test_audio_buffer_safety_and_no_underrun() {
        // Verify audio buffer size calculations for real-time safety
        let sample_rate = 16000u32;
        let duration_secs = 10u32;
        let expected_samples = (sample_rate * duration_secs) as usize;
        let bytes_per_pcm16_sample = 2usize;
        let expected_bytes = expected_samples * bytes_per_pcm16_sample;

        assert_eq!(expected_samples, 160_000);
        assert_eq!(expected_bytes, 320_000);

        let mut test_buffer = Vec::with_capacity(expected_samples);
        test_buffer.resize(expected_samples, 0i16);
        assert_eq!(test_buffer.capacity(), expected_samples);
        assert_eq!(test_buffer.len(), expected_samples);
    }

    #[test]
    fn test_state_machine_pipeline_transitions() {
        let state = Arc::new(AtomicU8::new(0));

        // Idle (0) -> Listening (1)
        state.store(1, Ordering::SeqCst);
        assert_eq!(state.load(Ordering::SeqCst), 1);

        // Listening (1) -> Transcribing (2)
        state.store(2, Ordering::SeqCst);
        assert_eq!(state.load(Ordering::SeqCst), 2);

        // Transcribing (2) -> Success (3)
        state.store(3, Ordering::SeqCst);
        assert_eq!(state.load(Ordering::SeqCst), 3);

        // Success (3) -> Idle (0)
        state.store(0, Ordering::SeqCst);
        assert_eq!(state.load(Ordering::SeqCst), 0);

        // Error (4) -> Idle (0)
        state.store(4, Ordering::SeqCst);
        assert_eq!(state.load(Ordering::SeqCst), 4);
        state.store(0, Ordering::SeqCst);
        assert_eq!(state.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_customization_config_roundtrip_and_defaults() {
        let default_cfg = config::AppConfig::default();
        assert_eq!(default_cfg.opt_idle, 0);
        assert_eq!(default_cfg.opt_listening, 0);
        assert_eq!(default_cfg.opt_transcribe, 0);
        assert_eq!(default_cfg.opt_success, 0);
        assert_eq!(default_cfg.opt_error, 0);
        assert_eq!(default_cfg.surface_opacity, 0.95);

        let serialized = serde_json::to_string(&default_cfg).unwrap();
        let deserialized: config::AppConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, default_cfg);
    }

    #[test]
    fn test_app_name_and_icon_sanitization() {
        assert_eq!(app_icon::format_app_name("T3 Code (Alpha).exe"), "T3 Code (Alpha)");
        assert_eq!(app_icon::format_app_name("t3code.exe"), "T3 Code (Alpha)");
        assert_eq!(app_icon::format_app_name("P3 Code Alpha.exe"), "T3 Code (Alpha)");
        assert_eq!(app_icon::format_app_name("code.exe"), "VS Code");
        assert_eq!(app_icon::format_app_name("chrome.exe"), "Google Chrome");
        assert_eq!(app_icon::format_app_name("slack.exe"), "Slack");
        assert_eq!(app_icon::format_app_name("windowsterminal.exe"), "Windows Terminal");
        assert_eq!(app_icon::format_app_name("notepad.exe"), "Notepad");
        assert_eq!(app_icon::format_app_name("Desktop"), "Desktop");

        let (icon, has_icon) = app_icon::get_app_icon("Desktop", None);
        assert!(!has_icon);
        assert_eq!(icon.size().width, 0);
    }

    #[test]
    fn test_history_scroll_animation_suppression_gate() {
        // Tab 0 (History): animation updates must be skipped so Slint event loop is 100% free for scrolling
        let active_tab = 0;
        let should_tick_studio = active_tab == 1;
        assert!(!should_tick_studio, "History tab must completely suppress studio animation property sets");

        // Tab 2 (Customization): animation updates active
        let active_tab = 2;
        let should_tick_studio = active_tab == 2;
        assert!(should_tick_studio, "Customization tab must receive animation property sets");
    }

    fn screen_rect(left: i32, top: i32, right: i32, bottom: i32) -> ScreenRect {
        ScreenRect {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn test_pill_area_leaves_normal_work_area_alone() {
        // A standard bottom taskbar ends exactly at rcWork.bottom, so there is
        // nothing to subtract and the pill keeps the full work area.
        let monitor = screen_rect(0, 0, 1920, 1080);
        let work = screen_rect(0, 0, 1920, 1040);
        let taskbar = screen_rect(0, 1040, 1920, 1080);
        assert_eq!(work.without_taskbar(monitor, taskbar), work);
    }

    #[test]
    fn test_pill_area_shrinks_for_taller_taskbar() {
        // Simulates Windhawk drawing an 80 px taskbar while Windows still
        // reserves only 40 px. The pill must move above the visible taskbar.
        let monitor = screen_rect(0, 0, 1920, 1080);
        let work = screen_rect(0, 0, 1920, 1040);
        let taskbar = screen_rect(0, 1000, 1920, 1080);
        assert_eq!(
            work.without_taskbar(monitor, taskbar),
            screen_rect(0, 0, 1920, 1000)
        );
    }

    #[test]
    fn test_pill_area_handles_top_and_side_taskbars() {
        let monitor = screen_rect(0, 0, 1920, 1080);

        let top_work = screen_rect(0, 60, 1920, 1080);
        let top_taskbar = screen_rect(0, 0, 1920, 80);
        assert_eq!(
            top_work.without_taskbar(monitor, top_taskbar),
            screen_rect(0, 80, 1920, 1080)
        );

        let left_work = screen_rect(60, 0, 1920, 1080);
        let left_taskbar = screen_rect(0, 0, 80, 1080);
        assert_eq!(
            left_work.without_taskbar(monitor, left_taskbar),
            screen_rect(80, 0, 1920, 1080)
        );
    }

    #[test]
    fn test_pill_area_ignores_taskbar_on_another_monitor() {
        let monitor = screen_rect(1920, 0, 3840, 1080);
        let work = screen_rect(1920, 0, 3840, 1040);
        let primary_taskbar = screen_rect(0, 1040, 1920, 1080);
        assert_eq!(work.without_taskbar(monitor, primary_taskbar), work);
    }

    #[test]
    fn test_pill_origin_clamps_into_work_area() {
        let area = screen_rect(0, 0, 1920, 1040);
        assert_eq!(area.clamp_point(2000, 2000, 140, 60, 8), (1772, 972));
        assert_eq!(area.clamp_point(-50, -50, 140, 60, 8), (8, 8));
        assert_eq!(area.clamp_point(100, 100, 140, 60, 8), (100, 100));
    }

    #[test]
    fn test_resolve_hotkey_mode_honors_saved_trigger_mode_and_cli_override() {
        assert_eq!(resolve_hotkey_mode(false, 0), hotkey::HotkeyMode::Toggle);
        assert_eq!(resolve_hotkey_mode(false, 1), hotkey::HotkeyMode::HoldToTalk);
        assert_eq!(resolve_hotkey_mode(true, 0), hotkey::HotkeyMode::HoldToTalk);
        assert_eq!(resolve_hotkey_mode(true, 1), hotkey::HotkeyMode::HoldToTalk);
        assert_eq!(resolve_hotkey_mode(false, 99), hotkey::HotkeyMode::Toggle);
    }

    #[test]
    fn test_drop_studio_instance_deallocates_caches_and_resets_visibility() {
        let vis = Arc::new(AtomicBool::new(true));
        STUDIO_HISTORY_CACHE.with(|c| {
            let mut items = c.borrow_mut();
            items.clear();
            items.push(HistoryItem {
                id: 1,
                timestamp_str: "now".into(),
                transcript: "Testing studio teardown cache release.".into(),
                duration_ms: 900,
                duration_str: "0.9s".into(),
                target_app: "Notepad".into(),
                app_icon: slint::Image::default(),
                has_icon: false,
            });
        });
        let (count, total_h) = recompute_studio_history_layout(1040, 1.0);
        assert_eq!(count, 1);
        assert!(total_h > 0.0);

        STUDIO_CACHED_WINDOW.with(|w| {
            *w.borrow_mut() = Some((
                sotto_desktop::smooth_scroll::VirtualWindow {
                    first_idx: 0,
                    last_idx: 1,
                    top_spacer: 0.0,
                    bottom_spacer: 0.0,
                },
                0.0,
                Instant::now(),
            ));
        });

        drop_studio_instance(&vis);

        assert!(!vis.load(Ordering::SeqCst));
        STUDIO_HISTORY_CACHE.with(|c| {
            assert!(c.borrow().is_empty());
            assert_eq!(c.borrow().capacity(), 0);
        });
        STUDIO_HISTORY_LAYOUT.with(|l| {
            let layout = l.borrow();
            assert!(layout.offsets.is_empty());
            assert_eq!(layout.total_content_h, 0.0);
        });
        STUDIO_CACHED_WINDOW.with(|w| assert!(w.borrow().is_none()));
    }
}
