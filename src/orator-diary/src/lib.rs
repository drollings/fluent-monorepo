#![forbid(unsafe_code)]

//! orator-diary: Meeting diarization: segmentation, speaker embeddings, clustering, alignment, and naming.

pub mod align;
pub mod cluster;
pub mod embed;
pub mod pipeline;
pub mod segment;

pub use align::{align_words, speaker_name, NameSuggestions};
pub use cluster::{cluster, diarization_error, smooth, DiarConfig};
pub use embed::{batch_embed, EmbedSeam, ScriptSeam, TEST_DIMS};
pub use pipeline::{MeetingPipeline, StepState, STEPS};
pub use segment::{segment, Segment, HOP_SAMPLES, WINDOW_SAMPLES};
