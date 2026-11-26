// Copyright (c) 2026 Sotto
// Router Module (Groq + Gemini HTTP/2 clients & provider-aware dispatch)

pub mod groq;
pub mod gemini;
pub mod transcriber;
pub mod language;
pub mod key_pool;

use std::sync::OnceLock;
use std::time::Duration;

static SHARED_HTTP_CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
static SHARED_TOKIO_RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();

/// Process-wide HTTP/2 + rustls client shared across Groq/Gemini transcription,
/// LLM polish, model refreshes, and key checks to avoid duplicate TLS stores
/// and idle socket pools in RAM.
pub fn shared_http_client() -> Result<&'static reqwest::Client, String> {
    let res = SHARED_HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .tcp_nodelay(true)
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Some(Duration::from_secs(45)))
            .pool_max_idle_per_host(2)
            .build()
            .map_err(|e| format!("Failed to build reqwest HTTP/2 client: {}", e))
    });
    res.as_ref().map_err(Clone::clone)
}

/// Process-wide lightweight Tokio runtime (1 worker thread) shared across all
/// async network/post-processing calls.
pub fn shared_runtime() -> Result<&'static tokio::runtime::Runtime, String> {
    let res = SHARED_TOKIO_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(2)
            .thread_name("sotto-net")
            .enable_all()
            .build()
            .map_err(|e| format!("Failed to create tokio runtime: {}", e))
    });
    res.as_ref().map_err(Clone::clone)
}

pub use key_pool::{mask_key, KeyPool, KeyStatus};
pub use groq::{GroqClient, TranscriptionResult, DEFAULT_GROQ_MODEL, DEFAULT_PROMPT};
pub use gemini::{GeminiClient, DEFAULT_GEMINI_MODEL};
pub use transcriber::{
    SharedTranscriber, Transcriber, FALLBACK_GEMINI_MODELS, FALLBACK_GROQ_MODELS,
    PROVIDER_GEMINI, PROVIDER_GROQ,
};
pub use language::{
    is_english_only_model, language_display_name, prompt_for_gemini, LanguageDef, OTHER_LANGUAGES,
};
