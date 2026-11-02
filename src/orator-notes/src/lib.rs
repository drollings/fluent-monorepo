#![forbid(unsafe_code)]

//! orator-notes: Live notes, map-reduce summarization with citations, and full-text memory search.

pub mod facts;
pub mod live;
pub mod search;
pub mod summarize;

pub use facts::{append_fact, roll_summary, SUMMARY_BUDGET};
pub use live::{NotesScheduler, Summarize, NOTES_MAX_SECS, NOTES_MIN_SECS};
pub use search::{extract_action_items, save_note, search_notes, NoteHit};
pub use summarize::{citations_valid, extract_seg_citations, summarize, CitedSegment, ChunkSummarize, SEG_PREFIX};
