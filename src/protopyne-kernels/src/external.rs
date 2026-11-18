//! External parity artifacts: third-party export numbers plus the provenance
//! binding them to the script that produced them.
//!
//! One JSON document carries `source`, `source_version`, `exported_at`,
//! `symbol`, `timeframe`, `script_sha256`, and the `rows`. [`load_external`]
//! refuses a document missing any provenance field (naming it) and refuses
//! a document whose hash is not the SHA-256 of the script bytes the caller
//! hands over — so editing the script invalidates the claim instead of
//! silently aging it. [`check_external`] compares the rows against our
//! values within a frozen eps and names the first bar that breaches it.
//!
//! Real exports land under `tests/data/external/` through an operator
//! TradingView run; fixtures for the refusal and comparison pins stay
//! inline in the tests, never committed as export files.

use sha2::{Digest, Sha256};

/// One exported observation in the document: our value is supplied by the
/// caller at check time, never stored beside the reference.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct ExternalRowDoc {
    /// Bar index in the export.
    pub bar: usize,
    /// The external reference's value.
    pub reference: f64,
}

/// One external export document, as committed under `tests/data/external/`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalDocument {
    /// Where the numbers came from (platform name).
    pub source: String,
    /// Source version the export was taken with.
    pub source_version: String,
    /// Export instant, ISO-8601 UTC.
    pub exported_at: String,
    /// Exported symbol, exchange-qualified.
    pub symbol: String,
    /// Chart timeframe the export was taken on.
    pub timeframe: String,
    /// SHA-256 of the script source the export ran, 64 lowercase hex digits.
    pub script_sha256: String,
    /// Exported observations, at least one.
    pub rows: Vec<ExternalRowDoc>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct RawDocument {
    source: Option<String>,
    source_version: Option<String>,
    exported_at: Option<String>,
    symbol: Option<String>,
    timeframe: Option<String>,
    script_sha256: Option<String>,
    rows: Option<Vec<ExternalRowDoc>>,
}

/// Why an external document or comparison was refused. Every refusal names
/// the field or the bar, never a bare verdict.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ExternalError {
    /// The document is not JSON, or names a provenance field it cannot
    /// supply; the detail carries serde's own words, which name the field.
    #[error("external artifact: {0}")]
    Refused(String),
}

/// SHA-256 of `script`, lowercase hex. The one home for the binding both
/// the loader and the artifact authors compute — a second spelling would
/// let the two disagree about what "the script hash" means.
#[must_use]
pub fn script_sha256_hex(script: &[u8]) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(script);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn required(value: Option<String>, field: &str) -> Result<String, ExternalError> {
    match value {
        Some(text) if !text.is_empty() => Ok(text),
        _ => Err(ExternalError::Refused(format!(
            "external artifact: {field} is required"
        ))),
    }
}

/// Load and validate one external document against the script bytes it
/// must name. Refuses missing or empty provenance (naming the field), a
/// hash that is not this script's SHA-256, and an empty row set.
pub fn load_external(json: &str, script: &[u8]) -> Result<ExternalDocument, ExternalError> {
    let raw: RawDocument = serde_json::from_str(json)
        .map_err(|e| ExternalError::Refused(format!("external artifact: not JSON: {e}")))?;
    let document = ExternalDocument {
        source: required(raw.source, "source")?,
        source_version: required(raw.source_version, "source_version")?,
        exported_at: required(raw.exported_at, "exported_at")?,
        symbol: required(raw.symbol, "symbol")?,
        timeframe: required(raw.timeframe, "timeframe")?,
        script_sha256: required(raw.script_sha256, "script_sha256")?,
        rows: raw.rows.filter(|rows| !rows.is_empty()).ok_or_else(|| {
            ExternalError::Refused("external artifact: rows is required".to_owned())
        })?,
    };
    if document.script_sha256 != script_sha256_hex(script) {
        return Err(ExternalError::Refused(
            "external artifact: script_sha256 does not match the script".to_owned(),
        ));
    }
    Ok(document)
}

/// Compare the document's rows against our values at the same bars within
/// `eps`. Names the first breaching bar; refuses non-finite references, an
/// out-of-range bar, and a non-finite or negative eps before comparing.
pub fn check_external(
    document: &ExternalDocument,
    ours: &[f64],
    eps: f64,
) -> Result<(), ExternalError> {
    if !eps.is_finite() || eps < 0.0 {
        return Err(ExternalError::Refused(
            "external artifact: eps must be finite and non-negative".to_owned(),
        ));
    }
    for row in &document.rows {
        if !row.reference.is_finite() {
            return Err(ExternalError::Refused(format!(
                "external artifact: bar {} reference is not finite",
                row.bar
            )));
        }
        let Some(value) = ours.get(row.bar) else {
            return Err(ExternalError::Refused(format!(
                "external artifact: bar {} is outside our {} values",
                row.bar,
                ours.len()
            )));
        };
        let gap = (value - row.reference).abs();
        if gap > eps {
            return Err(ExternalError::Refused(format!(
                "external artifact: bar {} differs by {gap} beyond eps {eps}",
                row.bar
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/external.rs"]
mod tests;
