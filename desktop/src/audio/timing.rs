// Copyright (c) 2026 Sotto
// Per-take capture timing, persisted as JSONL for before/after comparisons.
//
// The point of this module is to make latency claims falsifiable. Every take
// records when each source actually produced its first sample and first real
// speech, how long the splice took, and how long finalization took. The
// `--capture-bench` command prints the same numbers live.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

const NONE_MS: u64 = u64::MAX;

/// Shared clock for one capture source. All timestamps are relative to the
/// trigger instant so two sources can be compared directly.
#[derive(Debug)]
pub struct SourceClock {
    label: &'static str,
    trigger: Mutex<Option<Instant>>,
    first_callback_ms: AtomicU64,
    first_speech_ms: AtomicU64,
}

impl SourceClock {
    pub fn new(label: &'static str) -> Self {
        Self {
            label,
            trigger: Mutex::new(None),
            first_callback_ms: AtomicU64::new(NONE_MS),
            first_speech_ms: AtomicU64::new(NONE_MS),
        }
    }

    pub fn label(&self) -> &'static str {
        self.label
    }

    /// Arms both this clock and (by passing the same instant) its sibling.
    pub fn arm_at(&self, trigger: Instant) {
        if let Ok(mut guard) = self.trigger.lock() {
            *guard = Some(trigger);
        }
        self.first_callback_ms.store(NONE_MS, Ordering::SeqCst);
        self.first_speech_ms.store(NONE_MS, Ordering::SeqCst);
    }

    pub fn elapsed_ms(&self) -> Option<u64> {
        self.trigger
            .lock()
            .ok()
            .and_then(|guard| *guard)
            .map(|t| t.elapsed().as_millis() as u64)
    }

    pub fn record_callback(&self) {
        let Some(ms) = self.elapsed_ms() else {
            return;
        };
        let _ = self.first_callback_ms.compare_exchange(
            NONE_MS,
            ms,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    pub fn record_speech(&self) {
        let Some(ms) = self.elapsed_ms() else {
            return;
        };
        let _ = self.first_speech_ms.compare_exchange(
            NONE_MS,
            ms,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    pub fn first_callback_ms(&self) -> Option<u64> {
        let v = self.first_callback_ms.load(Ordering::SeqCst);
        (v != NONE_MS).then_some(v)
    }

    pub fn first_speech_ms(&self) -> Option<u64> {
        let v = self.first_speech_ms.load(Ordering::SeqCst);
        (v != NONE_MS).then_some(v)
    }
}

/// One take's measurements. Optional fields are absent when the mode did not
/// use that source (for example, `OnboardOnly` never opens a headset stream).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CaptureTiming {
    pub mode: String,
    pub take: u64,
    /// Headset / primary stream opened on trigger (does not exist for OnboardOnly).
    pub trigger_to_primary_play_ms: Option<u64>,
    pub trigger_to_primary_first_sample_ms: Option<u64>,
    pub trigger_to_primary_first_speech_ms: Option<u64>,
    /// Onboard bridge.
    pub onboard_lead_ms: u64,
    pub onboard_duration_ms: u64,
    pub onboard_first_speech_ms: Option<u64>,
    /// Headset take length actually captured.
    pub headset_duration_ms: u64,
    /// Time from trigger to the first real headset speech. This is exactly the
    /// window the onboard bridge has to cover.
    pub handshake_ms: Option<u64>,
    pub measured_lag_ms: Option<f32>,
    pub lag_confidence: Option<f32>,
    pub splice_processing_ms: u64,
    /// Time from `stop()` entry to a finalized payload.
    pub finalize_ms: u64,
    pub output_duration_ms: u64,
    /// First non-silent position in the finalized take.
    pub first_speech_offset_ms: Option<u64>,
    /// Same position expressed relative to the trigger. Negative means the
    /// take contains pre-roll before the keypress; ~0 means the first syllable
    /// was captured, while a large positive value is the lost-speech window.
    pub first_speech_after_trigger_ms: Option<i64>,
    pub fallback: Option<String>,
}

impl CaptureTiming {
    pub fn log_line(&self) -> String {
        let handshake = self
            .handshake_ms
            .map(|v| format!("{}ms", v))
            .unwrap_or_else(|| "n/a".to_string());
        let lag = self
            .measured_lag_ms
            .map(|v| format!("{:.1}ms", v))
            .unwrap_or_else(|| "n/a".to_string());
        let confidence = self
            .lag_confidence
            .map(|v| format!("{:.3}", v))
            .unwrap_or_else(|| "n/a".to_string());
        let first_speech = self
            .first_speech_offset_ms
            .map(|v| format!("{}ms", v))
            .unwrap_or_else(|| "n/a".to_string());
        let after_trigger = self
            .first_speech_after_trigger_ms
            .map(|v| format!("{}ms", v))
            .unwrap_or_else(|| "n/a".to_string());

        format!(
            "[Capture Timing] mode={} take={} onboard_lead={}ms onboard={}ms headset={}ms handshake={} lag={} conf={} splice={}ms finalize={}ms out={}ms first_speech={} after_trigger={} fallback={}",
            self.mode,
            self.take,
            self.onboard_lead_ms,
            self.onboard_duration_ms,
            self.headset_duration_ms,
            handshake,
            lag,
            confidence,
            self.splice_processing_ms,
            self.finalize_ms,
            self.output_duration_ms,
            first_speech,
            after_trigger,
            self.fallback.as_deref().unwrap_or("none"),
        )
    }
}

pub fn timing_log_path() -> PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
    let dir = Path::new(&appdata).join("Sotto");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("capture_timing.jsonl")
}

pub fn append_capture_timing(timing: &CaptureTiming) {
    let _ = append_capture_timing_to(&timing_log_path(), timing);
}

/// Appends one JSON object per line. Failures are intentionally non-fatal:
/// timing telemetry must never break dictation.
pub fn append_capture_timing_to(path: &Path, timing: &CaptureTiming) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let line = serde_json::to_string(timing).map_err(|e| e.to_string())?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    writeln!(file, "{}", line).map_err(|e| e.to_string())
}

pub fn load_capture_timings(path: &Path) -> Vec<CaptureTiming> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<CaptureTiming>(line).ok())
        .collect()
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TimingSummary {
    pub takes: usize,
    pub avg_handshake_ms: Option<f64>,
    pub avg_splice_ms: Option<f64>,
    pub avg_finalize_ms: Option<f64>,
    pub avg_onboard_coverage_ms: Option<f64>,
    pub onboard_only_fallbacks: usize,
}

pub fn summarize(timings: &[CaptureTiming]) -> TimingSummary {
    fn avg<I: Iterator<Item = u64>>(values: I) -> Option<f64> {
        let mut sum = 0u64;
        let mut n = 0u64;
        for v in values {
            sum += v;
            n += 1;
        }
        (n > 0).then_some(sum as f64 / n as f64)
    }

    TimingSummary {
        takes: timings.len(),
        avg_handshake_ms: avg(timings.iter().filter_map(|t| t.handshake_ms)),
        avg_splice_ms: avg(
            timings
                .iter()
                .filter(|t| t.splice_processing_ms > 0)
                .map(|t| t.splice_processing_ms),
        ),
        avg_finalize_ms: avg(timings.iter().map(|t| t.finalize_ms)),
        avg_onboard_coverage_ms: avg(
            timings
                .iter()
                .filter(|t| t.onboard_duration_ms > 0)
                .map(|t| t.onboard_duration_ms),
        ),
        onboard_only_fallbacks: timings
            .iter()
            .filter(|t| {
                t.fallback
                    .as_deref()
                    .map(|f| f.contains("NoHeadsetSpeech") || f.contains("NoOverlap"))
                    .unwrap_or(false)
            })
            .count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn clock_records_relative_timestamps_once() {
        let clock = SourceClock::new("test");
        clock.arm_at(Instant::now());
        std::thread::sleep(Duration::from_millis(5));
        clock.record_callback();
        std::thread::sleep(Duration::from_millis(5));
        clock.record_callback();
        clock.record_speech();

        let first = clock.first_callback_ms().expect("callback");
        assert!(first >= 4 && first < 100, "unexpected first callback: {}", first);
        assert_eq!(clock.first_callback_ms(), Some(first));
        assert!(clock.first_speech_ms().expect("speech") >= first);
    }

    #[test]
    fn timing_round_trips_and_appends_as_jsonl() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "sotto-timing-{}-{}",
            std::process::id(),
            unique
        ));
        let path = dir.join("capture_timing.jsonl");
        let timing = CaptureTiming {
            mode: "dual_mic_splice".to_string(),
            take: 1,
            handshake_ms: Some(242),
            measured_lag_ms: Some(238.5),
            lag_confidence: Some(0.81),
            splice_processing_ms: 3,
            finalize_ms: 9,
            output_duration_ms: 2400,
            onboard_lead_ms: 120,
            ..Default::default()
        };

        append_capture_timing_to(&path, &timing).unwrap();
        append_capture_timing_to(&path, &timing).unwrap();
        let loaded = load_capture_timings(&path);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0], timing);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summary_reports_averages_and_fallbacks() {
        let timings = vec![
            CaptureTiming {
                handshake_ms: Some(200),
                splice_processing_ms: 2,
                finalize_ms: 8,
                onboard_duration_ms: 1200,
                ..Default::default()
            },
            CaptureTiming {
                handshake_ms: Some(300),
                splice_processing_ms: 4,
                finalize_ms: 10,
                onboard_duration_ms: 800,
                fallback: Some("NoHeadsetSpeech".to_string()),
                ..Default::default()
            },
        ];
        let summary = summarize(&timings);
        assert_eq!(summary.takes, 2);
        assert_eq!(summary.avg_handshake_ms, Some(250.0));
        assert_eq!(summary.avg_splice_ms, Some(3.0));
        assert_eq!(summary.avg_finalize_ms, Some(9.0));
        assert_eq!(summary.avg_onboard_coverage_ms, Some(1000.0));
        assert_eq!(summary.onboard_only_fallbacks, 1);
    }
}
