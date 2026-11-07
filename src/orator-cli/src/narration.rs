#![forbid(unsafe_code)]

//! Chat narration levels: how much of a reply voices.
//!
//! Single source of truth for the four levels (`none`, `intro`,
//! `summary`, `full`): parsing, sentence slicing, summary-spoken
//! composition, and the fluent-wvr `NarrationSetting` component whose
//! schema describes the same levels the loop voices.

use fluent_wvr::{
    ArcIntern, Describable, FieldAccess, WorkContext, WorkError, WorkOutput, WorkUnit,
};

/// Spoken portion selector for one assistant reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NarrationLevel {
    /// Text only: nothing voices.
    #[default]
    None,
    /// First sentence voices; full text displays.
    Intro,
    /// First plus summarized middle plus last voices.
    Summary,
    /// Full reply voices as well as displays.
    Full,
}

impl NarrationLevel {
    /// All levels in stable order (help text, errors).
    pub const ALL: [&'static str; 4] = ["none", "intro", "summary", "full"];

    /// Canonical spelling for config, CLI, and `/set`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Intro => "intro",
            Self::Summary => "summary",
            Self::Full => "full",
        }
    }

    /// Human line for logs and help (same source as parsing).
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::None => "none (text only, nothing spoken)",
            Self::Intro => "intro (first sentence spoken, full text shown)",
            Self::Summary => "summary (first, summarized middle, and last spoken)",
            Self::Full => "full (entire reply spoken and shown)",
        }
    }
}

impl std::fmt::Display for NarrationLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for NarrationLevel {
    type Err = String;
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        parse_narration_level(raw)
    }
}

/// Parse one narration level; garbage names the four spellings.
pub fn parse_narration_level(raw: &str) -> Result<NarrationLevel, String> {
    match raw.trim() {
        "none" => Ok(NarrationLevel::None),
        "intro" => Ok(NarrationLevel::Intro),
        "summary" => Ok(NarrationLevel::Summary),
        "full" => Ok(NarrationLevel::Full),
        _ => Err(format!(
            "orator: unknown narration level: {raw:?} (expected none, intro, summary, or full)"
        )),
    }
}

/// Session start from the `chat.narration` leaf: valid spellings map,
/// anything else falls back to `none` so the loop can name the bad
/// value instead of refusing the session.
#[must_use]
pub fn initial_narration_mode(config: &orator_core::OratorConfig) -> NarrationLevel {
    parse_narration_level(config.chat.narration.trim()).unwrap_or_default()
}

/// Split `text` into sentences on `.`/`!`/`?`, keeping the terminator.
/// Empty fragments drop; a text with no terminator is one sentence.
#[must_use]
pub fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.trim().chars() {
        cur.push(ch);
        if matches!(ch, '.' | '!' | '?') {
            let piece = cur.trim().to_string();
            if !piece.is_empty() {
                out.push(piece);
            }
            cur.clear();
        }
    }
    let tail = cur.trim().to_string();
    if !tail.is_empty() {
        out.push(tail);
    }
    if out.is_empty() && !text.trim().is_empty() {
        out.push(text.trim().to_string());
    }
    out
}

/// First sentence of `reply` (shared `first_sentence` home).
#[must_use]
pub fn first_sentence_of(reply: &str) -> String {
    let first = common_core::string::first_sentence(reply);
    if first.is_empty() {
        return String::new();
    }
    // Shared home trims and caps terminator-less text already.
    if let Some(found) = split_sentences(reply).into_iter().next() {
        return found;
    }
    first
}

/// Last sentence of `reply`; single-sentence replies return it.
#[must_use]
pub fn last_sentence_of(reply: &str) -> String {
    split_sentences(reply).pop().unwrap_or_default()
}

/// Text between the first and last sentence, joined with spaces.
/// Empty for fewer than three sentences (nothing to summarize).
#[must_use]
pub fn middle_text_of(reply: &str) -> String {
    let sentences = split_sentences(reply);
    if sentences.len() < 3 {
        return String::new();
    }
    sentences[1..sentences.len() - 1].join(" ")
}

/// Spoken text for levels needing no LLM: `None` for none/empty,
/// first sentence for intro, full reply for full. Summary returns
/// `None` here (it needs the summarized middle first).
#[must_use]
pub fn spoken_for_narration(reply: &str, level: NarrationLevel) -> Option<String> {
    match level {
        // None voices nothing; summary needs the LLM middle first so
        // this pure helper returns nothing for it as well.
        NarrationLevel::None | NarrationLevel::Summary => None,
        NarrationLevel::Intro => {
            let first = first_sentence_of(reply);
            if first.trim().is_empty() {
                None
            } else {
                Some(first)
            }
        }
        NarrationLevel::Full => {
            if reply.trim().is_empty() {
                None
            } else {
                Some(reply.to_string())
            }
        }
    }
}

/// Compose summary-level spoken text from its three parts, skipping
/// empties and collapsing first==last (one-sentence replies voice
/// once, never twice).
#[must_use]
pub fn compose_summary_spoken(first: &str, summary: &str, last: &str) -> String {
    let first = first.trim();
    let summary = summary.trim();
    let last = last.trim();
    let mut parts: Vec<&str> = Vec::new();
    if !first.is_empty() {
        parts.push(first);
    }
    if !summary.is_empty() {
        parts.push(summary);
    }
    if !last.is_empty() && last != first {
        parts.push(last);
    }
    parts.join(" ")
}

/// Prompt asking the chat model to compress `middle` into one or two
/// short speakable sentences (no fences, no preamble).
#[must_use]
pub fn build_summary_prompt(middle: &str) -> String {
    format!(
        "Summarize the following in one or two short sentences suitable for spoken delivery. Reply with only the summary, no preamble, no fences: {middle}"
    )
}

/// Runtime-editable narration handle: the fluent-wvr single source
/// whose schema, field access, and validation all derive from one
/// definition (the `chat.narration` leaf round-trips through it).
#[derive(Debug, Clone, PartialEq, Eq, FieldAccess, Describable)]
pub struct NarrationSetting {
    /// Active level spelling (`none`|`intro`|`summary`|`full`).
    #[field(desc = "Chat narration level (none|intro|summary|full)", max_len = 16)]
    pub level: String,
}

impl Default for NarrationSetting {
    fn default() -> Self {
        Self {
            level: "none".to_string(),
        }
    }
}

impl NarrationSetting {
    /// Handle holding `level` (expects a valid spelling).
    #[must_use]
    pub fn new(level: NarrationLevel) -> Self {
        Self {
            level: level.as_str().to_string(),
        }
    }

    /// Resolve the held spelling through the single parser.
    pub fn resolve(&self) -> Result<NarrationLevel, String> {
        parse_narration_level(&self.level)
    }
}

fluent_wvr::impl_component!(NarrationSetting);

impl WorkUnit for NarrationSetting {
    fn name(&self) -> &str {
        "narration"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        match self.resolve() {
            Ok(level) => Ok(WorkOutput::ok(format!("narration: {}", level.as_str()))),
            Err(detail) => Err(WorkError::Execution(detail)),
        }
    }
}

#[cfg(test)]
#[path = "../tests/narration.rs"]
mod tests;
