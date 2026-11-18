// Benchmark and Comparison of Text Sanitization Approaches
// Compares:
// 1. Legacy sequential iterative replacements
// 2. Multi-Pass Regex with separate passes
// 3. Sotto Zero-Latency Hybrid Engine (Zero-Alloc Fast Path + O(N) Stutter Deduplicator)

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::Instant;
use regex::{Regex, RegexBuilder};

// =========================================================================
// APPROACH 1: Legacy Sequential Style (Sequential Regex Iteration + String Re-allocation)
// =========================================================================
pub struct LegacySequentialSanitizer {
    denylist: HashSet<&'static str>,
    replacements: Vec<Regex>,
}

impl LegacySequentialSanitizer {
    pub fn new() -> Self {
        let denylist = [
            "", "you", "thanks", "thank you", "thank you so much",
            "thanks for watching", "thank you for watching", "please subscribe",
            "subscribe to my channel", "like and subscribe", "see you next time",
            "music", "applause", "laughter", "silence", "the end", "uh", "um",
        ].into_iter().collect();

        let patterns = [
            r"\b(?i:um)\b,?",
            r"\b(?i:uh)\b,?",
            r"\b(?i:ah)\b,?",
            r"\b(?i:erm)\b,?",
            r"\b(?i:hmm)\b,?",
        ];

        let replacements = patterns
            .iter()
            .map(|p| RegexBuilder::new(p).build().unwrap())
            .collect();

        Self { denylist, replacements }
    }

    pub fn sanitize(&self, text: &str) -> String {
        // Sequential pass is_likely_hallucination
        let lower = text.trim().to_lowercase();
        let stripped: String = lower
            .chars()
            .filter(|c| !".,!?\"'".contains(*c))
            .collect();
        if self.denylist.contains(stripped.trim()) {
            return String::new();
        }

        // Sequential replace_all (allocating new string on each pass)
        let mut working = text.to_string();
        for re in &self.replacements {
            let replaced = re.replace_all(&working, "");
            working = replaced.into_owned();
        }

        // Space collapse
        let re_space = Regex::new(r"\s{2,}").unwrap();
        working = re_space.replace_all(&working, " ").into_owned();
        working.trim().to_string()
    }
}

// =========================================================================
// APPROACH 2: Multi-Pass Regex Pipeline
// =========================================================================
static RE_MULTI_FILLERS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(um+|uh+|uhm+|ah+|er+m*|hmm+)\b[,.]?\s*").unwrap()
});

static RE_MULTI_SPACES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[ \t]{2,}").unwrap()
});

static RE_MULTI_CLEAN_EDGES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[\s,;.-]+|[\s,;]+$").unwrap()
});

pub fn sanitize_multipass_regex(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    // Pass 1: Fillers
    let p1 = RE_MULTI_FILLERS.replace_all(trimmed, "");
    
    // Pass 2: Word-based stutter scan
    let words: Vec<&str> = p1.split_whitespace().collect();
    let mut deduped = Vec::with_capacity(words.len());
    let mut prev_clean = "";
    for w in words {
        let clean = w.trim_matches(|c: char| !c.is_alphanumeric());
        if !clean.is_empty() && clean.eq_ignore_ascii_case(prev_clean) {
            continue;
        }
        deduped.push(w);
        prev_clean = clean;
    }
    let p2 = deduped.join(" ");

    // Pass 3: Whitespace
    let p3 = RE_MULTI_SPACES.replace_all(&p2, " ");
    // Pass 4: Clean Edges
    let p4 = RE_MULTI_CLEAN_EDGES.replace_all(&p3, "");

    // Capitalize first character if needed
    let mut chars = p4.chars();
    match chars.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

// =========================================================================
// APPROACH 3: Sotto Zero-Latency Hybrid Engine (Zero-Alloc Fast Path + Smart Normalization)
// =========================================================================

static SOTTO_FILLERS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(um+|uh+|uhm+|ah+|er+m*|hmm+)\b[,;.]?\s*").unwrap()
});

static SOTTO_SPACES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[ \t]{2,}").unwrap()
});

pub struct SottoSanitizer;

impl SottoSanitizer {
    #[inline]
    pub fn is_hallucination(text: &str) -> bool {
        let t = text.trim();
        if t.is_empty() {
            return true;
        }

        // Fast ascii lower check without allocating a new String on heap
        let mut buf = [0u8; 64];
        let bytes = t.as_bytes();
        let len = bytes.len().min(64);
        let mut stripped_len = 0;
        for i in 0..len {
            let b = bytes[i];
            if b != b'.' && b != b',' && b != b'!' && b != b'?' && b != b'"' && b != b'\'' {
                buf[stripped_len] = b.to_ascii_lowercase();
                stripped_len += 1;
            }
        }
        let stripped = match std::str::from_utf8(&buf[..stripped_len]) {
            Ok(s) => s.trim(),
            Err(_) => return false,
        };

        const DENYLIST: &[&str] = &[
            "", "you", "thanks", "thank you", "thank you so much",
            "thanks for watching", "thank you for watching", "thanks for watching the video",
            "please subscribe", "subscribe to my channel", "subscribe to the channel",
            "like and subscribe", "see you in the next video", "see you next time",
            "see you next video", "bye", "bye bye", "goodbye", "music",
            "music playing", "soft music", "applause", "laughter", "silence",
            "the end", "uh", "um", "hmm", "mhm", "the", "a", "i", "and", "so",
        ];

        DENYLIST.iter().any(|&d| d == stripped)
    }

    pub fn sanitize<'a>(text: &'a str) -> Cow<'a, str> {
        let trimmed = text.trim();
        if Self::is_hallucination(trimmed) {
            return Cow::Borrowed("");
        }

        // Fast-path check: Is there any trigger character or substring?
        let bytes = trimmed.as_bytes();
        let mut has_trigger = false;
        for i in 0..bytes.len().saturating_sub(1) {
            let b0 = bytes[i].to_ascii_lowercase();
            let b1 = bytes[i + 1].to_ascii_lowercase();
            if (b0 == b'u' && (b1 == b'm' || b1 == b'h'))
                || (b0 == b'a' && b1 == b'h')
                || (b0 == b' ' && b1 == b' ')
            {
                has_trigger = true;
                break;
            }
        }

        let starts_with_punct = trimmed.starts_with(|c: char| c == ',' || c == ';' || c == '.' || c == '-');

        if !has_trigger && !starts_with_punct {
            // Check for consecutive duplicate words without regex
            let mut words = trimmed.split_whitespace();
            if let Some(prev) = words.next() {
                let mut prev_clean = prev.trim_matches(|c: char| !c.is_alphanumeric());
                let mut has_stutter = false;
                for w in words {
                    let clean = w.trim_matches(|c: char| !c.is_alphanumeric());
                    if !clean.is_empty() && clean.eq_ignore_ascii_case(prev_clean) {
                        has_stutter = true;
                        break;
                    }
                    prev_clean = clean;
                }
                if !has_stutter {
                    // ZERO ALLOCATION FAST PATH:
                    // Text is already clean! Return borrowed string slice directly!
                    return Cow::Borrowed(trimmed);
                }
            } else {
                return Cow::Borrowed("");
            }
        }

        // SLOW-PATH: Mutate string
        // 1. Remove filler words with precompiled regex
        let step1 = SOTTO_FILLERS.replace_all(trimmed, "");

        // 2. Deduplicate consecutive stutters in O(N) token pass
        let words: Vec<&str> = step1.split_whitespace().collect();
        let mut deduped_buf = String::with_capacity(step1.len());
        let mut prev_clean: Option<&str> = None;
        for w in words {
            let clean = w.trim_matches(|c: char| !c.is_alphanumeric());
            if let Some(prev) = prev_clean {
                if !clean.is_empty() && clean.eq_ignore_ascii_case(prev) {
                    continue; // Skip stutter
                }
            }
            if !deduped_buf.is_empty() {
                deduped_buf.push(' ');
            }
            deduped_buf.push_str(w);
            prev_clean = Some(clean);
        }

        // 3. Clean spaces
        let step3 = SOTTO_SPACES.replace_all(&deduped_buf, " ");

        // 4. Clean leading/trailing punctuation & whitespace
        let cleaned = step3.trim().trim_start_matches(|c: char| c == ',' || c == ';' || c == '.' || c == '-').trim();
        let cleaned = cleaned.trim_end_matches(|c: char| c == ',' || c == ';').trim();

        if cleaned.is_empty() {
            return Cow::Borrowed("");
        }

        // 5. Restore sentence capitalization if the leading filler was removed
        let mut chars = cleaned.chars();
        let first_char = chars.next().unwrap();
        if first_char.is_alphabetic() && first_char.is_lowercase() {
            let mut res = String::with_capacity(cleaned.len());
            for c in first_char.to_uppercase() {
                res.push(c);
            }
            res.push_str(chars.as_str());
            Cow::Owned(res)
        } else {
            Cow::Owned(cleaned.to_string())
        }
    }
}

pub fn run_benchmark_suite() {
    let test_corpus = vec![
        ("Clean phrase (zero modification)", "The quick brown fox jumps over the lazy dog."),
        ("Hesitations & Stutters", "Um, I think that uh we should go to to the store."),
        ("Whisper Hallucination", "Thank you for watching"),
        ("Multi Stutter & Edge Punctuation", ", uh, we need to to fix this right right now, please!"),
        ("Long dictation with fillers", "Ah, so basically um the architecture of the system is is very fast and uh it performs well."),
    ];

    println!("\n==========================================================");
    println!("SOTTO SANITIZER BENCHMARK & COMPARISON (Step 2.3)");
    println!("Comparing: Legacy Sequential vs Multi-Pass Regex vs Sotto Hybrid");
    println!("==========================================================\n");

    let legacy = LegacySequentialSanitizer::new();

    // Verify correctness first
    for (name, input) in &test_corpus {
        println!("Input [{}]: \"{}\"", name, input);
        println!("  -> Legacy Output:     \"{}\"", legacy.sanitize(input));
        println!("  -> Multi-Pass Output: \"{}\"", sanitize_multipass_regex(input));
        println!("  -> Sotto Hybrid:      \"{}\"", SottoSanitizer::sanitize(input));
        println!();
    }

    #[cfg(debug_assertions)]
    const ITERATIONS: u32 = 2_000;
    #[cfg(not(debug_assertions))]
    const ITERATIONS: u32 = 100_000;

    // Benchmark Legacy Sequential
    let t0 = Instant::now();
    for _ in 0..ITERATIONS {
        for (_, input) in &test_corpus {
            let _ = legacy.sanitize(input);
        }
    }
    let legacy_elapsed = t0.elapsed();

    // Benchmark Multi-Pass Regex
    let t1 = Instant::now();
    for _ in 0..ITERATIONS {
        for (_, input) in &test_corpus {
            let _ = sanitize_multipass_regex(input);
        }
    }
    let multi_elapsed = t1.elapsed();

    // Benchmark Sotto Hybrid
    let t2 = Instant::now();
    for _ in 0..ITERATIONS {
        for (_, input) in &test_corpus {
            let _ = SottoSanitizer::sanitize(input);
        }
    }
    let sotto_elapsed = t2.elapsed();

    let total_ops = ITERATIONS * test_corpus.len() as u32;

    println!("---------------- RESULTS ({} iterations / {} total sanitizations) ----------------", ITERATIONS, total_ops);
    println!("Legacy-style (Sequential passes): {:?} total | {:.3} µs / op", legacy_elapsed, legacy_elapsed.as_nanos() as f64 / (total_ops as f64 * 1000.0));
    println!("Multi-Pass Regex:                 {:?} total | {:.3} µs / op", multi_elapsed, multi_elapsed.as_nanos() as f64 / (total_ops as f64 * 1000.0));
    println!("Sotto Hybrid (Zero-Alloc Fast):   {:?} total | {:.3} µs / op", sotto_elapsed, sotto_elapsed.as_nanos() as f64 / (total_ops as f64 * 1000.0));
    println!("----------------------------------------------------------------------------------");
    let speedup_legacy = legacy_elapsed.as_nanos() as f64 / sotto_elapsed.as_nanos() as f64;
    let speedup_multi = multi_elapsed.as_nanos() as f64 / sotto_elapsed.as_nanos() as f64;
    println!("SPEEDUP vs Legacy:     {:.2}x FASTER!", speedup_legacy);
    println!("SPEEDUP vs Multi-Pass: {:.2}x FASTER!\n", speedup_multi);
}

#[test]
fn test_benchmark_performance_comparison() {
    run_benchmark_suite();
}
