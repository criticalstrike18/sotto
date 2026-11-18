// Copyright (c) 2026 Sotto
// Persistent Configuration for Voice Pill Customization & Post-Processing Pipeline

use crate::cleaner::{DictionaryEntry, PostProcessMode, PostProcessOptions};
use crate::hotkey::TriggerBinding;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

fn default_true() -> bool {
    true
}

fn default_provider() -> String {
    "groq".to_string()
}

fn default_language() -> String {
    "en".to_string()
}

fn default_secondary() -> Option<TriggerBinding> {
    Some(TriggerBinding::ctrl_shift_space())
}

fn default_primary_cancel() -> Option<TriggerBinding> {
    Some(TriggerBinding::escape())
}

fn default_secondary_cancel() -> Option<TriggerBinding> {
    None
}

fn default_llm_provider() -> String {
    "groq".to_string()
}

fn default_llm_model() -> String {
    "qwen/qwen3.8-27b".to_string()
}

/// How the recorder obtains microphone audio.
///
/// The three modes are intentionally independent of the trigger mode: this is
/// the audio-capture strategy, not how dictation starts and stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    /// Legacy behaviour: keep the default (often Bluetooth) stream alive, so
    /// the headset stays in HFP/SCO and repeat takes start instantly.
    AlwaysOn,
    /// A2DP-safe bridge: the onboard mic runs a rolling pre-roll while idle,
    /// the headset mic opens on trigger, and the two takes are aligned and
    /// spliced once the headset delivers real audio.
    DualMicSplice,
    /// Never open a Bluetooth capture endpoint. Dictation uses the onboard /
    /// USB microphone only; A2DP is never interrupted.
    OnboardOnly,
}

impl Default for CaptureMode {
    fn default() -> Self {
        // The new path is the product default; AlwaysOn remains the explicit
        // fallback for endpoints where the bridge misbehaves.
        Self::DualMicSplice
    }
}

impl CaptureMode {
    pub fn as_i32(self) -> i32 {
        match self {
            Self::AlwaysOn => 0,
            Self::DualMicSplice => 1,
            Self::OnboardOnly => 2,
        }
    }

    pub fn from_i32(value: i32) -> Self {
        match value {
            0 => Self::AlwaysOn,
            2 => Self::OnboardOnly,
            _ => Self::DualMicSplice,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::AlwaysOn => "headset_always_on",
            Self::DualMicSplice => "dual_mic_splice",
            Self::OnboardOnly => "onboard_only",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppConfig {
    pub opt_idle: i32,
    pub opt_listening: i32,
    pub opt_transcribe: i32,
    pub opt_success: i32,
    pub opt_error: i32,
    pub surface_opacity: f32,

    // Remembered floating pill coordinates
    #[serde(default)]
    pub pill_x: Option<i32>,
    #[serde(default)]
    pub pill_y: Option<i32>,

    // Unified trigger slots: primary always armed, secondary optional.
    // Either slot holds a keyboard combo, a single key, a mouse button, or a
    // keyboard+mouse chord (max 5 keys). See hotkey::TriggerBinding.
    #[serde(default = "TriggerBinding::f8")]
    pub primary_trigger: TriggerBinding,
    #[serde(default = "default_secondary")]
    pub secondary_trigger: Option<TriggerBinding>,

    // Cancel triggers: abort recording/transcription without pasting or sending request
    #[serde(default = "default_primary_cancel")]
    pub primary_cancel_trigger: Option<TriggerBinding>,
    #[serde(default = "default_secondary_cancel")]
    pub secondary_cancel_trigger: Option<TriggerBinding>,

    // Automatic Windows startup integration
    #[serde(default)]
    pub launch_on_startup: bool,

    // One-time migration flag for pre-slot configs (see migrate_legacy).
    #[serde(default)]
    pub migrated_v2: bool,

    // --- Legacy trigger fields (pre-slot era). Read once by migrate_legacy,
    // --- then ignored. Kept so old files still parse.
    // Hotkey & Trigger configuration
    #[serde(default)]
    pub hotkey_preset: i32,
    #[serde(default)]
    pub trigger_mode: i32,

    // Active transcription provider ("groq" | "gemini"). API keys themselves
    // live in the SQLite provider store (see db::providers), never here.
    #[serde(default = "default_provider")]
    pub active_provider: String,

    /// Audio capture strategy. Missing from older configs -> new bridge mode.
    #[serde(default)]
    pub capture_mode: CaptureMode,

    /// Play a short synthesized cue when recording starts and stops.
    #[serde(default = "default_true")]
    pub chimes_enabled: bool,

    /// Pause the currently-playing non-call media session for the dictation
    /// window, then resume the exact session that Sotto paused.
    #[serde(default = "default_true")]
    pub media_pause_enabled: bool,

    #[serde(default = "default_language")]
    pub transcription_language: String,

    // Mouse trigger: 0 = off, 1 = Back (XBUTTON1), 2 = Forward (XBUTTON2),
    // 3 = Middle. Independent of (and combinable with) the keyboard trigger.
    #[serde(default)]
    pub mouse_trigger: i32,

    // Dynamic custom hotkey configuration
    // `use_custom_hotkey` is the source of truth for whether the recorded
    // custom key (e.g. standalone Ctrl) overrides `hotkey_preset`.
    #[serde(default)]
    pub use_custom_hotkey: bool,
    #[serde(default)]
    pub custom_hotkey_display: Option<String>,
    #[serde(default)]
    pub custom_hotkey_modifiers: Option<u32>,
    #[serde(default)]
    pub custom_hotkey_vk: Option<u32>,

    // Post-processing options with serde defaults for backwards compatibility
    #[serde(default)]
    pub post_process_mode: PostProcessMode,
    #[serde(default)]
    pub custom_dictionary: Vec<DictionaryEntry>,
    #[serde(default = "default_true")]
    pub venue_formatting_enabled: bool,
    #[serde(default = "default_true")]
    pub number_symbol_normalization: bool,

    // Optional LLM post-processing (smart punctuation, grammar, context formatting)
    #[serde(default)]
    pub post_process_llm_enabled: bool,
    #[serde(default = "default_llm_provider")]
    pub post_process_llm_provider: String,
    #[serde(default = "default_llm_model")]
    pub post_process_llm_model: String,
    #[serde(default)]
    pub post_process_style: i32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            opt_idle: 0,
            opt_listening: 0,
            opt_transcribe: 0,
            opt_success: 0,
            opt_error: 0,
            surface_opacity: 0.95,
            pill_x: None,
            pill_y: None,
            hotkey_preset: 0,
            trigger_mode: 0,
            primary_trigger: TriggerBinding::f8(),
            secondary_trigger: default_secondary(),
            primary_cancel_trigger: default_primary_cancel(),
            secondary_cancel_trigger: default_secondary_cancel(),
            launch_on_startup: false,
            migrated_v2: true,
            mouse_trigger: 0,
            active_provider: default_provider(),
            capture_mode: CaptureMode::default(),
            chimes_enabled: true,
            media_pause_enabled: true,
            transcription_language: default_language(),
            use_custom_hotkey: false,
            custom_hotkey_display: None,
            custom_hotkey_modifiers: None,
            custom_hotkey_vk: None,
            post_process_mode: PostProcessMode::LocalFast,
            custom_dictionary: Vec::new(),
            venue_formatting_enabled: true,
            number_symbol_normalization: true,
            post_process_llm_enabled: false,
            post_process_llm_provider: default_llm_provider(),
            post_process_llm_model: default_llm_model(),
            post_process_style: 0,
        }
    }
}

impl AppConfig {
    pub fn to_post_process_options(&self) -> PostProcessOptions {
        PostProcessOptions {
            mode: if self.post_process_llm_enabled {
                PostProcessMode::SmartLLM
            } else {
                self.post_process_mode
            },
            custom_dictionary: self.custom_dictionary.clone(),
            venue_formatting_enabled: self.venue_formatting_enabled,
            number_symbol_normalization: self.number_symbol_normalization,
            llm_enabled: self.post_process_llm_enabled,
            llm_provider: self.post_process_llm_provider.clone(),
            llm_model: self.post_process_llm_model.clone(),
            llm_style: self.post_process_style,
        }
    }

    /// Migrates pre-slot configs exactly once. Rules preserve prior behavior:
    /// - recorded custom keyboard -> primary keyboard part
    /// - otherwise the numeric preset -> primary
    /// - configured mouse button -> secondary mouse part (nothing lost)
    /// - otherwise secondary defaults to Ctrl+Shift+Space, unless primary
    ///   already is (no duplicate arming)
    pub fn migrate_legacy(&mut self) {
        if self.migrated_v2 {
            return;
        }
        let css = TriggerBinding::ctrl_shift_space();
        let primary = if self.use_custom_hotkey {
            match (self.custom_hotkey_modifiers, self.custom_hotkey_vk) {
                (Some(mods), Some(vk)) => {
                    TriggerBinding::normalized(mods, Some(vk), None)
                }
                _ => TriggerBinding::f8(),
            }
        } else {
            match self.hotkey_preset {
                1 => css,
                2 => TriggerBinding::normalized(0x0001, Some(0x20), None), // Alt+Space
                3 => TriggerBinding::ctrl_space(),
                _ => TriggerBinding::f8(),
            }
        };

        let secondary = match self.mouse_trigger {
            1 => Some(TriggerBinding::mouse_back()),
            2 => Some(TriggerBinding::mouse_forward()),
            3 => Some(TriggerBinding::mouse_middle()),
            _ => {
                if primary == css {
                    None
                } else {
                    Some(css)
                }
            }
        };

        self.primary_trigger = primary;
        self.secondary_trigger = secondary;
        if self.primary_cancel_trigger.is_none() {
            self.primary_cancel_trigger = default_primary_cancel();
        }
        self.migrated_v2 = true;
    }

    pub fn config_path() -> PathBuf {
        let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
        let dir = Path::new(&appdata).join("Sotto");
        let _ = fs::create_dir_all(&dir);
        dir.join("config.json")
    }

    pub fn load_from_path(path: &Path) -> Self {
        if path.exists() {
            if let Ok(content) = fs::read_to_string(path) {
                if let Ok(mut config) = serde_json::from_str::<AppConfig>(&content) {
                    config.migrate_legacy();
                    return config;
                }
            }
        }
        let mut d = Self::default();
        d.migrated_v2 = true;
        d
    }

    pub fn load() -> Self {
        Self::load_from_path(&Self::config_path())
    }

    pub fn save_to_path(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Serialization error: {}", e))?;
        fs::write(path, json).map_err(|e| format!("Write error: {}", e))?;
        Ok(())
    }

    pub fn save(&self) -> Result<(), String> {
        self.save_to_path(&Self::config_path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_mode_defaults_to_dual_mic_splice() {
        assert_eq!(CaptureMode::default(), CaptureMode::DualMicSplice);
    }

    #[test]
    fn capture_mode_round_trips_through_serde() {
        for mode in [
            CaptureMode::AlwaysOn,
            CaptureMode::DualMicSplice,
            CaptureMode::OnboardOnly,
        ] {
            let json = serde_json::to_string(&mode).unwrap();
            let decoded: CaptureMode = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, mode, "mode {} lost in serde round-trip", mode.label());
            assert_eq!(CaptureMode::from_i32(mode.as_i32()), mode);
        }
    }

    #[test]
    fn older_config_without_capture_mode_gets_the_new_default() {
        // Simulate a config written before the toggle existed by serializing a
        // full default config and removing the new key.
        let mut value = serde_json::to_value(AppConfig::default()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("capture_mode");
        let cfg: AppConfig = serde_json::from_value(value).unwrap();
        assert_eq!(cfg.capture_mode, CaptureMode::DualMicSplice);
    }

    #[test]
    fn unknown_capture_mode_falls_back_to_dual_mic_splice() {
        assert_eq!(CaptureMode::from_i32(99), CaptureMode::DualMicSplice);
        assert_eq!(CaptureMode::from_i32(-1), CaptureMode::DualMicSplice);
    }

    #[test]
    fn feedback_settings_default_on_for_older_configs() {
        let mut value = serde_json::to_value(AppConfig::default()).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("chimes_enabled");
        object.remove("media_pause_enabled");

        let cfg: AppConfig = serde_json::from_value(value).unwrap();
        assert!(cfg.chimes_enabled);
        assert!(cfg.media_pause_enabled);
    }

    #[test]
    fn feedback_settings_round_trip() {
        let mut cfg = AppConfig::default();
        cfg.chimes_enabled = false;
        cfg.media_pause_enabled = false;

        let json = serde_json::to_string(&cfg).unwrap();
        let decoded: AppConfig = serde_json::from_str(&json).unwrap();
        assert!(!decoded.chimes_enabled);
        assert!(!decoded.media_pause_enabled);
    }
}
