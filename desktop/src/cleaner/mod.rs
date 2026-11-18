// Copyright (c) 2026 Sotto
// Post-Processing Layer: Modular Execution Pipeline & Zero-Latency Local Sanitizer

pub mod dictionary;
pub mod formatter;
pub mod pipeline;
pub mod sanitizer;

pub use dictionary::{DictionaryEngine, DictionaryEntry, BUILTIN_TECH_DICTIONARY};
pub use formatter::{NumberAndSymbolFormatter, VenueCategory, VenueFormatter};
pub use pipeline::{PostProcessMode, PostProcessOptions, PostProcessor};
pub use sanitizer::{
    is_hallucination, is_rate_hallucination, sanitize_transcript, SottoSanitizer,
    HALLUCINATION_DENYLIST, MAX_HUMAN_WORDS_PER_SEC, MIN_WORDS_FOR_RATE_CHECK,
};
