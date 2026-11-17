//! Ownership answers name every owner: the component table maps each
//! grounded evidence path to its package, and multi-owner results are
//! the truth, never ambiguity to hide.
use super::*;
use crate::query::search_db::GuidanceSearchDb;
use crate::tests::common::{db_file_record, db_project_card, db_text_fragment};

fn finding(id: &str, path: &str) -> Finding {
    Finding {
        id: id.to_string(),
        producer: FindingProducer::Database,
        kind: FindingKind::Code,
        source: crate::source_ref::SourceRef::from_workspace(
            std::path::Path::new("/workspace"),
            std::path::Path::new(path),
        ),
        start_line: Some(1),
        end_line: Some(3),
        text: "pub fn calibration_primitives() {}".to_string(),
        producer_score: 1.0,
        provenance: "database".to_string(),
    }
}

#[test]
fn one_symbol_in_two_crates_names_both_owners() {
    let stale = std::collections::HashSet::new();
    let rows = ownership_rows(
        &[
            finding("a", "src/common-core/src/calibration.rs"),
            finding("b", "src/guidance/src/query/calibration.rs"),
            finding("c", "src/common-core/src/other.rs"),
        ],
        None,
        &stale,
    );
    let bodies: Vec<&str> = rows.iter().map(|(_, body)| body.as_str()).collect();
    assert_eq!(
        bodies,
        vec!["owned by common-core", "owned by guidance-core"]
    );
    assert_eq!(rows.len(), 2, "one row per owner package");
}

fn ownership_workspace() -> (tempfile::TempDir, GuidanceSearchDb) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("write");
    let doc_text = "# Calibration\n\nDecision point promotion needs a passing registry entry.\n";
    std::fs::create_dir_all(root.join("doc/guidance")).expect("mkdir");
    std::fs::write(root.join("doc/guidance/calibration.md"), doc_text).expect("write");
    let code = |path: &str, symbol: &str, text: &str| {
        std::fs::create_dir_all(root.join(path).parent().expect("parent")).expect("mkdir");
        std::fs::write(root.join(path), text).expect("write");
        (path.to_string(), symbol.to_string(), text.to_string())
    };
    let common = code(
        "src/common-core/src/calibration.rs",
        "calibration_primitives",
        "/// Calibration decision point primitives.\npub fn calibration_primitives() {\n    // calibration decision point promotion\n}\n",
    );
    let guidance = code(
        "src/guidance/src/query/calibration.rs",
        "calibration_promotion",
        "/// Calibration decision point promotion.\npub fn calibration_promotion() {\n    // calibration decision point promotion\n}\n",
    );
    let db = GuidanceSearchDb::open_in_memory().expect("db");
    for (path, symbol, text) in [common, guidance] {
        db.upsert_fragments(
            &db_file_record(root, &path),
            &[db_text_fragment(&path, &symbol, &text, 4)],
            &[],
        )
        .expect("seed code");
    }
    let (chunks, _) =
        crate::read::doc_cards::chunk_markdown_sections("doc/guidance/calibration.md", doc_text);
    let chunk = chunks.first().expect("doc chunks");
    db.replace_project_cards(&[crate::query::search_db::ProjectCard {
        content_hash: chunk.content_hash.clone(),
        anchor: chunk.anchor.clone(),
        ..db_project_card(
            "doc:doc/guidance/calibration.md#calibration",
            "doc",
            "Calibration",
            &chunk.text,
            "doc/guidance/calibration.md",
            "",
        )
    }])
    .expect("cards seed");
    (dir, db)
}

fn explain(query: &str) -> ReadAnswer {
    let (dir, db) = ownership_workspace();
    let engine = ReadEngine::new();
    let ctx = ExplainContext {
        workspace: dir.path(),
        db: &db,
        nlp: None,
        coverage: None,
    };
    engine.explain(
        &ExplainRequest {
            query: query.to_string(),
            limit: 10,
            budget_override: None,
            no_llm: true,
            include_fixtures: false,
            answer_mode: AnswerMode::Auto,
        },
        &ctx,
        None,
    )
}

#[test]
fn anchored_owner_is_named_even_without_a_mapped_path() {
    let stale = std::collections::HashSet::new();
    let rows = ownership_rows(&[finding("a", "lib.rs")], Some("guidance-core"), &stale);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "owned by guidance-core");
}

#[test]
fn ownership_answers_name_every_owner() {
    let answer = explain("Which crate owns calibration, and how is a decision point promoted?");
    let body = answer
        .sections
        .iter()
        .find(|section| section.title == "Answer")
        .map(|section| section.body.clone())
        .unwrap_or_default();
    assert!(
        body.contains("common-core"),
        "calibration primitives owner named: {body}"
    );
    assert!(
        body.contains("guidance-core"),
        "decision-point owner named: {body}"
    );
    assert!(
        body.contains("src/common-core/src/calibration.rs")
            && body.contains("src/guidance/src/query/calibration.rs"),
        "owner rows cite their evidence: {body}"
    );
}

#[test]
fn ownership_without_owner_evidence_abstains() {
    let answer = explain("Which crate owns the zzqy turbulator?");
    assert!(
        !answer
            .sections
            .iter()
            .any(|section| section.title == "Answer"),
        "{:?}",
        answer.sections
    );
    assert!(
        answer
            .gaps
            .iter()
            .any(|gap| gap.starts_with("none found in covered index")),
        "{:?}",
        answer.gaps
    );
}
