//! PCM sample math: zero-domain `f32` ⇄ little-endian integer conversions.
//!
//! Every helper here is pure sample arithmetic with no I/O, no audio
//! device, and no codec state. Two wire scales coexist because the two
//! sides of the existing contract disagree and both are pinned by
//! characterization tests (see `orator_audio_codec_vectors`,
//! `orator_s2s_wav_frame`, `orator_daemon_transcribe_decode`):
//!
//! - [`Scale16::Symmetric32767`]: full-scale `1.0` maps to `i16::MAX`.
//!   Every encoder-side call site uses this scale.
//! - [`Scale16::Symmetric32768`]: full-scale maps to `2^15`, so the raw
//!   rail `32767` decodes to just under `1.0`. One legacy decoder uses
//!   this scale; it is preserved for byte-compatibility, never extended.
//!
//! Rounding is likewise explicit: [`Rounding::Nearest`] reproduces
//! `f32::round` (half away from zero) while [`Rounding::TowardZero`]
//! reproduces the `as i16` saturating cast (truncation). Call sites
//! must keep the variant their characterization tests pin.

use base64::prelude::*;

/// Integer wire scale for 16-bit PCM samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale16 {
    /// Divide/multiply by 32767 (`i16::MAX`): `1.0` ⇄ `32767` exactly.
    Symmetric32767,
    /// Divide/multiply by 32768 (`2^15`): `16384` ⇄ `0.5` exactly.
    Symmetric32768,
}

/// Rounding applied when quantizing `f32` to `i16`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rounding {
    /// `f32::round` (half away from zero) before the cast.
    Nearest,
    /// Bare `as i16` saturating cast (toward zero).
    TowardZero,
}

/// Divisor for a wire scale.
#[must_use]
pub fn scale_divisor(scale: Scale16) -> f32 {
    match scale {
        Scale16::Symmetric32767 => 32767.0,
        Scale16::Symmetric32768 => 32768.0,
    }
}

/// Quantize one sample: clamp to `[-1.0, 1.0]`, scale, round, cast.
///
/// NaN clamps to itself and the float-to-int cast saturates it to
/// zero (silence) under either rounding mode.
#[must_use]
pub fn f32_to_i16_sample(sample: f32, scale: Scale16, rounding: Rounding) -> i16 {
    let scaled = sample.clamp(-1.0, 1.0) * scale_divisor(scale);
    match rounding {
        Rounding::Nearest => scaled.round() as i16,
        Rounding::TowardZero => scaled as i16,
    }
}

/// Dequantize one sample under a wire scale.
#[must_use]
pub fn i16_to_f32_sample(sample: i16, scale: Scale16) -> f32 {
    f32::from(sample) / scale_divisor(scale)
}

/// Encode samples as little-endian `i16` bytes, one sample per pair.
#[must_use]
pub fn encode_i16_le(samples: &[f32], scale: Scale16, rounding: Rounding) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        out.extend_from_slice(&f32_to_i16_sample(*sample, scale, rounding).to_le_bytes());
    }
    out
}

/// Decode little-endian `i16` bytes to samples.
///
/// A trailing partial sample is dropped, never fabricated.
#[must_use]
pub fn decode_i16_le(bytes: &[u8], scale: Scale16) -> Vec<f32> {
    bytes
        .chunks_exact(2)
        .map(|pair| i16_to_f32_sample(i16::from_le_bytes([pair[0], pair[1]]), scale))
        .collect()
}

/// Encode samples as little-endian `f32` bytes, one sample per quad.
#[must_use]
pub fn encode_f32_le(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 4);
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

/// Decode little-endian `f32` bytes to samples.
///
/// A trailing partial sample is dropped, never fabricated.
#[must_use]
pub fn decode_f32_le(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|quad| f32::from_le_bytes([quad[0], quad[1], quad[2], quad[3]]))
        .collect()
}

/// Standard-base64 encode bytes for a message part.
#[must_use]
pub fn encode_base64(bytes: &[u8]) -> String {
    BASE64_STANDARD.encode(bytes)
}

/// Standard-base64 decode, trimming surrounding whitespace first.
///
/// The error is the raw [`base64::DecodeError`]: call sites keep their
/// own failure prefix so refusal strings stay verbatim contracts.
pub fn decode_base64_trimmed(text: &str) -> Result<Vec<u8>, base64::DecodeError> {
    BASE64_STANDARD.decode(text.trim())
}

/// Length of a PCM-16 mono WAV frame header in bytes.
pub const WAV_HEADER_LEN: usize = 44;

/// Assemble a mono PCM16 WAV frame: 44-byte header plus payload.
///
/// Builds the payload through [`encode_i16_le`], allocates exactly
/// `WAV_HEADER_LEN + payload.len()`, then appends [`wav_frame_header`]
/// followed by the payload. Scale and rounding stay caller-chosen so
/// each wrapper keeps its pinned convention; no second RIFF header
/// exists anywhere.
#[must_use]
pub fn encode_wav_mono_pcm16(
    samples: &[f32],
    sample_rate: u32,
    scale: Scale16,
    rounding: Rounding,
) -> Vec<u8> {
    let payload = encode_i16_le(samples, scale, rounding);
    let mut out = Vec::with_capacity(WAV_HEADER_LEN + payload.len());
    out.extend_from_slice(&wav_frame_header(sample_rate, payload.len() as u32));
    out.extend_from_slice(&payload);
    out
}

/// Append mono samples collapsed from interleaved channel frames.
///
/// Complete frames (exactly `channels` items) average in input order
/// using `f32` and append `sum / channels`. A trailing partial frame
/// contributes only to the returned remainder count — never to `out`
/// and never to another frame's sum; its partial sum is discarded.
/// `channels <= 1` (including zero) appends every converted input
/// and returns `0`.
///
/// The converter must be side-effect-free: it may observe trailing
/// partial-frame items, but nothing it returns for them is emitted.
pub fn extend_mono_from_interleaved<T>(
    input: impl IntoIterator<Item = T>,
    channels: usize,
    mut convert: impl FnMut(T) -> f32,
    out: &mut Vec<f32>,
) -> usize {
    if channels <= 1 {
        for item in input {
            out.push(convert(item));
        }
        return 0;
    }
    let mut sum = 0.0f32;
    let mut count = 0usize;
    for item in input {
        sum += convert(item);
        count += 1;
        if count == channels {
            out.push(sum / channels as f32);
            sum = 0.0;
            count = 0;
        }
    }
    count
}

/// Root mean square amplitude of one frame.
///
/// Empty input reads as digital silence (`0.0`). Otherwise the exact
/// `f32` accumulation order is the contract: sum of squares in input
/// order as `f32`, divided by the length as `f32`, then `sqrt()`.
/// Do not widen to `f64`, reorder, or Kahan-sum: callers pin bit
/// patterns through this function.
#[must_use]
pub fn root_mean_square_f32(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let energy: f32 = samples.iter().map(|sample| sample * sample).sum();
    (energy / samples.len() as f32).sqrt()
}

/// Build the 44-byte RIFF header for mono 16-bit PCM at `sample_rate`
/// carrying `data_bytes` of sample payload. Byte layout pinned by
/// `orator_s2s_wav_header_golden`: PCM format, one channel, byte rate
/// `sample_rate * 2`, block align 2, 16 bits per sample.
#[must_use]
pub fn wav_frame_header(sample_rate: u32, data_bytes: u32) -> [u8; WAV_HEADER_LEN] {
    let mut header = [0u8; WAV_HEADER_LEN];
    header[0..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&(36 + data_bytes).to_le_bytes());
    header[8..16].copy_from_slice(b"WAVEfmt ");
    header[16..20].copy_from_slice(&16u32.to_le_bytes());
    header[20..22].copy_from_slice(&1u16.to_le_bytes());
    header[22..24].copy_from_slice(&1u16.to_le_bytes());
    header[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    header[28..32].copy_from_slice(&(sample_rate * 2).to_le_bytes());
    header[32..34].copy_from_slice(&2u16.to_le_bytes());
    header[34..36].copy_from_slice(&16u16.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..44].copy_from_slice(&data_bytes.to_le_bytes());
    header
}
