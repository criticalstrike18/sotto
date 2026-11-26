// Copyright (c) 2026 Sotto
// Audio Safety Net & In-Memory RAM Retry Buffer (Step 2.2)

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One ordered piece of a recording that failed to resolve.
///
/// A long recording is transcribed as several chunks. If any chunk fails, the
/// chunks that succeeded are kept as text and only the unresolved chunks stay
/// as audio, so a retry can rebuild the exact original order without
/// re-uploading work that already succeeded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetrySegment {
    Text(String),
    Audio {
        /// In-memory 16 kHz mono WAV encoded bytes ready for upload.
        wav_bytes: Vec<u8>,
        duration_ms: u32,
    },
}

impl RetrySegment {
    pub fn audio(wav_bytes: Vec<u8>, duration_ms: u32) -> Self {
        Self::Audio {
            wav_bytes,
            duration_ms,
        }
    }

    pub fn duration_ms(&self) -> u32 {
        match self {
            Self::Text(_) => 0,
            Self::Audio { duration_ms, .. } => *duration_ms,
        }
    }

    pub fn audio_bytes(&self) -> usize {
        match self {
            Self::Text(_) => 0,
            Self::Audio { wav_bytes, .. } => wav_bytes.len(),
        }
    }

    pub fn audio_parts(&self) -> Option<(&[u8], u32)> {
        match self {
            Self::Text(_) => None,
            Self::Audio {
                wav_bytes,
                duration_ms,
            } => Some((wav_bytes, *duration_ms)),
        }
    }
}

/// In-memory cached audio payload retained for network failure safety net
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedAudio {
    /// Ordered text/audio segments that make up the recording.
    pub segments: Vec<RetrySegment>,
    /// Duration of the recorded speech in milliseconds
    pub duration_ms: u32,
    /// Handle of the window active when dictation began (for focus restoration)
    pub target_hwnd: isize,
    /// Instant when the audio recording was captured
    pub recorded_at: Instant,
    /// Total number of retry attempts executed for this buffer
    pub retry_count: u32,
}

impl CachedAudio {
    pub fn new(wav_bytes: Vec<u8>, duration_ms: u32, target_hwnd: isize) -> Self {
        Self::with_segments(
            vec![RetrySegment::audio(wav_bytes, duration_ms)],
            duration_ms,
            target_hwnd,
        )
    }

    pub fn with_segments(
        segments: Vec<RetrySegment>,
        duration_ms: u32,
        target_hwnd: isize,
    ) -> Self {
        Self {
            segments,
            duration_ms,
            target_hwnd,
            recorded_at: Instant::now(),
            retry_count: 0,
        }
    }

    /// True when at least one segment still needs to be uploaded.
    pub fn has_audio(&self) -> bool {
        self.segments.iter().any(|s| s.audio_parts().is_some())
    }

    /// Total encoded WAV bytes still held in RAM.
    pub fn retained_audio_bytes(&self) -> usize {
        self.segments.iter().map(RetrySegment::audio_bytes).sum()
    }

    /// Checks if the cached audio buffer has exceeded the safety retention TTL
    pub fn is_expired(&self, max_age_secs: u64) -> bool {
        self.recorded_at.elapsed() > Duration::from_secs(max_age_secs)
    }
}

/// Thread-safe in-memory cache manager for dictation safety net
#[derive(Clone, Default)]
pub struct AudioSafetyNet {
    cached: Arc<Mutex<Option<CachedAudio>>>,
}

impl AudioSafetyNet {
    pub fn new() -> Self {
        Self {
            cached: Arc::new(Mutex::new(None)),
        }
    }

    /// Stores a freshly finalized recording payload in RAM before network dispatch
    pub fn save(&self, wav_bytes: Vec<u8>, duration_ms: u32, target_hwnd: isize) {
        self.save_segments(
            vec![RetrySegment::audio(wav_bytes, duration_ms)],
            duration_ms,
            target_hwnd,
        );
    }

    /// Stores an ordered long-recording snapshot. Already-transcribed chunks
    /// should be `Text`; only unresolved chunks stay in RAM as `Audio`.
    pub fn save_segments(
        &self,
        segments: Vec<RetrySegment>,
        duration_ms: u32,
        target_hwnd: isize,
    ) {
        if let Ok(mut lock) = self.cached.lock() {
            *lock = Some(CachedAudio::with_segments(
                segments,
                duration_ms,
                target_hwnd,
            ));
        }
    }

    /// Retrieves the cached audio for retry, incrementing the retry attempt counter.
    /// Returns None if no audio is cached or if the buffer has expired (> 5 minutes).
    pub fn get_for_retry(&self) -> Option<CachedAudio> {
        if let Ok(mut lock) = self.cached.lock() {
            if let Some(ref mut audio) = *lock {
                if !audio.is_expired(300) {
                    audio.retry_count += 1;
                    return Some(audio.clone());
                }
            }
            *lock = None;
        }
        None
    }

    /// Purges the cached audio from RAM upon successful transcription or explicit dismissal
    pub fn clear(&self) {
        if let Ok(mut lock) = self.cached.lock() {
            *lock = None;
        }
    }

    /// Drops an expired retry payload (>5 minutes old) if present so stale
    /// multi-megabyte WAV buffers are reclaimed during idle maintenance.
    pub fn evict_if_expired(&self) -> bool {
        self.evict_if_expired_after(300)
    }

    pub fn evict_if_expired_after(&self, max_age_secs: u64) -> bool {
        if let Ok(mut lock) = self.cached.lock() {
            if lock.as_ref().is_some_and(|audio| audio.is_expired(max_age_secs)) {
                *lock = None;
                return true;
            }
        }
        false
    }

    /// Returns true if a valid, unexpired audio buffer is ready in RAM for retry
    pub fn has_retryable_audio(&self) -> bool {
        if let Ok(lock) = self.cached.lock() {
            if let Some(ref audio) = *lock {
                return !audio.is_expired(300) && audio.has_audio();
            }
        }
        false
    }

    /// Returns the number of retries attempted on the current cached buffer
    pub fn retry_count(&self) -> u32 {
        if let Ok(lock) = self.cached.lock() {
            if let Some(ref audio) = *lock {
                return audio.retry_count;
            }
        }
        0
    }

    /// Returns the duration in milliseconds of the cached audio, if any
    pub fn cached_duration_ms(&self) -> Option<u32> {
        if let Ok(lock) = self.cached.lock() {
            lock.as_ref().map(|a| a.duration_ms)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_safety_net_save_and_retrieve() {
        let net = AudioSafetyNet::new();
        assert!(!net.has_retryable_audio());
        assert_eq!(net.retry_count(), 0);

        let dummy_wav = vec![0x52, 0x49, 0x46, 0x46, 0x24, 0x00, 0x00, 0x00];
        net.save(dummy_wav.clone(), 1500, 0x12345);

        assert!(net.has_retryable_audio());
        assert_eq!(net.cached_duration_ms(), Some(1500));

        let retrieved = net.get_for_retry().expect("Audio must be retrievable");
        assert_eq!(
            retrieved.segments,
            vec![RetrySegment::audio(dummy_wav, 1500)]
        );
        assert_eq!(retrieved.duration_ms, 1500);
        assert_eq!(retrieved.target_hwnd, 0x12345);
        assert_eq!(retrieved.retry_count, 1);
        assert_eq!(net.retry_count(), 1);
    }

    #[test]
    fn test_safety_net_retry_counter_increments() {
        let net = AudioSafetyNet::new();
        net.save(vec![1, 2, 3], 800, 0);

        assert_eq!(net.retry_count(), 0);
        let a1 = net.get_for_retry().unwrap();
        assert_eq!(a1.retry_count, 1);
        let a2 = net.get_for_retry().unwrap();
        assert_eq!(a2.retry_count, 2);
        let a3 = net.get_for_retry().unwrap();
        assert_eq!(a3.retry_count, 3);
        assert_eq!(net.retry_count(), 3);
    }

    #[test]
    fn test_safety_net_clear_on_success() {
        let net = AudioSafetyNet::new();
        net.save(vec![10, 20, 30], 2500, 0x999);
        assert!(net.has_retryable_audio());

        net.clear();
        assert!(!net.has_retryable_audio());
        assert!(net.get_for_retry().is_none());
        assert_eq!(net.retry_count(), 0);
    }

    #[test]
    fn test_safety_net_overwrite() {
        let net = AudioSafetyNet::new();
        net.save(vec![1, 1, 1], 1000, 0x111);
        assert_eq!(net.cached_duration_ms(), Some(1000));

        // Subsequent dictation overwrites previous failed buffer
        net.save(vec![2, 2, 2, 2], 3000, 0x222);
        assert_eq!(net.cached_duration_ms(), Some(3000));
        let retrieved = net.get_for_retry().unwrap();
        assert_eq!(
            retrieved.segments,
            vec![RetrySegment::audio(vec![2, 2, 2, 2], 3000)]
        );
        assert_eq!(retrieved.target_hwnd, 0x222);
    }

    #[test]
    fn test_ordered_segments_keep_text_and_audio_positions() {
        let net = AudioSafetyNet::new();
        net.save_segments(
            vec![
                RetrySegment::Text("First chunk.".to_string()),
                RetrySegment::audio(vec![1, 2, 3], 1200),
                RetrySegment::Text("Third chunk.".to_string()),
            ],
            30_000,
            7,
        );

        assert!(net.has_retryable_audio());
        let cached = net.get_for_retry().unwrap();
        assert_eq!(cached.segments.len(), 3);
        assert_eq!(cached.segments[0], RetrySegment::Text("First chunk.".to_string()));
        assert_eq!(cached.segments[1].duration_ms(), 1200);
        assert_eq!(cached.retained_audio_bytes(), 3);
        assert_eq!(cached.duration_ms, 30_000);
    }

    #[test]
    fn test_text_only_snapshot_is_not_retryable() {
        let net = AudioSafetyNet::new();
        net.save_segments(
            vec![RetrySegment::Text("Everything already resolved.".to_string())],
            4000,
            0,
        );
        assert!(!net.has_retryable_audio());
    }

    #[test]
    fn test_safety_net_concurrency() {
        let net = AudioSafetyNet::new();
        let net_clone = net.clone();

        let handle = thread::spawn(move || {
            for i in 0..100 {
                net_clone.save(vec![i as u8], (i * 10) as u32, i as isize);
                let _ = net_clone.has_retryable_audio();
            }
        });

        for _ in 0..100 {
            let _ = net.get_for_retry();
            let _ = net.retry_count();
        }

        handle.join().unwrap();
    }
}
