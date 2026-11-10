//! Read-only storage and timing probes for the fragment index.
//!
//! All functions are pure math over counts the index already reports:
//! they never touch the database and never change recall behavior.
//! Producer self-doubt (score gaps) and task outcome (citation verifies)
//! are reported on separate fields wherever both appear.

/// Write amplification of one file commit.
///
/// One commit deletes prior rows then inserts fresh rows. FTS rows ride
/// the table triggers: each fragment delete/insert adds one FTS write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceFileCost {
    /// Fragments in the new commit.
    pub fragments: usize,
    /// Lemma rows in the new commit.
    pub lemmas: usize,
    /// FTS trigger writes (deletes + inserts, one per fragment each).
    pub fts_writes: usize,
    /// Total row writes including FTS and lemma rows.
    pub total_row_writes: usize,
}

/// Cost of committing `fragments` with `lemmas` lemma rows.
#[must_use]
pub fn replace_file_cost(fragments: usize, lemmas: usize) -> ReplaceFileCost {
    let fts_writes = fragments.saturating_mul(2);
    ReplaceFileCost {
        fragments,
        lemmas,
        fts_writes,
        total_row_writes: fragments
            .saturating_mul(2)
            .saturating_add(lemmas)
            .saturating_add(fts_writes),
    }
}

/// Byte footprint of one stored fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SizeProbe {
    /// Raw chunk bytes.
    pub text_bytes: usize,
    /// Bigram-expansion bytes stored alongside.
    pub cjk_bytes: usize,
    /// Quantized embedding bytes (`dims + 8`).
    pub embedding_q8_bytes: usize,
    /// Legacy FP32 bytes (`4 * dims`).
    pub embedding_fp32_bytes: usize,
}

/// Footprint for a fragment with `dims`-wide embeddings.
#[must_use]
pub fn size_probe_for_fragment(text: &str, cjk_text: &str, dims: usize) -> SizeProbe {
    SizeProbe {
        text_bytes: text.len(),
        cjk_bytes: cjk_text.len(),
        embedding_q8_bytes: dims.saturating_add(8),
        embedding_fp32_bytes: dims.saturating_mul(4),
    }
}

/// Mean lemma rows per fragment; task-value side, not confidence.
#[must_use]
pub fn lemma_density(lemma_rows: usize, fragments: usize) -> f64 {
    if fragments == 0 {
        return 0.0;
    }
    lemma_rows as f64 / fragments as f64
}

/// Wall-time split of one index run in milliseconds.
#[derive(Debug, Clone, PartialEq)]
pub struct PhaseSplit {
    /// Scan phase.
    pub scan_ms: u64,
    /// Parse phase.
    pub parse_ms: u64,
    /// Embed phase.
    pub embed_ms: u64,
    /// Commit phase.
    pub commit_ms: u64,
}

impl PhaseSplit {
    /// Total across phases, saturating.
    #[must_use]
    pub fn total_ms(&self) -> u64 {
        self.scan_ms
            .saturating_add(self.parse_ms)
            .saturating_add(self.embed_ms)
            .saturating_add(self.commit_ms)
    }

    /// Share of `embed_ms` in the total; producer-cost side.
    #[must_use]
    pub fn embed_share(&self) -> f64 {
        let total = self.total_ms() as f64;
        if total <= 0.0 {
            return 0.0;
        }
        self.embed_ms as f64 / total
    }
}

/// File-level recall agreement between the two writers.
///
/// Compares sorted file-id sets only: chunk shapes may differ while the
/// served file set must match.
#[must_use]
pub fn file_level_parity(mut live_file_ids: Vec<String>, mut batch_file_ids: Vec<String>) -> bool {
    live_file_ids.sort();
    batch_file_ids.sort();
    live_file_ids == batch_file_ids
}

/// Indexed source text is always data, never an instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredKind {
    /// Stored bytes are quoted content, never executed.
    Data,
}

/// Classification of indexed text for the trust boundary.
#[must_use]
pub fn store_kind_for_indexed_text(_text: &str) -> StoredKind {
    StoredKind::Data
}

#[cfg(test)]
#[path = "../tests/redteam_baseline.rs"]
mod baseline_tests;

#[cfg(test)]
#[path = "../tests/redteam_untrusted.rs"]
mod untrusted_tests;
