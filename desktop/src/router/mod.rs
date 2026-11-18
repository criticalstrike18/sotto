// Copyright (c) 2026 Sotto
// Router Module (Groq + Gemini HTTP/2 clients & provider-aware dispatch)

pub mod groq;
pub mod gemini;
pub mod transcriber;
pub mod language;
pub mod key_pool;

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
