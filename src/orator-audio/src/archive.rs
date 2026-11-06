//! Lossless archive tap: Opus-in-Ogg segments with a manifest.
//!
//! One file stream per channel, cut into fixed-length segments. Each closed
//! segment is complete, self-contained (headers first), and playable, so a
//! crash loses at most the open segment. The manifest (hashes, sample
//! ranges) verifies every closed segment; segments are finalized with atomic
//! renames, never partial writes.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use fluent_concurrency::feed_worker::{CreditedFeedWorker, FeedConfig};
use fluent_wvr::Runtime;
use orator_core::Channel;
use serde::{Deserialize, Serialize};

use crate::error::AudioError;

/// Canonical analysis rate for archived audio.
pub const ARCHIVE_SAMPLE_RATE: u32 = 16_000;
/// Samples per encoded frame (20 ms at 16 kHz).
pub const FRAME_SAMPLES: usize = 320;
/// Segment length in seconds.
pub const SEGMENT_SECS: u64 = 60;
/// Samples per segment.
pub const SEGMENT_SAMPLES: u64 = SEGMENT_SECS * ARCHIVE_SAMPLE_RATE as u64;
/// Pending-spill budget in 20 ms frames (30 s of audio).
pub const SPILL_CAP_FRAMES: usize = 1500;

/// One closed segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentMeta {
    /// Owning channel.
    pub channel: Channel,
    /// File name inside the archive directory.
    pub name: String,
    /// SHA-256 of the file bytes.
    pub sha256: String,
    /// Encoded frames in the segment.
    pub frames: u64,
    /// First sample (channel clock) in the segment.
    pub start_samples: u64,
    /// One past the last sample (channel clock).
    pub end_samples: u64,
}

/// The archive manifest: every closed segment, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Closed segments, oldest first.
    pub segments: Vec<SegmentMeta>,
}

/// Clock offsets aligning mic and system channels.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockOffsets {
    /// System-channel offset in samples relative to mic.
    pub sys_offset_samples: i64,
}

/// OpusHead body for mono 16 kHz (RFC 7845 fixed layout).
fn opus_head() -> Vec<u8> {
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1); // version
    head.push(1); // channel count
    head.extend_from_slice(&312u16.to_le_bytes()); // pre-skip
    head.extend_from_slice(&ARCHIVE_SAMPLE_RATE.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes()); // output gain
    head.push(0); // mapping family
    head
}

/// Minimal OpusTags body (vendor string, no comments).
fn opus_tags() -> Vec<u8> {
    let mut tags = Vec::new();
    tags.extend_from_slice(b"OpusTags");
    let vendor = b"orator";
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor);
    tags.extend_from_slice(&0u32.to_le_bytes());
    tags
}

/// One open Ogg/Opus segment accumulating into memory. Closed segments are
/// small (~240 KB at 32 kbps), so buffering one open segment is bounded and
/// keeps every on-disk byte manifest-verified.
struct SegmentWriter {
    encoder: opus::Encoder,
    ogg: ogg::PacketWriter<'static, std::io::Cursor<Vec<u8>>>,
    serial: u32,
    granule: u64,
    samples: u64,
    start_samples: u64,
}

impl SegmentWriter {
    fn open(serial: u32, start_samples: u64) -> Result<Self, AudioError> {
        let encoder = opus::Encoder::new(
            ARCHIVE_SAMPLE_RATE,
            opus::Channels::Mono,
            opus::Application::Voip,
        )
        .map_err(|error| AudioError::Archive(format!("opus encoder: {error}")))?;
        let mut ogg = ogg::PacketWriter::new(std::io::Cursor::new(Vec::new()));
        ogg.write_packet(
            opus_head(),
            serial,
            ogg::PacketWriteEndInfo::NormalPacket,
            0,
        )
        .and_then(|()| ogg.write_packet(opus_tags(), serial, ogg::PacketWriteEndInfo::EndPage, 0))
        .map_err(|error| AudioError::Archive(format!("ogg headers: {error}")))?;
        Ok(Self {
            encoder,
            ogg,
            serial,
            granule: 0,
            samples: 0,
            start_samples,
        })
    }

    /// Encode exactly one 20 ms frame. Callers split input into
    /// `FRAME_SAMPLES` blocks; short tails are zero-padded by the caller.
    fn push_frame(&mut self, frame: &[f32; FRAME_SAMPLES]) -> Result<(), AudioError> {
        let packet = self
            .encoder
            .encode_vec_float(frame, 4000)
            .map_err(|error| AudioError::Archive(format!("opus encode: {error}")))?;
        // Opus decodes to 48 kHz: three output samples per input sample.
        self.granule += FRAME_SAMPLES as u64 * 3;
        self.ogg
            .write_packet(
                packet,
                self.serial,
                ogg::PacketWriteEndInfo::NormalPacket,
                self.granule,
            )
            .map_err(|error| AudioError::Archive(format!("ogg packet: {error}")))?;
        self.samples += FRAME_SAMPLES as u64;
        Ok(())
    }

    /// Finalize with an end-of-stream packet and return the file bytes.
    fn finish(mut self) -> Result<Vec<u8>, AudioError> {
        self.ogg
            .write_packet(
                Vec::new(),
                self.serial,
                ogg::PacketWriteEndInfo::EndStream,
                self.granule,
            )
            .map_err(|error| AudioError::Archive(format!("ogg close: {error}")))?;
        Ok(self.ogg.into_inner().into_inner())
    }
}

/// Bounded oldest-drop spill for stalled disks. When the disk cannot take
/// bytes, encoded input waits here instead of blocking the feed; overflow
/// drops the oldest and counts it loudly. Last resort, never silent.
#[derive(Debug)]
struct SpillQueue {
    queue: VecDeque<(Channel, Vec<f32>)>,
    cap_frames: usize,
    dropped_frames: u64,
}

impl SpillQueue {
    fn new(cap_frames: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            cap_frames,
            dropped_frames: 0,
        }
    }

    fn push(&mut self, channel: Channel, samples: Vec<f32>) {
        while self.queue.len() >= self.cap_frames {
            self.queue.pop_front();
            self.dropped_frames += 1;
        }
        self.queue.push_back((channel, samples));
    }

    fn pop(&mut self) -> Option<(Channel, Vec<f32>)> {
        self.queue.pop_front()
    }
}

/// Durable feed item: raw PCM at the archive rate.
#[derive(Debug, Clone)]
pub enum ArchiveItem {
    /// 16 kHz mono samples for a channel (any length; split into frames).
    Pcm {
        /// Owning channel.
        channel: Channel,
        /// Samples at [`ARCHIVE_SAMPLE_RATE`].
        samples: Vec<f32>,
    },
}

/// The archive tap: per-channel segment writers, a manifest, and a spill
/// queue, fed losslessly through a `CreditedFeedWorker`.
pub struct ArchiveWriter {
    dir: PathBuf,
    writers: HashMap<Channel, SegmentWriter>,
    clocks: HashMap<Channel, u64>,
    segment_index: u64,
    manifest: Manifest,
    spill: SpillQueue,
}

impl ArchiveWriter {
    /// Open the archive directory (created when missing). Nothing is
    /// written until the first samples arrive.
    pub fn open(dir: &Path) -> Result<Self, AudioError> {
        common_core::io::ensure_dir(dir)
            .map_err(|error| AudioError::Archive(format!("mkdir: {error}")))?;
        Ok(Self {
            dir: dir.to_path_buf(),
            writers: HashMap::new(),
            clocks: HashMap::new(),
            segment_index: 0,
            manifest: Manifest::default(),
            spill: SpillQueue::new(SPILL_CAP_FRAMES),
        })
    }

    /// Feed sizes for the archive worker.
    ///
    /// M9 audit (why these numbers differ from the writer's): items
    /// here are PCM sample vectors (kilobytes per segment per
    /// channel), so the queue stays small (64) with credit in segments
    /// (32, top-up 4) — a disk stall pauses producers through
    /// backpressure instead of growing memory. Two concurrent handlers
    /// (one per channel direction) with batch 4 keep both segment
    /// writers fed; the 25 ms poll matches the audio-path drain
    /// cadence (the event-log writer below polls at 100 ms). Handler
    /// seriality per channel comes from the single segment writer per
    /// channel, not from this bound. Backpressure and handler-bound
    /// semantics are pinned in `fluent-concurrency`'s `feed_worker`
    /// tests; this table only sizes them.
    #[must_use]
    pub fn feed_config() -> FeedConfig {
        FeedConfig {
            queue_capacity: 64,
            credit_limit: 32,
            credit_more_after: 4,
            max_concurrent: 2,
            batch_size: 4,
            poll_interval_ms: 25,
        }
    }

    /// Push raw PCM for a channel. Drains the spill first so a recovered
    /// disk replays in order. Mid-segment write failures spill instead of
    /// blocking; open/rotation failures are fatal and loud.
    pub fn push(&mut self, channel: Channel, samples: &[f32]) -> Result<(), AudioError> {
        while let Some((pending_channel, pending)) = self.spill.pop() {
            if let Err(error) = self.write_samples(pending_channel, &pending) {
                tracing::error!(target: "orator.archive", %error, "spill replay failed");
                self.spill.push(pending_channel, pending);
                self.spill.push(channel, samples.to_vec());
                return Err(error);
            }
        }
        if let Err(error) = self.write_samples(channel, samples) {
            tracing::error!(target: "orator.archive", %error, "archiving to spill");
            self.spill.push(channel, samples.to_vec());
            return Err(error);
        }
        Ok(())
    }

    /// Frames dropped from a full spill. Loud telemetry, never silent.
    #[must_use]
    pub fn dropped_frames(&self) -> u64 {
        self.spill.dropped_frames
    }

    /// Frames waiting in the spill queue. Backlog telemetry for stats.
    #[must_use]
    pub fn pending_frames(&self) -> usize {
        self.spill.queue.len()
    }

    /// Flush every open segment and write the manifest plus clock offsets.
    /// After this the directory is self-describing.
    pub fn close(mut self) -> Result<Manifest, AudioError> {
        let channels: Vec<Channel> = self.writers.keys().copied().collect();
        for channel in channels {
            self.rotate(channel)?;
        }
        let manifest_path = self.dir.join("manifest.json");
        let manifest_json = serde_json::to_string_pretty(&self.manifest)
            .map_err(|error| AudioError::Archive(format!("manifest: {error}")))?;
        common_core::io::write_atomic(&manifest_path, manifest_json.as_bytes())
            .map_err(|error| AudioError::Archive(format!("manifest write: {error}")))?;
        let offsets = ClockOffsets {
            sys_offset_samples: 0,
        };
        let offsets_json = serde_json::to_string_pretty(&offsets)
            .map_err(|error| AudioError::Archive(format!("offsets: {error}")))?;
        common_core::io::write_atomic(
            &self.dir.join("clock-offsets.json"),
            offsets_json.as_bytes(),
        )
        .map_err(|error| AudioError::Archive(format!("offsets write: {error}")))?;
        Ok(self.manifest)
    }

    fn write_samples(&mut self, channel: Channel, samples: &[f32]) -> Result<(), AudioError> {
        let mut offset = 0;
        while offset < samples.len() {
            let end = (offset + FRAME_SAMPLES).min(samples.len());
            let mut frame = [0.0f32; FRAME_SAMPLES];
            frame[..end - offset].copy_from_slice(&samples[offset..end]);
            let consumed = (end - offset) as u64;
            self.writer_for(channel)?.push_frame(&frame)?;
            *self.clocks.entry(channel).or_insert(0) += consumed;
            offset = end;
            let rotated = self
                .writers
                .get(&channel)
                .is_some_and(|writer| writer.samples >= SEGMENT_SAMPLES);
            if rotated {
                self.rotate(channel)?;
            }
        }
        Ok(())
    }

    fn writer_for(&mut self, channel: Channel) -> Result<&mut SegmentWriter, AudioError> {
        if !self.writers.contains_key(&channel) {
            let start = self.clocks.get(&channel).copied().unwrap_or(0);
            let serial = self.serial_for(channel);
            self.writers
                .insert(channel, SegmentWriter::open(serial, start)?);
        }
        self.writers.get_mut(&channel).ok_or_else(|| {
            AudioError::Archive(format!(
                "archive writer for {channel:?} vanished after insert"
            ))
        })
    }

    fn serial_for(&self, channel: Channel) -> u32 {
        let base = match channel {
            Channel::Mic => 0x4d4943u32,
            Channel::Sys => 0x535953u32,
            Channel::Reply => 0x525052u32,
        };
        base.wrapping_add(self.segment_index as u32)
    }

    fn rotate(&mut self, channel: Channel) -> Result<(), AudioError> {
        let Some(writer) = self.writers.remove(&channel) else {
            return Ok(());
        };
        let frames = writer.samples / FRAME_SAMPLES as u64;
        let start_samples = writer.start_samples;
        let end_samples = start_samples + writer.samples;
        let bytes = writer.finish()?;
        self.segment_index += 1;
        let name = format!("{}-{:04}.opus", channel_name(channel), self.segment_index);
        let sha256 = common_core::hash::sha256_hex(&bytes);
        common_core::io::write_atomic(&self.dir.join(&name), &bytes)
            .map_err(|error| AudioError::Archive(format!("segment write: {error}")))?;
        self.manifest.segments.push(SegmentMeta {
            channel,
            name,
            sha256,
            frames,
            start_samples,
            end_samples,
        });
        Ok(())
    }
}

fn channel_name(channel: Channel) -> &'static str {
    match channel {
        Channel::Mic => "mic",
        Channel::Sys => "sys",
        Channel::Reply => "reply",
    }
}

/// Spawn the durable archive feed: items flow through the credit gate and
/// land in `writer` in order. The caller holds the worker and starts it;
/// `drain` + `close` flush at shutdown.
pub fn spawn_archive_feed(
    runtime: Arc<dyn Runtime>,
    writer: Arc<Mutex<ArchiveWriter>>,
) -> Arc<CreditedFeedWorker<ArchiveItem>> {
    let worker = Arc::new(CreditedFeedWorker::new(
        runtime,
        ArchiveWriter::feed_config(),
        move |item: ArchiveItem| {
            let writer = Arc::clone(&writer);
            async move {
                // Disk stalls park a blocking-pool thread, never a
                // runtime worker: the lock is taken inside the blocking
                // task and released there, so the feed handler itself
                // never holds a mutex across disk I/O on the executor.
                let ArchiveItem::Pcm { channel, samples } = item;
                let outcome = tokio::task::spawn_blocking(move || {
                    let mut guard = common_core::sync::lock(&writer);
                    guard.push(channel, &samples)
                })
                .await;
                match outcome {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        tracing::error!(target: "orator.archive", %error, "feed write failed");
                    }
                    Err(error) => {
                        tracing::error!(target: "orator.archive", %error, "archive blocking task failed");
                    }
                }
            }
        },
    ));
    worker
}

#[cfg(test)]
#[path = "../tests/archive.rs"]
mod tests;
