//! Closed PII label vocabulary for the LLM detector patterns.
//!
//! The wire type on `PiiSpan :: label` stays `String` so open-vocabulary
//! producers (classifiers emitting `credential.password`, `contact.email`)
//! keep working. Decision points parse via `PiiLabel :: try_from` and refuse
//! unknown input. The parse result measures correctness of the input for the
//! task, never producer confidence; `PiiSpan :: score` stays the confidence
//! axis (`1.0` regex, classifier average).

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Closed vocabulary of PII detector labels (union of the request-path table
/// and the anonymize-only statics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PiiLabel {
    #[serde(rename = "ssn")]
    Ssn,
    #[serde(rename = "card_number")]
    CardNumber,
    #[serde(rename = "email")]
    Email,
    #[serde(rename = "phone")]
    Phone,
    #[serde(rename = "api_key")]
    ApiKey,
    #[serde(rename = "ipv4")]
    Ipv4,
    #[serde(rename = "ipv6")]
    Ipv6,
    #[serde(rename = "bearer")]
    Bearer,
    #[serde(rename = "aws_key")]
    AwsKey,
    #[serde(rename = "generic_api_key")]
    GenericApiKey,
    #[serde(rename = "nino_uk")]
    NinoUk,
    #[serde(rename = "sin_ca")]
    SinCa,
}

/// Refusal for label input outside the closed vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown PII label: {0}")]
pub struct PiiLabelParseError(pub String);

impl PiiLabel {
    /// Canonical detector string for the label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ssn => "ssn",
            Self::CardNumber => "card_number",
            Self::Email => "email",
            Self::Phone => "phone",
            Self::ApiKey => "api_key",
            Self::Ipv4 => "ipv4",
            Self::Ipv6 => "ipv6",
            Self::Bearer => "bearer",
            Self::AwsKey => "aws_key",
            Self::GenericApiKey => "generic_api_key",
            Self::NinoUk => "nino_uk",
            Self::SinCa => "sin_ca",
        }
    }
}

impl std::str::FromStr for PiiLabel {
    type Err = PiiLabelParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ssn" => Ok(Self::Ssn),
            "card_number" => Ok(Self::CardNumber),
            "email" => Ok(Self::Email),
            "phone" => Ok(Self::Phone),
            "api_key" => Ok(Self::ApiKey),
            "ipv4" => Ok(Self::Ipv4),
            "ipv6" => Ok(Self::Ipv6),
            "bearer" => Ok(Self::Bearer),
            "aws_key" => Ok(Self::AwsKey),
            "generic_api_key" => Ok(Self::GenericApiKey),
            "nino_uk" => Ok(Self::NinoUk),
            "sin_ca" => Ok(Self::SinCa),
            _ => Err(PiiLabelParseError(s.to_string())),
        }
    }
}

impl std::fmt::Display for PiiLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
#[path = "../tests/pii_label.rs"]
mod tests;
