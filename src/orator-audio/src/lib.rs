#![forbid(unsafe_code)]

//! orator-audio: PipeWire/cpal capture and playout, resampling, the drop-oldest jitter buffer, mixing and ducking, and the lossless archive tap.

pub mod archive;
pub mod backend;
pub mod codec;
pub mod doctor;
pub mod envelope;
pub mod error;
pub mod jitter;
pub mod merge;
pub mod playback;
pub mod resample;
pub mod source;
pub mod wav;

pub use archive::{spawn_archive_feed, ArchiveItem, ArchiveWriter, Manifest, SegmentMeta};
pub use backend::{list_input_devices, CpalBackend, DeviceInfo};
pub use codec::{Codec, PcmCodec};
pub use doctor::{doctor_report, DoctorReport};
pub use envelope::{
    apply_fade_out, db_to_linear, duck_gain_at, duck_gain_at_with, CUT_FADE_MS, DUCK_DEPTH_DB,
    DUCK_RAMP_MS,
};
pub use error::AudioError;
pub use jitter::JitterBuffer;
pub use merge::{MergeQueue, WordRecord};
pub use playback::{play_blocking, play_blocking_abortable, StreamPlayer};
pub use resample::{resample_linear, RubatoResampler};
pub use source::{probe_device, supervise_driver, DriverError, HotPlug, SourceKind};
pub use wav::{decode_wav_bytes, encode_wav_mono, read_wav_mono, WavPcm};
