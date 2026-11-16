//! Citation truth: excerpt digests bind the served text, file-level
//! repeats collapse, and claim binding uses the stable citation text.
use crate::source_ref::SourceRef;

fn source(path: &str) -> SourceRef {
    SourceRef::from_workspace(
        std::path::Path::new("/workspace"),
        std::path::Path::new(path),
    )
}

#[test]
fn stamp_equals_digest_of_rendered_text() {
    use crate::read::engine::{Finding, FindingKind, FindingProducer};
    let excerpt = "pub fn stale_fn() {}\n";
    let expected = common_core::hash::blake3_hex(excerpt.as_bytes());
    let finding = Finding {
        id: "code-stale:0".to_string(),
        producer: FindingProducer::Database,
        kind: FindingKind::Code,
        source: source("src/stale.rs"),
        start_line: Some(1),
        end_line: Some(1),
        text: excerpt.to_string(),
        producer_score: 1.0,
        provenance: "code".to_string(),
    };
    let citation = finding
        .source
        .citation(finding.start_line, finding.end_line)
        .with_digest(common_core::hash::blake3_hex(finding.text.as_bytes()));
    assert_eq!(citation.digest, expected);
    assert_eq!(citation.digest.len(), 64);
}

#[test]
fn no_duplicate_file_within_one_envelope() {
    use crate::read::engine::{Finding, FindingKind, FindingProducer};
    use crate::read::evidence::EvidenceSet;
    let mk = |id: &str, start: u32| Finding {
        id: id.to_string(),
        producer: FindingProducer::Database,
        kind: FindingKind::Code,
        source: source("src/dup.rs"),
        start_line: Some(start),
        end_line: Some(start + 5),
        text: format!("excerpt {id}"),
        producer_score: 1.0,
        provenance: "code".to_string(),
    };
    let mut set = EvidenceSet::new();
    set.extend(vec![mk("a", 1), mk("b", 40)]);
    set.dedupe();
    let files: Vec<String> = set
        .into_findings()
        .iter()
        .map(|f| f.source.display().to_string())
        .collect();
    let mut seen = std::collections::HashSet::new();
    for file in &files {
        assert!(seen.insert(file.clone()), "duplicate file {file}");
    }
    assert_eq!(files.len(), 1, "one file survives file-level dedupe");
}

#[test]
fn no_envelope_claim_with_null_citation() {
    let stable = source("src/a.rs").citation(Some(10), Some(20)).text();
    let stale_display = source("src/a.rs")
        .citation(Some(10), Some(20))
        .with_status(true, false)
        .to_string();
    assert_ne!(stable, stale_display);
    let answer = format!("Sync reprocesses stale files ({stable}).");
    let manifest = vec![("[C1]".to_string(), stable.clone())];
    let claims = crate::read::claims::bind_claims(&answer, &manifest);
    assert!(!claims.is_empty());
    assert!(
        claims.iter().all(|c| c.citation.is_some()),
        "stable text binds: {claims:?}"
    );
}
