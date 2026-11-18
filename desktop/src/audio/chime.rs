// Copyright (c) 2026 Sotto
// Tiny synthesized dictation cues, played from memory through winmm.
//
// Design notes:
//   - Two pure sine bells a perfect fourth apart: G4 -> C5 on start, C5 -> G4
//     on stop. This mirrors the reference "digital success" cue, whose measured
//     notes were 394 Hz and 526 Hz (a 4:3 interval) with almost no harmonics.
//   - The reference's signature is the contrast between a fast, soft attack and
//     a long, smooth exponential ring. We keep that shape but shorten the tail
//     so a dictation cue never competes with the first spoken syllable.
//   - The second bell enters 87 ms after the first and both decay with a 68 ms
//     time constant. A short cosine fade closes the buffer, so the shortened
//     tail ends at silence instead of clicking.
//   - No square waves, no hard clipping, no system "ding", and no MP3 asset.
//     WAVs are synthesized once at first use and kept in static storage because
//     PlaySoundW(SND_MEMORY | SND_ASYNC) reads the buffer asynchronously.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

const SAMPLE_RATE: u32 = 48_000;
const TOTAL_MS: u32 = 300;
const NOTE_START_S: f32 = 0.005;
const SECOND_NOTE_DELAY_S: f32 = 0.087;
const ATTACK_S: f32 = 0.004;
const DECAY_S: f32 = 0.068;
const FADE_OUT_S: f32 = 0.025;
const PEAK: f32 = 0.16;

// Equal-tempered G4 and C5. The reference measured ~394 Hz and ~527 Hz, which
// is the same perfect-fourth interval with roughly ten cents of tuning offset.
const G4_HZ: f32 = 392.00;
const C5_HZ: f32 = 523.25;

static ENABLED: AtomicBool = AtomicBool::new(true);
static START_WAV: OnceLock<Vec<u8>> = OnceLock::new();
static STOP_WAV: OnceLock<Vec<u8>> = OnceLock::new();

pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::SeqCst);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// Soft rising fourth: "I am listening."
pub fn play_start() {
    if enabled() {
        play_wav(START_WAV.get_or_init(|| {
            crate::audio::wav::encode_pcm16_to_wav(&synthesize(G4_HZ, C5_HZ), SAMPLE_RATE)
        }));
    }
}

/// Soft falling fourth: "take captured."
pub fn play_stop() {
    if enabled() {
        play_wav(STOP_WAV.get_or_init(|| {
            crate::audio::wav::encode_pcm16_to_wav(&synthesize(C5_HZ, G4_HZ), SAMPLE_RATE)
        }));
    }
}

fn synthesize(first_hz: f32, second_hz: f32) -> Vec<i16> {
    let samples = (SAMPLE_RATE as f32 * TOTAL_MS as f32 / 1000.0) as usize;
    let total_s = TOTAL_MS as f32 / 1000.0;
    let fade_start_s = total_s - FADE_OUT_S;
    let mut floats = Vec::with_capacity(samples);
    let mut peak = 0.0f32;

    for index in 0..samples {
        let t = index as f32 / SAMPLE_RATE as f32;
        let first = bell(t - NOTE_START_S, first_hz);
        let second = bell(t - NOTE_START_S - SECOND_NOTE_DELAY_S, second_hz);
        let mut value = first + second;

        // Close the shortened ring smoothly. Without this the exponential tail
        // would be chopped at roughly -27 dB and could click on small speakers.
        if t > fade_start_s {
            let fade = ((total_s - t) / FADE_OUT_S).clamp(0.0, 1.0);
            value *= fade * fade * (3.0 - 2.0 * fade);
        }

        peak = peak.max(value.abs());
        floats.push(value);
    }

    // Normalize in float space before the i16 conversion. Converting first
    // would saturate the overlapping bells and make the final cue quieter than
    // intended.
    let gain = if peak > 0.0 { PEAK / peak } else { 0.0 };
    floats
        .into_iter()
        .map(|value| {
            (value * gain * i16::MAX as f32).clamp(i16::MIN as f32, i16::MAX as f32) as i16
        })
        .collect()
}

fn bell(t: f32, hz: f32) -> f32 {
    if t < 0.0 {
        return 0.0;
    }

    // A raised-cosine attack removes the leading edge without softening the
    // "struck glass" transient that gives the reference its digital character.
    let attack = (t / ATTACK_S).clamp(0.0, 1.0);
    let attack = attack * attack * (3.0 - 2.0 * attack);
    let decay = (-t / DECAY_S).exp();
    (std::f32::consts::TAU * hz * t).sin() * attack * decay
}

#[cfg(target_os = "windows")]
fn play_wav(bytes: &'static [u8]) {
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

    // winmm plays asynchronously; a tiny thread keeps first-use driver setup
    // off the hotkey path. The buffer is static, so it outlives playback.
    let _ = std::thread::Builder::new()
        .name("sotto-chime".to_string())
        .spawn(move || unsafe {
            let ok = PlaySoundW(
                bytes.as_ptr() as *const u16,
                std::ptr::null_mut(),
                SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
            );
            if ok == 0 {
                eprintln!("[Chime] PlaySoundW rejected the in-memory cue");
            }
        });
}

#[cfg(not(target_os = "windows"))]
fn play_wav(_bytes: &'static [u8]) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::wav::parse_wav_header;

    fn goertzel_energy(pcm: &[i16], hz: f32) -> f32 {
        let omega = std::f32::consts::TAU * hz / SAMPLE_RATE as f32;
        let coeff = 2.0 * omega.cos();
        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        for &sample in pcm {
            let s0 = sample as f32 / i16::MAX as f32 + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        (s1 * s1 + s2 * s2 - coeff * s1 * s2).sqrt()
    }

    #[test]
    fn cues_are_short_and_quiet() {
        for pcm in [synthesize(G4_HZ, C5_HZ), synthesize(C5_HZ, G4_HZ)] {
            let duration_ms = pcm.len() as u32 * 1000 / SAMPLE_RATE;
            assert!(
                (280..=330).contains(&duration_ms),
                "cue duration should stay near 300 ms, got {duration_ms}"
            );
            let peak = pcm.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
            let peak_ratio = peak as f32 / i16::MAX as f32;
            assert!(
                (0.13..=0.19).contains(&peak_ratio),
                "cue peak should stay soft, got {peak_ratio}"
            );
            assert!(pcm.iter().any(|s| *s != 0), "cue must contain audio");
        }
    }

    #[test]
    fn cues_keep_the_two_note_fourth_signature() {
        let start = synthesize(G4_HZ, C5_HZ);
        let stop = synthesize(C5_HZ, G4_HZ);
        let first_window =
            (0.010 * SAMPLE_RATE as f32) as usize..(0.075 * SAMPLE_RATE as f32) as usize;
        let second_window =
            (0.110 * SAMPLE_RATE as f32) as usize..(0.220 * SAMPLE_RATE as f32) as usize;

        let start_first_g4 = goertzel_energy(&start[first_window.clone()], G4_HZ);
        let start_first_c5 = goertzel_energy(&start[first_window.clone()], C5_HZ);
        let start_second_g4 = goertzel_energy(&start[second_window.clone()], G4_HZ);
        let start_second_c5 = goertzel_energy(&start[second_window.clone()], C5_HZ);
        assert!(
            start_first_g4 > start_first_c5 * 4.0,
            "start cue must open on G4 (G4 {start_first_g4}, C5 {start_first_c5})"
        );
        assert!(
            start_second_c5 > start_second_g4 * 2.0,
            "start cue must answer on C5 (C5 {start_second_c5}, G4 {start_second_g4})"
        );

        let stop_first_g4 = goertzel_energy(&stop[first_window.clone()], G4_HZ);
        let stop_first_c5 = goertzel_energy(&stop[first_window.clone()], C5_HZ);
        let stop_second_g4 = goertzel_energy(&stop[second_window.clone()], G4_HZ);
        let stop_second_c5 = goertzel_energy(&stop[second_window.clone()], C5_HZ);
        assert!(
            stop_first_c5 > stop_first_g4 * 4.0,
            "stop cue must open on C5 (C5 {stop_first_c5}, G4 {stop_first_g4})"
        );
        assert!(
            stop_second_g4 > stop_second_c5 * 2.0,
            "stop cue must answer on G4 (G4 {stop_second_g4}, C5 {stop_second_c5})"
        );

        assert!((C5_HZ / G4_HZ - 4.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn start_and_stop_are_distinct() {
        let start = synthesize(G4_HZ, C5_HZ);
        let stop = synthesize(C5_HZ, G4_HZ);
        assert_ne!(start, stop);
    }

    #[test]
    fn cues_have_soft_edges_and_a_clean_decay() {
        for pcm in [synthesize(G4_HZ, C5_HZ), synthesize(C5_HZ, G4_HZ)] {
            let head = pcm[..8].iter().map(|s| s.unsigned_abs()).max().unwrap();
            let tail = pcm[pcm.len() - 8..]
                .iter()
                .map(|s| s.unsigned_abs())
                .max()
                .unwrap();
            assert!(head < 500, "cue must start near silence, got {head}");
            assert!(tail < 500, "cue must end near silence, got {tail}");

            let early =
                &pcm[(0.090 * SAMPLE_RATE as f32) as usize..(0.130 * SAMPLE_RATE as f32) as usize];
            let late =
                &pcm[(0.260 * SAMPLE_RATE as f32) as usize..(0.290 * SAMPLE_RATE as f32) as usize];
            let early_rms = (early.iter().map(|s| (*s as f64).powi(2)).sum::<f64>()
                / early.len() as f64)
                .sqrt();
            let late_rms =
                (late.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / late.len() as f64).sqrt();
            assert!(
                late_rms < early_rms * 0.25,
                "cue should decay smoothly (early {early_rms}, late {late_rms})"
            );
        }
    }

    #[test]
    fn cues_are_valid_wav_images() {
        let wav = crate::audio::wav::encode_pcm16_to_wav(&synthesize(G4_HZ, C5_HZ), SAMPLE_RATE);
        let header = parse_wav_header(&wav).expect("cue must be a valid PCM WAV");
        assert_eq!(header.num_channels, 1);
        assert_eq!(header.sample_rate, SAMPLE_RATE);
        assert_eq!(header.bits_per_sample, 16);
    }
}
