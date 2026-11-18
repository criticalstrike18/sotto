// Copyright (c) 2026 Sotto
// Pre-Warmed Groq HTTP/2 Client (Whisper-Large-v3-Turbo + Low Latency Routing)

use std::sync::Arc;
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};

pub const DEFAULT_GROQ_MODEL: &str = "whisper-large-v3-turbo";
pub const DEFAULT_GROQ_LLM_MODEL: &str = "qwen/qwen3.8-27b";
pub const DEFAULT_PROMPT: &str = "Hello, this is clean speech dictation with proper capitalization, punctuation, and technical terms (e.g., API, HTTP, JSON, GitHub).";
pub const GROQ_ENDPOINT: &str = "https://api.groq.com/openai/v1/audio/transcriptions";
pub const GROQ_CHAT_ENDPOINT: &str = "https://api.groq.com/openai/v1/chat/completions";

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct GroqResponse {
    pub text: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ChatCompletionResponse {
    pub choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ChatChoice {
    pub message: ChatMessage,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ModelListResponse {
    pub data: Vec<ModelItem>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ModelItem {
    pub id: String,
    #[serde(default)]
    pub active: Option<bool>,
}

/// Transcription latency and result payload returned by Groq.
#[derive(Debug, Clone)]
pub struct TranscriptionResult {
    pub text: String,
    pub latency_ms: u128,
    pub wav_bytes_len: usize,
    pub model: String,
}

use super::key_pool::{mask_key, KeyPool};

#[derive(Clone)]
pub struct GroqClient {
    client: Arc<reqwest::Client>,
    key_pool: KeyPool,
    model: String,
    prompt: String,
    language: String,
}

impl GroqClient {
    /// Creates a new GroqClient with an optimized HTTP/2 connection pool.
    pub fn new(api_key: Option<String>) -> Result<Self, String> {
        let keys = api_key
            .or_else(|| std::env::var("GROQ_API_KEY").ok())
            .map(|k| vec![k])
            .unwrap_or_default();
        Self::with_keys(keys)
    }

    /// Creates a new GroqClient with a pool of keys for automated rotation.
    pub fn with_keys(keys: Vec<String>) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .tcp_nodelay(true)
            .pool_idle_timeout(Some(Duration::from_secs(120)))
            .pool_max_idle_per_host(8)
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| format!("Failed to build reqwest HTTP/2 client: {}", e))?;

        let instance = Self {
            client: Arc::new(client),
            key_pool: KeyPool::new(keys),
            model: DEFAULT_GROQ_MODEL.to_string(),
            prompt: DEFAULT_PROMPT.to_string(),
            language: "en".to_string(),
        };

        if !instance.key_pool.is_empty() {
            instance.prewarm();
        }

        Ok(instance)
    }

    pub fn key_pool(&self) -> &KeyPool {
        &self.key_pool
    }

    pub fn set_key_pool(&mut self, pool: KeyPool) {
        self.key_pool = pool;
        self.prewarm();
    }

    pub fn add_key(&self, key: &str) {
        if self.key_pool.add_key(key) {
            self.prewarm();
        }
    }

    pub fn remove_key(&self, key: &str) {
        self.key_pool.remove_key(key);
    }

    pub fn primary_key(&self) -> Option<String> {
        self.key_pool.primary_key()
    }

    /// Synchronous convenience wrapper for non-async calling threads.
    pub fn list_models_blocking(&self) -> Result<Vec<String>, String> {
        self.list_models()
    }

    /// Fetches whisper transcription models available to this account.
    pub fn list_models(&self) -> Result<Vec<String>, String> {
        let key = self.key_pool.get_next_key().ok_or_else(|| "GROQ_API_KEY is not set in key pool.".to_string())?;

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("Failed to create tokio runtime: {}", e))?;

        let client = Arc::clone(&self.client);
        let parsed: ModelListResponse = rt.block_on(async move {
            let res = client
                .get("https://api.groq.com/openai/v1/models")
                .header("Authorization", format!("Bearer {}", key))
                .send()
                .await
                .map_err(|e| format!("Network error: {}", e))?;

            if !res.status().is_success() {
                let status = res.status();
                let body = res.text().await.unwrap_or_default();
                return Err(format!("HTTP error {}: {}", status, body));
            }

            res.json().await.map_err(|e| format!("JSON error: {}", e))
        })?;

        Ok(filter_whisper_models(parsed.data))
    }

    /// Pre-warms the HTTP/2 and TLS socket connection to Groq in the background,
    /// so the first speech transcription incurs 0ms DNS and TLS handshake latency.
    pub fn prewarm(&self) {
        let client = Arc::clone(&self.client);
        let api_key = self.key_pool.get_next_key();

        // If inside a Tokio runtime, spawn directly; otherwise spawn in a detached background thread with its own runtime.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let mut req = client.get("https://api.groq.com/openai/v1/models");
                if let Some(ref k) = api_key {
                    req = req.header("Authorization", format!("Bearer {}", k));
                }
                let _ = req.send().await;
                println!("[Groq Client] HTTP/2 connection pre-warmed to api.groq.com");
            });
        } else {
            std::thread::spawn(move || {
                if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    rt.block_on(async move {
                        let mut req = client.get("https://api.groq.com/openai/v1/models");
                        if let Some(ref k) = api_key {
                            req = req.header("Authorization", format!("Bearer {}", k));
                        }
                        let _ = req.send().await;
                        println!("[Groq Client] HTTP/2 connection pre-warmed to api.groq.com");
                    });
                }
            });
        }
    }

    /// Asynchronously uploads in-memory WAV bytes to Groq and returns the transcribed text.
    /// Automatically rotates across keys in the pool if rate limits (HTTP 429) occur.
    pub async fn transcribe(&self, wav_bytes: Vec<u8>) -> Result<TranscriptionResult, String> {
        self.transcribe_with_prompt(wav_bytes, None).await
    }

    /// Same as [`transcribe`], but uses `prompt` (normally the tail of the
    /// previous chunk's transcript) instead of the default style hint. Whisper's
    /// prompt conditions spelling, casing, and punctuation across a split.
    pub async fn transcribe_with_prompt(
        &self,
        wav_bytes: Vec<u8>,
        prompt: Option<&str>,
    ) -> Result<TranscriptionResult, String> {
        if wav_bytes.is_empty() {
            return Err("Cannot transcribe empty audio buffer".to_string());
        }

        if self.key_pool.is_empty() {
            return Err("GROQ_API_KEY is not set. Please add an API key in Settings.".to_string());
        }

        let start = Instant::now();
        let wav_len = wav_bytes.len();
        let is_en_only = self.model.ends_with(".en");
        let lang = if is_en_only { "en" } else { self.language.as_str() };
        let prompt_text = prompt
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .unwrap_or(self.prompt.as_str())
            .to_string();

        let max_attempts = std::cmp::max(1, self.key_pool.len());
        let mut last_err = String::new();

        for _ in 0..max_attempts {
            let key = match self.key_pool.get_next_key() {
                Some(k) => k,
                None => break,
            };

            let part = reqwest::multipart::Part::bytes(wav_bytes.clone())
                .file_name("dictation.wav")
                .mime_str("audio/wav")
                .map_err(|e| format!("Failed to set mime type: {}", e))?;

            let mut form = reqwest::multipart::Form::new()
                .part("file", part)
                .text("model", self.model.clone())
                .text("response_format", "json")
                .text("temperature", "0.0")
                .text("prompt", prompt_text.clone());

            if lang != "auto" {
                form = form.text("language", lang.to_string());
            }

            let response = self
                .client
                .post(GROQ_ENDPOINT)
                .header("Authorization", format!("Bearer {}", key))
                .multipart(form)
                .send()
                .await;

            match response {
                Ok(resp) => {
                    let status = resp.status();
                    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                        eprintln!("[Groq KeyPool] Key {} hit rate limit (429)! Auto-rotating...", mask_key(&key));
                        self.key_pool.mark_rate_limited(&key, Duration::from_secs(60));
                        last_err = "Groq Rate Limit Exceeded (HTTP 429). Rotating key...".to_string();
                        continue;
                    }
                    if status == reqwest::StatusCode::UNAUTHORIZED {
                        eprintln!("[Groq KeyPool] Key {} rejected (401)! Auto-rotating...", mask_key(&key));
                        self.key_pool.mark_rate_limited(&key, Duration::from_secs(3600));
                        last_err = "Invalid Groq API key (HTTP 401). Rotating key...".to_string();
                        continue;
                    }
                    if !status.is_success() {
                        let error_body = resp.text().await.unwrap_or_else(|_| "Unknown server error".to_string());
                        return Err(format!("Groq API error (HTTP {}): {}", status, error_body));
                    }

                    let parsed: GroqResponse = resp
                        .json()
                        .await
                        .map_err(|e| format!("Failed to parse Groq response JSON: {}", e))?;

                    self.key_pool.mark_success(&key);

                    let latency_ms = start.elapsed().as_millis();
                    println!(
                        "[Groq Transcribe] Latency: {}ms | Audio: {} bytes | Model: {} | Text: '{}'",
                        latency_ms,
                        wav_len,
                        self.model,
                        parsed.text.trim()
                    );

                    return Ok(TranscriptionResult {
                        text: parsed.text.trim().to_string(),
                        latency_ms,
                        wav_bytes_len: wav_len,
                        model: self.model.clone(),
                    });
                }
                Err(e) => {
                    return Err(format!("Network error sending audio to Groq: {}", e));
                }
            }
        }

        Err(if last_err.is_empty() {
            "All Groq API keys in pool failed or on cooldown.".to_string()
        } else {
            last_err
        })
    }

    /// Uses Groq's high-speed LLM (Qwen 2.5 / GPT-OSS) to polish, punctuate, or format transcriptions.
    pub async fn rewrite_or_format(
        &self,
        text: &str,
        venue: Option<&str>,
        model_name: Option<&str>,
        style: i32,
    ) -> Result<String, String> {
        let key = self.key_pool.get_next_key().ok_or_else(|| {
            "GROQ_API_KEY is not set in key pool.".to_string()
        })?;

        let model = model_name.unwrap_or(DEFAULT_GROQ_LLM_MODEL);
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
        let max_tokens = if model.contains("gpt-oss") {
            std::cmp::max(256, (word_count * 4 + 64) as u32)
        } else {
            std::cmp::max(128, (word_count * 3 + 32) as u32)
        };

        let payload = serde_json::json!({
            "model": model,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": text}
            ],
            "temperature": 0.0,
            "max_tokens": max_tokens
        });

        let resp = self
            .client
            .post(GROQ_CHAT_ENDPOINT)
            .header("Authorization", format!("Bearer {}", key))
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| format!("Groq Chat request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Groq Chat error (HTTP {}): {}", status, body));
        }

        let parsed: ChatCompletionResponse = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse chat response: {}", e))?;

        let content = parsed
            .choices
            .first()
            .map(|c| c.message.content.trim().to_string())
            .unwrap_or_else(|| text.to_string());

        Ok(content)
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
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("Failed to create tokio runtime: {}", e))?;

        rt.block_on(self.transcribe_with_prompt(wav_bytes, prompt))
    }

    pub fn set_api_key(&mut self, key: String) {
        self.key_pool.set_keys(vec![key]);
        self.prewarm();
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

    /// Validates the API key synchronously with a lightweight models check.
    pub fn validate_key(&self) -> Result<usize, String> {
        let models = self.list_models()?;
        Ok(models.len())
    }
}

/// Filters raw Groq models list down to Whisper speech transcription models.
pub fn filter_whisper_models(models: Vec<ModelItem>) -> Vec<String> {
    let mut whisper_models: Vec<String> = models
        .into_iter()
        .filter(|m| m.active.unwrap_or(true))
        .map(|m| m.id)
        .filter(|id| id.to_lowercase().contains("whisper"))
        .collect();

    whisper_models.sort();

    // Ensure preferred model is first if present
    if let Some(pos) = whisper_models.iter().position(|m| m == DEFAULT_GROQ_MODEL) {
        let preferred = whisper_models.remove(pos);
        whisper_models.insert(0, preferred);
    }

    if whisper_models.is_empty() {
        vec![DEFAULT_GROQ_MODEL.to_string(), "whisper-large-v3".to_string()]
    } else {
        whisper_models
    }
}
