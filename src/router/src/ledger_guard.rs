//! Irreversible write-path scrubber for the Content-Node ledger.
//!
//! The durable ledger can never cache text matching the builtin filter engine:
//! every write-path payload is scrubbed through
//! [`scrub_for_ledger`] before it reaches `ContentNodeStore`. The guard lives here
//! (write policy *decisions*: engine, scopes, scrub shape); *enforcement* lives
//! at the store's own write entry points (`ContentNodeStore::{record_request,
//! record_result, record_content_node, insert_node}`) — the narrowest waist,
//! so even the `KnowledgeCapability` trait-object bypass, which cannot route
//! through any wrapper, writes scrubbed text (D1).
//!
//! The scrub is **irreversible by design**: `Redact`/`Anonymize` both collapse
//! to `[REDACTED:<pattern>]` and no codeword map is retained. This is the
//! `transform` hook's implementation behind [`pii_redacted_view`] — one
//! implementation, two callers.
//!
//! The engine is the same `DeterministicFilterEngine` built by
//! `crate::stages::deterministic::builtin_filter_engine`, evaluated with the
//! `ContentNodeWrite` scope active (`FilterContext::ledger_write`). It is
//! module-scoped and shared via `LazyLock` — never reconstructed per call.

use std::collections::HashSet;
use std::sync::LazyLock;

use fluent_knowledge::views::{FilteredLedger, LedgerView};
use fluent_types::NodeId;

use crate::config::FilterAction;
use crate::filters::{DeterministicFilterEngine, FilterContext, FilterDecision};
use crate::stages::deterministic::builtin_filter_engine;

/// The shared builtin engine. Evaluated with the `ContentNodeWrite` scope
/// active so `[Any]` filters apply while `FrontierBound`-only filters do not.
static BUILTIN_ENGINE: LazyLock<DeterministicFilterEngine> = LazyLock::new(builtin_filter_engine);

/// Result of scrubbing one write-path payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubbedText {
    /// The scrubbed text, safe to persist.
    pub text: String,
    /// Whether any filter decision fired.
    pub flagged: bool,
    /// The matching pattern name, when flagged.
    pub pattern: Option<String>,
}

pub fn emit_write_audit(scrubbed: &ScrubbedText) {
    if scrubbed.flagged {
        crate::audit::emit(
            "write_path",
            serde_json::json!({
                "stage": "ledger_guard",
                "verdict": "scrubbed",
                "pattern": scrubbed.pattern,
            }),
        );
        tracing::warn!(target: "router.audit", pattern = ?scrubbed.pattern, "write_path scrubbed");
    }
}

/// Evaluate + apply the builtin filter engine with the `ContentNodeWrite`
/// scope active. Always available (builtin engine) — no config flag (D1).
pub fn scrub_for_ledger(text: &str) -> ScrubbedText {
    let ctx = FilterContext::ledger_write(text.to_string());
    match BUILTIN_ENGINE.evaluate(&ctx) {
        Some(decision) => apply_filter_decision(text, &decision),
        None => ScrubbedText {
            text: text.to_string(),
            flagged: false,
            pattern: None,
        },
    }
}

/// Apply a `FilterDecision` to a plain string (not a `RouterRequest`).
///
/// - `OutputFilter`: replace every span through the shared
///   `common_core::string::apply_spans_rightmost` (rightmost-first, total:
///   degenerate spans are skipped, overlaps clamp fail-closed — see its
///   goldens); `Redact`/`Anonymize` → `[REDACTED:<pattern>]`, `Omit` → the
///   span is deleted. Irreversible: no codeword map is retained.
/// - `HardReject`: the whole payload collapses to `[rejected: <pattern>]`.
/// - `SoftRedirect`: unchanged (a redirect target is meaningless on the write
///   path), `flagged = false`.
fn apply_filter_decision(text: &str, decision: &FilterDecision) -> ScrubbedText {
    match decision {
        FilterDecision::HardReject { pattern, .. } => ScrubbedText {
            text: format!("[rejected: {pattern}]"),
            flagged: true,
            pattern: Some(pattern.clone()),
        },
        FilterDecision::OutputFilter {
            action,
            matched_pattern,
            matches,
            ..
        } => {
            let replacement = match action {
                FilterAction::Redact | FilterAction::Anonymize => {
                    format!("[REDACTED:{matched_pattern}]")
                }
                FilterAction::Omit => String::new(),
            };
            let spans: Vec<(usize, usize, &str)> = matches
                .iter()
                .map(|m| (m.start, m.end, replacement.as_str()))
                .collect();
            ScrubbedText {
                text: common_core::string::apply_spans_rightmost(text, spans),
                flagged: true,
                pattern: Some(matched_pattern.clone()),
            }
        }
        FilterDecision::SoftRedirect { .. } => ScrubbedText {
            text: text.to_string(),
            flagged: false,
            pattern: None,
        },
    }
}

/// The PII frontier view: a `FilteredLedger` whose transform scrubs every
/// rendered line through the write-path guard. Lives with the guard because
/// the transform IS the guard (`scrub_for_ledger`) — one implementation,
/// two callers (DRY).
pub fn pii_redacted_view<V: LedgerView, S: std::hash::BuildHasher>(
    inner: V,
    excluded: HashSet<NodeId, S>,
) -> FilteredLedger<V> {
    FilteredLedger::new(inner, excluded).with_transform(|_id, text| scrub_for_ledger(&text).text)
}

#[cfg(test)]
#[path = "../tests/ledger_guard_golden.rs"]
mod ledger_guard_golden;
#[cfg(test)]
#[path = "../tests/ledger_guard.rs"]
mod tests;
#[cfg(test)]
#[path = "../tests/views.rs"]
mod views_tests;
