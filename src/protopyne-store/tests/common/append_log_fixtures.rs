//! Test-only append-log record: a two-column key/value row.
//!
//! Included from `append_log.rs` for the log-contract suite; the tape and
//! journal define their own records. Values carry no newlines, so every
//! encoded row is exactly one terminated line — the torn-tail suite's
//! precondition.

#![allow(dead_code)]

use crate::append_log::AppendRecord;

/// One key/value row under the `test_log` domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kv {
    /// The row key.
    pub key: String,
    /// The row value.
    pub value: String,
}

impl AppendRecord for Kv {
    fn domain() -> &'static str {
        "test_log"
    }

    fn version() -> u32 {
        1
    }

    fn header() -> Vec<&'static str> {
        vec!["k", "v"]
    }

    fn encode(&self) -> Vec<String> {
        vec![self.key.clone(), self.value.clone()]
    }

    fn decode(fields: &[&str]) -> Result<Self, String> {
        let [key, value] = fields else {
            return Err(format!("expected 2 fields, saw {}", fields.len()));
        };
        Ok(Self {
            key: (*key).to_owned(),
            value: (*value).to_owned(),
        })
    }
}
