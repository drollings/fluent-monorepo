//! common-core: Zero-domain generic utility crate (hashing, formatting, I/O,
//! shell, metrics, string utilities). May NOT import any guidance-* crate.

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
pub mod error;
pub mod error_context;
pub mod format;
pub mod git;
pub mod hash;
pub mod io;
pub mod metrics;
pub mod shell;
pub mod shell_parser;
pub mod string;

pub use constants::{MAX_FILE_SIZE, MAX_JSON_DEPTH, MAX_VALUE_LEN};
pub use error::{IoError, ResolverError};
pub use error_context::{ErrorContext, HeapErrorContext};
pub use format::{format_csv, format_json, format_size, parse_size, Column, Table};
pub use hash::{
    blake3_hash, blake3_hex, content_hash_with_model, fnv1a64, hash_batch, hash_file, sha256_hex,
    BatchHashResult, HashAlgorithm, HashState,
};
pub use io::{
    make_path_absolute, read_file_alloc, read_file_alloc_err, resolve_path, strip_path_prefix,
};
pub use metrics::LatencyHistogram;
pub use string::{
    contains_any, contains_any_word, contains_ident_word, contains_ignore_case, contains_word,
    first_comment_line, has_extension, is_noisy_comment, is_path_token, is_test_path,
    looks_like_identifier, lower_into, skill_name_from_ref, slugify, strip_boilerplate,
    strip_nl_prefix, trim_left, trim_right, truncate_at_sentence, STOP_WORDS,
};
