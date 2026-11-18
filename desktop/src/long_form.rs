// Copyright (c) 2026 Sotto
// Long-recording session: seal upload-sized chunks while the user is still
// talking, transcribe them in order on a worker thread, and keep only the
// not-yet-uploaded tail in RAM.
//
// The recorder owns the pause-aware split planner (`audio::chunker`); this
// module owns the lifecycle: a pump thread polls the recorder, a worker thread
// transcribes sealed chunks in order with prompt continuity, and `finish`
// turns the remaining tail into the final ordered segment list.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::audio::chunker::{self, PreparedChunk};
use crate::audio::recorder::{AudioPayload, AudioRecorder};
use crate::audio::retry::RetrySegment;
use crate::router::Transcriber;

/// How often the pump asks the recorder whether a chunk can be sealed.
/// Fast enough to keep the 4 MB pause-search window useful, slow enough that
/// the recorder lock and the silence analysis stay off the audio callback.
const POLL_INTERVAL: Duration = Duration::from_millis(400);

/// A finished (or cancelled) long-form take.
pub enum LongFormFinish {
    /// No chunk was sealed; the normal single-upload pipeline should handle
    /// the returned payload exactly as before.
    Single(Option<AudioPayload>),
    /// At least one chunk was sealed. The tail has already been stopped; the
    /// caller resolves `LongFormResolution::wait()` in the background.
    Chunked(LongFormResolution),
}

/// The still-running transcription worker plus the total take duration.
pub struct LongFormResolution {
    worker: Option<JoinHandle<WorkerOutcome>>,
    total_duration_ms: u32,
}

pub struct LongFormOutcome {
    /// Final stitched transcript. Empty when `error` is set.
    pub text: String,
    pub total_duration_ms: u32,
    /// Ordered snapshot for the safety net. On success every entry is `Text`;
    /// on failure, unresolved chunks are `Audio` and completed ones `Text`.
    pub segments: Vec<RetrySegment>,
    pub error: Option<String>,
}

impl LongFormResolution {
    /// Blocks until every queued chunk (including the tail) has been resolved.
    /// Call this on a background thread, never on the UI thread.
    pub fn wait(mut self) -> LongFormOutcome {
        let outcome = match self.worker.take().map(|worker| worker.join()) {
            Some(Ok(outcome)) => outcome,
            Some(Err(_)) => WorkerOutcome {
                segments: Vec::new(),
                error: Some("Chunk transcription worker panicked".to_string()),
            },
            None => WorkerOutcome::default(),
        };

        let text = if outcome.error.is_none() {
            chunker::stitch_transcripts(outcome.segments.iter().filter_map(|segment| {
                match segment {
                    RetrySegment::Text(text) => Some(text.as_str()),
                    RetrySegment::Audio { .. } => None,
                }
            }))
        } else {
            String::new()
        };

        LongFormOutcome {
            text,
            total_duration_ms: self.total_duration_ms,
            segments: outcome.segments,
            error: outcome.error,
        }
    }
}

#[derive(Default)]
struct WorkerOutcome {
    segments: Vec<RetrySegment>,
    error: Option<String>,
}

/// Live handle stored by the dictation lifecycle. `start` is called when the
/// recorder starts; `finish` or `cancel` consumes it on stop/cancel.
pub struct LongFormSession {
    recorder: Arc<Mutex<Option<AudioRecorder>>>,
    stop_tx: Option<Sender<()>>,
    job_tx: Option<Sender<PreparedChunk>>,
    pump: Option<JoinHandle<()>>,
    worker: Option<JoinHandle<WorkerOutcome>>,
}

impl LongFormSession {
    /// Starts the pump and worker for one take. `transcriber` is a snapshot so
    /// provider/key changes in the Studio do not contend with chunk uploads.
    pub fn start(
        recorder: Arc<Mutex<Option<AudioRecorder>>>,
        transcriber: Transcriber,
    ) -> Self {
        {
            let mut guard = match recorder.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            if let Some(rec) = guard.as_mut() {
                let (soft, hard) = transcriber.chunk_limits();
                rec.set_chunk_limits(soft, hard);
            }
        }

        let (job_tx, job_rx) = mpsc::channel::<PreparedChunk>();
        let worker = thread::spawn(move || run_worker(job_rx, transcriber));

        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let pump_recorder = Arc::clone(&recorder);
        let pump_tx = job_tx.clone();
        let pump = thread::spawn(move || pump_loop(pump_recorder, pump_tx, stop_rx));

        Self {
            recorder,
            stop_tx: Some(stop_tx),
            job_tx: Some(job_tx),
            pump: Some(pump),
            worker: Some(worker),
        }
    }

    /// Stops audio capture and returns either the single payload (no chunk was
    /// sealed) or a resolution handle for the background stitcher.
    pub fn finish(mut self) -> LongFormFinish {
        self.stop_pump();

        // Seal anything the pump had not reached yet. This runs before
        // `recorder.stop()` so the remaining budget still follows the pause
        // planner instead of becoming one oversized tail.
        self.drain_sealed();

        let sealed_ms = self
            .with_recorder(|rec| rec.sealed_duration_ms())
            .unwrap_or(0);

        let stopped = self.with_recorder_mut(|rec| rec.stop());
        let tail = match stopped {
            Some(Ok(tail)) => tail,
            Some(Err(e)) => {
                eprintln!("[LongForm] Recorder stop failed: {}", e);
                None
            }
            None => None,
        };

        if sealed_ms == 0 {
            // Short take: preserve the existing single-upload path exactly.
            drop(self.job_tx.take());
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
            return LongFormFinish::Single(tail);
        }

        let tail_duration_ms = tail.as_ref().map(|p| p.duration_ms).unwrap_or(0);
        if let Some(payload) = tail {
            if let Err(e) = self.send_prepared(prepared_from_payload(payload)) {
                eprintln!("[LongForm] Failed to queue the tail chunk: {}", e);
            }
        }

        drop(self.job_tx.take());
        let worker = self.worker.take();
        LongFormFinish::Chunked(LongFormResolution {
            worker,
            total_duration_ms: sealed_ms
                .saturating_add(tail_duration_ms as u64)
                .min(u32::MAX as u64) as u32,
        })
    }

    /// Aborts without resolving anything. The recorder itself is stopped by the
    /// caller (cancel_dictation), which keeps the chime/stream ordering there.
    pub fn cancel(mut self) {
        self.stop_pump();
        // Dropping the sender lets the worker exit after any in-flight request.
        // Do not join: a network timeout must not delay the cancel cue.
        drop(self.job_tx.take());
        drop(self.worker.take());
    }

    fn stop_pump(&mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        if let Some(pump) = self.pump.take() {
            let _ = pump.join();
        }
    }

    fn drain_sealed(&self) {
        loop {
            let polled = {
                let mut guard = match self.recorder.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
                match guard.as_mut() {
                    Some(rec) => rec.poll_chunk(),
                    None => return,
                }
            };

            match polled {
                Ok(Some(sealed)) => {
                    if let Some(prepared) = chunker::prepare_chunk(sealed.pcm16k) {
                        if let Err(e) = self.send_prepared(prepared) {
                            eprintln!("[LongForm] Failed to queue a sealed chunk: {}", e);
                            return;
                        }
                    }
                }
                Ok(None) => return,
                Err(e) => {
                    eprintln!("[LongForm] Final chunk poll failed: {}", e);
                    return;
                }
            }
        }
    }

    fn send_prepared(&self, prepared: PreparedChunk) -> Result<(), String> {
        let tx = self
            .job_tx
            .as_ref()
            .ok_or_else(|| "Chunk worker already closed".to_string())?;
        tx.send(prepared)
            .map_err(|_| "Chunk worker stopped unexpectedly".to_string())
    }

    fn with_recorder<T>(&self, f: impl FnOnce(&AudioRecorder) -> T) -> Option<T> {
        let guard = match self.recorder.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.as_ref().map(f)
    }

    fn with_recorder_mut<T>(&self, f: impl FnOnce(&mut AudioRecorder) -> T) -> Option<T> {
        let mut guard = match self.recorder.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.as_mut().map(f)
    }
}

fn pump_loop(
    recorder: Arc<Mutex<Option<AudioRecorder>>>,
    job_tx: Sender<PreparedChunk>,
    stop_rx: Receiver<()>,
) {
    loop {
        match stop_rx.recv_timeout(POLL_INTERVAL) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }

        let polled = {
            let mut guard = match recorder.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            match guard.as_mut() {
                Some(rec) => rec.poll_chunk(),
                None => break,
            }
        };

        match polled {
            Ok(Some(sealed)) => {
                // Filtering/AGC/WAV encoding happens outside the recorder lock
                // so an audio callback never waits for this pass.
                if let Some(prepared) = chunker::prepare_chunk(sealed.pcm16k) {
                    println!(
                        "[LongForm] Sealed chunk queued: {}ms ({} bytes WAV)",
                        prepared.duration_ms,
                        prepared.wav_bytes.len()
                    );
                    if job_tx.send(prepared).is_err() {
                        break;
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("[LongForm] Chunk poll failed: {}", e);
                break;
            }
        }
    }
}

fn run_worker(rx: Receiver<PreparedChunk>, transcriber: Transcriber) -> WorkerOutcome {
    let mut outcome = WorkerOutcome::default();

    for job in rx {
        if outcome.error.is_some() {
            // Preserve order for the retry snapshot: this and every later chunk
            // stay as audio so a retry can insert their text in the right place.
            outcome
                .segments
                .push(RetrySegment::audio(job.wav_bytes, job.duration_ms));
            continue;
        }

        let prompt = {
            let resolved = chunker::stitch_transcripts(outcome.segments.iter().filter_map(
                |segment| match segment {
                    RetrySegment::Text(text) => Some(text.as_str()),
                    RetrySegment::Audio { .. } => None,
                },
            ));
            chunker::continuation_prompt(&resolved)
        };
        let prompt_opt = (!prompt.is_empty()).then_some(prompt.as_str());

        // Clone only for the call: on failure the original WAV is retained in
        // the retry snapshot, and on success the original is dropped. A panic
        // inside the provider client must not take down the whole take: treat
        // it like a failed request and keep every earlier segment.
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            transcriber.transcribe_blocking_with_prompt(job.wav_bytes.clone(), prompt_opt)
        }));
        let result = match attempt {
            Ok(result) => result,
            Err(_) => Err("Chunk transcription panicked".to_string()),
        };
        match result {
            Ok(result) => {
                println!(
                    "[LongForm] Chunk transcribed in {}ms ({} chars): \"{}\"",
                    result.latency_ms,
                    result.text.chars().count(),
                    truncate_for_log(&result.text, 120)
                );
                outcome.segments.push(RetrySegment::Text(result.text));
            }
            Err(e) => {
                eprintln!("[LongForm] Chunk transcription failed: {}", e);
                outcome.error = Some(e);
                outcome
                    .segments
                    .push(RetrySegment::audio(job.wav_bytes, job.duration_ms));
            }
        }
    }

    outcome
}

fn prepared_from_payload(payload: AudioPayload) -> PreparedChunk {
    PreparedChunk {
        wav_bytes: payload.wav_bytes,
        duration_ms: payload.duration_ms,
        rms: payload.rms,
        peak: payload.peak,
        digital_gain: payload.digital_gain,
    }
}

fn truncate_for_log(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_snapshot_preserves_text_audio_order() {
        let segments = vec![
            RetrySegment::Text("one".to_string()),
            RetrySegment::audio(vec![0; 8], 1000),
            RetrySegment::Text("three".to_string()),
        ];
        let stitched = chunker::stitch_transcripts(segments.iter().filter_map(|s| match s {
            RetrySegment::Text(t) => Some(t.as_str()),
            RetrySegment::Audio { .. } => None,
        }));
        assert_eq!(stitched, "one three");
    }

    #[test]
    fn prepared_from_payload_keeps_upload_fields() {
        let payload = AudioPayload {
            wav_bytes: vec![1, 2, 3],
            pcm_samples: vec![10, 20],
            duration_ms: 42,
            rms: 0.1,
            peak: 0.2,
            digital_gain: 1.5,
        };
        let prepared = prepared_from_payload(payload);
        assert_eq!(prepared.wav_bytes, vec![1, 2, 3]);
        assert_eq!(prepared.duration_ms, 42);
        assert_eq!(prepared.digital_gain, 1.5);
    }
}
