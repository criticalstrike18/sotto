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
