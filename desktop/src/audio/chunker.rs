// Copyright (c) 2026 Sotto
// Pause-aware long-recording chunk planner.
//
// The provider upload limit is a hard ~25 MB per request. A 16 kHz mono PCM16
// WAV reaches that budget after roughly 13.6 minutes, so a recording that is
// allowed to run longer must be cut into uploadable chunks. Cutting on a byte
// boundary clips a syllable in half and makes both Whisper halves hallucinate.
//
// This module is deliberately pure: it only inspects already-normalized 16 kHz
// mono PCM16 samples. It finds the last real pause before the byte budget so the
// recorder can seal a chunk while the user keeps talking, and it provides the
// small text helpers that keep the stitched transcript seamless.

use crate::audio::filter::{
    apply_digital_gain, validate_audio, AudioValidation, BiquadHighPass, MIN_RECORDING_MS,
    SILENCE_RMS_THRESHOLD,
};
use crate::audio::resample::{float_to_pcm16, TARGET_SAMPLE_RATE};
use crate::audio::wav::encode_pcm16_to_wav;

/// Standard 44-byte PCM WAV header.
pub const WAV_HEADER_BYTES: usize = 44;

/// Never build a request larger than this. Providers reject files at/above
/// 25 MB; 24,000,000 bytes leaves room for multipart headers, the prompt, and
/// the model field without assuming the provider means MiB rather than MB.
pub const HARD_REQUEST_BYTES: usize = 24_000_000;

/// Start looking for a pause at this size. The gap between soft and hard is the
/// "safe search window": a normal speaker will pause at least once inside it,
/// so we almost never have to cut through a word at the hard limit.
pub const SOFT_REQUEST_BYTES: usize = 20_000_000;

/// A pause must be at least this long to be a deliberate chunk boundary.
pub const MIN_PAUSE_MS: u32 = 900;

/// If the buffer has already reached the hard limit and no long pause exists,
/// accept a shorter breath before falling back to a hard byte cut.
pub const RELAXED_PAUSE_MS: u32 = 400;

/// Do not create tiny chunks; a sealed chunk is worth a network round-trip.
pub const MIN_CHUNK_MS: u32 = 30_000;

/// Energy analysis frame. 20 ms matches the existing VAD/splice code.
pub const FRAME_MS: u32 = 20;

/// How much previous transcript to feed back as the next request's prompt.
/// Whisper's prompt is limited to roughly 224 tokens; 200 characters of the
/// tail is comfortably inside that budget and preserves tone and spelling.
pub const PROMPT_TAIL_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitReason {
    /// Split at the end of a detected pause.
    Pause,
    /// No acceptable pause was found before the hard byte ceiling.
    HardLimit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitPlan {
    /// Number of 16 kHz samples that belong to the chunk being sealed.
    pub sample_index: usize,
    pub reason: SplitReason,
    /// Measured silence length for a pause split; zero for a hard cut.
    pub pause_ms: u32,
}

/// One normalized chunk ready for upload. Only the encoded WAV is retained:
/// the PCM copy would double the queue's RAM footprint for no benefit.
#[derive(Debug, Clone)]
pub struct PreparedChunk {
    pub wav_bytes: Vec<u8>,
    pub duration_ms: u32,
    pub rms: f32,
    pub peak: f32,
    pub digital_gain: f32,
}

pub fn wav_bytes_for_samples(samples: usize) -> usize {
    WAV_HEADER_BYTES + samples.saturating_mul(2)
}

pub fn samples_for_wav_bytes(bytes: usize) -> usize {
    bytes.saturating_sub(WAV_HEADER_BYTES) / 2
}

/// Chooses whether `samples` should be sealed into a chunk right now.
///
/// `soft_bytes` is when we start looking, `hard_bytes` is the upload ceiling.
/// The returned index always points at the end of a silence run (or at the
/// hard ceiling when the speaker truly never paused).
pub fn plan_chunk_split(
    samples: &[i16],
    soft_bytes: usize,
    hard_bytes: usize,
    min_pause_ms: u32,
    relaxed_pause_ms: u32,
    min_chunk_ms: u32,
) -> Option<SplitPlan> {
    if samples.is_empty() {
        return None;
    }

    let hard_samples = samples_for_wav_bytes(hard_bytes);
    let soft_samples = samples_for_wav_bytes(soft_bytes);
    if samples.len() < soft_samples {
        return None;
    }

    let min_chunk_samples =
        ((min_chunk_ms as u64 * TARGET_SAMPLE_RATE as u64) / 1000) as usize;

    // First choice: the latest deliberate pause between the soft threshold and
    // the hard ceiling. That is the "send up to the last visible long pause"
    // rule from the design discussion.
    if let Some(pause) = find_last_pause(
        samples,
        hard_samples.min(samples.len()),
        min_chunk_samples,
        min_pause_ms,
    ) {
        // A pause from before the soft budget is not useful: sealing there
        // would create a small chunk and leave the budget almost untouched.
        if pause.sample_index >= soft_samples {
            return Some(pause);
        }
    }

    if samples.len() < hard_samples {
        return None;
    }

    // The buffer is already at the upload ceiling. Try a shorter breath before
    // resorting to a hard cut; this is still much better than slicing a word.
    if let Some(pause) = find_last_pause(
        samples,
        hard_samples,
        min_chunk_samples,
        relaxed_pause_ms,
    ) {
        if pause.sample_index >= soft_samples {
            return Some(pause);
        }
    }

    Some(SplitPlan {
        sample_index: hard_samples.max(1),
        reason: SplitReason::HardLimit,
        pause_ms: 0,
    })
}

/// Finds the latest silence run that ends at or before `search_end` and lasts
/// at least `min_pause_ms`.
fn find_last_pause(
    samples: &[i16],
    search_end: usize,
    min_chunk_samples: usize,
    min_pause_ms: u32,
) -> Option<SplitPlan> {
    let frame_len = ((TARGET_SAMPLE_RATE as u64 * FRAME_MS as u64) / 1000) as usize;
    if frame_len == 0 || samples.len() < frame_len {
        return None;
    }

    let search_end = search_end.min(samples.len());
    let frame_count = search_end / frame_len;
    if frame_count == 0 {
        return None;
    }

    // A frame is "silent" when it sits close to the room's noise floor. Using
    // a percentile keeps the detector working in quiet rooms and in noisy ones
    // without a user-facing sensitivity setting.
    let mut energies = Vec::with_capacity(frame_count);
    for idx in 0..frame_count {
        let start = idx * frame_len;
        energies.push(frame_rms(&samples[start..start + frame_len]));
    }
    // Percentile selection reorders its input, so score a copy and keep the
    // time-ordered energies intact for the pause walk below.
    let mut sorted = energies.clone();
    let noise_floor = percentile(&mut sorted, 15).unwrap_or(0.0);
    let threshold = (noise_floor * 1.8).clamp(0.0012, 0.0060);

    let min_pause_samples = ((min_pause_ms as u64 * TARGET_SAMPLE_RATE as u64) / 1000) as usize;
    let mut best: Option<SplitPlan> = None;
    let mut run_start: Option<usize> = None;

    for (idx, &rms) in energies.iter().enumerate() {
        let start = idx * frame_len;
        let end = start + frame_len;

        if rms <= threshold {
            if run_start.is_none() {
                run_start = Some(start);
            }
        } else if let Some(run_begin) = run_start.take() {
            consider_run(
                run_begin,
                start,
                min_pause_samples,
                min_chunk_samples,
                search_end,
                &mut best,
            );
        }

        if end >= search_end {
            break;
        }
    }

    if let Some(run_begin) = run_start {
        consider_run(
            run_begin,
            search_end.min(frame_count * frame_len),
            min_pause_samples,
            min_chunk_samples,
            search_end,
            &mut best,
        );
    }

    best
}

fn consider_run(
    start: usize,
    end: usize,
    min_pause_samples: usize,
    min_chunk_samples: usize,
    search_end: usize,
    best: &mut Option<SplitPlan>,
) {
    if end < start || end > search_end {
        return;
    }
    let pause_samples = end - start;
    if pause_samples < min_pause_samples || end < min_chunk_samples {
        return;
    }
    // The latest qualifying run wins; it is closest to the budget ceiling.
    if best.map(|b| end > b.sample_index).unwrap_or(true) {
        *best = Some(SplitPlan {
            sample_index: end,
            reason: SplitReason::Pause,
            pause_ms: (pause_samples as u64 * 1000 / TARGET_SAMPLE_RATE as u64) as u32,
        });
    }
}

fn frame_rms(frame: &[i16]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    let sum: f64 = frame
        .iter()
        .map(|&s| {
            let x = s as f64 / 32768.0;
            x * x
        })
        .sum();
    ((sum / frame.len() as f64).sqrt() as f32).min(1.0)
}

fn percentile(values: &mut [f32], pct: usize) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    let pct = pct.min(100);
    let index = ((values.len() - 1) * pct) / 100;
    let (_, &mut value, _) = values.select_nth_unstable_by(index, |a, b| {
        a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
    });
    Some(value)
}

/// Applies the same 80 Hz rumble filter and speech-aware AGC used by the normal
/// finalizer, then validates the chunk. Returns `None` for silence/ghost taps.
pub fn prepare_chunk(pcm16: Vec<i16>) -> Option<PreparedChunk> {
    if pcm16.is_empty() {
        return None;
    }

    let mut mono: Vec<f32> = pcm16
        .iter()
        .map(|&s| s as f32 / 32768.0)
        .collect();
    let mut hp = BiquadHighPass::new(80.0, TARGET_SAMPLE_RATE as f32);
    hp.process_slice(&mut mono);
    let mut pcm16 = float_to_pcm16(&mono);
    match validate_audio(
        &pcm16,
        TARGET_SAMPLE_RATE,
        MIN_RECORDING_MS,
        SILENCE_RMS_THRESHOLD,
    ) {
        AudioValidation::Valid {
            duration_ms,
            rms,
            peak,
        } => {
            let digital_gain = apply_digital_gain(&mut pcm16, 0.72, 16.0);
            let wav_bytes = encode_pcm16_to_wav(&pcm16, TARGET_SAMPLE_RATE);
            Some(PreparedChunk {
                wav_bytes,
                duration_ms,
                rms,
                peak,
                digital_gain,
            })
        }
        AudioValidation::DiscardGhostTap { duration_ms } => {
            println!(
                "[Chunker] Dropped short chunk: {}ms (minimum: {}ms)",
                duration_ms, MIN_RECORDING_MS
            );
            None
        }
        AudioValidation::DiscardSilence { rms } => {
            println!(
                "[Chunker] Dropped silent chunk: RMS {:.5} (threshold: {:.5})",
                rms, SILENCE_RMS_THRESHOLD
            );
            None
        }
    }
}

/// Returns the tail of `text` suitable for the next Whisper prompt. Keeps
/// whole words and never splits a UTF-8 code point.
pub fn continuation_prompt(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }

    let mut count = 0usize;
    let mut start = 0usize;
    for (idx, _) in text.char_indices().rev() {
        count += 1;
        if count > PROMPT_TAIL_CHARS {
            start = idx;
            break;
        }
    }

    if start > 0 {
        while let Some((idx, ch)) = text[start..].char_indices().next() {
            if ch.is_whitespace() {
                start += idx + ch.len_utf8();
                break;
            }
            start += ch.len_utf8();
        }
    }

    text[start..].trim().to_string()
}

/// Joins chunk transcripts without introducing double spaces, missing spaces,
/// or a space before terminal punctuation.
pub fn stitch_transcripts<'a, I>(parts: I) -> String
where
    I: IntoIterator<Item = &'a str>,
{
    let mut out = String::new();
    for part in parts {
        append_fragment(&mut out, part);
    }
    out
}

fn append_fragment(out: &mut String, fragment: &str) {
    let fragment = fragment.trim();
    if fragment.is_empty() {
        return;
    }
    if out.is_empty() {
        out.push_str(fragment);
        return;
    }

    let last = out.chars().last().unwrap_or(' ');
    let first = fragment.chars().next().unwrap_or(' ');
    let closes_word = last.is_whitespace() || last == '-';
    let leading_punctuation = matches!(first, ',' | '.' | ';' | ':' | '!' | '?' | ')' | ']' | '}');

    if !closes_word && !leading_punctuation {
        out.push(' ');
    }
    out.push_str(fragment);
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: usize = TARGET_SAMPLE_RATE as usize;

    fn speech(seconds: f32) -> Vec<i16> {
        let len = (SR as f32 * seconds) as usize;
        (0..len)
            .map(|i| {
                let t = i as f32 / SR as f32;
                ((t * 220.0 * 2.0 * std::f32::consts::PI).sin() * 9000.0) as i16
            })
            .collect()
    }

    fn silence(seconds: f32) -> Vec<i16> {
        vec![0; (SR as f32 * seconds) as usize]
    }

    fn limits(soft_secs: f32, hard_secs: f32) -> (usize, usize) {
        (
            wav_bytes_for_samples((SR as f32 * soft_secs) as usize),
            wav_bytes_for_samples((SR as f32 * hard_secs) as usize),
        )
    }

    #[test]
    fn waits_below_soft_threshold() {
        let (soft, hard) = limits(3.0, 5.0);
        let audio = speech(2.0);
        assert!(plan_chunk_split(&audio, soft, hard, 300, 150, 1000).is_none());
    }

    #[test]
    fn picks_latest_long_pause_before_the_budget() {
        let (soft, hard) = limits(3.0, 5.0);
        let mut audio = speech(2.0);
        audio.extend(silence(0.4));
        audio.extend(speech(2.0));
        audio.extend(silence(0.5));
        audio.extend(speech(1.0));

        let plan = plan_chunk_split(&audio, soft, hard, 300, 150, 1000).expect("pause split");
        assert_eq!(plan.reason, SplitReason::Pause);
        let expected = ((2.0 + 0.4 + 2.0 + 0.5) * SR as f32) as usize;
        assert!(
            (plan.sample_index as i64 - expected as i64).abs() <= 64,
            "split {} expected ~{}",
            plan.sample_index,
            expected
        );
        assert!(plan.pause_ms >= 300);
    }

    #[test]
    fn ignores_pauses_before_the_soft_threshold() {
        let (soft, hard) = limits(4.0, 8.0);
        let mut audio = speech(1.0);
        audio.extend(silence(1.0));
        audio.extend(speech(5.0)); // no pause after the soft threshold
        let plan = plan_chunk_split(&audio, soft, hard, 300, 150, 1000);
        assert!(plan.is_none(), "must wait, not seal an early pause");
    }

    #[test]
    fn hard_cuts_when_no_pause_exists_at_the_ceiling() {
        let (soft, hard) = limits(2.0, 3.0);
        let audio = speech(4.0);
        let plan = plan_chunk_split(&audio, soft, hard, 300, 150, 1000).expect("hard split");
        assert_eq!(plan.reason, SplitReason::HardLimit);
        assert_eq!(plan.sample_index, samples_for_wav_bytes(hard));
    }

    #[test]
    fn relaxed_pause_saves_a_word_at_the_ceiling() {
        let (soft, hard) = limits(2.0, 3.0);
        let mut audio = speech(2.5);
        audio.extend(silence(0.5)); // shorter than the 900 ms primary gate
        audio.extend(speech(1.0));
        let plan = plan_chunk_split(&audio, soft, hard, 900, 250, 1000).expect("relaxed pause");
        assert_eq!(plan.reason, SplitReason::Pause);
        assert!(plan.pause_ms >= 400);
    }

    #[test]
    fn continuation_prompt_keeps_whole_words_and_a_short_tail() {
        let text = "The deployment finished successfully. We should now verify the staging database before the release.";
        let prompt = continuation_prompt(text);
        assert!(prompt.chars().count() <= PROMPT_TAIL_CHARS);
        assert!(text.ends_with(&prompt));
        assert!(prompt.starts_with(char::is_uppercase) || prompt.starts_with("we"));
    }

    #[test]
    fn stitch_adds_spaces_and_preserves_punctuation() {
        let text = stitch_transcripts(["First sentence.", " Second sentence.", "third, part"]);
        assert_eq!(text, "First sentence. Second sentence. third, part");

        let punctuation = stitch_transcripts(["He said", ", quietly."]);
        assert_eq!(punctuation, "He said, quietly.");
    }

    #[test]
    fn prepare_chunk_drops_pure_silence() {
        assert!(prepare_chunk(silence(2.0)).is_none());
    }

    #[test]
    fn prepare_chunk_returns_a_valid_wav() {
        let chunk = prepare_chunk(speech(2.0)).expect("speech chunk");
        assert!(chunk.wav_bytes.len() > WAV_HEADER_BYTES);
        assert_eq!(chunk.duration_ms, 2000);
        let header = crate::audio::wav::parse_wav_header(&chunk.wav_bytes).expect("wav header");
        assert_eq!(header.sample_rate, TARGET_SAMPLE_RATE);
        assert_eq!(header.num_channels, 1);
    }
}
