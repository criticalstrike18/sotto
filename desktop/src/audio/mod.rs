// Copyright (c) 2026 Sotto
// In-Memory RAM Audio Pipeline Module

pub mod chime;
pub mod chunker;
pub mod filter;
pub mod recorder;
pub mod resample;
pub mod retry;
pub mod splice;
pub mod timing;
pub mod wav;

pub use filter::{apply_digital_gain, compute_peak, compute_rms, trim_trailing_silence, validate_audio, AudioValidation};
pub use chunker::{
    plan_chunk_split, prepare_chunk, samples_for_wav_bytes, stitch_transcripts,
    wav_bytes_for_samples, PreparedChunk, SplitPlan, SplitReason, HARD_REQUEST_BYTES,
    SOFT_REQUEST_BYTES,
};
pub use recorder::{AudioPayload, AudioRecorder, SealedAudioChunk};
pub use resample::{downmix_interleaved_to_mono, float_to_pcm16, resample_linear, TARGET_SAMPLE_RATE};
pub use retry::{AudioSafetyNet, CachedAudio, RetrySegment};
pub use splice::{splice_takes, SpliceConfig, SpliceFallback, SpliceOutcome};
pub use timing::{CaptureTiming, SourceClock, TimingSummary};
pub use wav::{encode_pcm16_to_wav, parse_wav_header, WavHeaderInfo};
