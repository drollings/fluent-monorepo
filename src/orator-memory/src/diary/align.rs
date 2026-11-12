//! Alignment and naming: speaker segments meet word times.
//!
//! Words take the label of the segment whose midpoint overlap covers
//! them; ties break toward the earlier segment. Labels render as
//! `Speaker N`; model-proposed names arrive as suggestions attached
//! to the label, never as replacements — attribution stays audible,
//! suggestions stay advisory.

use crate::diary::segment::Segment;

/// Attribute each word midpoint `(word_ms)` to a segment label.
#[must_use]
/// Attribute each word midpoint `(word_ms)` to a segment label.
pub fn align_words(word_mid_ms: &[u64], segments: &[Segment], labels: &[usize]) -> Vec<usize> {
    word_mid_ms
        .iter()
        .map(|mid| {
            let mut best = 0usize;
            let mut best_dist = u64::MAX;
            for (index, segment) in segments.iter().enumerate() {
                let seg_mid = segment.midpoint() / 16;
                let dist = mid.abs_diff(seg_mid);
                if dist < best_dist {
                    best_dist = dist;
                    best = index;
                }
            }
            labels.get(best).copied().unwrap_or(0)
        })
        .collect()
}

/// Render a cluster label as `Speaker N` (1-based for humans).
#[must_use]
/// Render a cluster label as `Speaker N` (1-based for humans).
pub fn speaker_name(label: usize) -> String {
    format!("Speaker {}", label + 1)
}

/// Suggested names from the model for each label. Suggestions only:
/// the transcript keeps `Speaker N` attribution regardless.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
/// Suggested names from the model for each label. Suggestions only:
/// the transcript keeps `Speaker N` attribution regardless.
pub struct NameSuggestions {
    /// Suggested display name per label.
    pub names: std::collections::HashMap<usize, String>,
}

impl NameSuggestions {
    /// Empty suggestions.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Display name for `label`: the suggestion when present, else the
    /// stable `Speaker N` attribution.
    #[must_use]
    pub fn display(&self, label: usize) -> String {
        self.names.get(&label).cloned().unwrap_or_else(|| speaker_name(label))
    }
}

#[cfg(test)]
#[path = "../../tests/diary_align.rs"]
mod tests;