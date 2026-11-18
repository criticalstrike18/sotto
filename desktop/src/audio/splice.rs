// Copyright (c) 2026 Sotto
// Dual-microphone alignment and splice (16 kHz mono).
//
// The bridge records the onboard mic from (or before) the trigger while the
// Bluetooth/HFP stream negotiates. Once both takes contain the same speech we
// measure the true offset with normalized cross-correlation, then hand off
// from one source to the other at a low-energy point with a short crossfade.
//
// Nothing here touches hardware, so the whole decision can be unit-tested with
// synthetic takes. That is deliberate: it is the part that must be proven to
// preserve the timeline and not add seconds to a take.

pub const SPLICE_SAMPLE_RATE: u32 = 16_000;

/// Default correlation search half-width around the wall-clock prediction.
pub const DEFAULT_SEARCH_MS: u32 = 320;
/// Crossfade length at the hand-off.
pub const DEFAULT_CROSSFADE_MS: u32 = 50;
/// Below this normalized correlation peak we still splice at the predicted
/// offset, but report low confidence so the caller can log/fallback.
pub const DEFAULT_MIN_CONFIDENCE: f32 = 0.25;
/// RMS gate used to find the first real headset frame (not zeros / codec warmup).
pub const DEFAULT_SPEECH_RMS: f32 = 0.0015;

#[derive(Debug, Clone, Copy)]
pub struct SpliceConfig {
    pub sample_rate: u32,
    pub search_ms: u32,
    pub crossfade_ms: u32,
    pub min_confidence: f32,
    pub speech_rms: f32,
}

impl Default for SpliceConfig {
    fn default() -> Self {
        Self {
            sample_rate: SPLICE_SAMPLE_RATE,
            search_ms: DEFAULT_SEARCH_MS,
            crossfade_ms: DEFAULT_CROSSFADE_MS,
            min_confidence: DEFAULT_MIN_CONFIDENCE,
            speech_rms: DEFAULT_SPEECH_RMS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LagEstimate {
    /// `onboard_index = bt_index + lag_samples`.
    pub lag_samples: i32,
    /// Normalized cross-correlation at the peak, in [-1.0, 1.0].
    pub confidence: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpliceFallback {
    /// The headset never produced speech; the take is onboard-only.
    NoHeadsetSpeech,
    /// There was not enough shared audio to align safely.
    NoOverlap,
    /// Correlation ran but was weak; the wall-clock prior was used.
    LowConfidence,
}

#[derive(Debug, Clone)]
pub struct SpliceOutcome {
    pub samples: Vec<f32>,
    pub lag_samples: Option<i32>,
    pub confidence: f32,
    /// Index in `samples` where the onboard segment ends.
    pub splice_point_samples: usize,
    pub crossfade_samples: usize,
    pub fallback: Option<SpliceFallback>,
}

#[inline]
pub fn frame_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    ((sum_sq / samples.len() as f64).sqrt() as f32).min(1.0)
}

#[inline]
fn ms_to_samples(ms: u32, sample_rate: u32) -> usize {
    ((ms as u64 * sample_rate as u64) / 1000) as usize
}

/// Finds the first frame that stays above the speech gate for at least
/// `hold_ms`, returning the start of that run. Leading codec zeros and clicks
/// shorter than the hold window are ignored.
pub fn first_speech_index(
    samples: &[f32],
    sample_rate: u32,
    threshold: f32,
    hold_ms: u32,
) -> Option<usize> {
    let frame = ms_to_samples(20, sample_rate).max(1);
    let hold_frames = ((hold_ms as usize * sample_rate as usize) / 1000)
        .div_ceil(frame)
        .max(1);

    let mut run_start = 0usize;
    let mut run = 0usize;
    let mut pos = 0usize;

    while pos + frame <= samples.len() {
        if frame_rms(&samples[pos..pos + frame]) >= threshold {
            if run == 0 {
                run_start = pos;
            }
            run += 1;
            if run >= hold_frames {
                return Some(run_start);
            }
        } else {
            run = 0;
        }
        pos += frame;
    }
    None
}

/// Normalized cross-correlation around `predicted_lag`.
///
/// A coarse pass decimates both signals by 4x, then the winning lag is refined
/// at full resolution. This is ~1 ms of work for a typical 300 ms overlap and
/// avoids pulling in an FFT dependency.
pub fn estimate_lag(
    onboard: &[f32],
    bt: &[f32],
    bt_ref_start: usize,
    predicted_lag: i32,
    search_radius: usize,
) -> Option<LagEstimate> {
    const DECIMATE: usize = 4;
    const REF_WINDOW_MS: usize = 220;

    if bt_ref_start >= bt.len() || onboard.is_empty() {
        return None;
    }

    let ref_len = ms_to_samples(REF_WINDOW_MS as u32, SPLICE_SAMPLE_RATE)
        .min(bt.len().saturating_sub(bt_ref_start));
    if ref_len < ms_to_samples(80, SPLICE_SAMPLE_RATE) {
        return None;
    }

    let radius = search_radius.max(1) as i32;
    let mut best = (f32::NEG_INFINITY, predicted_lag);

    let mut lag = predicted_lag - radius;
    while lag <= predicted_lag + radius {
        if let Some(score) = ncc_at(onboard, bt, bt_ref_start, ref_len, lag, DECIMATE) {
            if score > best.0 {
                best = (score, lag);
            }
        }
        lag += DECIMATE as i32;
    }

    if !best.0.is_finite() {
        return None;
    }

    // Full-resolution refinement around the coarse winner. The coarse grid is
    // decimated by 4, so search the full ±4 neighbourhood to avoid missing the
    // true peak by one grid step.
    let coarse = best.1;
    for lag in (coarse - DECIMATE as i32)..=(coarse + DECIMATE as i32) {
        if let Some(score) = ncc_at(onboard, bt, bt_ref_start, ref_len, lag, 1) {
            if score > best.0 {
                best = (score, lag);
            }
        }
    }

    Some(LagEstimate {
        lag_samples: best.1,
        confidence: best.0.clamp(-1.0, 1.0),
    })
}

fn ncc_at(
    onboard: &[f32],
    bt: &[f32],
    bt_start: usize,
    len: usize,
    lag: i32,
    step: usize,
) -> Option<f32> {
    let on_start = bt_start as i64 + lag as i64;
    if on_start < 0 {
        return None;
    }
    let on_start = on_start as usize;
    if on_start + len > onboard.len() || bt_start + len > bt.len() {
        return None;
    }

    let mut ab = 0.0f64;
    let mut aa = 0.0f64;
    let mut bb = 0.0f64;
    let mut i = 0usize;
    while i < len {
        let a = bt[bt_start + i] as f64;
        let b = onboard[on_start + i] as f64;
        ab += a * b;
        aa += a * a;
        bb += b * b;
        i += step;
    }

    let denom = (aa * bb).sqrt();
    if denom <= 1e-12 {
        None
    } else {
        Some((ab / denom) as f32)
    }
}

/// Picks the quietest 10 ms frame in `[start, start + window)` and returns its
/// centre. Switching in a low-energy region hides the timbre change from both
/// listeners and Whisper.
pub fn choose_low_energy_index(samples: &[f32], start: usize, window: usize) -> usize {
    let frame = ms_to_samples(10, SPLICE_SAMPLE_RATE).max(1);
    let start = start.min(samples.len());
    let end = (start + window).min(samples.len());
    if end <= start + frame {
        return start;
    }

    let mut best_idx = start;
    let mut best_rms = f32::INFINITY;
    let mut pos = start;
    while pos + frame <= end {
        let rms = frame_rms(&samples[pos..pos + frame]);
        if rms < best_rms {
            best_rms = rms;
            best_idx = pos + frame / 2;
        }
        pos += frame;
    }
    best_idx
}

/// Aligns and joins an onboard take and a headset take.
///
/// `bt_speech` is the first non-silent index in the headset take.
/// `predicted_lag` is the wall-clock estimate (`onboard = bt + lag`), used to
/// bound the correlation search and as a fallback if correlation is weak.
pub fn splice_takes(
    onboard: &[f32],
    bt: &[f32],
    bt_speech: usize,
    predicted_lag: i32,
    cfg: SpliceConfig,
) -> SpliceOutcome {
    let fade = ms_to_samples(cfg.crossfade_ms, cfg.sample_rate)
        .max(ms_to_samples(20, cfg.sample_rate))
        .min(onboard.len().max(1) / 4)
        .max(1);
    let guard = ms_to_samples(10, cfg.sample_rate);

    if onboard.is_empty() || bt.is_empty() || bt_speech >= bt.len() {
        return SpliceOutcome {
            samples: onboard.to_vec(),
            lag_samples: None,
            confidence: 0.0,
            splice_point_samples: onboard.len(),
            crossfade_samples: 0,
            fallback: Some(SpliceFallback::NoHeadsetSpeech),
        };
    }

    // The caller normally passes a detected onset, but guard here too so a
    // silent/all-zero headset buffer can never become a splice.
    if first_speech_index(bt, cfg.sample_rate, cfg.speech_rms, 25).is_none() {
        return SpliceOutcome {
            samples: onboard.to_vec(),
            lag_samples: None,
            confidence: 0.0,
            splice_point_samples: onboard.len(),
            crossfade_samples: 0,
            fallback: Some(SpliceFallback::NoHeadsetSpeech),
        };
    }

    let search = ms_to_samples(cfg.search_ms, cfg.sample_rate);
    let bt_ref = (bt_speech + guard).min(bt.len().saturating_sub(1));
    let estimate = estimate_lag(onboard, bt, bt_ref, predicted_lag, search);

    let (lag, confidence, fallback) = match estimate {
        Some(est) if est.confidence >= cfg.min_confidence => {
            (est.lag_samples, est.confidence, None)
        }
        Some(est) => (
            est.lag_samples,
            est.confidence,
            Some(SpliceFallback::LowConfidence),
        ),
        None => (
            predicted_lag,
            0.0,
            Some(SpliceFallback::LowConfidence),
        ),
    };

    let bt_cut_raw = choose_low_energy_index(
        bt,
        bt_speech + guard,
        ms_to_samples(140, cfg.sample_rate),
    );
    let has_fade_room = onboard.len() >= fade * 2 && bt.len() >= fade * 2;
    // Clamp before deriving onboard_cut: the crossfade must read the same
    // sample in both takes. Clamping afterwards would silently misalign it.
    let bt_cut = if has_fade_room {
        bt_cut_raw.min(bt.len() - fade)
    } else {
        bt_cut_raw
    };
    let onboard_cut_i64 = bt_cut as i64 + lag as i64;

    // Not enough shared audio to place a fade on both sides, or the quietest
    // window is before the headset actually has speech.
    if !has_fade_room
        || bt_cut < bt_speech.saturating_add(guard)
        || onboard_cut_i64 < fade as i64
        || onboard_cut_i64 as usize + fade > onboard.len()
    {
        return SpliceOutcome {
            samples: onboard.to_vec(),
            lag_samples: Some(lag),
            confidence,
            splice_point_samples: onboard.len(),
            crossfade_samples: 0,
            fallback: Some(SpliceFallback::NoOverlap),
        };
    }

    let onboard_cut = onboard_cut_i64 as usize;

    // Level-match the headset tail to the onboard segment across the fade
    // window. Clamped: never boost the headset more than +6 dB or cut it more
    // than -6 dB, otherwise the seam becomes an audible step.
    let out_rms = frame_rms(&onboard[onboard_cut - fade..onboard_cut]);
    let in_rms = frame_rms(&bt[bt_cut..bt_cut + fade]);
    let gain = if out_rms > 1e-6 && in_rms > 1e-6 {
        (out_rms / in_rms).clamp(0.5, 2.0)
    } else {
        1.0
    };

    let mut out = Vec::with_capacity(onboard_cut + bt.len().saturating_sub(bt_cut) + 1);
    out.extend_from_slice(&onboard[..onboard_cut]);

    // Equal-power fade: uncorrelated room/mic noise keeps a roughly constant
    // loudness through the seam instead of dipping.
    for i in 0..fade {
        let t = (i + 1) as f32 / (fade + 1) as f32;
        let w_in = (t * std::f32::consts::FRAC_PI_2).sin();
        let w_out = (t * std::f32::consts::FRAC_PI_2).cos();
        out.push(onboard[onboard_cut + i] * w_out + bt[bt_cut + i] * gain * w_in);
    }
    for &s in &bt[bt_cut + fade..] {
        out.push(s * gain);
    }

    SpliceOutcome {
        samples: out,
        lag_samples: Some(lag),
        confidence,
        splice_point_samples: onboard_cut,
        crossfade_samples: fade,
        fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn speech_like(len: usize, sr: u32) -> Vec<f32> {
        // Deterministic broadband signal with syllable-rate amplitude changes.
        (0..len)
            .map(|i| {
                let t = i as f32 / sr as f32;
                let env = 0.55 + 0.45 * (2.0 * std::f32::consts::PI * 3.0 * t).sin();
                let a = (2.0 * std::f32::consts::PI * 220.0 * t).sin();
                let b = 0.5 * (2.0 * std::f32::consts::PI * 730.0 * t).sin();
                (a + b) * env * 0.20
            })
            .collect()
    }

    fn add_noise(signal: &[f32], amp: f32, seed: u32) -> Vec<f32> {
        let mut state = seed | 1;
        signal
            .iter()
            .map(|&s| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                let n = ((state >> 8) as f32 / 16_777_216.0) - 0.5;
                s + n * amp
            })
            .collect()
    }

    #[test]
    fn first_speech_ignores_leading_zeros_and_clicks() {
        let sr = SPLICE_SAMPLE_RATE;
        let mut buf = vec![0.0f32; sr as usize / 2];
        // 5 ms click at 50 ms
        for s in buf.iter_mut().skip(sr as usize / 20).take(80) {
            *s = 0.4;
        }
        let speech = speech_like(sr as usize / 2, sr);
        buf.extend_from_slice(&speech);

        let idx = first_speech_index(&buf, sr, 0.001, 25).expect("speech found");
        assert!(
            idx >= sr as usize / 2 - 20 && idx <= sr as usize / 2 + 400,
            "first speech index {} should be near 500ms",
            idx
        );
    }

    #[test]
    fn correlation_recovers_a_known_handshake_delay() {
        let sr = SPLICE_SAMPLE_RATE;
        let delay = sr as usize * 250 / 1000; // 250 ms
        let total = sr as usize * 3;
        let onboard = speech_like(total, sr);
        // Model a headset stream whose first delivered sample is already
        // 250 ms into the utterance (no leading zeros in the buffer).
        let bt = add_noise(&onboard[delay..], 0.002, 7);

        let estimate = estimate_lag(&onboard, &bt, 160, delay as i32, sr as usize / 2)
            .expect("lag estimate");
        let error_ms = (estimate.lag_samples - delay as i32).abs() as f32 / sr as f32 * 1000.0;
        assert!(
            error_ms <= 2.0,
            "lag error {:.2} ms (got {}, want {}) conf {:.3}",
            error_ms,
            estimate.lag_samples,
            delay,
            estimate.confidence
        );
        assert!(estimate.confidence > 0.5);
    }

    #[test]
    fn correlation_also_handles_a_timeline_with_leading_zeros() {
        let sr = SPLICE_SAMPLE_RATE;
        let delay = sr as usize * 250 / 1000;
        let total = sr as usize * 3;
        let onboard = speech_like(total, sr);
        // Timeline-preserving buffer: 250 ms of handshake zeros, then audio
        // that lines up 1:1 with the onboard take.
        let mut bt = vec![0.0f32; delay];
        bt.extend_from_slice(&onboard[delay..]);
        let bt = add_noise(&bt, 0.002, 9);

        let estimate = estimate_lag(&onboard, &bt, delay + 160, 0, sr as usize / 2)
            .expect("lag estimate");
        assert!(
            estimate.lag_samples.abs() <= 32,
            "expected ~0 lag for a timeline-preserving buffer, got {}",
            estimate.lag_samples
        );
    }

    #[test]
    fn splice_keeps_the_full_timeline_and_covers_the_start() {
        let sr = SPLICE_SAMPLE_RATE;
        let delay = sr as usize * 250 / 1000;
        let total = sr as usize * 3;
        let onboard = speech_like(total, sr);
        let bt = add_noise(&onboard[delay..], 0.002, 11);

        let bt_speech =
            first_speech_index(&bt, sr, DEFAULT_SPEECH_RMS, 25).expect("headset speech");
        let outcome = splice_takes(
            &onboard,
            &bt,
            bt_speech,
            delay as i32,
            SpliceConfig::default(),
        );

        assert!(outcome.fallback.is_none(), "unexpected fallback: {:?}", outcome.fallback);
        let duration_error_ms =
            (outcome.samples.len() as i64 - total as i64).abs() as f32 / sr as f32 * 1000.0;
        assert!(
            duration_error_ms <= 30.0,
            "spliced duration drifted {:.1} ms",
            duration_error_ms
        );

        // The first 200 ms must contain speech energy (the legacy path would
        // have delivered 250 ms of silence here).
        let head_rms = frame_rms(&outcome.samples[..sr as usize / 5]);
        let tail_rms = frame_rms(&outcome.samples[outcome.samples.len() - sr as usize / 5..]);
        assert!(head_rms > 0.02, "first 200 ms is silent: RMS {:.4}", head_rms);
        assert!(tail_rms > 0.02, "tail was lost: RMS {:.4}", tail_rms);
    }

    #[test]
    fn no_headset_speech_returns_onboard_only() {
        let sr = SPLICE_SAMPLE_RATE;
        let onboard = speech_like(sr as usize, sr);
        let bt = vec![0.0f32; sr as usize];
        let outcome = splice_takes(&onboard, &bt, 0, 0, SpliceConfig::default());
        assert_eq!(outcome.samples, onboard);
        assert_eq!(outcome.fallback, Some(SpliceFallback::NoHeadsetSpeech));
    }

    #[test]
    fn splice_processing_is_well_under_a_frame_budget() {
        let sr = SPLICE_SAMPLE_RATE;
        let delay = sr as usize * 300 / 1000;
        let total = sr as usize * 5;
        let onboard = speech_like(total, sr);
        let bt = add_noise(&onboard[delay..], 0.002, 3);
        let bt_speech = first_speech_index(&bt, sr, DEFAULT_SPEECH_RMS, 25).unwrap();

        let started = Instant::now();
        let outcome = splice_takes(
            &onboard,
            &bt,
            bt_speech,
            delay as i32,
            SpliceConfig::default(),
        );
        let elapsed = started.elapsed();
        assert!(outcome.fallback.is_none());
        println!(
            "[Splice Test] 5.0 s stereo-mic take processed in {} ms (lag {} samples, conf {:.3})",
            elapsed.as_millis(),
            outcome.lag_samples.unwrap_or(0),
            outcome.confidence
        );
        // Deliberately generous for debuggable CI builds; real release builds
        // are an order of magnitude faster.
        assert!(
            elapsed.as_millis() < 100,
            "splice took {} ms for 5 s of audio",
            elapsed.as_millis()
        );
    }
}
