// Copyright (c) 2026 Sotto
// Post-Processing Layer: Modular Execution Pipeline

use super::dictionary::{DictionaryEngine, DictionaryEntry};
use super::formatter::{NumberAndSymbolFormatter, VenueCategory, VenueFormatter};
use super::sanitizer::{is_hallucination, is_rate_hallucination, SottoSanitizer};
use crate::router::gemini::GeminiClient;
use crate::router::groq::GroqClient;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Post-processing execution mode
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum PostProcessMode {
    /// Zero-latency sub-microsecond local pipeline (fast-path Cow, stutters, dictionary, venue rules)
    #[default]
    LocalFast,
    /// Smart formatting via pre-warmed Groq/Gemini LLM completion with graceful instant fallback
    SmartLLM,
}

fn default_llm_provider() -> String {
    "groq".to_string()
}

fn default_llm_model() -> String {
    "qwen/qwen3.8-27b".to_string()
}

/// Options configuring the Post-Processing Pipeline
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PostProcessOptions {
    pub mode: PostProcessMode,
    pub custom_dictionary: Vec<DictionaryEntry>,
    pub venue_formatting_enabled: bool,
    pub number_symbol_normalization: bool,
    #[serde(default)]
    pub llm_enabled: bool,
    #[serde(default = "default_llm_provider")]
    pub llm_provider: String,
    #[serde(default = "default_llm_model")]
    pub llm_model: String,
    #[serde(default)]
    pub llm_style: i32,
}

impl Default for PostProcessOptions {
    fn default() -> Self {
        Self {
            mode: PostProcessMode::LocalFast,
            custom_dictionary: Vec::new(),
            venue_formatting_enabled: true,
            number_symbol_normalization: true,
            llm_enabled: false,
            llm_provider: default_llm_provider(),
            llm_model: default_llm_model(),
            llm_style: 0,
        }
    }
}

/// Central Post-Processing Engine coordinating sanitization, dictionary, formatting, and LLM smart modes.
pub struct PostProcessor {
    options: RwLock<PostProcessOptions>,
    dictionary: Arc<DictionaryEngine>,
}

impl Default for PostProcessor {
    fn default() -> Self {
        Self::new(PostProcessOptions::default())
    }
}

impl PostProcessor {
    /// Creates a new PostProcessor with initial options.
    pub fn new(options: PostProcessOptions) -> Self {
        let dictionary = Arc::new(DictionaryEngine::new(&options.custom_dictionary));
        Self {
            options: RwLock::new(options),
            dictionary,
        }
    }

    /// Updates pipeline options and reloads custom dictionary rules.
    pub fn update_options(&self, new_opts: PostProcessOptions) {
        self.dictionary.set_custom_entries(&new_opts.custom_dictionary);
        if let Ok(mut lock) = self.options.write() {
            *lock = new_opts;
        }
    }

    /// Gets current options snapshot.
    pub fn get_options(&self) -> PostProcessOptions {
        self.options.read().map(|g| g.clone()).unwrap_or_default()
    }

    /// Zero-latency synchronous local post-processing pipeline with duration sanity checks.
    /// Runs in ~1-2µs with zero network dependency.
    pub fn process_local_with_duration(
        &self,
        raw_transcript: &str,
        app_venue_name: Option<&str>,
        duration_ms: Option<u32>,
    ) -> String {
        let trimmed = raw_transcript.trim();
        if trimmed.is_empty() || is_hallucination(trimmed) {
            return String::new();
        }

        // Duration-to-word count sanity check: physically impossible speech rates (>10 words/sec)
        // are recognized as Whisper hallucinations on short microphone noise and safely dropped.
        if let Some(dur) = duration_ms {
            if is_rate_hallucination(trimmed, dur) {
                println!(
                    "[PostProcessor] Dropped duration rate hallucination ({} words in {}ms)",
                    trimmed.split_whitespace().count(),
                    dur
                );
                return String::new();
            }
        }

        let opts = self.get_options();

        // Stage 1: Local Sanitizer (removes fillers, stutters, hallucinations, edge punct)
        let cleaned = SottoSanitizer::clean(trimmed);
        if cleaned.is_empty() {
            return String::new();
        }

        // Stage 2: Spoken Numbers, Currency & Symbols Normalization
        let mut working = if opts.number_symbol_normalization {
            NumberAndSymbolFormatter::format(&cleaned)
        } else {
            cleaned.into_owned()
        };

        // Stage 3: Dictionary & Casing Rules (Tech Vocabulary + Custom Entries)
        working = self.dictionary.apply(&working);

        // Stage 4: Venue & Application Context Formatting
        if opts.venue_formatting_enabled {
            let app_name = app_venue_name.unwrap_or("Desktop");
            let venue = VenueCategory::from_app_name(app_name);
            working = VenueFormatter::format(&working, venue);
        }

        working
    }

    /// Asynchronous post-processing pipeline with duration sanity checks.
    /// If LLM mode is enabled, routes to Groq (Qwen / GPT-OSS) or Gemini with pre-warmed HTTP/2,
    /// falling back gracefully to `LocalFast` on any error or timeout (>1.5s).
    pub async fn process_async_with_duration(
        &self,
        raw_transcript: &str,
        app_venue_name: Option<&str>,
        groq_client: Option<&GroqClient>,
        gemini_client: Option<&GeminiClient>,
        duration_ms: Option<u32>,
    ) -> String {
        let local_result = self.process_local_with_duration(raw_transcript, app_venue_name, duration_ms);
        if local_result.is_empty() {
            return String::new();
        }

        let opts = self.get_options();
        let use_llm = opts.llm_enabled || opts.mode == PostProcessMode::SmartLLM;
        if !use_llm {
            return local_result;
        }

        // Fast-path bypass for trivial short utterances (e.g. "yes", "okay", "cancel")
        let word_count = local_result.split_whitespace().count();
        if word_count <= 2 {
            return local_result;
        }

        let provider = opts.llm_provider.to_lowercase();
        let model = opts.llm_model.trim();
        let style = opts.llm_style;

        let llm_future = async {
            if provider == "gemini" {
                if let Some(client) = gemini_client {
                    let m = if model.is_empty() { crate::router::gemini::DEFAULT_GEMINI_LLM_MODEL } else { model };
                    return client.rewrite_or_format(&local_result, app_venue_name, Some(m), style).await;
                }
            } else {
                if let Some(client) = groq_client {
                    let m = if model.is_empty() { crate::router::groq::DEFAULT_GROQ_LLM_MODEL } else { model };
                    return client.rewrite_or_format(&local_result, app_venue_name, Some(m), style).await;
                }
            }
            Err("Requested LLM post-processing provider/client not configured or available".to_string())
        };

        // Strict 1.5s timeout for zero blocking / seamless fallback
        match tokio::time::timeout(Duration::from_millis(1500), llm_future).await {
            Ok(Ok(smart_result)) => {
                let smart_trimmed = smart_result.trim();
                if !smart_trimmed.is_empty() {
                    return smart_trimmed.to_string();
                }
            }
            Ok(Err(e)) => {
                eprintln!("[PostProcessor] LLM post-processing failed, falling back to local: {}", e);
            }
            Err(_) => {
                eprintln!("[PostProcessor] LLM post-processing timed out (>1500ms), falling back to local result");
            }
        }

        local_result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pipeline_defaults() {
        let processor = PostProcessor::default();
        let opts = processor.get_options();
        assert_eq!(opts.mode, PostProcessMode::LocalFast);
        assert!(!opts.llm_enabled);
        assert_eq!(opts.llm_provider, "groq");
        assert_eq!(opts.llm_model, "qwen/qwen3.8-27b");
    }
}
