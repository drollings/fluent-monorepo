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
