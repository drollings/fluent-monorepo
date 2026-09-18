//! Canonical routing model-key split: the single home for the
//! `base:qualifier` grammar shared by router dispatch, pipeline typing, and
//! backend pre-filtering.
//!
//! Qualified iff the first colon has non-empty halves on both sides
//! (`"a:b:c"` qualifies with qualifier `"b:c"`); everything else is bare
//! (the whole input is the base, qualifier `None`). Total and infallible by
//! design — this is classification, not validation:
//!
//! - `provider:model` references with provider-defaulting live in
//!   [`crate::model_ref`] (different contract: empty halves still split);
//! - name validation (`[A-Za-z0-9._-]+`, strict instance ids) lives in
//!   [`crate::instance_id`] (fallible — split ≠ validate);
//! - path sanitization (`/` and `:` → `_`) stays with its consumer
//!   (router `kv_cache::model_key` — a different job, not a split);
//! - last-colon tag grammars (`rsplit_once`, docker-style `base:tag`,
//!   server-reported instance ids) are different grammars, not copies.
//!
//! Moved verbatim from the router's `config::split_model_key` (single
//! `split_once(':')` site); the router now composes this. Model-id parsing
//! is neither confidence nor task-value — no threshold.

/// Split a routing model key into `(base, qualifier)`.
///
/// The qualifier is `Some` iff the first colon has non-empty text on both
/// sides; otherwise the whole input is the base.
///
/// # Examples
///
/// ```
/// use fluent_types::model_key::split_model_key;
///
/// assert_eq!(split_model_key("llama"), ("llama", None));
/// assert_eq!(split_model_key("llama:latest"), ("llama", Some("latest")));
/// assert_eq!(split_model_key("a:b:c"), ("a", Some("b:c")));
/// assert_eq!(split_model_key("llama:"), ("llama:", None));
/// ```
#[must_use]
pub fn split_model_key(key: &str) -> (&str, Option<&str>) {
    match key.split_once(':') {
        Some((base, qualifier)) if !base.is_empty() && !qualifier.is_empty() => {
            (base, Some(qualifier))
        }
        _ => (key, None),
    }
}

#[cfg(test)]
#[path = "../tests/model_key.rs"]
mod tests;
