#![forbid(unsafe_code)]

//! orator-memory: durability and knowledge.
//!
//! Three layers, one crate: `store` is the SQLite event log,
//! sessions, transcripts, and retention; `diary` is the meeting
//! diarization pipeline (segmentation, embeddings, clustering,
//! alignment); `notes` is live notes, map-reduce summarization with
//! citations, and full-text search. The pipeline terminates in
//! summarization and export — meetings that keep themselves.

pub mod diary;
pub mod notes;
pub mod store;
