//! WAV tests: hand-built fixtures, exact samples, clean rejections.

use crate::wav::read_wav_mono;

fn wav_bytes(channels: u16, rate: u32, pcm16: &[i16]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&36u32.to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
    bytes.extend_from_slice(&(channels * 2).to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(pcm16.len() as u32 * 2).to_le_bytes());
    for sample in pcm16 {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

fn wav_bytes_f32(channels: u16, rate: u32, samples: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&36u32.to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&3u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * u32::from(channels) * 4).to_le_bytes());
    bytes.extend_from_slice(&(channels * 4).to_le_bytes());
    bytes.extend_from_slice(&32u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(samples.len() as u32 * 4).to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

fn write_fixture(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).expect("fixture");
    // Leak the dir: the test reads the file before exiting.
    let leaked = dir.keep();
    leaked.join(name)
}

#[test]
fn mono_pcm16_decodes_exactly() {
    let path = write_fixture("mono.wav", &wav_bytes(1, 16_000, &[0, 16_384, -16_384]));
    let pcm = read_wav_mono(&path).expect("parses");
    assert_eq!(pcm.sample_rate, 16_000);
    assert_eq!(pcm.samples.len(), 3);
    assert!((pcm.samples[1] - 0.5).abs() < 1e-4);
}

#[test]
fn stereo_mixes_down() {
    let path = write_fixture("stereo.wav", &wav_bytes(2, 8_000, &[16_384, -16_384]));
    let pcm = read_wav_mono(&path).expect("parses");
    assert_eq!(pcm.samples.len(), 1);
    assert!(pcm.samples[0].abs() < 1e-4);
}

#[test]
fn garbage_is_rejected() {
    let path = write_fixture("nope.wav", b"definitely not audio");
    read_wav_mono(&path).expect_err("non-wav must fail");
}

/// M1-S1 characterization: exact decode scale (÷ 32767, i.e. i16::MAX —
/// NOT the daemon Transcribe decoder's ÷ 32768). Pins the rail and
/// half-scale vectors the shared primitive must reproduce.
#[test]
fn orator_audio_wav_scale_vectors() {
    let path = write_fixture("scale.wav", &wav_bytes(1, 16_000, &[32767, -32768, 16384]));
    let pcm = read_wav_mono(&path).expect("parses");
    assert!(
        (pcm.samples[0] - 1.0).abs() < f32::EPSILON,
        "rail decodes to exactly 1.0"
    );
    assert!(
        (pcm.samples[1] - f32::from(-32768i16) / f32::from(i16::MAX)).abs() < f32::EPSILON,
        "floor overshoots -1.0 under the 32767 scale"
    );
    assert!(
        (pcm.samples[2] - f32::from(16384i16) / f32::from(i16::MAX)).abs() < f32::EPSILON,
        "half-scale is exact under the 32767 scale"
    );
}

/// Trailing partial channel frames are ignored, never audio: stereo
/// PCM16 with three samples yields one averaged frame and drops the tail.
#[test]
fn stereo_pcm16_ignores_a_trailing_partial_frame() {
    use crate::wav::decode_wav_bytes;
    let bytes = wav_bytes(2, 16_000, &[16384, 16384, 16384]);
    let pcm = decode_wav_bytes(&bytes).expect("parses");
    assert_eq!(pcm.samples.len(), 1);
    assert!((pcm.samples[0] - 0.5).abs() < 1e-4);
}

/// Same contract for IEEE-float: the trailing sample never reaches the
/// mono output, even when its value is loud.
#[test]
fn stereo_float_ignores_a_trailing_partial_frame() {
    use crate::wav::decode_wav_bytes;
    let bytes = wav_bytes_f32(2, 16_000, &[0.5, 0.25, 99.0]);
    let pcm = decode_wav_bytes(&bytes).expect("parses");
    assert_eq!(pcm.samples, vec![0.375]);
}

/// Multi-frame stereo averages stay exact across frames.
#[test]
fn stereo_pcm16_averages_every_frame_in_order() {
    use crate::wav::decode_wav_bytes;
    let bytes = wav_bytes(2, 16_000, &[32767, 32767, -32768, -32768]);
    let pcm = decode_wav_bytes(&bytes).expect("parses");
    assert_eq!(pcm.samples.len(), 2);
    assert!((pcm.samples[0] - 1.0).abs() < f32::EPSILON);
    assert!((pcm.samples[1] - f32::from(-32768i16) / f32::from(i16::MAX)).abs() < f32::EPSILON);
}
/// M2-S2 characterization: the new writer mirrors the parser and the
/// s2s frame encoder — same 44-byte header, same truncating payload —
/// so `decode(encode(x))` stays within one LSB of `x`.
#[test]
fn orator_audio_wav_writer_round_trips() {
    use crate::wav::{decode_wav_bytes, encode_wav_mono};
    let samples = vec![0.0f32, 1.0, -1.0, 0.5, -0.25];
    let bytes = encode_wav_mono(&samples, 16_000);
    assert_eq!(bytes.len(), 44 + samples.len() * 2);
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[36..40], b"data");
    let back = decode_wav_bytes(&bytes).expect("writer output parses");
    assert_eq!(back.sample_rate, 16_000);
    let worst = samples
        .iter()
        .zip(back.samples.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(worst < 1e-4, "writer round-trips within one LSB: {worst}");
    assert_eq!(
        encode_wav_mono(&[], 24_000).len(),
        44,
        "empty is header-only"
    );
}
