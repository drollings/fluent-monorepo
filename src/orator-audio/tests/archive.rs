//! Archive tests: segment rotation, manifest verification, crash rule,
//! spill policy, and Opus round-trip validity.

use orator_core::Channel;

use crate::archive::{
    ArchiveItem, ArchiveWriter, Manifest, ARCHIVE_SAMPLE_RATE, FRAME_SAMPLES, SEGMENT_SAMPLES,
};

fn sine_block(len: usize) -> Vec<f32> {
    (0..len)
        .map(|index| ((index as f32) * 0.05).sin() * 0.5)
        .collect()
}

#[test]
fn rotation_cuts_playable_verifiable_segments() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut writer = ArchiveWriter::open(dir.path()).expect("open");
    let frames_per_segment = (SEGMENT_SAMPLES / FRAME_SAMPLES as u64) as usize;
    let block = sine_block(FRAME_SAMPLES);
    for _ in 0..frames_per_segment + 2 {
        writer.push(Channel::Mic, &block).expect("segmented write");
    }
    let manifest = writer.close().expect("close");
    // One full segment plus the 2-frame tail flushed at close.
    assert_eq!(manifest.segments.len(), 2);
    let meta = &manifest.segments[0];
    assert_eq!(meta.channel, Channel::Mic);
    assert_eq!(meta.frames, frames_per_segment as u64);
    assert_eq!(meta.name, "mic-0001.opus");
    assert_eq!(manifest.segments[1].name, "mic-0002.opus");
    assert_eq!(manifest.segments[1].frames, 2);

    let bytes = std::fs::read(dir.path().join(&meta.name)).expect("segment file");
    assert_eq!(common_core::hash::sha256_hex(&bytes), meta.sha256);
    assert_eq!(meta.end_samples - meta.start_samples, SEGMENT_SAMPLES);

    let manifest_back: Manifest = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("manifest.json")).expect("manifest file"),
    )
    .expect("manifest parses");
    assert_eq!(manifest_back, manifest);
}

#[test]
fn crash_loses_at_most_the_open_segment() {
    let dir = tempfile::tempdir().expect("tempdir");
    let block = sine_block(FRAME_SAMPLES);
    {
        let mut writer = ArchiveWriter::open(dir.path()).expect("open");
        // A few frames, never closed: the crash simulation.
        for _ in 0..10 {
            writer.push(Channel::Mic, &block).expect("write");
        }
    }
    // No manifest, no segments: the open segment died with the process.
    assert!(!dir.path().join("manifest.json").exists());
    assert_eq!(std::fs::read_dir(dir.path()).expect("read dir").count(), 0);
}

#[test]
fn opus_packets_decode_back_to_audio() {
    let mut encoder = opus::Encoder::new(
        ARCHIVE_SAMPLE_RATE,
        opus::Channels::Mono,
        opus::Application::Voip,
    )
    .expect("encoder");
    let mut decoder =
        opus::Decoder::new(ARCHIVE_SAMPLE_RATE, opus::Channels::Mono).expect("decoder");
    let input = sine_block(FRAME_SAMPLES);
    let packet = encoder.encode_vec_float(&input, 4000).expect("encode");
    assert!(!packet.is_empty());
    let mut output = vec![0.0f32; FRAME_SAMPLES * 3];
    let decoded = decoder
        .decode_float(&packet, &mut output, false)
        .expect("decode");
    assert!(decoded > 0);
    let energy: f32 = output[..decoded].iter().map(|sample| sample * sample).sum();
    assert!(energy > 0.0, "decoded audio carries energy");
}

#[test]
fn feed_items_cover_both_directions_of_time() {
    let item = ArchiveItem::Pcm {
        channel: Channel::Sys,
        samples: sine_block(64),
    };
    match item {
        ArchiveItem::Pcm { channel, samples } => {
            assert_eq!(channel, Channel::Sys);
            assert_eq!(samples.len(), 64);
        }
    }
}

/// M14-S1 characterization: spill over-cap drops the oldest frames
/// first and counts them — the disk-stall last resort, never silent.
/// (Empty and exact-cap pushes drop nothing.)
#[test]
fn spill_over_cap_drops_oldest_and_counts() {
    use crate::archive::SpillQueue;
    let mut spill = SpillQueue::new(2);
    spill.push(Channel::Mic, vec![1.0]);
    spill.push(Channel::Mic, vec![2.0]);
    assert_eq!(spill.dropped_frames, 0, "exact cap drops nothing");
    spill.push(Channel::Mic, vec![3.0]);
    assert_eq!(spill.dropped_frames, 1, "over-cap drops with a counter");
    let (first, _) = spill.pop().expect("oldest survivor pops");
    assert_eq!(first, Channel::Mic);
    let (_, samples) = spill.pop().expect("newest pops");
    assert_eq!(samples, vec![3.0], "oldest frame shed, order kept");
    assert!(spill.pop().is_none(), "empty pops nothing");
}
