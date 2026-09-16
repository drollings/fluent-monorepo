use std::sync::LazyLock;

use regex::Regex;

pub static EMAIL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}").unwrap());
pub static API_KEY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(api[_-]?key|apikey|secret|token)\s*[=:]\s*[a-zA-Z0-9_-]{16,}").unwrap()
});
pub static IPV4_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}\b").unwrap());
pub static PHONE_US_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\d{3}[-.]?\d{3}[-.]?\d{4}\b").unwrap());
pub static CREDIT_CARD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\d{4}[-\s]?\d{4}[-\s]?\d{4}[-\s]?\d{4}\b").unwrap());
pub static SSN_US_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\d{3}-\d{2}-\d{4}\b").unwrap());
pub static NINO_UK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[A-Z]{2}\d{6}[A-Z]\b").unwrap());
pub static SIN_CA_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\d{3}-\d{3}-\d{3}\b").unwrap());
pub static BEARER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Bearer\s+[A-Za-z0-9_\-]{8,}").unwrap());
pub static AWS_KEY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bAKIA[0-9A-Z]{16}\b").unwrap());
pub static GENERIC_API_KEY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[a-zA-Z0-9]{32,}\b").unwrap());
pub static IPV6_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b([0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}\b").unwrap());

/// Canonical PII detection patterns.
///
/// The compiled statics above are the single source of truth. This table is
/// derived from them so the names and regex strings can never drift from the
/// compiled patterns used by `anonymize`.
///
/// M6: exactly 5 of the 12 statics are exposed here — deliberately, not an
/// oversight. `RegexPiiDetector` (the review pre-filter baseline) consumes
/// this table, and widening it would change which spans the pre-filter
/// reports (a behavior change). The other 7 (`NINO_UK`, `SIN_CA`, `BEARER`,
/// `AWS_KEY`, `GENERIC_API_KEY`, `IPV6`, `IPV4`) fire only inside
/// `anonymize`, which references its statics directly. Closing the gap
/// (unifying the two consumers on one table) is a behavior-change proposal,
/// filed separately — see `tests/pii_patterns.rs` for the lock.
pub struct PiiPattern {
    pub name: &'static str,
    pub regex: &'static LazyLock<Regex>,
}

pub fn pii_patterns() -> &'static [PiiPattern] {
    static PATTERNS: &[PiiPattern] = &[
        PiiPattern {
            name: "ssn",
            regex: &SSN_US_RE,
        },
        PiiPattern {
            name: "card_number",
            regex: &CREDIT_CARD_RE,
        },
        PiiPattern {
            name: "email",
            regex: &EMAIL_RE,
        },
        PiiPattern {
            name: "phone",
            regex: &PHONE_US_RE,
        },
        PiiPattern {
            name: "api_key",
            regex: &API_KEY_RE,
        },
    ];
    PATTERNS
}

/// One `anonymize` rewrite: a compiled static plus its stable `[TYPE]`
/// placeholder.
pub struct AnonymizeAction {
    pub name: &'static str,
    pub regex: &'static LazyLock<Regex>,
    pub placeholder: &'static str,
}

/// The `anonymize` action table (M12): all 12 compiled statics with their
/// placeholders, in the historical application order. Derived beside the
/// statics so the mapping can never drift from them — `anonymize` consumes
/// this table instead of an inline chain, making `pii_patterns.rs` the
/// single pattern home both consumers read (the request-path table above,
/// the rewrite table here).
pub fn anonymize_actions() -> &'static [AnonymizeAction] {
    static ACTIONS: &[AnonymizeAction] = &[
        AnonymizeAction {
            name: "email",
            regex: &EMAIL_RE,
            placeholder: "[EMAIL]",
        },
        AnonymizeAction {
            name: "credit_card",
            regex: &CREDIT_CARD_RE,
            placeholder: "[CREDIT_CARD]",
        },
        AnonymizeAction {
            name: "ssn_us",
            regex: &SSN_US_RE,
            placeholder: "[SSN]",
        },
        AnonymizeAction {
            name: "nino_uk",
            regex: &NINO_UK_RE,
            placeholder: "[NINO]",
        },
        AnonymizeAction {
            name: "sin_ca",
            regex: &SIN_CA_RE,
            placeholder: "[SIN]",
        },
        AnonymizeAction {
            name: "bearer",
            regex: &BEARER_RE,
            placeholder: "[BEARER_TOKEN]",
        },
        AnonymizeAction {
            name: "aws_key",
            regex: &AWS_KEY_RE,
            placeholder: "[AWS_KEY]",
        },
        AnonymizeAction {
            name: "generic_api_key",
            regex: &GENERIC_API_KEY_RE,
            placeholder: "[API_KEY]",
        },
        AnonymizeAction {
            name: "ipv6",
            regex: &IPV6_RE,
            placeholder: "[IPv6]",
        },
        AnonymizeAction {
            name: "ipv4",
            regex: &IPV4_RE,
            placeholder: "[IP_ADDRESS]",
        },
        AnonymizeAction {
            name: "phone_us",
            regex: &PHONE_US_RE,
            placeholder: "[PHONE]",
        },
        AnonymizeAction {
            name: "api_key",
            regex: &API_KEY_RE,
            placeholder: "[REDACTED]",
        },
    ];
    ACTIONS
}

#[cfg(test)]
#[path = "../tests/pii_patterns.rs"]
mod tests;
