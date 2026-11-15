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
/// One extracted sentence plus its citation binding.
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
/// Split prose into sentences on `.`/`!`/`?` followed by whitespace
/// or end. Deterministic; abbreviations may over-split — binding is
/// per-sentence, so a split never invents support.
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

/// Bind each sentence to the first citation it names
/// (`path:line[-line]`). Sentences without a citation bind to
/// nothing; binding is structural, never semantic.
#[must_use]
/// Bind each sentence to the first citation it names
/// (`path:line[-line]`). Sentences without a citation bind to
/// nothing; binding is structural, never semantic.
pub fn bind_claims(answer: &str, citations: &[String]) -> Vec<Claim> {
    extract_sentences(answer)
        .into_iter()
        .map(|text| {
            let citation = citations
                .iter()
                .find(|citation| text.contains(citation.as_str()))
                .cloned();
            Claim {
                citation,
                support: Support::Unverified,
                text,
            }
        })
        .collect()
}

/// Gate an answer on claim support: drop contradicted and
/// insufficient sentences, label the rest `[unverified]`. An empty
/// result means evidence-only — the caller renders sections without
/// an answer. Until the support point calibrates, nothing verifies.
#[must_use]
/// Gate an answer on claim support: drop contradicted and
/// insufficient sentences, label the rest `[unverified]`. An empty
/// result means evidence-only — the caller renders sections without
/// an answer. Until the support point calibrates, nothing verifies.
pub fn gate_answer(claims: &[Claim]) -> Option<String> {
    let kept: Vec<String> = claims
        .iter()
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
/// Constrain a model answer to the displayed evidence: every
/// `path:line` citation must name a shown snippet file with the line
/// inside a shown range. Violations (unknown file, line outside every
/// shown range) fall back to an abstain-with-evidence template that
/// lists the shown ranges instead of echoing unverified claims.
/// Citation-free prose passes through: with no extractable claim the
/// model's own "say so" stands.
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
/// Card-aware constrain: citations naming a shown card (verified
/// doc or project card) are whole-card citations — the path alone
/// binds, with no line range to check. Snippet spans rule code.
/// Capability briefs stay snippet-side: `considered` until calibrated.
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
    fn parse_line_spec(spec: &str) -> Option<(u32, u32)> {
        let (first, second) = match spec.split_once('-') {
            Some((first, second)) => (first, second.split_whitespace().next().unwrap_or("")),
            None => (spec, ""),
        };
        let start: u32 = first.parse().ok()?;
        if second.is_empty() {
            return Some((start, start));
        }
        let end: u32 = second.parse().ok()?;
        Some((start.min(end), start.max(end)))
    }
    fn cited_spans(answer: &str) -> Vec<(String, u32, u32)> {
        let bytes = answer.as_bytes();
        let mut spans = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b':' {
                let mut head = i;
                while head > 0 {
                    let cell = bytes[head - 1];
                    if cell.is_ascii_alphanumeric() || matches!(cell, b'.' | b'_' | b'-' | b'/') {
                        head -= 1;
                    } else {
                        break;
                    }
                }
                let mut tail = i + 1;
                while tail < bytes.len() && bytes[tail].is_ascii_digit() {
                    tail += 1;
                }
                let mut tail_end = tail;
                if tail < bytes.len() && bytes[tail] == b'-' {
                    let mut digits = tail + 1;
                    while digits < bytes.len() && bytes[digits].is_ascii_digit() {
                        digits += 1;
                    }
                    if digits > tail + 1 {
                        tail_end = digits;
                    }
                }
                if head < i && tail > i + 1 {
                    let path = &answer[head..i];
                    let has_ext = path
                        .rsplit('/')
                        .next()
                        .is_some_and(|base| base.contains('.'));
                    if has_ext {
                        if let Some((start, end)) = parse_line_spec(&answer[i + 1..tail_end]) {
                            spans.push((path.to_string(), start, end));
                        }
                    }
                }
                i = tail_end.max(i + 1);
            } else {
                i += 1;
            }
        }
        spans
    }
    let mut violated = false;
    for (path, start, end) in cited_spans(answer) {
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

#[cfg(test)]
#[path = "../../tests/read_claims.rs"]
mod read_claims_tests;
