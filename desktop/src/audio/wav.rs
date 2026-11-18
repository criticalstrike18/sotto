// Copyright (c) 2026 Sotto
// In-Memory WAV Encoder & Header Validator (Zero Disk I/O)

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WavHeaderInfo {
    pub num_channels: u16,
    pub sample_rate: u32,
    pub byte_rate: u32,
    pub block_align: u16,
    pub bits_per_sample: u16,
    pub data_length: u32,
}

/// Encodes raw 16-bit signed PCM audio samples into a standard RIFF/WAVE container entirely in RAM.
pub fn encode_pcm16_to_wav(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let num_channels: u16 = 1; // Mono
    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * (num_channels as u32) * ((bits_per_sample as u32) / 8);
    let block_align = num_channels * (bits_per_sample / 8);
    let data_len = (samples.len() * 2) as u32;
    let riff_chunk_size = 36 + data_len;

    let mut buffer = Vec::with_capacity(44 + (samples.len() * 2));

    // 1. RIFF Chunk Descriptor
    buffer.extend_from_slice(b"RIFF");
    buffer.extend_from_slice(&riff_chunk_size.to_le_bytes());
    buffer.extend_from_slice(b"WAVE");

    // 2. "fmt " Sub-chunk
    buffer.extend_from_slice(b"fmt ");
    buffer.extend_from_slice(&16u32.to_le_bytes()); // Sub-chunk 1 size (16 for PCM)
    buffer.extend_from_slice(&1u16.to_le_bytes());  // AudioFormat (1 = PCM)
    buffer.extend_from_slice(&num_channels.to_le_bytes());
    buffer.extend_from_slice(&sample_rate.to_le_bytes());
    buffer.extend_from_slice(&byte_rate.to_le_bytes());
    buffer.extend_from_slice(&block_align.to_le_bytes());
    buffer.extend_from_slice(&bits_per_sample.to_le_bytes());

    // 3. "data" Sub-chunk
    buffer.extend_from_slice(b"data");
    buffer.extend_from_slice(&data_len.to_le_bytes());

    // 4. Sample payload
    for &sample in samples {
        buffer.extend_from_slice(&sample.to_le_bytes());
    }

    buffer
}

/// Parses and validates a standard 44-byte WAV header in RAM for tests and pipeline validation.
pub fn parse_wav_header(bytes: &[u8]) -> Result<WavHeaderInfo, String> {
    if bytes.len() < 44 {
        return Err(format!("WAV payload too short: {} bytes (minimum 44 bytes required)", bytes.len()));
    }

    if &bytes[0..4] != b"RIFF" {
        return Err(format!("Invalid RIFF magic bytes: {:?}", &bytes[0..4]));
    }

    if &bytes[8..12] != b"WAVE" {
        return Err(format!("Invalid WAVE format bytes: {:?}", &bytes[8..12]));
    }

    if &bytes[12..16] != b"fmt " {
        return Err(format!("Missing fmt chunk: {:?}", &bytes[12..16]));
    }

    let subchunk1_size = u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    if subchunk1_size != 16 {
        return Err(format!("Expected PCM subchunk1 size 16, got {}", subchunk1_size));
    }

    let audio_format = u16::from_le_bytes([bytes[20], bytes[21]]);
    if audio_format != 1 {
        return Err(format!("Expected audio format 1 (PCM), got {}", audio_format));
    }

    let num_channels = u16::from_le_bytes([bytes[22], bytes[23]]);
    let sample_rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
    let byte_rate = u32::from_le_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]);
    let block_align = u16::from_le_bytes([bytes[32], bytes[33]]);
    let bits_per_sample = u16::from_le_bytes([bytes[34], bytes[35]]);

    if &bytes[36..40] != b"data" {
        return Err(format!("Missing data chunk: {:?}", &bytes[36..40]));
    }

    let data_length = u32::from_le_bytes([bytes[40], bytes[41], bytes[42], bytes[43]]);

    Ok(WavHeaderInfo {
        num_channels,
        sample_rate,
        byte_rate,
        block_align,
        bits_per_sample,
        data_length,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_empty_wav() {
        let wav = encode_pcm16_to_wav(&[], 16000);
        assert_eq!(wav.len(), 44);
        let header = parse_wav_header(&wav).expect("Valid WAV header");
        assert_eq!(header.num_channels, 1);
        assert_eq!(header.sample_rate, 16000);
        assert_eq!(header.byte_rate, 32000);
        assert_eq!(header.block_align, 2);
        assert_eq!(header.bits_per_sample, 16);
        assert_eq!(header.data_length, 0);
    }

    #[test]
    fn test_encode_and_parse_synthetic_sine_samples() {
        // 1 second of 16kHz audio = 16000 samples
        let mut samples = Vec::with_capacity(16000);
        for i in 0..16000 {
            let t = (i as f32) / 16000.0;
            let sample = (t * 440.0 * 2.0 * std::f32::consts::PI).sin() * 16384.0;
            samples.push(sample as i16);
        }

        let wav = encode_pcm16_to_wav(&samples, 16000);
        assert_eq!(wav.len(), 44 + (16000 * 2));

        let header = parse_wav_header(&wav).expect("Valid WAV header");
        assert_eq!(header.num_channels, 1);
        assert_eq!(header.sample_rate, 16000);
        assert_eq!(header.byte_rate, 32000);
        assert_eq!(header.block_align, 2);
        assert_eq!(header.bits_per_sample, 16);
        assert_eq!(header.data_length, 32000);

        // Verify sample byte reconstruction
        let sample_0 = i16::from_le_bytes([wav[44], wav[45]]);
        assert_eq!(sample_0, samples[0]);
    }
}
