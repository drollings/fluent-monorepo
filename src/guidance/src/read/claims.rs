//! Claim extraction, citation binding, and support gating.
//!
//! The path/range check (`constrain_answer_to_evidence`, moved here
//! from the binary) stays a cheap prefilter: it rejects citations
//! that name nothing shown. Claim support is the task-value verdict:
//! until the support point calibrates, every sentence is
//! `[unverified]` and the evidence sections stay authoritative.
//! Unsupported sentences are removed; an answer with no supported
//! sentence falls back to evidence-only.

/// Claim support verdict (placeholder until calibration).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Claim extraction, citation binding, and support gating.
///
/// The path/range check (`constrain_answer_to_evidence`, moved here
/// from the binary) stays a cheap prefilter: it rejects citations
/// that name nothing shown. Claim support is the task-value verdict:
/// until the support point calibrates, every sentence is
/// `[unverified]` and the evidence sections stay authoritative.
/// Unsupported sentences are removed; an answer with no supported
/// sentence falls back to evidence-only.
/// Claim support verdict (placeholder until calibration).
pub enum Support {
    /// Bound to a shown citation (post-calibration only).
    Verified,
    /// Default: kept with an `[unverified]` label, never presented
    /// as settled.
    Unverified,
    /// Contradicted by shown evidence: dropped.
    Contradicted,
    /// No citation to bind: dropped.
    Insufficient,
}

/// One extracted sentence plus its citation binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// Sentence text.
    pub text: String,
    /// Bound citation (`path:start-end`), if the sentence names one.
    pub citation: Option<String>,
    /// Support verdict.
    pub support: Support,
}

/// Split prose into sentences on `.`/`!`/`?` followed by whitespace
/// or end. Deterministic; abbreviations may over-split — binding is
/// per-sentence, so a split never invents support.
#[must_use]
pub fn extract_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let ender = matches!(bytes[index], b'.' | b'!' | b'?');
        let boundary = if ender {
            let next = text[index + 1..].chars().next();
            next.is_none_or(char::is_whitespace)
        } else {
            false
        };
        if boundary {
            let sentence = text[start..=index].trim();
            if !sentence.is_empty() {
                out.push(sentence.to_string());
            }
            start = index + 1;
        }
        index += 1;
    }
    let tail = text[start..].trim();
    if !tail.is_empty() {
        out.push(tail.to_string());
    }
    out
}

/// Bind each sentence to the evidence manifest: a `[Cn]`/`[Dn]`
/// token resolves to its canonical citation, otherwise a raw
/// `file:start-end` range binds when it names shown evidence.
/// Sentences with no resolvable citation bind to nothing; the gate
/// drops them, so uncited prose never renders.
#[must_use]
pub fn bind_claims(answer: &str, manifest: &[(String, String)]) -> Vec<Claim> {
    extract_sentences(answer)
        .into_iter()
        .map(|text| {
            let citation =
                resolve_token(&text, manifest).or_else(|| resolve_raw_range(&text, manifest));
            Claim {
                citation,
                support: Support::Unverified,
                text,
            }
        })
        .collect()
}

/// First `[Cn]`/`[Dn]` token in the sentence, resolved through the
/// manifest to its canonical citation.
fn resolve_token(sentence: &str, manifest: &[(String, String)]) -> Option<String> {
    for token in evidence_tokens(sentence) {
        if let Some(canonical) = manifest
            .iter()
            .find(|(shown, _)| *shown == token)
            .map(|(_, canonical)| canonical.clone())
        {
            return Some(canonical);
        }
    }
    None
}

/// Raw `file:start-end` fallback: the first manifest canonical named
/// verbatim by the sentence binds; otherwise a cited range that
/// overlaps a shown canonical range binds (a line inside the shown
/// span names shown evidence). Structural, never semantic.
fn resolve_raw_range(sentence: &str, manifest: &[(String, String)]) -> Option<String> {
    if let Some(canonical) = manifest
        .iter()
        .find(|(_, canonical)| sentence.contains(canonical.as_str()))
        .map(|(_, canonical)| canonical.clone())
    {
        return Some(canonical);
    }
    let cited = common_core::cite::extract_citation_ranges(sentence);
    for range in &cited {
        for (_, canonical) in manifest {
            if canonical_covers(canonical, range) {
                return Some(canonical.clone());
            }
        }
    }
    None
}

/// Whether a manifest canonical (`path`, `path:start`,
/// `path:start-end`, `path#anchor`) covers a cited range: same file
/// by suffix, with range overlap when the canonical carries one. A
/// range-free canonical (whole card) covers any same-file range.
fn canonical_covers(canonical: &str, range: &common_core::cite::CitationRange) -> bool {
    let anchor_free = canonical.split('#').next().unwrap_or(canonical);
    let (file, start, end) = match anchor_free.split_once(':') {
        Some((file, spec)) => {
            let (start, end): (Option<u32>, Option<u32>) = match spec.split_once('-') {
                Some((first, second)) => (first.parse().ok(), second.parse().ok()),
                None => (spec.parse().ok(), None),
            };
            match (start, end) {
                (Some(first), Some(second)) => (
                    file.to_string(),
                    Some(first.min(second)),
                    Some(first.max(second)),
                ),
                (Some(only), None) => (file.to_string(), Some(only), Some(only)),
                _ => (anchor_free.to_string(), None, None),
            }
        }
        None => (anchor_free.to_string(), None, None),
    };
    if !(file == range.file
        || file.ends_with(range.file.as_str())
        || range.file.ends_with(file.as_str()))
    {
        return false;
    }
    match (start, end) {
        (Some(first), Some(second)) => range.start <= second && range.end >= first,
        _ => true,
    }
}

/// Scan one sentence for `[Cn]`/`[Dn]` evidence tokens.
fn evidence_tokens(sentence: &str) -> Vec<String> {
    let bytes = sentence.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' && i + 2 < bytes.len() && (bytes[i + 1] == b'C' || bytes[i + 1] == b'D')
        {
            let mut j = i + 2;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 2 && j < bytes.len() && bytes[j] == b']' {
                tokens.push(sentence[i..=j].to_string());
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    tokens
}

/// Gate an answer on claim support: claims with no resolved
/// citation drop, contradicted and insufficient sentences drop, and
/// the rest render verbatim when verified or `[unverified]` when the
/// support verdict has not run. An empty result means evidence-only —
/// the caller renders sections without an answer.
#[must_use]
pub fn gate_answer(claims: &[Claim]) -> Option<String> {
    let kept: Vec<String> = claims
        .iter()
        .filter(|claim| claim.citation.is_some())
        .filter(|claim| {
            claim.support != Support::Contradicted && claim.support != Support::Insufficient
        })
        .map(|claim| match claim.support {
            Support::Verified => claim.text.clone(),
            _ => format!("{} [unverified]", claim.text),
        })
        .collect();
    if kept.is_empty() {
        return None;
    }
    Some(kept.join(" "))
}

/// Constrain a model answer to the displayed evidence: every
/// `path:line` citation must name a shown snippet file with the line
/// inside a shown range. Violations (unknown file, line outside every
/// shown range) fall back to an abstain-with-evidence template that
/// lists the shown ranges instead of echoing unverified claims.
/// Citation-free prose passes through: with no extractable claim the
/// model's own "say so" stands.
#[must_use]
pub fn constrain_answer_to_evidence(
    answer: &str,
    snippets: &[(String, String, u32, u32, String)],
) -> String {
    constrain_answer_to_evidence_with_cards(answer, snippets, &[])
}

/// Card-aware constrain: citations naming a shown card (verified
/// doc or project card) are whole-card citations — the path alone
/// binds, with no line range to check. Snippet spans rule code.
/// Capability briefs stay snippet-side: `considered` until calibrated.
#[must_use]
pub fn constrain_answer_to_evidence_with_cards(
    answer: &str,
    snippets: &[(String, String, u32, u32, String)],
    card_paths: &[String],
) -> String {
    fn suffixes(path: &str) -> Vec<&str> {
        let mut out = vec![path];
        let mut rest = path;
        while let Some(index) = rest.find('/') {
            rest = &rest[index + 1..];
            out.push(rest);
        }
        out
    }
    let cited: Vec<(String, u32, u32)> = crate::grounding::extract_citation_ranges(answer)
        .into_iter()
        .map(|range| (range.file, range.start, range.end))
        .collect();
    let mut violated = false;
    for (path, start, end) in cited {
        // Whole-card citations cover their path with no range check.
        if card_paths
            .iter()
            .any(|card| suffixes(card).iter().any(|suffix| *suffix == path))
        {
            continue;
        }
        let known = snippets
            .iter()
            .any(|(_, file, _, _, _)| suffixes(file).iter().any(|suffix| *suffix == path));
        if !known {
            violated = true;
            break;
        }
        let covered = snippets.iter().any(|(_, file, shown_start, shown_end, _)| {
            suffixes(file).iter().any(|suffix| *suffix == path)
                && start <= *shown_end
                && end >= *shown_start
        });
        if !covered {
            violated = true;
            break;
        }
    }
    if !violated {
        return answer.to_string();
    }
    let mut out = String::from(
        "The excerpts do not support a verified answer to this question. Shown evidence:\n",
    );
    for (_, file, start, end, _) in snippets {
        let _ = std::fmt::Write::write_fmt(&mut out, format_args!("- {file}:{start}-{end}\n"));
    }
    for card in card_paths {
        let _ = std::fmt::Write::write_fmt(&mut out, format_args!("- {card}\n"));
    }
    out
}

/// Deterministic entailment prefilter: a bound claim whose cited
/// range shares no identifier with the claim text is `Insufficient`
/// without spending a model call. The cited range must contain at
/// least one identifier-shaped term from the claim
/// (`read::vocabulary::symbol_terms`). Unbound claims and decided
/// claims pass through untouched; the gate drops the rest.
pub fn prefilter_support(claims: &mut [Claim], evidence: &[(String, String)]) {
    for claim in claims.iter_mut() {
        if claim.support != Support::Unverified {
            continue;
        }
        let Some(canonical) = claim.citation.as_deref() else {
            continue;
        };
        let Some((_, excerpt)) = evidence.iter().find(|(shown, _)| shown == canonical) else {
            continue;
        };
        let fold = excerpt.to_lowercase();
        let shares_identifier = crate::read::vocabulary::symbol_terms(&claim.text)
            .iter()
            .any(|term| fold.contains(term.as_str()));
        if !shares_identifier {
            claim.support = Support::Insufficient;
        }
    }
}

#[cfg(test)]
#[path = "../../tests/read_claims.rs"]
mod read_claims_tests;
