// Copyright (c) 2026 Sotto
// Silence, Ghost-Tap Filter, 80Hz Rumble Highpass & Adaptive Soft-Knee AGC Booster

pub const MIN_RECORDING_MS: u32 = 250;
pub const SILENCE_RMS_THRESHOLD: f32 = 0.0006; // ~ -64dBFS (Ultra-sensitive to distant & whispered voice)

#[derive(Debug, Clone, PartialEq)]
pub enum AudioValidation {
    Valid {
        duration_ms: u32,
        rms: f32,
        peak: f32,
    },
    DiscardGhostTap {
        duration_ms: u32,
    },
    DiscardSilence {
        rms: f32,
    },
}

/// 2nd-Order Butterworth High-Pass Filter (Biquad IIR) for removing DC offset,
/// 50/60 Hz mains hum, desk vibrations, and laptop fan rumble.
pub struct BiquadHighPass {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl BiquadHighPass {
    pub fn new(cutoff_hz: f32, sample_rate: f32) -> Self {
        let w0 = 2.0 * std::f32::consts::PI * cutoff_hz / sample_rate;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * std::f32::consts::FRAC_1_SQRT_2);

        let b0 = (1.0 + cos_w0) / 2.0;
        let b1 = -(1.0 + cos_w0);
        let b2 = (1.0 + cos_w0) / 2.0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha;

        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    #[inline]
    pub fn process_sample(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2 - self.a1 * self.y1 - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }

    pub fn process_slice(&mut self, samples: &mut [f32]) {
        for s in samples.iter_mut() {
            *s = self.process_sample(*s);
        }
    }
}

/// Computes the normalized Root Mean Square (RMS) energy in `[0.0, 1.0]`.
pub fn compute_rms(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }

    let sum_sq: f64 = samples
        .iter()
        .map(|&s| {
            let norm = (s as f64) / 32768.0;
            norm * norm
        })
        .sum();

    ((sum_sq / (samples.len() as f64)).sqrt() as f32).min(1.0)
}

/// Computes the normalized peak amplitude in `[0.0, 1.0]`.
pub fn compute_peak(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }

    let max_abs = samples.iter().map(|&s| (s as i32).abs()).max().unwrap_or(0);
    ((max_abs as f32) / 32768.0).min(1.0)
}

/// Inspects recording duration and RMS energy to filter out accidental key taps and silence.
pub fn validate_audio(
    samples: &[i16],
    sample_rate: u32,
    min_duration_ms: u32,
    silence_threshold_rms: f32,
) -> AudioValidation {
    if sample_rate == 0 || samples.is_empty() {
        return AudioValidation::DiscardGhostTap { duration_ms: 0 };
    }

    let duration_ms = ((samples.len() as f64) / (sample_rate as f64) * 1000.0) as u32;

    // 1. Ghost-tap filter (tap shorter than minimum duration)
    if duration_ms < min_duration_ms {
        return AudioValidation::DiscardGhostTap { duration_ms };
    }

    // 2. Silence / Muted mic filter
    let rms = compute_rms(samples);
    if rms < silence_threshold_rms {
        return AudioValidation::DiscardSilence { rms };
    }

    let peak = compute_peak(samples);
    AudioValidation::Valid {
        duration_ms,
        rms,
        peak,
    }
}

/// Trims trailing silence from the end of a PCM16 buffer to prevent Whisper from
/// hallucinating endings (such as "thanks", "thank you", or YouTube outro phrases)
/// during room noise or pauses after speech has stopped.
///
/// Keeps `pad_ms` (e.g. 100ms) of acoustic decay after the last detected speech frame.
pub fn trim_trailing_silence(
    samples: &mut Vec<i16>,
    sample_rate: u32,
    speech_threshold_rms: f32,
    pad_ms: u32,
) -> usize {
    if samples.is_empty() || sample_rate == 0 {
        return 0;
    }

    let frame_samples = (sample_rate as usize * 20) / 1000; // 20ms frame
    if frame_samples == 0 || samples.len() <= frame_samples {
        return 0;
    }

    let pad_samples = (sample_rate as usize * pad_ms as usize) / 1000;
    let mut last_speech_idx = None;

    // Scan backwards in 20ms frames
    let mut end_idx = samples.len();
    while end_idx >= frame_samples {
        let start_idx = end_idx - frame_samples;
        let frame_rms = compute_rms(&samples[start_idx..end_idx]);
        if frame_rms >= speech_threshold_rms {
            last_speech_idx = Some(end_idx);
            break;
        }
        end_idx = start_idx;
    }

    if let Some(speech_end) = last_speech_idx {
        let safe_end = (speech_end + pad_samples).min(samples.len());
        if safe_end < samples.len() {
            let trimmed_count = samples.len() - safe_end;
            samples.truncate(safe_end);
            return trimmed_count;
        }
    }

    0
}

/// Speech-Aware Dynamic Automatic Gain Control (AGC) with Tanh Soft-Knee Limiter.
/// Evaluates both the vocal body (RMS) and transient peaks. Faint whispers and distant voices
/// are cleanly elevated to optimal transcription amplitude (~ -22 dBFS) while transients
/// that push past 0.85 are smoothly rounded by a hyperbolic tangent soft-limiter to prevent clipping clicks.
pub fn apply_digital_gain(samples: &mut [i16], target_peak: f32, max_gain: f32) -> f32 {
    if samples.is_empty() {
        return 1.0;
    }

    let current_peak = compute_peak(samples);
    let current_rms = compute_rms(samples);

    if current_peak < 0.0005 || current_peak >= target_peak {
        return 1.0;
    }

    // Target vocal RMS sweet spot for Whisper acoustic encoder: ~0.065 (-24 dBFS)
    let rms_gain = if current_rms > 0.0005 {
        0.065 / current_rms
    } else {
        target_peak / current_peak
    };
    let peak_gain = target_peak / current_peak;

    // Allow vocal-body boosting up to 1.5x of peak gain, capped by max_gain
    let desired_gain = rms_gain.min(peak_gain * 1.5).max(peak_gain);
    let applied_gain = desired_gain.min(max_gain).max(1.0);

    if applied_gain <= 1.01 {
        return 1.0;
    }

    // Soft-knee limiting: values below knee_threshold are scaled linearly.
    // Values above knee_threshold are smoothly compressed using tanh.
    let knee_threshold = 0.85f32;
    let inv_knee = 1.0 / (1.0 - knee_threshold);

    for s in samples.iter_mut() {
        let x = (*s as f32) / 32768.0 * applied_gain;
        let abs_x = x.abs();
        let y = if abs_x <= knee_threshold {
            x
        } else {
            let over = abs_x - knee_threshold;
            let compressed = knee_threshold + (1.0 - knee_threshold) * (over * inv_knee).tanh();
            if x >= 0.0 { compressed } else { -compressed }
        };
        *s = (y.clamp(-1.0, 1.0) * 32767.0).round() as i16;
    }

    applied_gain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ghost_tap_filter() {
        // 100ms of 16kHz audio = 1600 samples (< 250ms min)
        let short_audio = vec![5000i16; 1600];
        let res = validate_audio(&short_audio, 16000, 250, SILENCE_RMS_THRESHOLD);
        assert_eq!(res, AudioValidation::DiscardGhostTap { duration_ms: 100 });
    }

    #[test]
    fn test_silence_filter() {
        // 500ms of absolute silence
        let silent_audio = vec![0i16; 8000];
        let res = validate_audio(&silent_audio, 16000, 250, SILENCE_RMS_THRESHOLD);
        assert_eq!(res, AudioValidation::DiscardSilence { rms: 0.0 });
    }

    #[test]
    fn test_valid_speech_audio() {
        // 1 second of audible signal
        let speech: Vec<i16> = (0..16000)
            .map(|i| {
                let t = (i as f32) / 16000.0;
                ((t * 300.0 * 2.0 * std::f32::consts::PI).sin() * 8000.0) as i16
            })
            .collect();

        let res = validate_audio(&speech, 16000, 250, SILENCE_RMS_THRESHOLD);
        match res {
            AudioValidation::Valid { duration_ms, rms, peak } => {
                assert_eq!(duration_ms, 1000);
                assert!(rms > 0.1);
                assert!(peak > 0.2);
            }
            other => panic!("Expected Valid, got {:?}", other),
        }
    }

    #[test]
    fn test_digital_gain_quiet_speech() {
        let mut quiet = vec![1000i16; 4000];
        let gain = apply_digital_gain(&mut quiet, 0.6, 3.0);
        assert!(gain > 1.0);
        assert!(quiet[0] > 1000);
    }

    #[test]
    fn test_highpass_dc_rejection() {
        let mut hp = BiquadHighPass::new(80.0, 16000.0);
        // Constant DC offset of 0.5
        let mut dc = vec![0.5f32; 1600]; // 100ms
        hp.process_slice(&mut dc);
        // After settling, DC should be close to 0.0
        let last_samples = &dc[1500..];
        for &s in last_samples {
            assert!(s.abs() < 0.005, "DC offset not rejected: {}", s);
        }
    }

    #[test]
    fn test_highpass_passes_speech_frequencies() {
        let mut hp = BiquadHighPass::new(80.0, 16000.0);
        // 1000 Hz tone (vocal formant region)
        let mut tone: Vec<f32> = (0..1600)
            .map(|i| {
                let t = (i as f32) / 16000.0;
                (t * 1000.0 * 2.0 * std::f32::consts::PI).sin()
            })
            .collect();
        hp.process_slice(&mut tone);
        // After settling, 1000Hz amplitude should be near 1.0
        let settled_peak = tone[800..].iter().map(|s| s.abs()).fold(0.0f32, f32::max);
        assert!((settled_peak - 1.0).abs() < 0.05, "1000Hz tone attenuated: {}", settled_peak);
    }

    #[test]
    fn test_soft_knee_limiting_no_hard_clipping() {
        // Feed high amplitude audio that requires compression
        let mut audio: Vec<i16> = (0..1600)
            .map(|i| {
                let t = (i as f32) / 1600.0;
                ((t * 200.0 * 2.0 * std::f32::consts::PI).sin() * 20000.0) as i16
            })
            .collect();
        let gain = apply_digital_gain(&mut audio, 0.90, 4.0);
        assert!(gain > 1.0);
        // Confirm no samples exceed 32767
        let peak = compute_peak(&audio);
        assert!(peak <= 1.0);
    }

    #[test]
    fn test_trim_trailing_silence() {
        // 1 second of speech (16000 samples with strong amplitude) followed by 1 second of silence
        let mut audio = Vec::with_capacity(32000);
        for i in 0..16000 {
            let t = (i as f32) / 16000.0;
            audio.push(((t * 300.0 * 2.0 * std::f32::consts::PI).sin() * 8000.0) as i16);
        }
        // 16000 samples of low amplitude silence (room noise)
        audio.extend(vec![10i16; 16000]);

        assert_eq!(audio.len(), 32000);
        // Trim trailing silence with 100ms (1600 samples) padding
        let trimmed = trim_trailing_silence(&mut audio, 16000, 0.003, 100);
        assert!(trimmed > 0);
        // Target length should be around 16000 + 1600 = 17600
        assert_eq!(audio.len(), 17600);
        assert_eq!(trimmed, 14400); // 900ms of dead silence trimmed
    }
}

