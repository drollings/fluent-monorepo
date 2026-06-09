#![deny(warnings, clippy::all, clippy::pedantic)]
#![allow(
    clippy::module_name_repetitions,
    clippy::must_use_candidate,
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::too_many_lines,
    clippy::large_stack_arrays,
    clippy::non_std_lazy_statics,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::zero_sized_map_values,
    clippy::unnecessary_literal_bound,
    clippy::cast_possible_wrap,
    clippy::unreadable_literal,
    clippy::similar_names,
    clippy::single_char_pattern,
    clippy::byte_char_slices
)]
pub mod constants;
pub mod csr_graph;
pub mod error;
pub mod error_context;
pub mod format;
pub mod freq_table;
pub mod hash;
pub mod index_header;
pub mod io;
pub mod metrics;
pub mod query_cache;
pub mod shell;
pub mod shell_parser;
pub mod string;
pub mod terminal;
pub mod tokenizer;
pub mod trigram_index;
pub mod word_index;

pub use constants::{MAX_FILE_SIZE, MAX_JSON_DEPTH, MAX_VALUE_LEN};
pub use csr_graph::CsrGraph;
pub use error::{CacheError, DbError, IoError};
pub use error_context::{ErrorContext, HeapErrorContext};
pub use format::{format_csv, format_json, format_size, parse_size, Column, Table};
pub use freq_table::{build_frequency_table, default_frequency_table, pair_weight};
pub use hash::{
    blake3_hash, blake3_hex, content_hash_with_model, fnv1a64, hash_batch, hash_file, sha256_hex,
    BatchHashResult, HashAlgorithm, HashState,
};
pub use index_header::Header as IndexHeader;
pub use io::{
    make_path_absolute, read_file_alloc, read_file_alloc_err, resolve_path, strip_path_prefix,
};
pub use metrics::LatencyHistogram;
pub use query_cache::QueryCache;
pub use string::{
    contains_any, contains_any_word, contains_ident_word, contains_ignore_case, contains_word,
    first_comment_line, has_extension, looks_like_identifier, lower_into, slugify,
    trim_left, trim_right, truncate_at_sentence, STOP_WORDS,
};
pub use terminal::{get_terminal_height, get_terminal_width, is_terminal, Color, ProgressBar};
pub use guidance_traits::{
    Describable, FieldAccess, FieldError, WorkContext, WorkError, WorkOutput, WorkUnit,
};
