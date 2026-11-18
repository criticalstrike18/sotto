// Copyright (c) 2026 Sotto
// Post-Processing Layer: Dictionary & Casing Normalization Engine

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::sync::RwLock;

/// User-customizable dictionary replacement pair.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DictionaryEntry {
    pub from: String,
    pub to: String,
}

impl DictionaryEntry {
    pub fn new(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
        }
    }
}

/// Built-in technical and developer terms mapping spoken phonetic forms to exact branding.
pub const BUILTIN_TECH_DICTIONARY: &[(&str, &str)] = &[
    ("github", "GitHub"),
    ("git hub", "GitHub"),
    ("youtube", "YouTube"),
    ("you tube", "YouTube"),
    ("vs code", "VS Code"),
    ("vscode", "VS Code"),
    ("visual studio code", "VS Code"),
    ("javascript", "JavaScript"),
    ("typescript", "TypeScript"),
    ("next js", "Next.js"),
    ("nextjs", "Next.js"),
    ("node js", "Node.js"),
    ("nodejs", "Node.js"),
    ("react js", "React"),
    ("vue js", "Vue.js"),
    ("slint", "Slint"),
    ("rust", "Rust"),
    ("sqlite", "SQLite"),
    ("sql lite", "SQLite"),
    ("postgresql", "PostgreSQL"),
    ("postgres", "PostgreSQL"),
    ("mongodb", "MongoDB"),
    ("mongo db", "MongoDB"),
    ("redis", "Redis"),
    ("docker", "Docker"),
    ("kubernetes", "Kubernetes"),
    ("k8s", "K8s"),
    ("graphql", "GraphQL"),
    ("graph ql", "GraphQL"),
    ("rest api", "REST API"),
    ("rest apis", "REST APIs"),
    ("api", "API"),
    ("apis", "APIs"),
    ("ui", "UI"),
    ("ux", "UX"),
    ("ui ux", "UI/UX"),
    ("html", "HTML"),
    ("css", "CSS"),
    ("json", "JSON"),
    ("yaml", "YAML"),
    ("toml", "TOML"),
    ("wasm", "WebAssembly"),
    ("webassembly", "WebAssembly"),
    ("ai", "AI"),
    ("llm", "LLM"),
    ("llms", "LLMs"),
    ("groq", "Groq"),
    ("openai", "OpenAI"),
    ("whisper", "Whisper"),
];

struct CompiledRule {
    pattern: Regex,
    replacement: String,
}

/// High-performance Dictionary Engine with compiled word-boundary matching.
pub struct DictionaryEngine {
    compiled_builtin: Vec<CompiledRule>,
    custom_rules: RwLock<Vec<CompiledRule>>,
}

impl Default for DictionaryEngine {
    fn default() -> Self {
        Self::new(&[])
    }
}

impl DictionaryEngine {
    /// Creates a new DictionaryEngine with built-in technical rules and optional custom entries.
    pub fn new(custom_entries: &[DictionaryEntry]) -> Self {
        let compiled_builtin = BUILTIN_TECH_DICTIONARY
            .iter()
            .filter_map(|(from, to)| {
                let pat = format!(r"(?i)\b{}\b", regex::escape(from));
                RegexBuilder::new(&pat).build().ok().map(|pattern| CompiledRule {
                    pattern,
                    replacement: to.to_string(),
                })
            })
            .collect();

        let engine = Self {
            compiled_builtin,
            custom_rules: RwLock::new(Vec::new()),
        };

        engine.set_custom_entries(custom_entries);
        engine
    }

    /// Replaces the active custom user entries.
    pub fn set_custom_entries(&self, entries: &[DictionaryEntry]) {
        let compiled: Vec<CompiledRule> = entries
            .iter()
            .filter_map(|entry| {
                let from_trimmed = entry.from.trim();
                if from_trimmed.is_empty() {
                    return None;
                }
                let pat = format!(r"(?i)\b{}\b", regex::escape(from_trimmed));
                RegexBuilder::new(&pat).build().ok().map(|pattern| CompiledRule {
                    pattern,
                    replacement: entry.to.clone(),
                })
            })
            .collect();

        if let Ok(mut lock) = self.custom_rules.write() {
            *lock = compiled;
        }
    }

    /// Applies technical and custom vocabulary substitutions to the text.
    pub fn apply(&self, text: &str) -> String {
        if text.trim().is_empty() {
            return text.to_string();
        }

        let mut working = text.to_string();

        // 1. Apply user custom entries first (higher priority)
        if let Ok(custom) = self.custom_rules.read() {
            for rule in custom.iter() {
                if rule.pattern.is_match(&working) {
                    working = rule.pattern.replace_all(&working, rule.replacement.as_str()).into_owned();
                }
            }
        }

        // 2. Apply built-in technical vocabulary
        for rule in &self.compiled_builtin {
            if rule.pattern.is_match(&working) {
                working = rule.pattern.replace_all(&working, rule.replacement.as_str()).into_owned();
            }
        }

        working
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_tech_replacements() {
        let engine = DictionaryEngine::default();
        let input = "I am writing a rest api in rust using slint, sqlite, and groq for ai.";
        let output = engine.apply(input);
        assert_eq!(
            output,
            "I am writing a REST API in Rust using Slint, SQLite, and Groq for AI."
        );
    }

    #[test]
    fn test_custom_user_dictionary_overrides() {
        let custom = vec![
            DictionaryEntry::new("my email", "saksham@example.com"),
            DictionaryEntry::new("sotto", "Sotto"),
        ];
        let engine = DictionaryEngine::new(&custom);

        let input = "Send an email to my email for sotto updates on github.";
        let output = engine.apply(input);
        assert_eq!(
            output,
            "Send an email to saksham@example.com for Sotto updates on GitHub."
        );
    }

    #[test]
    fn test_case_insensitivity_and_word_boundaries() {
        let engine = DictionaryEngine::default();
        let input = "GITHUB and github and GITHUBLIKE should not replace githublike";
        let output = engine.apply(input);
        assert_eq!(
            output,
            "GitHub and GitHub and GITHUBLIKE should not replace githublike"
        );
    }
}
