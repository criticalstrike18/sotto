// Copyright (c) 2026 Sotto
// Post-Processing Layer: Number, Symbol, Punctuation & App-Venue Formatter

use regex::{Regex, RegexBuilder};
use std::sync::LazyLock;

/// Application category determined from foreground window or process name
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueCategory {
    /// Code editors, IDEs, and developer terminals (VS Code, Windows Terminal, PowerShell, CMD)
    DeveloperVenue,
    /// Chat and messaging platforms (Slack, Discord, WhatsApp, Telegram, Teams)
    ChatVenue,
    /// Documents, notes, browsers, and standard text fields
    GeneralVenue,
}

impl VenueCategory {
    /// Detects category from executable or friendly application name
    pub fn from_app_name(name: &str) -> Self {
        let lower = name.to_lowercase();

        if lower.contains("code")
            || lower.contains("terminal")
            || lower.contains("powershell")
            || lower.contains("pwsh")
            || lower.contains("cmd")
            || lower.contains("cursor")
            || lower.contains("windsurf")
            || lower.contains("zed")
            || lower.contains("studio")
            || lower.contains("git")
        {
            VenueCategory::DeveloperVenue
        } else if lower.contains("slack")
            || lower.contains("discord")
            || lower.contains("whatsapp")
            || lower.contains("telegram")
            || lower.contains("teams")
        {
            VenueCategory::ChatVenue
        } else {
            VenueCategory::GeneralVenue
        }
    }
}

/// Spoken word to digit mapping (zero through twenty, plus tens up to hundred)
static NUMBER_WORDS: &[(&str, i32)] = &[
    ("zero", 0),
    ("one", 1),
    ("two", 2),
    ("three", 3),
    ("four", 4),
    ("five", 5),
    ("six", 6),
    ("seven", 7),
    ("eight", 8),
    ("nine", 9),
    ("ten", 10),
    ("eleven", 11),
    ("twelve", 12),
    ("thirteen", 13),
    ("fourteen", 14),
    ("fifteen", 15),
    ("sixteen", 16),
    ("seventeen", 17),
    ("eighteen", 18),
    ("nineteen", 19),
    ("twenty", 20),
    ("thirty", 30),
    ("forty", 40),
    ("fifty", 50),
    ("sixty", 60),
    ("seventy", 70),
    ("eighty", 80),
    ("ninety", 90),
    ("hundred", 100),
];

// Precompiled regexes for spoken symbols & commands
static RE_PERCENT: LazyLock<Regex> = LazyLock::new(|| {
    RegexBuilder::new(r"(?i)\b(\d+|zero|one|two|three|four|five|six|seven|eight|nine|ten|twenty|thirty|forty|fifty|sixty|seventy|eighty|ninety|hundred)\s+percent\b")
        .build()
        .unwrap()
});

static RE_DOLLARS: LazyLock<Regex> = LazyLock::new(|| {
    RegexBuilder::new(r"(?i)\b(\d+|zero|one|two|three|four|five|six|seven|eight|nine|ten|twenty|thirty|forty|fifty|sixty|seventy|eighty|ninety|hundred)\s+dollars?\b")
        .build()
        .unwrap()
});

static RE_AT_SIGN: LazyLock<Regex> = LazyLock::new(|| {
    RegexBuilder::new(r"(?i)\s+at\s+sign\s+").build().unwrap()
});

static RE_HASHTAG: LazyLock<Regex> = LazyLock::new(|| {
    RegexBuilder::new(r"(?i)\b(hashtag|hash\s+tag|pound\s+sign)\s*(\w+)?").build().unwrap()
});

static RE_PLUS_SIGN: LazyLock<Regex> = LazyLock::new(|| {
    RegexBuilder::new(r"(?i)\s+plus\s+sign\s+").build().unwrap()
});

static RE_EQUALS_SIGN: LazyLock<Regex> = LazyLock::new(|| {
    RegexBuilder::new(r"(?i)\s+equals\s+sign\s+").build().unwrap()
});

static RE_NEWLINE: LazyLock<Regex> = LazyLock::new(|| {
    RegexBuilder::new(r"(?i)\b(new\s+line|newline)\b").build().unwrap()
});

static RE_NEW_PARAGRAPH: LazyLock<Regex> = LazyLock::new(|| {
    RegexBuilder::new(r"(?i)\b(new\s+paragraph)\b").build().unwrap()
});

fn word_to_num(w: &str) -> Option<i32> {
    if let Ok(n) = w.parse::<i32>() {
        return Some(n);
    }
    let lower = w.to_lowercase();
    NUMBER_WORDS.iter().find(|(name, _)| *name == lower).map(|(_, val)| *val)
}

/// Normalizes spoken numbers, currency, and symbols.
pub struct NumberAndSymbolFormatter;

impl NumberAndSymbolFormatter {
    pub fn format(text: &str) -> String {
        if text.trim().is_empty() {
            return text.to_string();
        }

        let mut res = text.to_string();

        // 1. Spoken currency: e.g. "five dollars" -> "$5", "50 dollars" -> "$50"
        res = RE_DOLLARS.replace_all(&res, |caps: &regex::Captures| {
            let num_str = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            if let Some(val) = word_to_num(num_str) {
                format!("${}", val)
            } else {
                caps.get(0).map(|m| m.as_str()).unwrap_or("").to_string()
            }
        }).into_owned();

        // 2. Spoken percent: e.g. "ten percent" -> "10%", "50 percent" -> "50%"
        res = RE_PERCENT.replace_all(&res, |caps: &regex::Captures| {
            let num_str = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            if let Some(val) = word_to_num(num_str) {
                format!("{}%", val)
            } else {
                caps.get(0).map(|m| m.as_str()).unwrap_or("").to_string()
            }
        }).into_owned();

        // 3. Spoken symbols
        res = RE_AT_SIGN.replace_all(&res, " @ ").into_owned();
        res = RE_PLUS_SIGN.replace_all(&res, " + ").into_owned();
        res = RE_EQUALS_SIGN.replace_all(&res, " = ").into_owned();

        // 4. Hashtag: "hashtag rust" -> "#rust", "hashtag sotto" -> "#sotto"
        res = RE_HASHTAG.replace_all(&res, |caps: &regex::Captures| {
            if let Some(tag) = caps.get(2) {
                format!("#{}", tag.as_str())
            } else {
                "#".to_string()
            }
        }).into_owned();

        // 5. Spoken punctuation commands
        res = RE_NEW_PARAGRAPH.replace_all(&res, "\n\n").into_owned();
        res = RE_NEWLINE.replace_all(&res, "\n").into_owned();

        res
    }
}

/// Applies app-venue aware formatting conventions.
pub struct VenueFormatter;

impl VenueFormatter {
    /// Formats text based on active venue conventions.
    pub fn format(text: &str, venue: VenueCategory) -> String {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return String::new();
        }

        match venue {
            VenueCategory::DeveloperVenue => {
                // In developer/terminal venues:
                // If text begins with common CLI commands or looks like code/flags,
                // strip trailing period that would break the command execution.
                let first_word = trimmed
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_lowercase();

                let is_command = matches!(
                    first_word.as_str(),
                    "git"
                        | "cargo"
                        | "npm"
                        | "pnpm"
                        | "yarn"
                        | "bun"
                        | "docker"
                        | "kubectl"
                        | "cd"
                        | "ls"
                        | "dir"
                        | "grep"
                        | "rg"
                        | "python"
                        | "pip"
                        | "node"
                        | "code"
                        | "ssh"
                        | "curl"
                        | "wget"
                ) || trimmed.contains("--") || trimmed.contains("-v");

                if is_command && trimmed.ends_with('.') {
                    return trimmed.trim_end_matches('.').trim().to_string();
                }
                trimmed.to_string()
            }
            VenueCategory::ChatVenue => {
                // In chat venues (Slack, Discord, WhatsApp):
                // Modern informal chat convention: short 1-4 word confirmations
                // ("sure", "yes", "sounds good", "okay thank you") feel passive-aggressive
                // or overly rigid when terminated with a period. Strip trailing period for short bursts.
                let words: Vec<&str> = trimmed.split_whitespace().collect();
                if words.len() <= 4 && trimmed.ends_with('.') && !trimmed.contains('\n') {
                    return trimmed.trim_end_matches('.').trim().to_string();
                }
                trimmed.to_string()
            }
            VenueCategory::GeneralVenue => {
                // General text: maintain standard sentence formatting
                trimmed.to_string()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spoken_numbers_and_currency() {
        let input = "The price was five dollars and discount was ten percent.";
        let formatted = NumberAndSymbolFormatter::format(input);
        assert_eq!(formatted, "The price was $5 and discount was 10%.");
    }

    #[test]
    fn test_spoken_symbols_and_hashtags() {
        let input = "Check out hashtag sotto or contact support at sign example dot com";
        let formatted = NumberAndSymbolFormatter::format(input);
        assert_eq!(formatted, "Check out #sotto or contact support @ example dot com");
    }

    #[test]
    fn test_spoken_newlines() {
        let input = "First line newline second line new paragraph third line";
        let formatted = NumberAndSymbolFormatter::format(input);
        assert_eq!(formatted, "First line \n second line \n\n third line");
    }

    #[test]
    fn test_developer_venue_command_period_strip() {
        let input = "git status.";
        let res = VenueFormatter::format(input, VenueCategory::DeveloperVenue);
        assert_eq!(res, "git status");

        let input_cargo = "cargo build --release.";
        let res_cargo = VenueFormatter::format(input_cargo, VenueCategory::DeveloperVenue);
        assert_eq!(res_cargo, "cargo build --release");

        // Non-command in terminal still retains punctuation
        let input_comment = "This is a descriptive comment.";
        let res_comment = VenueFormatter::format(input_comment, VenueCategory::DeveloperVenue);
        assert_eq!(res_comment, "This is a descriptive comment.");
    }

    #[test]
    fn test_chat_venue_short_reply_formatting() {
        let input = "Sounds good.";
        let res = VenueFormatter::format(input, VenueCategory::ChatVenue);
        assert_eq!(res, "Sounds good");

        // Multi-sentence or long message keeps punctuation
        let long_input = "Sounds good. Let us review the pull request together tomorrow morning.";
        let long_res = VenueFormatter::format(long_input, VenueCategory::ChatVenue);
        assert_eq!(long_res, long_input);
    }
}
