// Copyright (c) 2026 Sotto
// In-Memory Audio Downmixer, Resampler & Format Normalizer (Target: 16kHz Mono PCM16)

pub const TARGET_SAMPLE_RATE: u32 = 16000;

/// Downmixes multi-channel interleaved float audio samples into a caller-owned
/// `mono` vector, reusing its existing allocation when called on audio threads.
pub fn downmix_interleaved_into(interleaved: &[f32], channels: u16, mono: &mut Vec<f32>) {
    mono.clear();
    if channels <= 1 {
        mono.extend_from_slice(interleaved);
        return;
    }

    let channels_usize = channels as usize;
    let num_frames = interleaved.len() / channels_usize;
    if num_frames == 0 {
        return;
    }

    // Measure energy per channel to detect if one channel is dominant (e.g. laptop mic array)
    let mut stack_energies = [0.0f64; 16];
    let mut heap_energies = Vec::new();
    let channel_energies: &mut [f64] = if channels_usize <= stack_energies.len() {
        &mut stack_energies[..channels_usize]
    } else {
        heap_energies.resize(channels_usize, 0.0f64);
        &mut heap_energies
    };
    for frame in interleaved.chunks_exact(channels_usize) {
        for (i, &s) in frame.iter().enumerate() {
            channel_energies[i] += (s as f64) * (s as f64);
        }
    }

    let total_energy: f64 = channel_energies.iter().sum();
    let (max_ch_idx, &max_energy) = channel_energies
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or((0, &0.0));

    // If one channel has > 75% of total energy, use it directly to preserve full vocal amplitude
    let is_single_dominant = total_energy > 0.0 && (max_energy / total_energy) > 0.75;

    mono.reserve(num_frames);
    if is_single_dominant {
        for frame in interleaved.chunks_exact(channels_usize) {
            mono.push(frame[max_ch_idx]);
        }
    } else {
        let factor = 1.0 / (channels as f32);
        for frame in interleaved.chunks_exact(channels_usize) {
            let sum: f32 = frame.iter().copied().sum();
            mono.push(sum * factor);
        }
    }
}

/// Downmixes multi-channel interleaved float audio samples into a single mono channel.
pub fn downmix_interleaved_to_mono(interleaved: &[f32], channels: u16) -> Vec<f32> {
    let mut mono = Vec::with_capacity(if channels <= 1 {
        interleaved.len()
    } else {
        interleaved.len() / (channels as usize).max(1)
    });
    downmix_interleaved_into(interleaved, channels, &mut mono);
    mono
}

/// Resamples a mono f32 stream from `source_sample_rate` to `target_sample_rate` (16,000 Hz).
/// Uses an anti-aliased linear interpolation filter.
pub fn resample_linear(
    input: &[f32],
    source_sample_rate: u32,
    target_sample_rate: u32,
) -> Vec<f32> {
    if input.is_empty() {
        return Vec::new();
    }

    if source_sample_rate == target_sample_rate {
        return input.to_vec();
    }

    let ratio = (source_sample_rate as f64) / (target_sample_rate as f64);
    let output_len = ((input.len() as f64) / ratio).round() as usize;
    let mut output = Vec::with_capacity(output_len);

    let filter_width = if ratio > 1.0 { ratio } else { 1.0 };
    let half_width = filter_width / 2.0;

    for i in 0..output_len {
        let center = (i as f64) * ratio;
        let start = (center - half_width).floor().max(0.0) as usize;
        let end = (center + half_width).ceil().min((input.len() - 1) as f64) as usize;

        if start >= input.len() {
            break;
        }

        let mut sum = 0.0f64;
        let mut weight_sum = 0.0f64;

        for (idx, &sample) in input.iter().enumerate().take(end + 1).skip(start) {
            let dist = ((idx as f64) - center).abs();
            let weight = (1.0 - (dist / filter_width)).max(0.0);
            sum += (sample as f64) * weight;
            weight_sum += weight;
        }

        if weight_sum > 0.0 {
            output.push((sum / weight_sum) as f32);
        } else if (center.floor() as usize) < input.len() {
            output.push(input[center.floor() as usize]);
        }
    }

    output
}

/// Converts normalized float samples `[-1.0, 1.0]` to signed 16-bit integer PCM `[-32768, 32767]`.
pub fn float_to_pcm16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|&s| {
            let clamped = s.clamp(-1.0, 1.0);
            if clamped >= 0.0 {
                (clamped * 32767.0).round() as i16
            } else {
                (clamped * 32768.0).round() as i16
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_downmix_stereo_to_mono() {
        let stereo = vec![0.5, 0.5, -0.2, 0.4, 1.0, -1.0];
        let mono = downmix_interleaved_to_mono(&stereo, 2);
        assert_eq!(mono.len(), 3);
        assert!((mono[0] - 0.5).abs() < 1e-5);
        assert!((mono[1] - 0.1).abs() < 1e-5);
        assert!((mono[2] - 0.0).abs() < 1e-5);
    }

    #[test]
    fn test_resample_48k_to_16k_ratio() {
        // 48,000 samples at 48kHz is 1 second of audio.
        // Resampled to 16kHz, it must yield exactly 16,000 samples.
        let input_48k: Vec<f32> = (0..48000)
            .map(|i| ((i as f32) * 440.0 * 2.0 * std::f32::consts::PI / 48000.0).sin())
            .collect();

        let output_16k = resample_linear(&input_48k, 48000, 16000);
        assert_eq!(output_16k.len(), 16000);
    }

    #[test]
    fn test_float_to_pcm16_clamping() {
        let floats = vec![-1.5, -1.0, 0.0, 0.5, 1.0, 2.0];
        let pcm = float_to_pcm16(&floats);
        assert_eq!(pcm[0], -32768);
        assert_eq!(pcm[1], -32768);
        assert_eq!(pcm[2], 0);
        assert!((pcm[3] - 16384).abs() <= 1);
        assert_eq!(pcm[4], 32767);
        assert_eq!(pcm[5], 32767);
    }
}
