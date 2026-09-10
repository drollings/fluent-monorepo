//! zvec-grep tested constants, ported verbatim (P0 contracts, no I/O).
//!
//! Every value below is a behavior-preserving port of a zvec-grep constant or
//! pure helper. Sources are cited per item. Values change only with measured
//! evidence (roadmap "Tested constants" rule).

/// RRF fusion depth: `score = Σ 1/(K + rank)`.
/// Source: `src/engine/pipeline/search/index.ts` (`RRF_K = 60`).
pub const RRF_K: f64 = 60.0;

/// Adaptive recall: initial per-route depth.
/// Source: `pipeline/search/index.ts` (`RECALL_INITIAL_DEPTH = 200`).
pub const RECALL_INITIAL_DEPTH: usize = 200;

/// Adaptive recall: maximum per-route depth.
/// Source: `pipeline/search/index.ts` (`RECALL_MAX_DEPTH = 2000`).
pub const RECALL_MAX_DEPTH: usize = 2000;

/// Adaptive recall: depth growth factor per pass.
/// Source: `pipeline/search/index.ts` (`RECALL_GROWTH_FACTOR = 2`).
pub const RECALL_GROWTH_FACTOR: usize = 2;

/// Adaptive recall: target factor applied to the request limit.
/// Source: `pipeline/search/index.ts` (`RECALL_TARGET_FACTOR = 5`).
pub const RECALL_TARGET_FACTOR: usize = 5;

/// Adaptive recall: minimum target candidate count.
/// Source: `pipeline/search/index.ts` (`RECALL_MIN_TARGET_CANDIDATES = 50`).
pub const RECALL_MIN_TARGET_CANDIDATES: usize = 50;

/// Default result limit when the plan carries none.
/// Source: `pipeline/search/index.ts` (`DEFAULT_LIMIT = 7`).
pub const DEFAULT_LIMIT: usize = 7;

/// Hard cap on the MCP result limit.
/// Source: `src/engine/types.ts` (`limit`, MCP cap 50).
pub const MCP_LIMIT_CAP: usize = 50;

/// Adaptive-recall target: `max(limit × 5, 50)`.
/// Source: `pipeline/search/index.ts` (`recallTargetCandidateCount`).
#[must_use]
pub fn recall_target_candidate_count(limit: usize) -> usize {
    limit
        .saturating_mul(RECALL_TARGET_FACTOR)
        .max(RECALL_MIN_TARGET_CANDIDATES)
}

/// Default code/markdown chunk size in characters.
/// Source: `extraction/code/extractor.ts` (`DEFAULT_CODE_CHUNK_CHARS = 3600`).
pub const CODE_CHUNK_CHARS: usize = 3600;

/// Default chunk overlap in characters (15% of [`CODE_CHUNK_CHARS`]).
/// Source: `extraction/code/extractor.ts` (`DEFAULT_CODE_CHUNK_OVERLAP_CHARS = 540`).
pub const CODE_CHUNK_OVERLAP_CHARS: usize = 540;

/// Chunk overlap as a percentage of the chunk size (15%).
/// Source: `pipeline/indexing/input-budget.ts` (`CHUNK_OVERLAP_PERCENT = 15`).
pub const CHUNK_OVERLAP_PERCENT: u64 = 15;

/// Metadata-prefix budget as a percentage of the chunk (25%).
/// Source: `extraction/vector-content.ts` (`MAX_METADATA_BUDGET_RATIO = 0.25`).
pub const METADATA_BUDGET_PERCENT: u64 = 25;

/// Metadata-prefix budget in characters: `floor(max × 25%)`.
/// Mirrors `metadataBudget` in `extraction/vector-content.ts`
/// (`None` in, `None` out — the TS `undefined` case).
#[must_use]
pub fn metadata_budget_chars(max_chars: Option<usize>) -> Option<usize> {
    max_chars.map(|max| (max as u64 * METADATA_BUDGET_PERCENT / 100) as usize)
}

/// Outline fragment header cap in characters.
/// Source: `extraction/code/extractor.ts` (outline builder, `maxChars = 1200`).
pub const OUTLINE_HEADER_MAX_CHARS: usize = 1200;

/// Outline fragment header cap in lines.
/// Source: `extraction/code/extractor.ts` (outline builder, `maxLines = 24`).
pub const OUTLINE_HEADER_MAX_LINES: usize = 24;

/// Outline member-bullet cap.
/// Source: `extraction/code/extractor.ts` (`OUTLINE_MAX_MEMBERS = 32`).
pub const OUTLINE_MAX_MEMBERS: usize = 32;

/// Outline call-name cap (functions).
/// Source: `extraction/code/extractor.ts` (`OUTLINE_MAX_CALLS = 24`).
pub const OUTLINE_MAX_CALLS: usize = 24;

/// Outline line truncation width.
/// Source: `extraction/code/extractor.ts` (`OUTLINE_MAX_LINE_CHARS = 180`).
pub const OUTLINE_MAX_LINE_CHARS: usize = 180;

/// Outline member-walk depth cap.
/// Source: `extraction/code/extractor.ts` (`depth > 12` guard).
pub const OUTLINE_WALK_MAX_DEPTH: u32 = 12;

/// Default characters-per-100-tokens estimate for ordinary text (185).
/// Source: `pipeline/indexing/input-budget.ts`.
pub const CHARS_PER_100_TOKENS_DEFAULT: u64 = 185;

/// Conservative characters-per-100-tokens estimate for token-dense text (100).
/// Source: `pipeline/indexing/input-budget.ts`.
pub const CHARS_PER_100_TOKENS_DENSE: u64 = 100;

/// Token-dense detector window size (16 KiB in chars).
/// Source: `pipeline/indexing/input-budget.ts`.
pub const TOKEN_DENSE_WINDOW_CHARS: usize = 16 * 1024;

/// Token-dense detector window step (8 KiB in chars).
/// Source: `pipeline/indexing/input-budget.ts`.
pub const TOKEN_DENSE_WINDOW_STEP_CHARS: usize = 8 * 1024;

/// Token-dense detector threshold: 30% non-ASCII (non-letter, non-blank).
/// Source: `pipeline/indexing/input-budget.ts` (`TOKEN_DENSE_PERCENT = 30`).
pub const TOKEN_DENSE_NON_ASCII_PERCENT: usize = 30;

/// Resolved chunk sizing. `None` fields mirror the TS `{}` (no model limit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkOptions {
    /// Maximum chunk size in characters.
    pub max_chunk_chars: Option<usize>,
    /// Chunk overlap in characters.
    pub chunk_overlap_chars: Option<usize>,
}

/// Model-aware chunk resize (`indexChunkOptions`).
/// Source: `pipeline/indexing/input-budget.ts` — ported verbatim, operating
/// on Unicode scalar values where the TS operates on UTF-16 code units
/// (identical for BMP text; P0 goldens pin the ASCII/CJK cases).
#[must_use]
pub fn index_chunk_options(
    max_input_tokens: Option<u64>,
    text: Option<&str>,
) -> ChunkOptions {
    let Some(max_input_tokens) = max_input_tokens else {
        return ChunkOptions { max_chunk_chars: None, chunk_overlap_chars: None };
    };
    let chars_per_100 = if is_token_dense_text(text, max_input_tokens) {
        CHARS_PER_100_TOKENS_DENSE
    } else {
        CHARS_PER_100_TOKENS_DEFAULT
    };
    let max_chunk_chars = (max_input_tokens * chars_per_100 / 100) as usize;
    let chunk_overlap_chars = (max_chunk_chars as u64 * CHUNK_OVERLAP_PERCENT / 100) as usize;
    ChunkOptions { max_chunk_chars: Some(max_chunk_chars), chunk_overlap_chars: Some(chunk_overlap_chars) }
}

/// Token-dense detector (`isTokenDenseText`).
/// Source: `pipeline/indexing/input-budget.ts`.
#[must_use]
pub fn is_token_dense_text(text: Option<&str>, max_input_tokens: u64) -> bool {
    let Some(text) = text else { return false };
    let chars: Vec<char> = text.chars().collect();
    if chars.len() as u64 <= max_input_tokens * CHARS_PER_100_TOKENS_DENSE / 100 {
        return false;
    }
    if chars.len() <= TOKEN_DENSE_WINDOW_CHARS {
        return is_token_dense_window(&chars, 0, chars.len());
    }
    let last_window_start = chars.len() - TOKEN_DENSE_WINDOW_CHARS;
    let mut start = 0;
    while start <= last_window_start {
        if is_token_dense_window(&chars, start, TOKEN_DENSE_WINDOW_CHARS) {
            return true;
        }
        start += TOKEN_DENSE_WINDOW_STEP_CHARS;
    }
    last_window_start % TOKEN_DENSE_WINDOW_STEP_CHARS != 0
        && is_token_dense_window(&chars, last_window_start, TOKEN_DENSE_WINDOW_CHARS)
}

/// Single-window density check (`isTokenDenseWindow`).
/// A char counts as dense unless it is an ASCII letter, space, or tab.
fn is_token_dense_window(chars: &[char], start: usize, length: usize) -> bool {
    // ceil(length * 30 / 100), mirroring Math.ceil in the TS source.
    let required = (length * TOKEN_DENSE_NON_ASCII_PERCENT).div_ceil(100);
    let mut dense = 0_usize;
    for c in chars.iter().skip(start).take(length) {
        let ascii_letter = c.is_ascii_alphabetic();
        if !ascii_letter && *c != ' ' && *c != '\t' {
            dense += 1;
            if dense >= required {
                return true;
            }
        }
    }
    false
}

/// `ChangeSet` burst-compaction budget: above this many paths, compact to
/// directory scope. Roadmap-authoritative value (P3 ports the behavior).
pub const CHANGE_SET_PATH_BUDGET: usize = 1000;

/// Watcher debounce delay.
/// Source: `src/daemon/watch-manager.ts` (`debounceMs ?? 750`).
pub const WATCH_DEBOUNCE_MS: u64 = 750;

/// Watcher max-wait (forced flush) delay.
/// Source: `src/daemon/watch-manager.ts` (`maxWaitMs ?? 5_000`).
pub const WATCH_MAX_WAIT_MS: u64 = 5_000;

/// Watcher resume-check interval (30 s).
/// Source: `src/daemon/watch-manager.ts` (`resumeCheckIntervalMs ?? 30_000`).
pub const WATCH_RESUME_CHECK_INTERVAL_MS: u64 = 30_000;

/// Watcher resume drift threshold (90 s).
/// Source: roadmap freshness constants (resume 30 s–90 s).
pub const WATCH_RESUME_THRESHOLD_MS: u64 = 90_000;

/// Hourly full-reconcile probe interval — watchers lie, the probe stays.
/// Source: `src/daemon/watch-manager.ts` (`reconcileIntervalMs ?? 60 * 60_000`).
pub const WATCH_RECONCILE_INTERVAL_MS: u64 = 60 * 60_000;

/// Idle watcher eviction timeout (4 h; 0 disables).
/// Source: `src/daemon/config.ts` (`DEFAULT_WATCHER_IDLE_TIMEOUT_MS = 4 * 60 * 60_000`).
pub const WATCHER_IDLE_TIMEOUT_MS: u64 = 4 * 60 * 60_000;

/// Embed retry: transient failures, max attempts.
/// Source: `pipeline/indexing/index.ts` (`EMBEDDING_TRANSIENT_MAX_RETRIES = 3`).
pub const EMBEDDING_TRANSIENT_MAX_RETRIES: u32 = 3;

/// Embed retry: transient base delay (500 ms).
/// Source: `pipeline/indexing/index.ts`.
pub const EMBEDDING_TRANSIENT_RETRY_BASE_DELAY_MS: u64 = 500;

/// Embed retry: transient delay cap (8 s).
/// Source: `pipeline/indexing/index.ts`.
pub const EMBEDDING_TRANSIENT_RETRY_MAX_DELAY_MS: u64 = 8_000;

/// Embed retry: rate-limit failures, max attempts (6).
/// Source: `pipeline/indexing/index.ts` (`EMBEDDING_RATE_LIMIT_MAX_RETRIES = 6`).
pub const EMBEDDING_RATE_LIMIT_MAX_RETRIES: u32 = 6;

/// Embed retry: rate-limit base delay (2000 ms).
/// Source: `pipeline/indexing/index.ts`.
pub const EMBEDDING_RATE_LIMIT_RETRY_BASE_DELAY_MS: u64 = 2_000;

/// Embed retry: rate-limit delay cap (30 s).
/// Source: `pipeline/indexing/index.ts`.
pub const EMBEDDING_RATE_LIMIT_RETRY_MAX_DELAY_MS: u64 = 30_000;

/// Embed retry jitter (500 ms, `Retry-After` honored).
/// Source: `pipeline/indexing/index.ts` (`EMBEDDING_RETRY_JITTER_MS = 500`).
pub const EMBEDDING_RETRY_JITTER_MS: u64 = 500;

/// Storage upsert batch size (1024 docs).
/// Source: `storage/zvec.ts` (`ZVEC_UPSERT_BATCH_SIZE = 1024`).
pub const ZVEC_UPSERT_BATCH_SIZE: usize = 1024;

/// Storage open retry attempts (8).
/// Source: `storage/zvec.ts` (`ZVEC_OPEN_RETRY_ATTEMPTS = 8`).
pub const ZVEC_OPEN_RETRY_ATTEMPTS: u32 = 8;

/// Storage open retry base delay (100 ms).
/// Source: `storage/zvec.ts`.
pub const ZVEC_OPEN_RETRY_BASE_DELAY_MS: u64 = 100;

/// Storage open retry delay cap (1000 ms).
/// Source: `storage/zvec.ts`.
pub const ZVEC_OPEN_RETRY_MAX_DELAY_MS: u64 = 1_000;

/// Storage open backoff: `min(100 × 2^attempt, 1000)` ms.
/// Source: `storage/zvec.ts` (`zvecOpenRetryDelayMs`).
#[must_use]
pub fn zvec_open_retry_delay_ms(attempt: u32) -> u64 {
    ZVEC_OPEN_RETRY_BASE_DELAY_MS
        .saturating_mul(2_u64.saturating_pow(attempt.min(31)))
        .min(ZVEC_OPEN_RETRY_MAX_DELAY_MS)
}

/// Type-aware size cap: code files (1 MiB).
/// Source: `src/engine/file-size-policy.ts`.
pub const MAX_CODE_FILE_SIZE_BYTES: u64 = 1024 * 1024;

/// Type-aware size cap: text/markdown files (256 MiB).
/// Source: `src/engine/file-size-policy.ts`.
pub const MAX_TEXT_FILE_SIZE_BYTES: u64 = 256 * 1024 * 1024;

/// Type-aware size cap: structured data files (16 MiB).
/// Source: `src/engine/file-size-policy.ts`.
pub const MAX_DATA_FILE_SIZE_BYTES: u64 = 16 * 1024 * 1024;

/// Type-aware size cap: image files (10 MiB).
/// Source: `src/engine/file-size-policy.ts`.
pub const MAX_IMAGE_FILE_SIZE_BYTES: u64 = 10 * 1024 * 1024;

/// Binary sniff window (8 KiB).
/// Source: `pipeline/indexing/scanner/index.ts` (`BINARY_SNIFF_BYTES = 8192`).
pub const BINARY_SNIFF_BYTES: usize = 8192;

/// Binary sniff threshold: 30% control chars.
/// Source: `pipeline/indexing/scanner/index.ts`.
pub const BINARY_CONTROL_CHAR_RATIO_PERCENT: u64 = 30;

/// FTS tokenizer for the Latin text column (P0 decision, C.6).
///
/// zvec-grep uses jieba + lowercase. SQLite FTS5 has no jieba tokenizer;
/// the P0 decision is FTS5 `unicode61` with diacritics removed (case folding
/// is inherent), plus application-level CJK bigram expansion (see
/// [`crate::zg_types::cjk_bigrams`]) so CJK substring queries match without
/// a dictionary. Spelling is the space-separated FTS5 directive form
/// (verified against bundled SQLite — the quoted `=` form does not parse).
/// Revisit only with A/B evidence in P1.
pub const FTS_TEXT_TOKENIZER: &str = "unicode61 remove_diacritics 2";

/// CJK strategy tag matching [`FTS_TEXT_TOKENIZER`].
pub const FTS_CJK_STRATEGY: &str = "cjk-bigram-expansion";

#[cfg(test)]
#[path = "../tests/zg_constants.rs"]
mod tests;
