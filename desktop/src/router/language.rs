// Copyright (c) 2026 Sotto
// Provider and model adaptive language catalog for voice transcription.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageDef {
    pub code: &'static str,
    pub name: &'static str,
}

pub const OTHER_LANGUAGES: &[(&str, &str)] = &[
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("ja", "Japanese"),
    ("zh", "Chinese"),
    ("hi", "Hindi"),
    ("nl", "Dutch"),
    ("ru", "Russian"),
    ("ko", "Korean"),
    ("ar", "Arabic"),
    ("pl", "Polish"),
    ("tr", "Turkish"),
    ("uk", "Ukrainian"),
    ("sv", "Swedish"),
];

/// Determines if a specific model is strictly English-only.
/// English-only models (like whisper-base.en or distil-whisper-large-v3-en)
/// cannot transcribe other languages and will error or hallucinate if sent foreign language codes.
pub fn is_english_only_model(model: &str) -> bool {
    let m = model.trim().to_lowercase();
    m.ends_with(".en") || m.contains("-en") || m.contains(".en-") || m.contains("english")
}

/// Returns a human-friendly display name for the language code.
pub fn language_display_name(code: &str) -> String {
    let c = code.trim().to_lowercase();
    match c.as_str() {
        "en" => "English".to_string(),
        "auto" => "Auto-Detect".to_string(),
        _ => {
            for &(c_code, name) in OTHER_LANGUAGES {
                if c_code.eq_ignore_ascii_case(&c) {
                    return format!("{} ({})", name, c_code);
                }
            }
            code.to_string()
        }
    }
}

/// Generates a model prompt customized for the target transcription language in Gemini.
pub fn prompt_for_gemini(language_code: &str) -> String {
    match language_code.trim().to_lowercase().as_str() {
        "auto" => "Transcribe this audio to text in its original spoken language exactly as spoken. Apply clean punctuation and capitalization. Remove filler words (um, uh, ah). Return ONLY the transcript, no quotes, no commentary.".to_string(),
        "en" | "" => "Transcribe this audio to English text exactly as spoken. Apply clean punctuation and capitalization. Remove filler words (um, uh, ah). Return ONLY the transcript, no quotes, no commentary.".to_string(),
        code => {
            let name = OTHER_LANGUAGES.iter()
                .find(|(c, _)| c.eq_ignore_ascii_case(code))
                .map(|(_, n)| *n)
                .unwrap_or(code);
            format!("Transcribe this audio to {} text exactly as spoken. Apply clean punctuation and capitalization. Remove filler words (um, uh, ah). Return ONLY the transcript, no quotes, no commentary.", name)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_english_only_model() {
        assert!(is_english_only_model("distil-whisper-large-v3-en"));
        assert!(is_english_only_model("whisper-tiny.en"));
        assert!(is_english_only_model("custom-english-model"));
        assert!(!is_english_only_model("whisper-large-v3"));
        assert!(!is_english_only_model("whisper-large-v3-turbo"));
        assert!(!is_english_only_model("gemini-3.5-transcribe"));
        assert!(!is_english_only_model("gemini-2.0-flash"));
    }

    #[test]
    fn test_language_display_name() {
        assert_eq!(language_display_name("en"), "English");
        assert_eq!(language_display_name("auto"), "Auto-Detect");
        assert_eq!(language_display_name("es"), "Spanish (es)");
        assert_eq!(language_display_name("ja"), "Japanese (ja)");
    }

    #[test]
    fn test_prompt_for_gemini() {
        assert!(prompt_for_gemini("en").contains("English text"));
        assert!(prompt_for_gemini("auto").contains("original spoken language"));
        assert!(prompt_for_gemini("es").contains("Spanish text"));
    }
}
