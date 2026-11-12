//! KRunner runner: `orator` plus a question asks the assistant.
//!
//! The runner parses the typed query and offers follow-up actions
//! (ask, start a meeting, dictate) without executing anything: offers
//! are suggestions the user picks, never ambient effects.

/// One runner offer.
#[derive(Debug, Clone, PartialEq, Eq)]
/// KRunner runner: `orator` plus a question asks the assistant.
/// 
/// The runner parses the typed query and offers follow-up actions
/// (ask, start a meeting, dictate) without executing anything: offers
/// are suggestions the user picks, never ambient effects.
/// One runner offer.
pub enum RunnerOffer {
    /// Ask the assistant `question`.
    Ask(String),
    /// Open a meeting session.
    StartMeeting,
    /// Open dictation into the focused window.
    Dictate,
}

/// A parsed runner query.
#[derive(Debug, Clone, PartialEq, Eq)]
/// A parsed runner query.
pub struct RunnerQuery {
    /// The question text after the `orator` prefix.
    pub question: String,
    /// Follow-up offers.
    pub offers: Vec<RunnerOffer>,
}

/// Parse a KRunner query. `None` when the text is not addressed to the
/// assistant. The prefix match is case-insensitive; an empty question
/// still offers the two session openers.
#[must_use]
/// Parse a KRunner query. `None` when the text is not addressed to the
/// assistant. The prefix match is case-insensitive; an empty question
/// still offers the two session openers.
pub fn parse_query(text: &str) -> Option<RunnerQuery> {
    let trimmed = text.trim();
    let rest = trimmed.strip_prefix("orator").or_else(|| {
        trimmed
            .get(..6)
            .filter(|prefix| prefix.eq_ignore_ascii_case("orator"))
            .map(|_| &trimmed[6..])
    })?;
    // Word boundary: the prefix must end the text or be followed by a
    // separator, so `oratorian` is not ours.
    if let Some(next) = rest.chars().next() {
        if next.is_alphanumeric() {
            return None;
        }
    }
    let question = rest.trim().trim_start_matches([':', '-', ' ']).trim().to_string();
    let mut offers = Vec::new();
    if !question.is_empty() {
        offers.push(RunnerOffer::Ask(question.clone()));
    }
    offers.push(RunnerOffer::StartMeeting);
    offers.push(RunnerOffer::Dictate);
    Some(RunnerQuery { question, offers })
}

#[cfg(test)]
#[path = "../tests/krunner.rs"]
mod tests;