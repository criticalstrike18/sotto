// Copyright (c) 2026 Sotto
// Gemini HTTP/2 Client (audio transcription via generateContent + model discovery).
// Transcription uses inline WAV data; keys are stored in the SQLite provider
// store (db::providers) and never committed to the repo.

use std::sync::Arc;
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};

use super::groq::TranscriptionResult;

pub const DEFAULT_GEMINI_MODEL: &str = "gemini-3.5-transcribe";
pub const DEFAULT_GEMINI_LLM_MODEL: &str = "gemini-2.5-flash";
pub const GEMINI_LIST_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/models";

fn generate_endpoint(model: &str) -> String {
    format!("https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent", model)
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct GeminiModelInfo {
    pub name: Option<String>,
    #[serde(rename = "supportedGenerationMethods")]
    pub supported_methods: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct GeminiListResponse {
    models: Option<Vec<GeminiModelInfo>>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct GeminiContentPart {
    text: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct GeminiContent {
    parts: Option<Vec<GeminiContentPart>>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct GeminiCandidate {
    content: Option<GeminiContent>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct GeminiGenerateResponse {
    candidates: Option<Vec<GeminiCandidate>>,
}

use super::key_pool::{mask_key, KeyPool};

#[derive(Clone)]
pub struct GeminiClient {
    client: Arc<reqwest::Client>,
    key_pool: KeyPool,
    model: String,
    language: String,
}

impl GeminiClient {
    /// Creates a new GeminiClient with an optimized HTTP/2 connection pool.
    pub fn new(api_key: Option<String>) -> Result<Self, String> {
        let keys = api_key
            .or_else(|| std::env::var("GEMINI_API_KEY").ok())
            .map(|k| vec![k])
            .unwrap_or_default();
        Self::with_keys(keys)
    }

    /// Creates a new GeminiClient with a pool of keys for automated rotation.
    pub fn with_keys(keys: Vec<String>) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .tcp_nodelay(true)
            .pool_idle_timeout(Some(Duration::from_secs(120)))
            .pool_max_idle_per_host(8)
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| format!("Failed to build reqwest HTTP/2 client: {}", e))?;

        Ok(Self {
            client: Arc::new(client),
            key_pool: KeyPool::new(keys),
            model: DEFAULT_GEMINI_MODEL.to_string(),
            language: "en".to_string(),
        })
    }

    pub fn key_pool(&self) -> &KeyPool {
        &self.key_pool
    }

    pub fn set_key_pool(&mut self, pool: KeyPool) {
        self.key_pool = pool;
    }

    pub fn add_key(&self, key: &str) {
        self.key_pool.add_key(key);
    }

    pub fn remove_key(&self, key: &str) {
        self.key_pool.remove_key(key);
    }

    pub fn primary_key(&self) -> Option<String> {
        self.key_pool.primary_key()
    }

    pub fn set_api_key(&mut self, key: String) {
        self.key_pool.set_keys(vec![key]);
    }

    pub fn set_model(&mut self, model: String) {
        if !model.trim().is_empty() {
            self.model = model;
        }
    }

    pub fn has_api_key(&self) -> bool {
        !self.key_pool.is_empty()
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn language(&self) -> &str {
        &self.language
    }

    pub fn set_language(&mut self, language: String) {
        self.language = language;
    }

    fn require_key(&self) -> Result<String, String> {
        self.key_pool.get_next_key().ok_or_else(|| {
            "Gemini API key is not set. Add it under Studio -> Providers.".to_string()
        })
    }

    fn runtime() -> Result<tokio::runtime::Runtime, String> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("Failed to create tokio runtime: {}", e))
    }

    /// Validates the key with a cheap models-list call. Ok(model_count) on success.
    pub fn validate_key_blocking(&self) -> Result<usize, String> {
        let rt = Self::runtime()?;
        rt.block_on(self.validate_key())
    }

    pub async fn validate_key(&self) -> Result<usize, String> {
        let models = self.list_models().await?;
        Ok(models.len())
    }

    /// Fetches transcription-capable model ids (generateContent, no TTS/image/live/embedding).
    pub fn list_models_blocking(&self) -> Result<Vec<String>, String> {
        let rt = Self::runtime()?;
        rt.block_on(self.list_models())
    }

    pub async fn list_models(&self) -> Result<Vec<String>, String> {
        let key = self.require_key()?;
        let resp = self
            .client
            .get(GEMINI_LIST_ENDPOINT)
            .header("x-goog-api-key", key.as_str())
            .query(&[("pageSize", "100")])
            .send()
            .await
            .map_err(|e| format!("Gemini models request failed: {}", e))?;

        let status = resp.status();
        if status.as_u16() == 400 || status.as_u16() == 403 {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Invalid Gemini API key (HTTP {}). Get one at aistudio.google.com. {}", status, truncate(&body, 200)));
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Gemini models error (HTTP {}): {}", status, truncate(&body, 300)));
        }

        let parsed: GeminiListResponse = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse Gemini models JSON: {}", e))?;

        Ok(filter_transcription_models(parsed.models.unwrap_or_default()))
    }

    /// Uploads in-memory WAV bytes inline and returns the transcript.
    /// Automatically rotates across keys in the pool if rate limits (HTTP 429) occur.
    pub async fn transcribe(&self, wav_bytes: Vec<u8>) -> Result<TranscriptionResult, String> {
        self.transcribe_with_prompt(wav_bytes, None).await
    }

    /// Same as [`transcribe`], but tells Gemini how the previous chunk ended so
    /// a split recording is punctuated as one continuous sentence stream.
    pub async fn transcribe_with_prompt(
        &self,
        wav_bytes: Vec<u8>,
        prompt: Option<&str>,
    ) -> Result<TranscriptionResult, String> {
        if wav_bytes.is_empty() {
            return Err("Cannot transcribe empty audio buffer".to_string());
        }

        if self.key_pool.is_empty() {
            return Err("Gemini API key is not set. Add it under Studio -> Providers.".to_string());
        }

        let start = Instant::now();
        let wav_len = wav_bytes.len();
        let audio_b64 = base64_encode(&wav_bytes);

        let is_transcribe_model = self.model.to_lowercase().contains("transcribe");
        let base_prompt = super::language::prompt_for_gemini(&self.language);
        let prompt_text = match prompt.map(str::trim).filter(|p| !p.is_empty()) {
            Some(tail) => format!(
                "{}\n\nThis audio continues a longer recording. The previous segment ended with: \"{}\". \
                 Transcribe only this segment, continue naturally from that text, and do not repeat it.",
                base_prompt, tail
            ),
            None => base_prompt,
        };
        let payload = if is_transcribe_model {
            serde_json::json!({
                "contents": [{
                    "parts": [
                        {"text": prompt_text},
                        {"inlineData": {"mimeType": "audio/wav", "data": audio_b64}}
                    ]
                }],
                "generationConfig": {
                    "audioTranscriptionConfig": {"mode": "SMART"},
                    "maxOutputTokens": 1024
                }
            })
        } else {
            serde_json::json!({
                "contents": [{
                    "parts": [
                        {"text": prompt_text},
                        {"inlineData": {"mimeType": "audio/wav", "data": audio_b64}}
                    ]
                }],
                "generationConfig": {"temperature": 0.0, "maxOutputTokens": 1024}
            })
        };

        let max_attempts = std::cmp::max(1, self.key_pool.len());
        let mut last_err = String::new();

        for _ in 0..max_attempts {
            let key = match self.key_pool.get_next_key() {
                Some(k) => k,
                None => break,
            };

            let response = self
                .client
                .post(generate_endpoint(&self.model))
                .header("x-goog-api-key", key.as_str())
                .json(&payload)
                .send()
                .await;

            match response {
                Ok(resp) => {
                    let status = resp.status();
                    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                        eprintln!("[Gemini KeyPool] Key {} hit rate limit (429)! Auto-rotating...", mask_key(&key));
                        self.key_pool.mark_rate_limited(&key, Duration::from_secs(60));
                        last_err = "Gemini Rate Limit Exceeded (429). Rotating key...".to_string();
                        continue;
                    }
                    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::BAD_REQUEST {
                        eprintln!("[Gemini KeyPool] Key {} rejected ({})! Auto-rotating...", mask_key(&key), status);
                        self.key_pool.mark_rate_limited(&key, Duration::from_secs(3600));
                        last_err = format!("Gemini key rejected (HTTP {}). Rotating key...", status);
                        continue;
                    }
                    if !status.is_success() {
                        let body = resp.text().await.unwrap_or_else(|_| "Unknown server error".to_string());
                        return Err(format!("Gemini transcription error (HTTP {}): {}", status, truncate(&body, 300)));
                    }

                    let parsed: GeminiGenerateResponse = resp
                        .json()
                        .await
                        .map_err(|e| format!("Failed to parse Gemini response JSON: {}", e))?;

                    self.key_pool.mark_success(&key);

                    let text = extract_transcript(&parsed);
                    let latency_ms = start.elapsed().as_millis() as u64;

                    println!(
                        "[Gemini Transcribe] Latency: {}ms | Audio: {} bytes | Model: {} | Text: \"{}\"",
                        latency_ms,
                        wav_len,
                        self.model,
                        truncate(&text, 80)
                    );

                    return Ok(TranscriptionResult {
                        text: text.trim().to_string(),
                        latency_ms: latency_ms as u128,
                        wav_bytes_len: wav_len,
                        model: self.model.clone(),
                    });
                }
                Err(e) => {
                    return Err(format!("Gemini HTTP request failed: {}", e));
                }
            }
        }

        Err(if last_err.is_empty() {
            "All Gemini API keys in pool failed or on cooldown.".to_string()
        } else {
            last_err
        })
    }

    /// Uses Gemini's high-speed LLM to polish, punctuate, or format transcriptions.
    pub async fn rewrite_or_format(
        &self,
        text: &str,
        venue: Option<&str>,
        model_name: Option<&str>,
        style: i32,
    ) -> Result<String, String> {
        let key = self.require_key()?;
        let model = model_name.unwrap_or(DEFAULT_GEMINI_LLM_MODEL);
        let venue_hint = venue.unwrap_or("General text editor");
        let system_prompt = if style == 1 {
            format!(
                "You are an ultra-fast precision transcription post-processor. The user's text will be pasted into {}. \
                 Fix grammar, casing, and punctuation according to that application's style. \
                 Return ONLY the polished text. Do not add quotes, introductory remarks, or explanations.",
                venue_hint
            )
        } else {
            "You are an ultra-fast precision transcription post-processor. \
             Fix grammar, casing, and punctuation only. Preserve all words and meaning. \
             Return ONLY the polished text with no surrounding quotes or commentary.".to_string()
        };

        let word_count = text.split_whitespace().count();
        let max_tokens = std::cmp::max(64, (word_count * 3 + 16) as u32);

        let payload = serde_json::json!({
            "systemInstruction": {
                "parts": [{"text": system_prompt}]
            },
            "contents": [{
                "parts": [{"text": text}]
            }],
            "generationConfig": {
                "temperature": 0.0,
                "maxOutputTokens": max_tokens
            }
        });

        let response = self
            .client
            .post(generate_endpoint(model))
            .header("x-goog-api-key", key.as_str())
            .json(&payload)
            .send()
            .await
            .map_err(|e| format!("Gemini LLM HTTP request failed: {}", e))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_else(|_| "Unknown server error".to_string());
            return Err(format!("Gemini LLM error (HTTP {}): {}", status, truncate(&body, 250)));
        }

        let parsed: GeminiGenerateResponse = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse Gemini LLM response JSON: {}", e))?;

        let content = extract_transcript(&parsed);
        if content.trim().is_empty() {
            Ok(text.to_string())
        } else {
            Ok(content.trim().to_string())
        }
    }

    /// Synchronous convenience wrapper for non-async calling threads.
    pub fn transcribe_blocking(&self, wav_bytes: Vec<u8>) -> Result<TranscriptionResult, String> {
        self.transcribe_blocking_with_prompt(wav_bytes, None)
    }

    pub fn transcribe_blocking_with_prompt(
        &self,
        wav_bytes: Vec<u8>,
        prompt: Option<&str>,
    ) -> Result<TranscriptionResult, String> {
        let rt = Self::runtime()?;
        rt.block_on(self.transcribe_with_prompt(wav_bytes, prompt))
    }
}

/// Standard base64 (RFC 4648) encoder — avoids an extra dependency for one call.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { ALPHABET[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Keeps models that can transcribe speech over unary generateContent REST:
/// dedicated transcribe models first, then general flash models.
/// Live-API-only models (`*-live*`, native-audio) are websocket-exclusive and
/// would fail a generateContent call, so they are excluded, not hidden.
pub fn filter_transcription_models(models: Vec<GeminiModelInfo>) -> Vec<String> {
    let mut ids: Vec<String> = models
        .into_iter()
        .filter_map(|m| {
            let name = m.name?;
            let id = name.strip_prefix("models/").unwrap_or(&name).to_string();
            let methods = m.supported_methods.unwrap_or_default();
            if !methods.iter().any(|method| method == "generateContent") {
                return None;
            }
            let lower = id.to_lowercase();
            if lower.contains("embedding")
                || lower.contains("aqa")
                || lower.contains("imagen")
                || lower.contains("tts")
                || lower.contains("-live")
                || lower.contains("realtime")
            {
                return None;
            }
            Some(id)
        })
        .collect();

    ids.sort_by(|a, b| {
        let a_trans = a.to_lowercase().contains("transcribe");
        let b_trans = b.to_lowercase().contains("transcribe");
        match (a_trans, b_trans) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.cmp(b),
        }
    });

    if let Some(pos) = ids.iter().position(|m| m == DEFAULT_GEMINI_MODEL) {
        let preferred = ids.remove(pos);
        ids.insert(0, preferred);
    }

    if ids.is_empty() {
        vec![DEFAULT_GEMINI_MODEL.to_string()]
    } else {
        ids
    }
}

fn extract_transcript(resp: &GeminiGenerateResponse) -> String {
    let candidates = match resp.candidates.as_ref() {
        Some(c) => c,
        None => return String::new(),
    };
    let first = match candidates.first() {
        Some(c) => c,
        None => return String::new(),
    };
    let content = match first.content.as_ref() {
        Some(c) => c,
        None => return String::new(),
    };
    let parts = match content.parts.as_ref() {
        Some(p) => p,
        None => return String::new(),
    };
    parts
        .iter()
        .filter_map(|p| p.text.as_ref())
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{}...", truncated)
    }
}
