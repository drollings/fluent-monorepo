//! Live notes, map-reduce summarization with citations, and full-text memory search.

pub mod export;
pub mod facts;
pub mod live;
pub mod search;
pub mod summarize;

pub use export::render_meeting_export;
pub use facts::{append_fact, roll_summary, SUMMARY_BUDGET};
pub use live::{NotesScheduler, Summarize, NOTES_MAX_SECS, NOTES_MIN_SECS};
pub use search::{extract_action_items, save_note, search_notes, NoteHit};
pub use summarize::{
    citations_valid, extract_seg_citations, summarize, ChunkSummarize, CitedSegment, SEG_PREFIX,
};
