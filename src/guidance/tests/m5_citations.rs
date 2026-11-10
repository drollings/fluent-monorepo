//! Citation-integrity pins: member persistence, range re-hash,
//! mutation flips, fabricated shapes, and the verification control group.

use fluent_types::StageKind;
use fluent_wvr_testutil::tempdir;

use crate::grounding::{verify_stage_ranges, CitationFreshness, MemberRange};
use crate::query::synthesize::Stage;

fn code_stage(source: &str, line: u32, member: &str) -> Stage {
    Stage {
        kind: StageKind::Code,
        content: "fn f() {}".to_string(),
        source: source.to_string(),
        line: Some(line),
        end_line: Some(line + 2),
        member_name: Some(member.to_string()),
        member_type: None,
        trace: None,
    }
}

fn parse_range(source: &str, member: &str) -> MemberRange {
    let mut parser = crate::ast_parser::AstParser::new();
    let doc = parser
        .parse_file(std::path::Path::new("m.rs"), source)
        .expect("rust parses");
    let found = doc
        .members
        .iter()
        .find(|m| m.name.as_str() == member)
        .expect("member parsed");
    let (start, end) = (
        found.span_start.expect("span stamped"),
        found.span_end.expect("span stamped"),
    );
    let range = MemberRange {
        start,
        end,
        range_hash: found.range_hash.as_deref().expect("range hash").to_string(),
        item_hash: found.item_hash.as_deref().map(str::to_string),
    };
    assert!(end > start, "span must cover the item");
    assert!(end as usize <= source.len(), "span must stay in bounds");
    let covered = &source.as_bytes()[start as usize..end as usize];
    assert!(covered.starts_with(b"pub fn f()"), "span must start at the item");
    range
}

#[test]
fn member_spans_survive_sidecar_round_trip() {
    let dir = tempdir();
    let source = "/// Adds one.\npub fn f() -> u64 {\n    1\n}\n";
    let mut parser = crate::ast_parser::AstParser::new();
    let doc = parser
        .parse_file(std::path::Path::new("m.rs"), source)
        .expect("rust parses");
    assert!(!doc.members.is_empty());
    for member in &doc.members {
        assert!(member.span_start.is_some(), "span stamped");
        assert!(member.span_end.is_some(), "span stamped");
        assert!(member.item_hash.is_some(), "item hash stamped");
        assert!(member.range_hash.is_some(), "range hash stamped");
    }
    let path = dir.path().join("m.json");
    let receipt = crate::components::SidecarReceipt::default();
    crate::sync::json_store::save_guidance(&path, &doc, &receipt).expect("save");
    let loaded = crate::sync::json_store::load_guidance(&path)
        .expect("load")
        .expect("doc");
    assert_eq!(loaded.members.len(), doc.members.len());
    for (old, new) in doc.members.iter().zip(loaded.members.iter()) {
        assert_eq!(old.span_start, new.span_start);
        assert_eq!(old.span_end, new.span_end);
        assert_eq!(old.item_hash, new.item_hash);
        assert_eq!(old.range_hash, new.range_hash);
    }
}

#[test]
fn range_rehash_matches_live_bytes() {
    let dir = tempdir();
    let body = "pub fn f() -> u64 {\n    1\n}\n";
    let file = dir.path().join("m.rs");
    std::fs::write(&file, body).expect("write");
    let range = parse_range(body, "f");
    let path = file.to_string_lossy().into_owned();
    let cited = verify_stage_ranges(&[code_stage(&path, 1, "f")], dir.path(), |_| {
        Some(MemberRange {
            start: range.start,
            end: range.end,
            range_hash: range.range_hash.clone(),
            item_hash: range.item_hash.clone(),
        })
    });
    assert_eq!(cited.len(), 1);
    assert_eq!(cited[0].freshness, CitationFreshness::Fresh);
    assert!(cited[0].cite.contains("@ b3:"), "new shape carries a hash");
    assert!(cited[0].cite.contains(":L1-L3"), "new shape carries a range");
}

#[test]
fn single_byte_mutation_flips_stale_and_names_the_item() {
    let dir = tempdir();
    let body = "pub fn f() -> u64 {\n    1\n}\n";
    let file = dir.path().join("m.rs");
    std::fs::write(&file, body).expect("write");
    let range = parse_range(body, "f");
    let path = file.to_string_lossy().into_owned();
    let lookup = |_: &_| {
        Some(MemberRange {
            start: range.start,
            end: range.end,
            range_hash: range.range_hash.clone(),
            item_hash: range.item_hash.clone(),
        })
    };
    let fresh = verify_stage_ranges(&[code_stage(&path, 1, "f")], dir.path(), lookup);
    assert_eq!(fresh[0].freshness, CitationFreshness::Fresh);
    // Mutate one byte inside the range: stale, and the item is named.
    std::fs::write(&file, "pub fn f() -> u64 {\n    2\n}\n").expect("mutate");
    let stale = verify_stage_ranges(&[code_stage(&path, 1, "f")], dir.path(), lookup);
    assert_eq!(stale[0].freshness, CitationFreshness::Stale);
    assert_eq!(stale[0].member.as_deref(), Some("f"));
    let requeue = crate::grounding::stale_members(&stale);
    assert_eq!(requeue, vec![(path.clone(), "f".to_string())]);
    // Mutate outside the range: the cited range still verifies.
    std::fs::write(
        &file,
        "pub fn f() -> u64 {\n    2\n}\n\npub fn g() -> u64 {\n    3\n}\n",
    )
    .expect("append");
    std::fs::write(&file, body).expect("restore");
    let restored = verify_stage_ranges(&[code_stage(&path, 1, "f")], dir.path(), lookup);
    assert_eq!(restored[0].freshness, CitationFreshness::Fresh);
}

#[test]
fn missing_row_is_unverified_never_fresh() {
    let dir = tempdir();
    let file = dir.path().join("m.rs");
    std::fs::write(&file, "pub fn f() -> u64 {\n    1\n}\n").expect("write");
    let path = file.to_string_lossy().into_owned();
    let cited = verify_stage_ranges(&[code_stage(&path, 1, "f")], dir.path(), |_| None);
    assert_eq!(cited[0].freshness, CitationFreshness::Unverified);
    assert!(!cited[0].cite.contains("@ b3:"));
}

#[test]
fn fabricated_new_shape_citations_reject() {
    let stages = vec![code_stage("src/main.rs", 42, "f")];
    let exact = "see src/main.rs:L42-L50 @ b3:0123456789abcdef for details";
    assert!(crate::grounding::verify_citations(exact, &stages).is_grounded());
    let wrong_line = "see src/main.rs:L99-L101 @ b3:0123456789abcdef for details";
    assert!(!crate::grounding::verify_citations(wrong_line, &stages).is_grounded());
    let wrong_file = "see src/other.rs:L42-L50 @ b3:0123456789abcdef for details";
    assert!(!crate::grounding::verify_citations(wrong_file, &stages).is_grounded());
    let legacy = "see main.rs:42 for details";
    assert!(crate::grounding::verify_citations(legacy, &stages).is_grounded());
}

#[test]
fn citation_control_group_measures_before_caching() {
    // Control record: exact pairs verify, fabricated pairs reject, with
    // no cache in the path — run this before trusting any citation cache.
    let stages = vec![
        code_stage("src/nested/main.rs", 7, "a"),
        code_stage("src/main.rs", 42, "f"),
    ];
    let positives = [
        "main.rs:7",
        "main.rs:42",
        "src/main.rs:L42-L50 @ b3:deadbeef",
        "nested/main.rs:L7-L9 @ b3:deadbeef",
    ];
    let negatives = [
        "main.rs:8",
        "other.rs:42",
        "src/main.rs:L43-L50 @ b3:deadbeef",
        "src/other.rs:L42-L50 @ b3:deadbeef",
    ];
    let mut true_positives = 0;
    for output in positives {
        if crate::grounding::verify_citations(output, &stages).is_grounded() {
            true_positives += 1;
        }
    }
    let mut true_negatives = 0;
    for output in negatives {
        if !crate::grounding::verify_citations(output, &stages).is_grounded() {
            true_negatives += 1;
        }
    }
    assert_eq!(
        (true_positives, true_negatives),
        (positives.len(), negatives.len()),
        "control group must verify at full recall with zero false accepts"
    );
}

#[test]
fn envelope_carries_shape_freshness_and_gaps() {
    use crate::query::envelope::AnswerEnvelope;
    use crate::query::strategy::FsmEngine;
    let dir = tempdir();
    let body = "pub fn f() -> u64 {\n    1\n}\n";
    let file = dir.path().join("m.rs");
    std::fs::write(&file, body).expect("write");
    let range = parse_range(body, "f");
    let path = file.to_string_lossy().into_owned();
    let stages = vec![code_stage(&path, 1, "f")];
    let class = FsmEngine::new().run("f");
    let cited = verify_stage_ranges(&stages, dir.path(), |_| {
        Some(MemberRange {
            start: range.start,
            end: range.end,
            range_hash: range.range_hash.clone(),
            item_hash: range.item_hash.clone(),
        })
    });
    let envelope = AnswerEnvelope::from_verified_stages(&class, "word_index", stages, &cited);
    assert!(envelope.citations.iter().all(|c| c.contains("@ b3:")));
    assert_eq!(envelope.citation_freshness, CitationFreshness::Fresh);
    assert!(envelope.is_answerable());
    // A stale citation flips the envelope summary and files a gap.
    std::fs::write(&file, "pub fn f() -> u64 {\n    9\n}\n").expect("mutate");
    let stages = vec![code_stage(&path, 1, "f")];
    let cited = verify_stage_ranges(&stages, dir.path(), |_| {
        Some(MemberRange {
            start: range.start,
            end: range.end,
            range_hash: range.range_hash.clone(),
            item_hash: range.item_hash.clone(),
        })
    });
    let envelope = AnswerEnvelope::from_verified_stages(&class, "word_index", stages, &cited);
    assert_eq!(envelope.citation_freshness, CitationFreshness::Stale);
    assert!(envelope.gaps.iter().any(|g| g.contains("stale")));
}
