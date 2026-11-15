//! Evidence-set dedupe pins.
use crate::read::engine::{Finding, FindingKind, FindingProducer};
use crate::read::evidence::EvidenceSet;
use crate::source_ref::SourceRef;

fn finding(id: &str, path: &str, start: Option<u32>) -> Finding {
    finding_with_text(id, path, start, start, &format!("text {id}"))
}

fn finding_with_text(
    id: &str,
    path: &str,
    start: Option<u32>,
    end: Option<u32>,
    text: &str,
) -> Finding {
    Finding {
        id: id.to_string(),
        producer: FindingProducer::Database,
        kind: FindingKind::Code,
        source: SourceRef::from_workspace(
            std::path::Path::new("/workspace"),
            std::path::Path::new(path),
        ),
        start_line: start,
        end_line: end,
        text: text.to_string(),
        producer_score: 1.0,
        provenance: "code".to_string(),
    }
}

#[test]
fn dedupe_keeps_first_location_and_drops_id_dupes() {
    let mut set = EvidenceSet::new();
    set.extend(vec![
        finding("a", "src/a.rs", Some(1)),
        finding("b", "src/a.rs", Some(1)),
        finding("a", "src/b.rs", Some(2)),
        finding("c", "src/b.rs", Some(2)),
    ]);
    set.dedupe();
    let ids: Vec<String> = set.into_findings().iter().map(|f| f.id.clone()).collect();
    assert_eq!(ids, vec!["a".to_string(), "c".to_string()]);
}

#[test]
fn dedupe_unions_distinct_excerpts_for_one_file() {
    // One file, two distinct excerpts: the row keeps both texts so a
    // fact living only in the shorter excerpt still renders, while the
    // cited span follows the longest excerpt (span stability).
    let mut set = EvidenceSet::new();
    set.extend(vec![
        finding_with_text(
            "sig",
            "src/a.rs",
            Some(1),
            Some(1),
            "pub fn clean_build_cache() {}",
        ),
        finding_with_text(
            "prose",
            "src/a.rs",
            Some(40),
            Some(45),
            "Command entry: clean, sync, and explain over one workspace.",
        ),
    ]);
    set.dedupe();
    let rows = set.into_findings();
    assert_eq!(rows.len(), 1, "one file cites once");
    assert!(
        rows[0].text.contains("pub fn clean_build_cache() {}"),
        "shorter excerpt survives: {}",
        rows[0].text
    );
    assert!(
        rows[0].text.contains("Command entry"),
        "longest excerpt survives: {}",
        rows[0].text
    );
    assert_eq!(rows[0].start_line, Some(40));
    assert_eq!(rows[0].end_line, Some(45));
}

#[test]
fn dedupe_drops_excerpt_contained_in_winner() {
    // A shorter excerpt fully contained in the longest adds no
    // information: the row keeps the longest verbatim, not doubled.
    let long = "pub fn alpha() -> u64 {\n    1\n}\n";
    let mut set = EvidenceSet::new();
    set.extend(vec![
        finding_with_text("long", "src/a.rs", Some(1), Some(4), long),
        finding_with_text(
            "short",
            "src/a.rs",
            Some(2),
            Some(2),
            "pub fn alpha() -> u64 {",
        ),
    ]);
    set.dedupe();
    let rows = set.into_findings();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].text, long);
}

#[test]
fn dedupe_bounds_merged_row_by_excerpt_cap() {
    // The union never costs more than one excerpt's budget: a distinct
    // excerpt that would push the row past the cap is dropped, and a
    // small one still merges.
    use crate::read::engine::RG_EXCERPT_CHAR_CAP;
    let mut full = EvidenceSet::new();
    full.extend(vec![
        finding_with_text(
            "big",
            "src/a.rs",
            Some(1),
            Some(9),
            &"x".repeat(RG_EXCERPT_CHAR_CAP - 10),
        ),
        finding_with_text("extra", "src/a.rs", Some(20), Some(21), &"y".repeat(50)),
    ]);
    full.dedupe();
    let rows = full.into_findings();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].text.len(), RG_EXCERPT_CHAR_CAP - 10);
    let mut roomy = EvidenceSet::new();
    roomy.extend(vec![
        finding_with_text("big", "src/a.rs", Some(1), Some(9), &"x".repeat(500)),
        finding_with_text("extra", "src/a.rs", Some(20), Some(21), &"y".repeat(50)),
    ]);
    roomy.dedupe();
    let rows = roomy.into_findings();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].text.contains(&"x".repeat(500)));
    assert!(rows[0].text.contains(&"y".repeat(50)));
}
