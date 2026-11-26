// Copyright (c) 2026 Sotto
// Post-Processing Layer: Dictionary & Casing Normalization Engine

use regex::{Captures, Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::sync::{LazyLock, RwLock};

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

static BUILTIN_DICTIONARY: LazyLock<Regex> = LazyLock::new(|| {
    // Sort longest phrase first so multi-word entries ("visual studio code",
    // "rest apis", "ui ux") win over embedded single-word rules in one scan.
    let mut keys: Vec<&'static str> = BUILTIN_TECH_DICTIONARY.iter().map(|(k, _)| *k).collect();
    keys.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    let escaped: Vec<String> = keys.into_iter().map(regex::escape).collect();
    let pat = format!(r"(?i)\b(?:{})\b", escaped.join("|"));
    RegexBuilder::new(&pat)
        .build()
        .expect("builtin dictionary regex must compile")
});

#[inline]
fn lookup_builtin_replacement(matched: &str) -> &'static str {
    BUILTIN_TECH_DICTIONARY
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(matched))
        .map(|(_, v)| *v)
        .unwrap_or("")
}

/// High-performance Dictionary Engine with compiled word-boundary matching.
pub struct DictionaryEngine {
    custom_rules: RwLock<Vec<CompiledRule>>,
}

impl Default for DictionaryEngine {
    fn default() -> Self {
        Self::new(&[])
    }
}

impl DictionaryEngine {
    /// Creates a new DictionaryEngine sharing the process-wide built-in rules
    /// and optional user custom entries.
    pub fn new(custom_entries: &[DictionaryEntry]) -> Self {
        let engine = Self {
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

        let mut working: Cow<'_, str> = Cow::Borrowed(text);

        // 1. Apply user custom entries first (higher priority)
        if let Ok(custom) = self.custom_rules.read() {
            for rule in custom.iter() {
                if let Cow::Owned(next) = rule.pattern.replace_all(&working, rule.replacement.as_str()) {
                    working = Cow::Owned(next);
                }
            }
        }

        // 2. Apply built-in technical vocabulary in a single O(n) pass
        match BUILTIN_DICTIONARY.replace_all(&working, |caps: &Captures<'_>| {
            let m = caps.get(0).map(|m| m.as_str()).unwrap_or("");
            lookup_builtin_replacement(m)
        }) {
            Cow::Borrowed(_) => working.into_owned(),
            Cow::Owned(updated) => updated,
        }
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

    #[test]
    fn test_multi_word_priority_all_builtin_rules_and_dynamic_reload() {
        let engine = DictionaryEngine::default();
        // Verify multi-word rules take precedence over embedded shorter words and empty fast-path works.
        assert_eq!(engine.apply("   "), "   ");
        assert_eq!(
            engine.apply("using visual studio code with ui ux for rest apis and rest api on git hub via next js and node js"),
            "using VS Code with UI/UX for REST APIs and REST API on GitHub via Next.js and Node.js"
        );

        // Every built-in dictionary entry resolves to its canonical spelling.
        for &(raw, expected) in BUILTIN_TECH_DICTIONARY {
            assert_eq!(engine.apply(raw), expected, "failed for builtin key {raw:?}");
        }

        // Dynamic custom dictionary reload replaces old custom rules cleanly.
        engine.set_custom_entries(&[DictionaryEntry::new("acme", "ACME Corp")]);
        assert_eq!(engine.apply("deploy acme to docker"), "deploy ACME Corp to Docker");
        engine.set_custom_entries(&[]);
        assert_eq!(engine.apply("deploy acme to docker"), "deploy acme to Docker");
    }
}
