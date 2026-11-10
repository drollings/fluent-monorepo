//! Speech synthesis: the Kokoro backend, the sentence streamer, the text normalizer, and DAC speech marks.

pub mod kokoro;
pub mod normalize;
pub mod pipeline;
pub mod splitter;

pub use kokoro::{
    default_paths, uniform_marks, KokoroBackend, KOKORO_MODEL_FILE, KOKORO_SAMPLE_RATE,
    KOKORO_VOICES_DIR,
};
pub use normalize::normalize_for_speech;
pub use pipeline::{chunk_tail_gap, SynthOut, SynthSeam, TtsPipeline, PREBUFFER_SECS};
pub use splitter::{
    find_cut, split_all, tail_gap_ms, Chunk, Cut, SentenceTools, CAP_CHUNK, FIRST_MAX, MIN_CHUNK,
    SENTENCE_GAP_MS, SOFT_GAP_MS,
};
