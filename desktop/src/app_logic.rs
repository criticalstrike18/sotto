// Copyright (c) 2026 Sotto
// Testable application controller logic shared between main.rs UI wiring and integration tests.

use crate::cleaner::{PostProcessOptions, PostProcessor};
use crate::config::{AppConfig, CaptureMode};
use crate::hotkey::TriggerBinding;
use crate::router::{is_english_only_model, language_display_name};

/// Action decided when the user clicks the floating pill or presses Error-retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PillClickAction {
    StartListening,
    StopListening,
    IgnoreTranscribing,
    DismissSuccess,
    RetryAudio,
    ResetErrorToIdle,
    IgnoreUnknown,
}

/// Pure state-machine transition helper for floating pill clicks (states 0..=4).
pub fn decide_pill_click_action(current_state: u8, has_retry_audio: bool) -> PillClickAction {
    match current_state {
        0 => PillClickAction::StartListening,
        1 => PillClickAction::StopListening,
        2 => PillClickAction::IgnoreTranscribing,
        3 => PillClickAction::DismissSuccess,
        4 => {
            if has_retry_audio {
                PillClickAction::RetryAudio
            } else {
                PillClickAction::ResetErrorToIdle
            }
        }
        _ => PillClickAction::IgnoreUnknown,
    }
}

/// Normalizes a transcription language against the selected speech model.
/// Returns `(effective_lang_code, dropdown_display_label, supports_multilingual)`.
pub fn normalize_language_for_model(requested_lang: &str, model_name: &str) -> (String, String, bool) {
    let is_en_only = is_english_only_model(model_name);
    let effective = if is_en_only {
        "en".to_string()
    } else {
        requested_lang.to_string()
    };
    let display = if effective == "en" || effective == "auto" {
        "More Languages".to_string()
    } else {
        language_display_name(&effective)
    };
    (effective, display, !is_en_only)
}

/// Updates Smart Polish LLM enable state in config and pushes options to `PostProcessor`.
pub fn apply_post_process_toggle(cfg: &mut AppConfig, processor: &PostProcessor, enabled: bool) -> PostProcessOptions {
    cfg.post_process_llm_enabled = enabled;
    let opts = cfg.to_post_process_options();
    processor.update_options(opts.clone());
    opts
}

/// Switches LLM post-processing provider, auto-selecting a compatible default LLM model if needed.
/// Returns the resulting `post_process_llm_model`.
pub fn apply_post_process_provider(cfg: &mut AppConfig, processor: &PostProcessor, provider: &str) -> String {
    cfg.post_process_llm_provider = provider.to_string();
    if provider == "groq"
        && !cfg.post_process_llm_model.starts_with("qwen")
        && !cfg.post_process_llm_model.starts_with("openai")
    {
        cfg.post_process_llm_model = "qwen/qwen3.8-27b".to_string();
    } else if provider == "gemini" && !cfg.post_process_llm_model.starts_with("gemini") {
        cfg.post_process_llm_model = "gemini-2.5-flash".to_string();
    }
    processor.update_options(cfg.to_post_process_options());
    cfg.post_process_llm_model.clone()
}

/// Sets the LLM post-processing model and updates the live `PostProcessor`.
pub fn apply_post_process_model(cfg: &mut AppConfig, processor: &PostProcessor, model: &str) {
    cfg.post_process_llm_model = model.to_string();
    processor.update_options(cfg.to_post_process_options());
}

/// Sets the LLM post-processing style (`0 = Minimal`, `1 = Context-Aware`) and updates the live `PostProcessor`.
pub fn apply_post_process_style(cfg: &mut AppConfig, processor: &PostProcessor, style: i32) {
    cfg.post_process_style = style;
    processor.update_options(cfg.to_post_process_options());
}

/// Formatted trigger labels matching Studio's 4 hotkey slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerSlotDisplays {
    pub primary: String,
    pub secondary: String,
    pub cancel_primary: String,
    pub cancel_secondary: String,
}

impl TriggerSlotDisplays {
    pub fn from_config(cfg: &AppConfig) -> Self {
        Self {
            primary: cfg.primary_trigger.display(),
            secondary: cfg
                .secondary_trigger
                .as_ref()
                .map(|b| b.display())
                .unwrap_or_else(|| "Not set".to_string()),
            cancel_primary: cfg
                .primary_cancel_trigger
                .as_ref()
                .map(|b| b.display())
                .unwrap_or_else(|| "Not set".to_string()),
            cancel_secondary: cfg
                .secondary_cancel_trigger
                .as_ref()
                .map(|b| b.display())
                .unwrap_or_else(|| "Not set".to_string()),
        }
    }
}

/// Resets one of the 4 trigger slots (`0 = Primary -> F8`, `1 = Secondary -> None`,
/// `2 = Cancel Primary -> Esc`, `3 = Cancel Secondary -> None`) and returns display + status text.
pub fn apply_reset_trigger_slot(slot: i32, cfg: &mut AppConfig) -> (TriggerSlotDisplays, String) {
    match slot {
        1 => cfg.secondary_trigger = None,
        2 => cfg.primary_cancel_trigger = Some(TriggerBinding::escape()),
        3 => cfg.secondary_cancel_trigger = None,
        _ => cfg.primary_trigger = TriggerBinding::f8(),
    }
    let displays = TriggerSlotDisplays::from_config(cfg);
    let status = match slot {
        1 => "Secondary trigger cleared".to_string(),
        2 => format!("Primary Cancel reset to {}", displays.cancel_primary),
        3 => "Secondary Cancel cleared".to_string(),
        _ => format!("Primary trigger reset to {}", displays.primary),
    };
    (displays, status)
}

/// Pure snapshot of Studio customization settings applied onto `AppConfig`.
#[derive(Debug, Clone, PartialEq)]
pub struct CustomizationSnapshot {
    pub opt_idle: i32,
    pub opt_listening: i32,
    pub opt_transcribe: i32,
    pub opt_success: i32,
    pub opt_error: i32,
    pub surface_opacity: f32,
    pub trigger_mode: i32,
    pub capture_mode: CaptureMode,
    pub chimes_enabled: bool,
    pub media_pause_enabled: bool,
}

impl CustomizationSnapshot {
    pub fn apply_to(&self, cfg: &mut AppConfig) {
        cfg.opt_idle = self.opt_idle;
        cfg.opt_listening = self.opt_listening;
        cfg.opt_transcribe = self.opt_transcribe;
        cfg.opt_success = self.opt_success;
        cfg.opt_error = self.opt_error;
        cfg.surface_opacity = self.surface_opacity;
        cfg.trigger_mode = self.trigger_mode;
        cfg.capture_mode = self.capture_mode;
        cfg.chimes_enabled = self.chimes_enabled;
        cfg.media_pause_enabled = self.media_pause_enabled;
    }
}

/// Splits a formatted shortcut label (`"Ctrl + Shift + Space"`, `"F8"`, `"Not set"`)
/// into individual keycap labels for rendering `Keycap` components.
pub fn binding_keycaps(display: &str) -> Vec<String> {
    let trimmed = display.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("Not set") {
        return Vec::new();
    }
    trimmed
        .split(" + ")
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}
