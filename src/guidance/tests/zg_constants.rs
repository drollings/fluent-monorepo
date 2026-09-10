use super::*;

// Ported from `test/unit/index-input-budget.test.mjs` (exact expectations)
// plus constant-value pins for every roadmap "Tested constants" row.

#[test]
fn recall_target_is_max_limit_times_five_or_fifty() {
    assert_eq!(recall_target_candidate_count(7), 50);
    assert_eq!(recall_target_candidate_count(10), 50);
    assert_eq!(recall_target_candidate_count(20), 100);
    assert_eq!(recall_target_candidate_count(0), 50);
}

#[test]
fn recall_walk_constants_match_zvec() {
    assert_eq!(RECALL_INITIAL_DEPTH, 200);
    assert_eq!(RECALL_MAX_DEPTH, 2000);
    assert_eq!(RECALL_GROWTH_FACTOR, 2);
    assert_eq!(RECALL_TARGET_FACTOR, 5);
    assert_eq!(RECALL_MIN_TARGET_CANDIDATES, 50);
    assert!((RRF_K - 60.0).abs() < f64::EPSILON);
    assert_eq!(DEFAULT_LIMIT, 7);
    assert_eq!(MCP_LIMIT_CAP, 50);
}

#[test]
fn chunk_and_outline_constants_match_zvec() {
    assert_eq!(CODE_CHUNK_CHARS, 3600);
    assert_eq!(CODE_CHUNK_OVERLAP_CHARS, 540);
    assert_eq!(CHUNK_OVERLAP_PERCENT, 15);
    assert_eq!(METADATA_BUDGET_PERCENT, 25);
    assert_eq!(OUTLINE_HEADER_MAX_CHARS, 1200);
    assert_eq!(OUTLINE_HEADER_MAX_LINES, 24);
    assert_eq!(OUTLINE_MAX_MEMBERS, 32);
    assert_eq!(OUTLINE_MAX_CALLS, 24);
    assert_eq!(OUTLINE_MAX_LINE_CHARS, 180);
    assert_eq!(OUTLINE_WALK_MAX_DEPTH, 12);
}

#[test]
fn uses_default_input_estimate_for_ordinary_text() {
    let text = "ordinary text ".repeat(20_000);
    assert_eq!(
        index_chunk_options(Some(128_000), Some(&text)),
        ChunkOptions {
            max_chunk_chars: Some(236_800),
            chunk_overlap_chars: Some(35_520),
        }
    );
}

#[test]
fn uses_conservative_input_estimate_for_token_dense_text() {
    let text = "<123-456>".repeat(30_000);
    assert_eq!(
        index_chunk_options(Some(128_000), Some(&text)),
        ChunkOptions {
            max_chunk_chars: Some(128_000),
            chunk_overlap_chars: Some(19_200),
        }
    );
}

#[test]
fn detects_a_localized_token_dense_region() {
    let text = format!(
        "{}{}",
        "ordinary text ".repeat(12_000),
        "<123-456>".repeat(2_000)
    );
    assert_eq!(
        index_chunk_options(Some(100_000), Some(&text)).max_chunk_chars,
        Some(100_000)
    );
}

#[test]
fn omits_input_limits_when_model_declares_none() {
    let text = "<123-456>".repeat(30_000);
    assert_eq!(
        index_chunk_options(None, Some(&text)),
        ChunkOptions {
            max_chunk_chars: None,
            chunk_overlap_chars: None,
        }
    );
}

#[test]
fn dense_detector_short_circuits_below_threshold() {
    // Short text can never be dense regardless of content.
    assert!(!is_token_dense_text(Some("<123-456>"), 128_000));
    assert!(!is_token_dense_text(None, 128_000));
    // Pure ASCII letters + spaces are never dense.
    let text = "ordinary text ".repeat(20_000);
    assert!(!is_token_dense_text(Some(&text), 128_000));
}

#[test]
fn metadata_budget_is_quarter_floor_with_none_passthrough() {
    assert_eq!(metadata_budget_chars(Some(3600)), Some(900));
    assert_eq!(metadata_budget_chars(Some(100)), Some(25));
    assert_eq!(metadata_budget_chars(None), None);
}

#[test]
fn open_retry_backoff_is_capped_exponential() {
    assert_eq!(zvec_open_retry_delay_ms(0), 100);
    assert_eq!(zvec_open_retry_delay_ms(1), 200);
    assert_eq!(zvec_open_retry_delay_ms(2), 400);
    assert_eq!(zvec_open_retry_delay_ms(3), 800);
    assert_eq!(zvec_open_retry_delay_ms(4), 1000);
    assert_eq!(zvec_open_retry_delay_ms(7), 1000);
    assert_eq!(zvec_open_retry_delay_ms(100), 1000);
    assert_eq!(ZVEC_OPEN_RETRY_ATTEMPTS, 8);
    assert_eq!(ZVEC_UPSERT_BATCH_SIZE, 1024);
}

#[test]
fn freshness_and_retry_constants_match_zvec() {
    assert_eq!(CHANGE_SET_PATH_BUDGET, 1000);
    assert_eq!(WATCH_DEBOUNCE_MS, 750);
    assert_eq!(WATCH_MAX_WAIT_MS, 5_000);
    assert_eq!(WATCH_RESUME_CHECK_INTERVAL_MS, 30_000);
    assert_eq!(WATCH_RESUME_THRESHOLD_MS, 90_000);
    assert_eq!(WATCH_RECONCILE_INTERVAL_MS, 3_600_000);
    assert_eq!(WATCHER_IDLE_TIMEOUT_MS, 14_400_000);
    assert_eq!(EMBEDDING_TRANSIENT_MAX_RETRIES, 3);
    assert_eq!(EMBEDDING_TRANSIENT_RETRY_BASE_DELAY_MS, 500);
    assert_eq!(EMBEDDING_TRANSIENT_RETRY_MAX_DELAY_MS, 8_000);
    assert_eq!(EMBEDDING_RATE_LIMIT_MAX_RETRIES, 6);
    assert_eq!(EMBEDDING_RATE_LIMIT_RETRY_BASE_DELAY_MS, 2_000);
    assert_eq!(EMBEDDING_RATE_LIMIT_RETRY_MAX_DELAY_MS, 30_000);
    assert_eq!(EMBEDDING_RETRY_JITTER_MS, 500);
}

#[test]
fn size_and_binary_caps_match_zvec() {
    assert_eq!(MAX_CODE_FILE_SIZE_BYTES, 1_048_576);
    assert_eq!(MAX_TEXT_FILE_SIZE_BYTES, 268_435_456);
    assert_eq!(MAX_DATA_FILE_SIZE_BYTES, 16_777_216);
    assert_eq!(MAX_IMAGE_FILE_SIZE_BYTES, 10_485_760);
    assert_eq!(BINARY_SNIFF_BYTES, 8192);
    assert_eq!(BINARY_CONTROL_CHAR_RATIO_PERCENT, 30);
}

#[test]
fn fts_tokenizer_decision_is_recorded() {
    // C.6: the one open risk, pinned as a constant pair.
    assert_eq!(FTS_TEXT_TOKENIZER, "unicode61 remove_diacritics 2");
    assert_eq!(FTS_CJK_STRATEGY, "cjk-bigram-expansion");
}
