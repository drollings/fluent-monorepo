//! Minimal WAV reader: PCM16 and IEEE-float mono or stereo to f32 mono.
//!
//! Only what offline audio needs: RIFF/WAVE with one `fmt ` and one
//! `data` chunk. Anything else is rejected with a message, never guessed.

use orator_core::SttError;

/// PCM audio in memory.
#[derive(Debug, Clone, PartialEq)]
pub struct WavPcm {
    /// Mono f32 samples.
    pub samples: Vec<f32>,
    /// File sample rate in Hz.
    pub sample_rate: u32,
}

/// Read a WAV file into mono f32. Stereo mixes down; other widths fail.
pub fn read_wav_mono(path: &std::path::Path) -> Result<WavPcm, SttError> {
    let bytes =
        std::fs::read(path).map_err(|error| SttError::Backend(format!("wav read: {error}")))?;
    parse_wav(&bytes)
}

/// Decode in-memory WAV bytes into mono f32 (e.g. a `/tts` HTTP
/// body). Same parser as [`read_wav_mono`], no temp file.
pub fn decode_wav_bytes(bytes: &[u8]) -> Result<WavPcm, SttError> {
    parse_wav(bytes)
}

/// Encode mono f32 samples as 16-bit PCM WAV bytes at `sample_rate`.
///
/// The writer mirrors the parser: the payload quantizes with the
/// (32767, TowardZero) convention shared with the s2s frame encoder
/// (`common_core::pcm`), so `decode_wav_bytes(encode_wav_mono(x))`
/// stays within one LSB of `x`. The header comes from
/// `common_core::pcm::wav_frame_header`.
#[must_use]
pub fn encode_wav_mono(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    use common_core::pcm::{Rounding, Scale16};
    common_core::pcm::encode_wav_mono_pcm16(
        samples,
        sample_rate,
        Scale16::Symmetric32767,
        Rounding::TowardZero,
    )
}

fn parse_wav(bytes: &[u8]) -> Result<WavPcm, SttError> {
    let fail = |why: &str| SttError::Backend(format!("wav parse: {why}"));
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(fail("not a RIFF/WAVE file"));
    }
    let mut offset = 12;
    let mut format: Option<(u16, u16, u32)> = None;
    let mut data: Option<&[u8]> = None;
    while offset + 8 <= bytes.len() {
        let tag = &bytes[offset..offset + 4];
        let size =
            u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().expect("slice")) as usize;
        let body = offset + 8;
        let end = body.saturating_add(size).min(bytes.len());
        if tag == b"fmt " && size >= 16 {
            let audio_format = u16::from_le_bytes(bytes[body..body + 2].try_into().expect("slice"));
            let channels = u16::from_le_bytes(bytes[body + 2..body + 4].try_into().expect("slice"));
            let rate = u32::from_le_bytes(bytes[body + 4..body + 8].try_into().expect("slice"));
            format = Some((audio_format, channels, rate));
        } else if tag == b"data" {
            data = Some(&bytes[body..end]);
        }
        offset = end + (end % 2);
    }
    let (audio_format, channels, rate) = format.ok_or_else(|| fail("missing fmt chunk"))?;
    let data = data.ok_or_else(|| fail("missing data chunk"))?;
    if rate == 0 || channels == 0 {
        return Err(fail("degenerate format"));
    }
    let samples = match audio_format {
        1 => decode_pcm16(data, channels)?,
        3 => decode_ieee_float(data, channels)?,
        other => return Err(fail(&format!("unsupported format {other}"))),
    };
    Ok(WavPcm {
        samples,
        sample_rate: rate,
    })
}

fn decode_pcm16(data: &[u8], channels: u16) -> Result<Vec<f32>, SttError> {
    if data.len() % 2 != 0 {
        return Err(SttError::Backend("wav parse: odd pcm16 length".to_string()));
    }
    let frames = data.len() / 2 / channels as usize;
    let mut mono = Vec::with_capacity(frames);
    common_core::pcm::extend_mono_from_interleaved(
        data.chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]])),
        channels as usize,
        |sample| {
            common_core::pcm::i16_to_f32_sample(sample, common_core::pcm::Scale16::Symmetric32767)
        },
        &mut mono,
    );
    Ok(mono)
}

fn decode_ieee_float(data: &[u8], channels: u16) -> Result<Vec<f32>, SttError> {
    if data.len() % 4 != 0 {
        return Err(SttError::Backend("wav parse: odd float length".to_string()));
    }
    let frames = data.len() / 4 / channels as usize;
    let mut mono = Vec::with_capacity(frames);
    common_core::pcm::extend_mono_from_interleaved(
        data.chunks_exact(4)
            .map(|quad| f32::from_le_bytes([quad[0], quad[1], quad[2], quad[3]])),
        channels as usize,
        |sample| sample,
        &mut mono,
    );
    Ok(mono)
}

#[cfg(test)]
#[path = "../tests/wav.rs"]
mod tests;
