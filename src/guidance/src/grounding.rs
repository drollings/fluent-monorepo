//! Grounding enforcement — ensures LLM synthesis is grounded in source excerpts.
//!
//! Implements the VISION's "no synthesis without source" invariant:
//! - Pre-synthesis: verifies Code stages have source + line numbers
//! - Post-synthesis: parses LLM output for file:line citations and verifies
//!   each citation exists in the provided stages

use std::collections::HashSet;

use fluent_types::GuidanceDoc;

use crate::query::synthesize::Stage;

/// Result of grounding verification on LLM synthesis output.
#[derive(Debug, Clone)]
pub struct GroundingResult {
    /// True if all citations in the output trace to provided stages.
    pub verified: bool,
    /// Citations found in the output that match provided stages.
    pub verified_citations: Vec<String>,
    /// Citations found in the output that do NOT match any provided stage.
    pub unverified_citations: Vec<String>,
}

impl GroundingResult {
    pub fn is_grounded(&self) -> bool {
        self.verified && self.unverified_citations.is_empty()
    }
}

/// Check whether the stages provide sufficient grounding for LLM synthesis.
///
/// Returns true if at least one Code stage has both a non-empty source
/// and a line number — this is the minimum requirement for grounded synthesis.
pub fn can_synthesize(stages: &[Stage]) -> bool {
    stages.iter().any(|s| {
        s.kind == fluent_types::StageKind::Code && !s.source.is_empty() && s.line.is_some()
    })
}

pub use common_core::cite::{extract_citation_at, extract_citations, Citation};

/// Stored byte range plus hashes for one member, as stamped at parse.
#[derive(Debug, Clone)]
pub struct MemberRange {
    /// Byte start of the member span.
    pub start: u64,
    /// Byte end (exclusive) of the member span.
    pub end: u64,
    /// Byte-exact hash of the span at stamp time.
    pub range_hash: String,
    /// Normalized item identity, when recorded.
    pub item_hash: Option<String>,
}

/// Freshness of one citation against live bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CitationFreshness {
    /// Live range re-hashes to the stored receipt.
    Fresh,
    /// Live bytes moved under the citation; the item requeues.
    Stale,
    /// No stored row to check against; hash the live bytes and say so.
    Unverified,
}

/// One verified citation line plus its verdict.
#[derive(Debug, Clone)]
pub struct StageCitation {
    /// Rendered citation (`path:Lstart-Lend @ b3:hash`, or legacy shape).
    pub cite: String,
    /// Verdict against live bytes.
    pub freshness: CitationFreshness,
    /// Cited member name, when the stage names one.
    pub member: Option<String>,
    /// Stage source path, as cited.
    pub source: String,
    /// Resolution tier that produced the stage (`fts`, `vector`,
    /// `fts+vector`, or `doc` for parse-built stages).
    pub tier: String,
}

/// Map a stage tier string to its retrieval label. Known vocabularies
/// map exactly; anything else returns `None` rather than inventing
/// precision.
#[must_use]
pub fn retrieval_tier_for_stage_tier(tier: &str) -> Option<crate::search_types::RetrievalTier> {
    use crate::search_types::RetrievalTier;
    match tier {
        "doc" => Some(RetrievalTier::Exact),
        "fts" => Some(RetrievalTier::Lexical),
        "vector" => Some(RetrievalTier::Semantic),
        "fts+vector" => Some(RetrievalTier::Fusion),
        _ => None,
    }
}

impl StageCitation {
    /// Additive retrieval label for this citation's tier. `None` when
    /// the tier string is outside the known vocabulary.
    #[must_use]
    pub fn retrieval_tier(&self) -> Option<crate::search_types::RetrievalTier> {
        retrieval_tier_for_stage_tier(&self.tier)
    }
}

/// Short display form of a hash (long enough to spot-check, never the
/// comparison input — comparison always uses the full stored hash).
fn short_hash(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

/// Re-hash every code stage's stored range against live bytes. `lookup`
/// maps a stage to its stamped row (sidecar, index, or test fixture);
/// `root` resolves relative sources. Mismatches name their item so the
/// orchestrator can requeue exactly that unit; the read plane itself
/// writes nothing.
#[must_use]
pub fn verify_stage_ranges(
    stages: &[Stage],
    root: &std::path::Path,
    lookup: impl Fn(&Stage) -> Option<MemberRange>,
) -> Vec<StageCitation> {
    stages
        .iter()
        .filter(|stage| stage.kind == fluent_types::StageKind::Code)
        .map(|stage| verify_one_stage(stage, root, &lookup))
        .collect()
}

fn resolve_source(source: &str, root: &std::path::Path) -> std::path::PathBuf {
    let path = std::path::Path::new(source);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn verify_one_stage(
    stage: &Stage,
    root: &std::path::Path,
    lookup: &dyn Fn(&Stage) -> Option<MemberRange>,
) -> StageCitation {
    let member = stage.member_name.clone();
    let source = stage.source.clone();
    let tier = stage.trace.as_ref().map_or_else(
        || "doc".to_owned(),
        |trace| trace.matched_by.clone(),
    );
    let Some(row) = lookup(stage) else {
        return StageCitation {
            cite: legacy_cite(stage),
            freshness: CitationFreshness::Unverified,
            member,
            source,
            tier,
        };
    };
    let path = resolve_source(&stage.source, root);
    let live = std::fs::read(&path)
        .ok()
        .and_then(|bytes| bytes.get(row.start as usize..row.end as usize).map(ToOwned::to_owned));
    let freshness = match live {
        Some(range) if common_core::hash::blake3_hex(&range) == row.range_hash => {
            CitationFreshness::Fresh
        }
        _ => CitationFreshness::Stale,
    };
    StageCitation {
        cite: ranged_cite(stage, &row.range_hash),
        freshness,
        member,
        source,
        tier,
    }
}

/// Legacy `source:line` shape for rows without hashes. Accepted as
/// input everywhere; emitted only when no receipt exists to hash.
fn legacy_cite(stage: &Stage) -> String {
    match stage.line {
        Some(line) => format!("{}:{line}", stage.source),
        None => stage.source.clone(),
    }
}

/// Hash-carrying shape with the stage's line range when known.
fn ranged_cite(stage: &Stage, range_hash: &str) -> String {
    let short = short_hash(range_hash);
    match (stage.line, stage.end_line) {
        (Some(first), Some(last)) => format!("{}:L{first}-L{last} @ b3:{short}", stage.source),
        (Some(first), None) => format!("{}:L{first} @ b3:{short}", stage.source),
        (None, _) => format!("{} @ b3:{short}", stage.source),
    }
}

/// Stale items as `(source, member)` requeue targets for the
/// orchestrator. The bytes-decide staleness gate picks them up on the
/// next pass; this list only names them.
#[must_use]
pub fn stale_members(cited: &[StageCitation]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for citation in cited {
        if citation.freshness == CitationFreshness::Stale {
            if let Some(member) = &citation.member {
                out.push((citation.source.clone(), member.clone()));
            }
        }
    }
    out
}

/// Citations in the emitted shape (`path:Lstart-Lend @ b3:hash` or
/// `path:Lstart`): the file-plus-start-line pairs they claim, so model
/// output that quotes verified citations checks against the same
/// grounded set as the legacy `path:line` shape.
fn extract_ranged_citations(output: &str) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    let mut rest = output;
    let mut base = 0;
    while let Some(found) = rest.find(":L") {
        let abs = base + found;
        let before = &output[..abs];
        let file_start = before
            .rfind(|c: char| {
                !c.is_alphanumeric() && c != '/' && c != '\\' && c != '.' && c != '_' && c != '-'
            })
            .map_or(0, |p| p + 1);
        let file_part = &before[file_start..];
        let after = &output[abs + 2..];
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        if !file_part.is_empty() && file_part.contains('.') && !digits.is_empty() {
            if let Ok(line) = digits.parse::<u32>() {
                out.push((file_part.to_string(), line));
            }
        }
        rest = &rest[found + 2..];
        base = abs + 2;
    }
    out
}

/// Verify that all citations in LLM output trace to provided stages.
///
/// Builds a set of grounded `(basename, line)` pairs from Code stages, then
/// checks each citation in the output against this set. The match is exact
/// on both halves: a citation to the right file at a fabricated line does
/// NOT verify (line enforcement — the one behavior change of this
/// comparator; basename symmetry is preserved for exact pairs).
pub fn verify_citations(output: &str, stages: &[Stage]) -> GroundingResult {
    let grounded_refs: HashSet<(String, u32)> = stages
        .iter()
        .filter(|s| s.kind == fluent_types::StageKind::Code && s.line.is_some())
        .map(|s| {
            let file = common_core::path::file_name_lexical(&s.source).unwrap_or(&s.source);
            (file.to_string(), s.line.unwrap())
        })
        .collect();

    let citations = extract_citations(output);
    let mut claimed: Vec<(String, u32)> = citations
        .iter()
        .map(|citation| {
            let file =
                common_core::path::file_name_lexical(&citation.file).unwrap_or(&citation.file);
            (file.to_string(), citation.line)
        })
        .collect();
    for (file, line) in extract_ranged_citations(output) {
        let base = common_core::path::file_name_lexical(&file).unwrap_or(&file);
        let pair = (base.to_string(), line);
        if !claimed.contains(&pair) {
            claimed.push(pair);
        }
    }

    let mut verified = Vec::new();
    let mut unverified = Vec::new();
    for (citation_file, citation_line) in &claimed {
        let citation_str = format!("{citation_file}:{citation_line}");        let is_grounded = grounded_refs.contains(&(citation_file.clone(), *citation_line));

        if is_grounded {
            verified.push(citation_str.clone());
        } else {
            unverified.push(citation_str.clone());
        }
    }

    let all_verified = unverified.is_empty();
    GroundingResult {
        verified: all_verified,
        verified_citations: verified,
        unverified_citations: unverified,
    }
}

/// Resolve requested names against current members, mirroring
/// `Synthesizer::synthesize`'s own lookup predicate exactly (same match,
/// no second implementation). Empty means stale index names or rogue
/// filter output: the caller takes the pinned gap instead of
/// synthesizing from no excerpts.
#[must_use]
pub fn grounded_names(doc: &GuidanceDoc, names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|name| {
            doc.members
                .iter()
                .any(|member| member.name.as_str() == name.as_str())
        })
        .cloned()
        .collect()
}

/// Content hash over grounded stages (source + line + content) for
/// synthesis cache keys. Changed source changes the key, so a stored
/// summary can never override fresh bytes.
#[must_use]
pub fn stage_content_hash(stages: &[Stage]) -> String {
    let mut bytes = Vec::new();
    for stage in stages {
        bytes.extend_from_slice(stage.source.as_bytes());
        bytes.extend_from_slice(&stage.line.unwrap_or(0).to_le_bytes());
        bytes.extend_from_slice(stage.content.as_bytes());
    }
    common_core::hash::blake3_hex(&bytes)
}

#[cfg(test)]
#[path = "../tests/synthesis_gate.rs"]
mod synthesis_gate_tests;

#[cfg(test)]
#[path = "../tests/m5_citations.rs"]
mod m5_citations_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use fluent_types::StageKind;

    fn make_code_stage(source: &str, line: u32) -> Stage {
        Stage {
            kind: StageKind::Code,
            content: "fn test() {}".to_string(),
            source: source.to_string(),
            line: Some(line),
            end_line: None,
            member_name: Some("test".to_string()),
            member_type: None,
            trace: None,
        }
    }

    fn make_prose_stage(content: &str) -> Stage {
        Stage {
            kind: StageKind::Prose,
            content: content.to_string(),
            source: String::new(),
            line: None,
            end_line: None,
            member_name: None,
            member_type: None,
            trace: None,
        }
    }

    #[test]
    fn test_can_synthesize_with_grounded_code() {
        let stages = vec![
            make_prose_stage("Module description"),
            make_code_stage("src/main.rs", 42),
        ];
        assert!(can_synthesize(&stages));
    }

    #[test]
    fn test_can_synthesize_without_code() {
        let stages = vec![make_prose_stage("Only prose, no code")];
        assert!(!can_synthesize(&stages));
    }

    #[test]
    fn test_can_synthesize_with_code_no_line() {
        let stages = vec![Stage {
            kind: StageKind::Code,
            content: "fn test() {}".to_string(),
            source: "src/main.rs".to_string(),
            line: None,
            end_line: None,
            member_name: None,
            member_type: None,
            trace: None,
        }];
        assert!(!can_synthesize(&stages));
    }

    #[test]
    fn test_can_synthesize_empty() {
        assert!(!can_synthesize(&[]));
    }

    #[test]
    fn test_extract_citations_basic() {
        let text = "See src/main.rs:42 for details";
        let cites = extract_citations(text);
        assert_eq!(cites.len(), 1);
        assert_eq!(cites[0].file, "src/main.rs");
        assert_eq!(cites[0].line, 42);
    }

    #[test]
    fn test_extract_citations_multiple() {
        let text = "Implemented in foo.rs:10 and bar.rs:20";
        let cites = extract_citations(text);
        assert!(cites.iter().any(|c| c.file == "foo.rs" && c.line == 10));
        assert!(cites.iter().any(|c| c.file == "bar.rs" && c.line == 20));
    }

    #[test]
    fn test_extract_citations_no_file_extension() {
        let text = "See Makefile:42";
        let cites = extract_citations(text);
        assert!(
            cites.is_empty(),
            "citations without file extension should be ignored"
        );
    }

    #[test]
    fn test_verify_citations_all_grounded() {
        let stages = vec![make_code_stage("src/main.rs", 42)];
        let output = "The function is at main.rs:42 and does X";
        let result = verify_citations(output, &stages);
        assert!(result.is_grounded());
        assert!(result.unverified_citations.is_empty());
    }

    #[test]
    fn test_verify_citations_unverified() {
        let stages = vec![make_code_stage("src/main.rs", 42)];
        let output = "Related to main.rs:42 and also other.rs:99";
        let result = verify_citations(output, &stages);
        assert!(!result.is_grounded());
        assert!(result
            .unverified_citations
            .contains(&"other.rs:99".to_string()));
    }

    #[test]
    fn test_verify_citations_no_citations() {
        let stages = vec![make_code_stage("src/main.rs", 42)];
        let output = "This function does something interesting.";
        let result = verify_citations(output, &stages);
        assert!(result.is_grounded());
        assert!(result.verified_citations.is_empty());
    }

    #[test]
    fn test_verify_citations_empty_output() {
        let stages = vec![make_code_stage("src/main.rs", 42)];
        let result = verify_citations("", &stages);
        assert!(result.is_grounded());
    }

    // M1.1 characterization: basename extraction pinned verbatim before the
    // `common_core::path::file_name_lexical` migration. Basenames compare by
    // final `/` segment only — both sides go through the same extraction, so
    // the relation is symmetric.
    #[test]
    fn m1_basename_trailing_slash_and_nesting() {
        let stages = vec![make_code_stage("src/nested/main.rs", 7)];
        let grounded = verify_citations("see main.rs:7", &stages);
        assert!(grounded.is_grounded());
        let unverified = verify_citations("see nested.rs:7", &stages);
        assert!(!unverified.is_grounded());
    }

    #[test]
    fn m1_basename_backslash_input() {
        // `file_name_lexical` splits `/` only (like the `std` extraction
        // before it — backslash splitting lives in `normalize_lexical`,
        // which callers apply first). A raw backslash source therefore
        // keeps the whole string as its basename both pre- and post-M1.6:
        // no behavior change at this call site.
        let stages = vec![make_code_stage("src\\main.rs", 7)];
        let result = verify_citations("see main.rs:7", &stages);
        assert!(
            !result.is_grounded(),
            "backslash basename does not match: {result:?}"
        );
    }

    // Calibration gate for line enforcement: the control group below must
    // NOT verify (100% rejection) and the positives must (100% recall)
    // before verification results may be cached or persisted.
    #[test]
    fn citation_line_wrong_line_does_not_verify() {
        // Right file, fabricated line — the core hole: the line was
        // decorative before enforcement.
        let stages = vec![make_code_stage("src/main.rs", 42)];
        let result = verify_citations("see main.rs:99", &stages);
        assert!(!result.is_grounded(), "wrong line must not verify");
        assert!(result.verified_citations.is_empty());
        assert_eq!(result.unverified_citations, vec!["main.rs:99".to_string()]);
    }

    #[test]
    fn citation_line_wrong_file_right_line_does_not_verify() {
        let stages = vec![make_code_stage("src/main.rs", 42)];
        let result = verify_citations("see other.rs:42", &stages);
        assert!(!result.is_grounded(), "wrong file must not verify");
        assert!(result.verified_citations.is_empty());
    }

    #[test]
    fn citation_line_extensionless_citation_never_verifies() {
        // `Makefile:42` is never extracted (no extension), so there is
        // nothing to verify — vacuous grounding with empty citation lists.
        let stages = vec![make_code_stage("Makefile", 42)];
        let result = verify_citations("See Makefile:42", &stages);
        assert!(result.verified_citations.is_empty());
        assert!(result.unverified_citations.is_empty());
    }

    #[test]
    fn citation_line_backslash_source_does_not_verify() {
        // Raw backslash source keeps the whole string as basename, so a
        // forward-slash citation must not match it.
        let stages = vec![make_code_stage("src\\main.rs", 7)];
        let result = verify_citations("see main.rs:7", &stages);
        assert!(!result.is_grounded());
        assert!(result.verified_citations.is_empty());
    }

    #[test]
    fn citation_line_exact_pairs_verify() {
        // Positives: exact (basename, line) pairs verify, including nested
        // sources via basename symmetry and repeated basenames at
        // distinct lines.
        let stages = vec![
            make_code_stage("src/nested/main.rs", 7),
            make_code_stage("src/main.rs", 42),
        ];
        let result = verify_citations("see main.rs:7 and main.rs:42", &stages);
        assert!(result.is_grounded(), "exact pairs must verify");
        assert_eq!(result.verified_citations.len(), 2);
    }

    #[test]
    fn citation_line_mixed_exact_and_fabricated_splits() {
        // One exact pair verifies while the same file at a fabricated line
        // does not — per-citation precision, not whole-output guessing.
        let stages = vec![make_code_stage("src/main.rs", 42)];
        let result = verify_citations("see main.rs:42 and main.rs:43", &stages);
        assert!(!result.is_grounded());
        assert_eq!(result.verified_citations, vec!["main.rs:42".to_string()]);
        assert_eq!(result.unverified_citations, vec!["main.rs:43".to_string()]);
    }
}
