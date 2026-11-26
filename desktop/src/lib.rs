// Copyright (c) 2026 Sotto
// Core library exposing subsystems for desktop application and integration testing

slint::include_modules!();

/// Testable UI components: each `ui/components/*.slint` root compiled
/// separately (see build.rs) and re-exported flatly here. Production renders
/// the same sources through hud.slint; these handles exist so tests can mount
/// components in isolation without a StudioWindow.
pub mod components {
    macro_rules! component_module {
        ($module:ident, $file:literal, $($item:ident),+) => {
            mod $module {
                include!(concat!(env!("OUT_DIR"), "/", $file));
            }
            $(pub use $module::$item;)+
        };
    }

    component_module!(stat_chip_mod, "stat_chip.rs", StatChip);
    component_module!(section_header_mod, "section_header.rs", SectionHeader);
    component_module!(tab_bar_mod, "tab_bar.rs", TabBar);
    component_module!(
        provider_card_mod,
        "provider_card.rs",
        ProviderCard
    );
    component_module!(model_row_mod, "model_row.rs", ModelRow);
    component_module!(key_field_mod, "key_field.rs", KeyField);
    component_module!(history_card_mod, "history_card.rs", HistoryCard);
    component_module!(switch_mod, "switch.rs", SottoSwitch);
    component_module!(keycap_mod, "keycap.rs", Keycap, KeycapSize, KeycapTone);
    component_module!(keycap_combo_mod, "keycap_combo.rs", KeycapCombo);
}

pub mod app_logic;
pub mod audio;
pub mod media;
pub mod long_form;
pub mod router;
pub mod inject;
pub mod hotkey;
pub mod tray;
pub mod db;
pub mod app_icon;
pub mod config;
pub mod cleaner;
pub mod smooth_scroll;
pub mod startup;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const IPC_ADDR: &str = "127.0.0.1:49281";

/// Trims idle physical memory working set using standard Win32 API.
/// Safely returns dormant unreferenced pages to the OS standby list.
#[cfg(target_os = "windows")]
pub fn trim_working_set() {
    unsafe {
        windows_sys::Win32::System::Threading::SetProcessWorkingSetSize(
            windows_sys::Win32::System::Threading::GetCurrentProcess(),
            usize::MAX,
            usize::MAX,
        );
    }
}

#[cfg(not(target_os = "windows"))]
pub fn trim_working_set() {}

/// Tracks user interaction and safely applies conservative working set trimming
/// only after a prolonged period (e.g. 45 seconds) of complete inactivity, and
/// periodically (e.g. every 45s) during prolonged idle to reclaim memory from dropped
/// network connections (such as the 2-minute HTTP/2 socket timeout).
#[derive(Clone)]
pub struct InactivityTracker {
    last_activity: Arc<Mutex<Instant>>,
    last_trim: Arc<Mutex<Option<Instant>>>,
}

impl InactivityTracker {
    pub fn new() -> Self {
        Self {
            last_activity: Arc::new(Mutex::new(Instant::now())),
            last_trim: Arc::new(Mutex::new(None)),
        }
    }

    /// Marks that the user interacted or dictated, keeping memory warm.
    pub fn record_activity(&self) {
        if let Ok(mut lock) = self.last_activity.lock() {
            *lock = Instant::now();
        }
        if let Ok(mut trim_lock) = self.last_trim.lock() {
            *trim_lock = None;
        }
    }

    /// Checks if inactivity threshold has elapsed;
    /// Trims at the threshold (e.g. 45s), and periodically (every maintenance_interval)
    /// during prolonged continuous idle so that memory released by dropped connections (like HTTP/2 at 2 mins)
    /// is returned to the OS, keeping idle memory strictly below ~0.9 MB!
    pub fn check_and_trim_maintenance(&self, idle_threshold: Duration, maintenance_interval: Duration) -> bool {
        let (elapsed_activity, elapsed_trim) = {
            let act = self.last_activity.lock().map(|l| l.elapsed()).unwrap_or(Duration::ZERO);
            let trim = self.last_trim.lock().map(|l| *l).unwrap_or(None);
            (act, trim)
        };

        if elapsed_activity >= idle_threshold {
            let should_trim = match elapsed_trim {
                None => true, // First time entering idle state past threshold
                Some(last) => last.elapsed() >= maintenance_interval, // Periodic maintenance during prolonged idle
            };

            if should_trim {
                if let Ok(mut trim_lock) = self.last_trim.lock() {
                    *trim_lock = Some(Instant::now());
                }
                trim_working_set();
                return true;
            }
        }
        false
    }

    pub fn check_and_trim(&self, idle_threshold: Duration) -> bool {
        self.check_and_trim_maintenance(idle_threshold, Duration::from_secs(45))
    }

    pub fn is_trimmed(&self) -> bool {
        self.last_trim.lock().map(|l| l.is_some()).unwrap_or(false)
    }
}

impl Default for InactivityTracker {
    fn default() -> Self {
        Self::new()
    }
}
