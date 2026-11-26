// Copyright (c) 2026 Sotto
// Integration & Contract Test Suite
// Covers all subsystems: Audio, Router, Text Injection, History DB, Pipeline

use sotto_desktop::audio::filter::*;
use sotto_desktop::audio::resample::*;
use sotto_desktop::audio::wav::*;
use sotto_desktop::cleaner::*;
use sotto_desktop::db::history::*;
use sotto_desktop::router::*;

// =========================================================================
// 1. AUDIO SUBSYSTEM & DSP PIPELINE
// =========================================================================

#[test]
fn test_audio_full_pipeline_roundtrip() {
    let sample_rate = 48000;
    let duration_secs = 1;
    let total_samples = sample_rate * duration_secs;

    // Generate stereo 440Hz sine wave (simulating mic input)
    let mut stereo_input = Vec::with_capacity(total_samples * 2);
    for i in 0..total_samples {
        let t = i as f32 / sample_rate as f32;
        let sample = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.4;
        stereo_input.push(sample); // L
        stereo_input.push(sample); // R
    }

    // Step 1: Downmix to mono
    let mono = downmix_interleaved_to_mono(&stereo_input, 2);
    assert_eq!(mono.len(), total_samples);

    // Step 2: Resample 48kHz -> 16kHz
    let mut resampled = resample_linear(&mono, 48000, 16000);
    assert_eq!(resampled.len(), 16000);

    // Step 3: Biquad High-Pass DC Filter (80Hz cut)
    let mut filter = BiquadHighPass::new(80.0, 16000.0);
    filter.process_slice(&mut resampled);
    assert_eq!(resampled.len(), 16000);

    // Step 4: Soft-Knee Limiter & Float-to-PCM16
    let mut pcm = float_to_pcm16(&resampled);
    assert_eq!(pcm.len(), 16000);

    // Step 5: Digital Gain Calibration
    let gain = apply_digital_gain(&mut pcm, 28000.0, 4.0);
    assert!(gain >= 1.0);
    assert_eq!(pcm.len(), 16000);

    // Step 6: WAV Header & Payload Serialization
    let wav_bytes = encode_pcm16_to_wav(&pcm, 16000);
    assert_eq!(wav_bytes.len(), 44 + 16000 * 2);
    assert_eq!(&wav_bytes[0..4], b"RIFF");
    assert_eq!(&wav_bytes[8..12], b"WAVE");

    // Step 7: Parse header back and verify integrity
    let info = parse_wav_header(&wav_bytes).expect("Valid WAV header");
    assert_eq!(info.sample_rate, 16000);
    assert_eq!(info.num_channels, 1);
    assert_eq!(info.data_length, 16000 * 2);
}

#[test]
fn test_audio_pipeline_ghost_tap_suppression() {
    // Generate a single sharp transient spike (< 50ms total recording)
    let click_samples = vec![20000i16; 800]; // 50ms at 16kHz

    let validation = validate_audio(&click_samples, 16000, 300, 0.001);
    match validation {
        AudioValidation::DiscardGhostTap { duration_ms } => {
            assert!(duration_ms < 300);
        }
        other => panic!("Expected DiscardGhostTap rejection, got: {:?}", other),
    }
}

#[test]
fn test_audio_pipeline_silence_rejection() {
    // Pure silence buffer (2 seconds)
    let silence = vec![2i16; 16000 * 2];
    let validation = validate_audio(&silence, 16000, 300, 0.001);
    match validation {
        AudioValidation::DiscardSilence { rms } => {
            assert!(rms < 0.001);
        }
        other => panic!("Expected DiscardSilence rejection, got: {:?}", other),
    }
}

// =========================================================================
// 2. INTELLIGENT ROUTER & GROQ / GEMINI CLIENT
// =========================================================================

#[test]
fn test_router_single_key_groq_dispatch() {
    let router = Transcriber::new(
        "groq",
        Some("gsk_test_key_123".to_string()),
        None,
        None,
        None,
    );
    assert_eq!(router.active_provider(), "groq");
    assert_eq!(router.chip_text(), "Groq whisper-large-v3-turbo");
}

#[test]
fn test_router_single_key_gemini_dispatch() {
    let router = Transcriber::new(
        "gemini",
        None,
        None,
        Some("AIzaSy_test_key_456".to_string()),
        None,
    );
    assert_eq!(router.active_provider(), "gemini");
    assert_eq!(router.chip_text(), "Gemini gemini-3.5-transcribe");
}

#[test]
fn test_router_key_pool_failover() {
    let pool = KeyPool::new(vec![
        "gsk_primary_1234567890".to_string(),
        "gsk_secondary_123456789".to_string(),
    ]);

    // Initial state: primary key active
    let key1 = pool.get_next_key();
    assert!(key1.is_some());
    assert_eq!(key1.unwrap(), "gsk_primary_1234567890");

    // Simulate Rate Limit (429) on primary
    pool.mark_rate_limited("gsk_primary_1234567890", std::time::Duration::from_secs(60));

    // Failover rotates to secondary key
    let key2 = pool.get_next_key();
    assert!(key2.is_some());
    assert_eq!(key2.unwrap(), "gsk_secondary_123456789");

    // Secondary succeeds (200 OK)
    pool.mark_success("gsk_secondary_123456789");
    assert_eq!(pool.get_next_key(), Some("gsk_secondary_123456789".to_string()));
}

#[test]
fn test_groq_empty_audio_buffer_validation() {
    let client = GroqClient::new(Some("mock_test_key".to_string())).expect("Client creation");
    let err = client.transcribe_blocking(Vec::new()).unwrap_err();
    assert_eq!(err, "Cannot transcribe empty audio buffer");
}

// =========================================================================
// 3. PERSISTENT HISTORY & WAL SQLITE
// =========================================================================

#[test]
fn test_history_db_crud_and_wal_journal() {
    let db = HistoryDb::open_in_memory().expect("In-memory DB opens successfully");

    // Insert 3 records
    let rec1 = db
        .insert("First test transcript", 1250, "Code.exe")
        .expect("Insert 1");
    let rec2 = db
        .insert("Second voice entry", 3400, "slack.exe")
        .expect("Insert 2");
    let rec3 = db
        .insert("Third final recording", 800, "chrome.exe")
        .expect("Insert 3");

    // Query recent (limit 10) -> Most recent first
    let recent = db.get_recent(10).expect("Query recent");
    assert_eq!(recent.len(), 3);
    assert_eq!(recent[0].id, rec3.id);
    assert_eq!(recent[1].id, rec2.id);
    assert_eq!(recent[2].id, rec1.id);

    // Verify fields
    assert_eq!(recent[0].transcript, "Third final recording");
    assert_eq!(recent[0].duration_ms, 800);
    assert_eq!(recent[0].target_app, "chrome.exe");

    // Delete single record
    db.delete_by_id(rec2.id).expect("Delete record 2");
    let remaining = db.get_recent(10).expect("Query after delete");
    assert_eq!(remaining.len(), 2);
    assert_eq!(remaining[0].id, rec3.id);
    assert_eq!(remaining[1].id, rec1.id);

    // Clear all
    db.clear_all().expect("Clear database");
    let empty = db.get_recent(10).expect("Query empty db");
    assert_eq!(empty.len(), 0);
}

// =========================================================================
// 4. POST-PROCESSING CLEANER PIPELINE
// =========================================================================

#[test]
fn test_cleaner_pipeline_end_to_end() {
    let options = PostProcessOptions {
        mode: PostProcessMode::LocalFast,
        custom_dictionary: vec![
            DictionaryEntry {
                from: "react js".to_string(),
                to: "React.js".to_string(),
            },
            DictionaryEntry {
                from: "github".to_string(),
                to: "GitHub".to_string(),
            },
        ],
        venue_formatting_enabled: true,
        number_symbol_normalization: true,
        llm_enabled: false,
        llm_provider: "groq".to_string(),
        llm_model: "qwen/qwen3.8-27b".to_string(),
        llm_style: 0,
    };

    let post_processor = PostProcessor::new(options);

    // Complex raw whisper transcription:
    // Has fillers ("um", "uh"), spoken punctuation ("period", "comma"),
    // tech jargon ("react js", "github"), and awkward stuttering ("I I think").
    let raw = "um I I think that react js and github are awesome period let me know";
    let cleaned = post_processor.process_local_with_duration(raw, Some("Code.exe"), None);

    // Verify transformations:
    // - "um" removed
    // - "I I" deduplicated to "I"
    // - "react js" expanded to "React.js"
    // - "github" capitalized to "GitHub"
    // - "period" turned into "."
    assert!(cleaned.contains("React.js"));
    assert!(cleaned.contains("GitHub"));
    assert!(!cleaned.contains("um"));
    assert!(!cleaned.contains("I I"));
}

// =========================================================================
// 5. APP CONTROLLER LOGIC, REAL HTTP KEY ROTATION & MEMORY RETENTION
// =========================================================================

#[test]
fn test_app_logic_controller_flows_end_to_end() {
    use sotto_desktop::app_logic::{
        apply_post_process_model, apply_post_process_provider, apply_post_process_style,
        apply_post_process_toggle, apply_reset_trigger_slot, decide_pill_click_action,
        normalize_language_for_model, CustomizationSnapshot, PillClickAction,
    };
    use sotto_desktop::config::{AppConfig, CaptureMode};
    use sotto_desktop::hotkey::TriggerBinding;

    // 1. Pill click state machine across states 0..=4 & error retry vs idle fallback.
    assert_eq!(decide_pill_click_action(0, false), PillClickAction::StartListening);
    assert_eq!(decide_pill_click_action(1, false), PillClickAction::StopListening);
    assert_eq!(decide_pill_click_action(2, true), PillClickAction::IgnoreTranscribing);
    assert_eq!(decide_pill_click_action(3, false), PillClickAction::DismissSuccess);
    assert_eq!(decide_pill_click_action(4, true), PillClickAction::RetryAudio);
    assert_eq!(decide_pill_click_action(4, false), PillClickAction::ResetErrorToIdle);

    // 2. English-only model lock & multilingual language formatting.
    assert_eq!(
        normalize_language_for_model("es", "distil-whisper-large-v3-en"),
        ("en".to_string(), "More Languages".to_string(), false)
    );
    assert_eq!(
        normalize_language_for_model("es", "whisper-large-v3-turbo"),
        ("es".to_string(), "Spanish (es)".to_string(), true)
    );
    assert_eq!(
        normalize_language_for_model("auto", "gemini-3.5-transcribe"),
        ("auto".to_string(), "More Languages".to_string(), true)
    );

    // 3. Post-processing toggles, provider model auto-switching & live PostProcessor sync.
    let mut cfg = AppConfig::default();
    let processor = PostProcessor::default();
    let opts = apply_post_process_toggle(&mut cfg, &processor, true);
    assert!(opts.llm_enabled);
    assert_eq!(opts.mode, PostProcessMode::SmartLLM);

    let switched = apply_post_process_provider(&mut cfg, &processor, "gemini");
    assert_eq!(switched, "gemini-2.5-flash");
    apply_post_process_model(&mut cfg, &processor, "gemini-2.5-pro");
    apply_post_process_style(&mut cfg, &processor, 1);
    assert_eq!(processor.get_options().llm_model, "gemini-2.5-pro");
    assert_eq!(processor.get_options().llm_style, 1);

    let back_to_groq = apply_post_process_provider(&mut cfg, &processor, "groq");
    assert_eq!(back_to_groq, "qwen/qwen3.8-27b");

    // 4. Trigger slot resets & formatted displays across all 4 slots.
    cfg.primary_trigger = TriggerBinding::ctrl_space();
    cfg.secondary_trigger = Some(TriggerBinding::mouse_back());
    cfg.primary_cancel_trigger = Some(TriggerBinding::ctrl_shift_space());
    cfg.secondary_cancel_trigger = Some(TriggerBinding::mouse_forward());
    let (disp_s, _) = apply_reset_trigger_slot(1, &mut cfg);
    assert_eq!(disp_s.secondary, "Not set");
    let (disp_cs, _) = apply_reset_trigger_slot(3, &mut cfg);
    assert_eq!(disp_cs.cancel_secondary, "Not set");
    let (disp_cp, _) = apply_reset_trigger_slot(2, &mut cfg);
    assert_eq!(disp_cp.cancel_primary, "Esc");
    let (disp_p, _) = apply_reset_trigger_slot(0, &mut cfg);
    assert_eq!(disp_p.primary, "F8");

    // 5. Customization snapshot & isolated temp-file AppConfig persistence roundtrip.
    let snap = CustomizationSnapshot {
        opt_idle: 2,
        opt_listening: 1,
        opt_transcribe: 2,
        opt_success: 1,
        opt_error: 2,
        surface_opacity: 0.7,
        trigger_mode: 1,
        capture_mode: CaptureMode::OnboardOnly,
        chimes_enabled: false,
        media_pause_enabled: false,
    };
    snap.apply_to(&mut cfg);
    let tmp_path = std::env::temp_dir().join(format!(
        "sotto_app_logic_test_{}_{:?}.json",
        std::process::id(),
        std::thread::current().id()
    ));
    cfg.save_to_path(&tmp_path).expect("temp config save");
    let loaded = AppConfig::load_from_path(&tmp_path);
    let _ = std::fs::remove_file(&tmp_path);
    assert_eq!(loaded.opt_idle, 2);
    assert_eq!(loaded.capture_mode, CaptureMode::OnboardOnly);
    assert!(!loaded.chimes_enabled);
    assert!(!loaded.media_pause_enabled);
}

#[test]
fn test_post_processor_end_to_end_pipeline_functionality() {
    // 1. Duration rate hallucination guard (>10 words/sec on short noise bursts dropped).
    let proc_default = PostProcessor::default();
    assert_eq!(
        proc_default.process_local_with_duration("this is way too many impossible words for a short blip", Some("Code.exe"), Some(200)),
        ""
    );

    // 2. Trailing hallucination outro strip preserves preceding dictation & punctuation.
    let cleaned = proc_default.process_local_with_duration("Ship the build now, thanks for watching.", Some("Code.exe"), Some(2500));
    assert_eq!(cleaned, "Ship the build now");

    // 3. Number & symbol normalization flag OFF preserves spoken words while custom dict overrides built-in rules.
    let mut opts = PostProcessOptions::default();
    opts.number_symbol_normalization = false;
    opts.custom_dictionary = vec![DictionaryEntry::new("github", "GH Enterprise")];
    let proc_custom = PostProcessor::new(opts);
    let out = proc_custom.process_local_with_duration("um check github for ten dollars", Some("Slack.exe"), Some(2500));
    assert!(out.contains("GH Enterprise"), "custom dict must override built-in entry: {out}");
    assert!(out.contains("ten dollars"), "disabled number normalization must keep literal phrase: {out}");

    // 4. SmartLLM async pipeline fast-path bypass (<= 2 words) & offline local fallback.
    let mut llm_opts = PostProcessOptions::default();
    llm_opts.llm_enabled = true;
    llm_opts.mode = PostProcessMode::SmartLLM;
    let proc_llm = PostProcessor::new(llm_opts);
    let rt = shared_runtime().expect("shared runtime builds");
    let fast_out = rt.block_on(proc_llm.process_async_with_duration("check github", Some("Code.exe"), None, None, Some(1000)));
    assert_eq!(fast_out, "check GitHub");
    let fallback_out = rt.block_on(proc_llm.process_async_with_duration("um open sqlite and rust", Some("Code.exe"), None, None, Some(2500)));
    assert!(fallback_out.contains("SQLite") && fallback_out.contains("Rust"));
}

#[test]
fn test_groq_client_real_http_multipart_and_429_key_rotation() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback HTTP server");
    let addr = listener.local_addr().unwrap();

    let server = std::thread::spawn(move || {
        let mut seen_auths = Vec::new();
        for idx in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            loop {
                let n = stream.read(&mut tmp).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                let raw = String::from_utf8_lossy(&buf);
                if let Some(hdr_end) = raw.find("\r\n\r\n") {
                    let content_len = raw[..hdr_end]
                        .lines()
                        .find_map(|l| {
                            let lower = l.to_ascii_lowercase();
                            lower.strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if buf.len() >= hdr_end + 4 + content_len {
                        break;
                    }
                }
            }
            let raw = String::from_utf8_lossy(&buf);
            let auth = raw
                .lines()
                .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                .unwrap_or("")
                .trim()
                .to_string();
            seen_auths.push(auth);
            assert!(raw.contains("dictation.wav"), "request #{idx} must include multipart wav filename");

            if idx == 0 {
                let resp = "HTTP/1.1 429 Too Many Requests\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
                stream.write_all(resp.as_bytes()).unwrap();
            } else {
                let body = r#"{"text":"rotated transcript ok"}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(resp.as_bytes()).unwrap();
            }
        }
        seen_auths
    });

    let endpoint = format!("http://{}/transcribe", addr);
    let client = GroqClient::with_endpoint(
        vec!["gsk_key_one".to_string(), "gsk_key_two".to_string()],
        endpoint,
    )
    .expect("GroqClient builds");

    let wav = encode_pcm16_to_wav(&vec![100i16; 1600], 16000);
    let res = client.transcribe_blocking(wav).expect("429 key rotation must succeed on second key");
    assert_eq!(res.text, "rotated transcript ok");

    let seen_auths = server.join().unwrap();
    assert_eq!(seen_auths.len(), 2);
    assert_ne!(seen_auths[0], seen_auths[1], "client must rotate Authorization key after 429");
    let statuses = client.key_pool().get_statuses();
    assert_eq!(statuses.iter().filter(|s| s.is_rate_limited).count(), 1, "first key must enter cooldown");
}

#[test]
fn test_audio_safety_net_expiry_and_db_retention_and_shrink() {
    use sotto_desktop::audio::{AudioSafetyNet, RetrySegment};

    let safety = AudioSafetyNet::new();
    safety.save_segments(
        vec![
            RetrySegment::Text("Part one.".to_string()),
            RetrySegment::audio(vec![1, 2, 3, 4], 1000),
        ],
        2000,
        99,
    );
    assert!(safety.has_retryable_audio());
    assert!(!safety.evict_if_expired_after(300));
    assert!(safety.has_retryable_audio());
    std::thread::sleep(std::time::Duration::from_millis(5));
    assert!(safety.evict_if_expired_after(0));
    assert!(!safety.has_retryable_audio());

    let mut db = HistoryDb::open_in_memory().unwrap();
    db.set_max_retention(4);
    for i in 1..=6 {
        db.insert(&format!("Entry {i}"), 500, "Notepad.exe").unwrap();
    }
    assert_eq!(db.get_count().unwrap(), 4);
    let recent = db.get_recent(10).unwrap();
    assert_eq!(recent[0].transcript, "Entry 6");
    assert_eq!(recent[3].transcript, "Entry 3");

    let q = AsyncHistoryQueue::new(db);
    let (tx, rx) = std::sync::mpsc::channel();
    q.enqueue_clear_all(move || {
        let _ = tx.send(());
    });
    rx.recv_timeout(std::time::Duration::from_secs(3)).unwrap();
    assert_eq!(q.db().get_count().unwrap(), 0);
}


#[test]
fn test_long_form_chunk_safety_net_stitching_and_expiry() {
    use sotto_desktop::audio::chunker::{continuation_prompt, stitch_transcripts, PROMPT_TAIL_CHARS};
    use sotto_desktop::audio::retry::{AudioSafetyNet, RetrySegment};
    use sotto_desktop::audio::wav::{encode_pcm16_to_wav, parse_wav_header};

    let safety = AudioSafetyNet::new();
    let tail_wav = encode_pcm16_to_wav(&[1200i16; 3200], 16_000);
    assert_eq!(parse_wav_header(&tail_wav).unwrap().sample_rate, 16_000);

    // Simulate a chunked recording where chunks 0 and 2 resolved cleanly while chunk 1 failed.
    let seg0 = "We should configure the PostgreSQL cluster cleanly before deploying.";
    let seg2 = "Then run verification.";
    safety.save_segments(
        vec![
            RetrySegment::Text(seg0.to_string()),
            RetrySegment::audio(tail_wav.clone(), 200),
            RetrySegment::Text(seg2.to_string()),
        ],
        12_500,
        0x4242,
    );

    assert!(safety.has_retryable_audio());
    // Must not evict fresh payloads with non-zero TTL.
    assert!(!safety.evict_if_expired_after(300));
    let snapshot = safety.get_for_retry().expect("retry snapshot present");
    assert_eq!(snapshot.retained_audio_bytes(), tail_wav.len());

    // Continuation prompt keeps whole-word tail within PROMPT_TAIL_CHARS budget.
    let long_prefix = "word ".repeat(80);
    let prompt = continuation_prompt(&long_prefix);
    assert!(!prompt.is_empty());
    assert!(prompt.chars().count() <= PROMPT_TAIL_CHARS + 16);

    // Once the failed middle segment resolves on retry, segments stitch in chronological order.
    let recovered = stitch_transcripts([seg0, "Next validate backups", seg2]);
    assert_eq!(
        recovered,
        "We should configure the PostgreSQL cluster cleanly before deploying. Next validate backups Then run verification."
    );

    safety.clear();
    assert!(!safety.has_retryable_audio());
}

#[test]
fn test_dictionary_number_and_venue_formatting_workflow() {
    use sotto_desktop::cleaner::{
        DictionaryEngine, DictionaryEntry, NumberAndSymbolFormatter, VenueCategory, VenueFormatter,
    };

    let dict = DictionaryEngine::new(&[DictionaryEntry::new("sotto", "Sotto")]);
    let raw_dev = "git status --short.";
    assert_eq!(
        VenueFormatter::format(raw_dev, VenueCategory::DeveloperVenue),
        "git status --short"
    );

    let spoken = "Configure sotto for vs code with git hub and ten percent discount at five dollars hashtag rust.";
    let with_vocab = dict.apply(spoken);
    let normalized = NumberAndSymbolFormatter::format(&with_vocab);
    assert_eq!(
        normalized,
        "Configure Sotto for VS Code with GitHub and 10% discount at $5 #Rust."
    );

    // ChatVenue strips trailing period only on short 1-4 word acknowledgements without newlines.
    assert_eq!(VenueFormatter::format("Sounds good.", VenueCategory::ChatVenue), "Sounds good");
    assert_eq!(
        VenueFormatter::format("First line.\nSecond.", VenueCategory::ChatVenue),
        "First line.\nSecond."
    );
    assert_eq!(
        VenueFormatter::format("This has more than four words.", VenueCategory::ChatVenue),
        "This has more than four words."
    );
}
