// Copyright (c) 2026 Sotto
// Provider-aware transcription dispatcher. Owns one client per provider plus the
// active selection, so the Studio Providers tab can hot-swap keys / models /
// providers without restarting the engine.

use std::sync::{Arc, Mutex};

use super::gemini::{GeminiClient, DEFAULT_GEMINI_MODEL};
use super::groq::{GroqClient, TranscriptionResult, DEFAULT_GROQ_MODEL};

pub const PROVIDER_GROQ: &str = "groq";
pub const PROVIDER_GEMINI: &str = "gemini";

pub const FALLBACK_GROQ_MODELS: &[&str] = &["whisper-large-v3-turbo", "whisper-large-v3"];
pub const FALLBACK_GEMINI_MODELS: &[&str] = &[DEFAULT_GEMINI_MODEL];

/// Gemini's `generateContent` inlineData request body is limited to ~20 MB and
/// base64 expands audio by 4/3, so a 14 MB WAV stays safely below the cap.
pub const GEMINI_INLINE_SOFT_BYTES: usize = 12_000_000;
pub const GEMINI_INLINE_HARD_BYTES: usize = 14_000_000;

/// Thread-safe shared handle passed through the dictation pipeline.
pub type SharedTranscriber = Arc<Mutex<Transcriber>>;

#[derive(Clone)]
pub struct Transcriber {
    active_provider: String,
    groq: Option<GroqClient>,
    gemini: Option<GeminiClient>,
    language: String,
}

impl Transcriber {
    pub fn new(
        active: &str,
        groq_key: Option<String>,
        groq_model: Option<String>,
        gemini_key: Option<String>,
        gemini_model: Option<String>,
    ) -> Self {
        Self::with_language(active, groq_key, groq_model, gemini_key, gemini_model, "en")
    }

    pub fn with_key_pools(
        active: &str,
        groq_keys: Vec<String>,
        groq_model: Option<String>,
        gemini_keys: Vec<String>,
        gemini_model: Option<String>,
        language: &str,
    ) -> Self {
        let mut groq = GroqClient::with_keys(groq_keys).expect("GroqClient builds with keys");
        if let Some(m) = groq_model.filter(|m| !m.trim().is_empty()) {
            groq.set_model(m);
        }
        groq.set_language(language.to_string());

        let mut gemini = GeminiClient::with_keys(gemini_keys).expect("GeminiClient builds with keys");
        if let Some(m) = gemini_model.filter(|m| !m.trim().is_empty()) {
            gemini.set_model(m);
        }
        gemini.set_language(language.to_string());

        Self {
            active_provider: normalize_provider(active).to_string(),
            groq: Some(groq),
            gemini: Some(gemini),
            language: language.to_string(),
        }
    }

    pub fn add_provider_key(&mut self, provider: &str, key: &str) {
        match normalize_provider(provider) {
            PROVIDER_GEMINI => {
                if let Some(g) = self.gemini.as_mut() {
                    g.add_key(key);
                }
            }
            _ => {
                if let Some(g) = self.groq.as_mut() {
                    g.add_key(key);
                }
            }
        }
    }

    pub fn remove_provider_key(&mut self, provider: &str, key: &str) {
        match normalize_provider(provider) {
            PROVIDER_GEMINI => {
                if let Some(g) = self.gemini.as_mut() {
                    g.remove_key(key);
                }
            }
            _ => {
                if let Some(g) = self.groq.as_mut() {
                    g.remove_key(key);
                }
            }
        }
    }

    pub fn get_key_statuses(&self, provider: &str) -> Vec<super::key_pool::KeyStatus> {
        match normalize_provider(provider) {
            PROVIDER_GEMINI => self.gemini.as_ref().map(|g| g.key_pool().get_statuses()).unwrap_or_default(),
            _ => self.groq.as_ref().map(|g| g.key_pool().get_statuses()).unwrap_or_default(),
        }
    }

    pub fn with_language(
        active: &str,
        groq_key: Option<String>,
        groq_model: Option<String>,
        gemini_key: Option<String>,
        gemini_model: Option<String>,
        language: &str,
    ) -> Self {
        let mut groq = GroqClient::new(None).expect("GroqClient builds without key");
        if let Some(k) = groq_key.filter(|k| !k.trim().is_empty()) {
            groq.set_api_key(k);
        }
        if let Some(m) = groq_model.filter(|m| !m.trim().is_empty()) {
            groq.set_model(m);
        }
        groq.set_language(language.to_string());

        let mut gemini = GeminiClient::new(None).expect("GeminiClient builds without key");
        if let Some(k) = gemini_key.filter(|k| !k.trim().is_empty()) {
            gemini.set_api_key(k);
        }
        if let Some(m) = gemini_model.filter(|m| !m.trim().is_empty()) {
            gemini.set_model(m);
        }
        gemini.set_language(language.to_string());

        Self {
            active_provider: normalize_provider(active).to_string(),
            groq: Some(groq),
            gemini: Some(gemini),
            language: language.to_string(),
        }
    }

    pub fn active_provider(&self) -> &str {
        &self.active_provider
    }

    pub fn set_active_provider(&mut self, provider: &str) {
        self.active_provider = normalize_provider(provider).to_string();
    }

    pub fn language(&self) -> &str {
        &self.language
    }

    pub fn set_language(&mut self, language: &str) {
        let lang = language.trim();
        self.language = lang.to_string();
        if let Some(g) = self.groq.as_mut() {
            g.set_language(lang.to_string());
        }
        if let Some(g) = self.gemini.as_mut() {
            g.set_language(lang.to_string());
        }
    }

    pub fn set_provider_model(&mut self, provider: &str, model: String) {
        match normalize_provider(provider) {
            PROVIDER_GEMINI => {
                if let Some(g) = self.gemini.as_mut() {
                    g.set_model(model);
                }
            }
            _ => {
                if let Some(g) = self.groq.as_mut() {
                    g.set_model(model);
                }
            }
        }
    }

    /// Cloned Groq client for the Smart-LLM post-processing path (None if absent).
    pub fn groq_client(&self) -> Option<GroqClient> {
        self.groq.clone().filter(|c| c.has_api_key())
    }

    /// Cloned Gemini client for the Smart-LLM post-processing path (None if absent).
    pub fn gemini_client(&self) -> Option<GeminiClient> {
        self.gemini.clone().filter(|c| c.has_api_key())
    }

    pub fn active_model(&self) -> &str {
        self.provider_model(&self.active_provider.clone())
    }

    pub fn provider_model(&self, provider: &str) -> &str {
        match normalize_provider(provider) {
            PROVIDER_GEMINI => self.gemini.as_ref().map(|g| g.model()).unwrap_or(DEFAULT_GEMINI_MODEL),
            _ => self.groq.as_ref().map(|g| g.model()).unwrap_or(DEFAULT_GROQ_MODEL),
        }
    }

    pub fn has_usable_key(&self) -> bool {
        match self.active_provider.as_str() {
            PROVIDER_GEMINI => self.gemini.as_ref().map(|g| g.has_api_key()).unwrap_or(false),
            _ => self.groq.as_ref().map(|g| g.has_api_key()).unwrap_or(false),
        }
    }

    /// Safe per-chunk WAV budgets for the active provider's request encoding.
    /// Groq uses multipart (raw WAV), while Gemini inlines base64 JSON.
    ///
    /// When Gemini has a key it can receive a failed Groq chunk, so the smaller
    /// Gemini budget applies even if Groq is active: a Groq-sized 24 MB chunk
    /// would expand past Gemini's inline request limit and lose the failover.
    pub fn chunk_limits(&self) -> (usize, usize) {
        let gemini_reachable = self
            .gemini
            .as_ref()
            .map(|g| g.has_api_key())
            .unwrap_or(false);
        if self.active_provider == PROVIDER_GEMINI || gemini_reachable {
            (GEMINI_INLINE_SOFT_BYTES, GEMINI_INLINE_HARD_BYTES)
        } else {
            (
                crate::audio::chunker::SOFT_REQUEST_BYTES,
                crate::audio::chunker::HARD_REQUEST_BYTES,
            )
        }
    }

    /// Short label for the Studio stats chip, e.g. "Groq whisper-large-v3-turbo".
    /// (The ⚡/✦ icon is rendered from provider-id on the UI side so the label
    /// text shares identical font metrics with the neighbouring chips.)
    pub fn chip_text(&self) -> String {
        match self.active_provider.as_str() {
            PROVIDER_GEMINI => format!("Gemini {}", self.active_model()),
            _ => format!("Groq {}", self.active_model()),
        }
    }

    /// Runs transcription through the active provider's client.
    /// If all keys of the active provider fail or exhaust, seamlessly falls back to the other provider.
    pub async fn transcribe(&self, wav_bytes: Vec<u8>) -> Result<TranscriptionResult, String> {
        self.transcribe_with_prompt(wav_bytes, None).await
    }

    /// Prompt-aware variant used by long-recording chunk stitching. The prompt
    /// is normally the tail of the previous chunk's transcript, so the provider
    /// keeps spelling, casing, and punctuation continuous across the split.
    pub async fn transcribe_with_prompt(
        &self,
        wav_bytes: Vec<u8>,
        prompt: Option<&str>,
    ) -> Result<TranscriptionResult, String> {
        if self.active_provider == PROVIDER_GEMINI {
            let primary_err = match self.gemini.as_ref().filter(|g| g.has_api_key()) {
                Some(gemini) => match gemini.transcribe_with_prompt(wav_bytes.clone(), prompt).await {
                    Ok(res) => return Ok(res),
                    Err(e) => e,
                },
                None => "No Gemini API keys configured.".to_string(),
            };

            // Cross-provider automated failover to Groq
            if let Some(groq) = self.groq.as_ref().filter(|g| g.has_api_key()) {
                eprintln!(
                    "[Router Failover] Gemini failed ({}); auto-switching to Groq...",
                    primary_err
                );
                return groq.transcribe_with_prompt(wav_bytes, prompt).await;
            }

            Err(primary_err)
        } else {
            let primary_err = match self.groq.as_ref().filter(|g| g.has_api_key()) {
                Some(groq) => match groq.transcribe_with_prompt(wav_bytes.clone(), prompt).await {
                    Ok(res) => return Ok(res),
                    Err(e) => e,
                },
                None => "No Groq API keys configured.".to_string(),
            };

            // Cross-provider automated failover to Gemini
            if let Some(gemini) = self.gemini.as_ref().filter(|g| g.has_api_key()) {
                eprintln!(
                    "[Router Failover] Groq failed ({}); auto-switching to Gemini...",
                    primary_err
                );
                return gemini.transcribe_with_prompt(wav_bytes, prompt).await;
            }

            Err(primary_err)
        }
    }

    /// Synchronous convenience wrapper for non-async calling threads.
    pub fn transcribe_blocking(&self, wav_bytes: Vec<u8>) -> Result<TranscriptionResult, String> {
        self.transcribe_blocking_with_prompt(wav_bytes, None)
    }

    /// Blocking prompt-aware variant. Uses the same cross-provider failover as
    /// the async path, which matters when a chunk is sealed mid-recording.
    pub fn transcribe_blocking_with_prompt(
        &self,
        wav_bytes: Vec<u8>,
        prompt: Option<&str>,
    ) -> Result<TranscriptionResult, String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("Failed to create tokio runtime: {}", e))?;
        rt.block_on(self.transcribe_with_prompt(wav_bytes, prompt))
    }
}

pub fn normalize_provider(p: &str) -> &str {
    let lower = p.trim().to_lowercase();
    if lower == PROVIDER_GEMINI {
        PROVIDER_GEMINI
    } else {
        PROVIDER_GROQ
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(value: &str) -> Option<String> {
        Some(value.to_string())
    }

    #[test]
    fn chunk_limits_shrink_when_gemini_can_take_groq_failover() {
        let groq_only = Transcriber::new(
            PROVIDER_GROQ,
            key("groq-key"),
            None,
            None,
            None,
        );
        assert_eq!(
            groq_only.chunk_limits(),
            (
                crate::audio::chunker::SOFT_REQUEST_BYTES,
                crate::audio::chunker::HARD_REQUEST_BYTES,
            )
        );

        let groq_with_gemini_fallback = Transcriber::new(
            PROVIDER_GROQ,
            key("groq-key"),
            None,
            key("gemini-key"),
            None,
        );
        assert_eq!(
            groq_with_gemini_fallback.chunk_limits(),
            (GEMINI_INLINE_SOFT_BYTES, GEMINI_INLINE_HARD_BYTES)
        );

        let gemini_only = Transcriber::new(
            PROVIDER_GEMINI,
            None,
            None,
            key("gemini-key"),
            None,
        );
        assert_eq!(
            gemini_only.chunk_limits(),
            (GEMINI_INLINE_SOFT_BYTES, GEMINI_INLINE_HARD_BYTES)
        );
    }
}
