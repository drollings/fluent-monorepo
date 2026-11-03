//! Word error rate: the STT accuracy yardstick, per tier.
//!
//! Pure word-level Levenshtein over whitespace tokens, normalized by
//! the reference length: substitutions plus deletions plus insertions,
//! divided by reference words. Identical transcripts score 0.0;
//! anything decodes to something finite. Empty reference with empty
//! hypothesis is 0.0 (nothing to get wrong); empty reference with
//! words hypothesized is 1.0.
//!
//! The per-tier runner lives in `tests/live.rs` (real models plus the
//! checked-in corpus, skip-cleanly without them). What ships here is
//! the scoring — scripted decoders ignore audio by contract, so a WER
//! over scripted output would be a false measurement and none is
//! claimed.

/// Word error rate of `hypothesis` against `reference` in `[0, ∞)`.
/// Insertions can push it past 1.0; identical transcripts give 0.0.
#[must_use]
pub fn wer(reference: &str, hypothesis: &str) -> f64 {
    let refs: Vec<&str> = reference.split_whitespace().collect();
    let hyps: Vec<&str> = hypothesis.split_whitespace().collect();
    if refs.is_empty() {
        return if hyps.is_empty() { 0.0 } else { 1.0 };
    }
    let edits = levenshtein(&refs, &hyps);
    edits as f64 / refs.len() as f64
}

fn levenshtein(refs: &[&str], hyps: &[&str]) -> usize {
    let mut prev: Vec<usize> = (0..=hyps.len()).collect();
    for ref_word in refs {
        let mut curr = vec![0; hyps.len() + 1];
        curr[0] = prev[0] + 1;
        for (j, hyp_word) in hyps.iter().enumerate() {
            let cost = usize::from(ref_word != hyp_word);
            curr[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(curr[j] + 1);
        }
        prev = curr;
    }
    prev[hyps.len()]
}

#[cfg(test)]
#[path = "../tests/wer.rs"]
mod tests;
