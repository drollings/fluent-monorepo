//! Evidence-set dedupe pins.
use crate::read::engine::{Finding, FindingKind, FindingProducer};
use crate::read::evidence::EvidenceSet;
use crate::source_ref::SourceRef;

fn finding(id: &str, path: &str, start: Option<u32>) -> Finding {
    Finding {
        id: id.to_string(),
        producer: FindingProducer::Database,
        kind: FindingKind::Code,
        source: SourceRef::from_workspace(
            std::path::Path::new("/workspace"),
            std::path::Path::new(path),
        ),
        start_line: start,
        end_line: start,
        text: format!("text {id}"),
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
