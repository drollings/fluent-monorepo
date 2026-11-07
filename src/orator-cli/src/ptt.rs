#![forbid(unsafe_code)]

//! Chat push-to-talk: whether the mic waits for an explicit latch.
//!
//! Single source of truth for the two states (`off`, `on`): parsing,
//! session starts, and the fluent-wvr `PttSetting` component whose
//! schema describes the same states the loop gates on.

use fluent_wvr::{
    ArcIntern, Describable, FieldAccess, WorkContext, WorkError, WorkOutput, WorkUnit,
};

/// Mic latch selector for one chat session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PttMode {
    /// VAD-gated capture: hot spans commit on endpoint.
    #[default]
    Off,
    /// Mic hot only while latched on: toggle-off endpoints the turn.
    On,
}

impl PttMode {
    /// All states in stable order (help text, errors).
    pub const ALL: [&'static str; 2] = ["off", "on"];

    /// Canonical spelling for config, CLI, and `/set`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
        }
    }

    /// Human line for logs and help (same source as parsing).
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Off => "off (VAD-gated capture)",
            Self::On => "on (mic hot only while latched on)",
        }
    }
}

impl std::fmt::Display for PttMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for PttMode {
    type Err = String;
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        parse_ptt_mode(raw)
    }
}

/// Parse one PTT state; garbage names both spellings.
pub fn parse_ptt_mode(raw: &str) -> Result<PttMode, String> {
    match raw.trim() {
        "off" => Ok(PttMode::Off),
        "on" => Ok(PttMode::On),
        _ => Err(format!(
            "orator: unknown ptt state: {raw:?} (expected off or on)"
        )),
    }
}

/// Session start from the `chat.ptt` leaf: valid spellings map,
/// anything else falls back to `off` so the loop can name the bad
/// value instead of refusing the session.
#[must_use]
pub fn initial_ptt_mode(config: &orator_core::OratorConfig) -> PttMode {
    parse_ptt_mode(config.chat.ptt.trim()).unwrap_or_default()
}

/// Runtime-editable PTT handle: the fluent-wvr single source whose
/// schema, field access, and validation all derive from one
/// definition (the `chat.ptt` leaf round-trips through it).
#[derive(Debug, Clone, PartialEq, Eq, FieldAccess, Describable)]
pub struct PttSetting {
    /// Active state spelling (`off`|`on`).
    #[field(desc = "Chat push-to-talk (off|on)", max_len = 16)]
    pub level: String,
}

impl Default for PttSetting {
    fn default() -> Self {
        Self {
            level: "off".to_string(),
        }
    }
}

impl PttSetting {
    /// Handle holding `level` (expects a valid spelling).
    #[must_use]
    pub fn new(level: PttMode) -> Self {
        Self {
            level: level.as_str().to_string(),
        }
    }

    /// Resolve the held spelling through the single parser.
    pub fn resolve(&self) -> Result<PttMode, String> {
        parse_ptt_mode(&self.level)
    }
}

fluent_wvr::impl_component!(PttSetting);

impl WorkUnit for PttSetting {
    fn name(&self) -> &str {
        "ptt"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        match self.resolve() {
            Ok(level) => Ok(WorkOutput::ok(format!("ptt: {}", level.as_str()))),
            Err(detail) => Err(WorkError::Execution(detail)),
        }
    }
}

#[cfg(test)]
#[path = "../tests/ptt.rs"]
mod tests;
