//! Hybrid-search calibration: `min_matches` noise gate + `k_constant` fusion
//! lock-in, with a must-not-fire control group.
//!
//! Cost/recall heuristic, not confidence and not correctness: these constants
//! decide how much evidence to fuse, never whether an answer is right.

#![allow(clippy::float_cmp)]

use super::*;

fn cal_emb(i: usize) -> Vec<f32> {
    (0..4)
        .map(|j| (((i * 31 + j * 17 + 7) % 97) as f32 + 1.0) / 98.0)
        .collect()
}

fn cal_db() -> VectorDocStore {
    let db = VectorDocStore::open_in_memory().expect("in-memory");
    db.insert_node(
        "alpha beta gamma",
        "s",
        None,
        Some("alpha test doc"),
        "m",
        "l",
        Some(&cal_emb(1)),
    )
    .expect("insert A");
    db.insert_node(
        "delta epsilon zeta",
        "s",
        None,
        Some("unrelated doc"),
        "m",
        "l",
        Some(&cal_emb(2)),
    )
    .expect("insert B");
    db.insert_node(
        "coral reef survey",
        "s",
        None,
        Some("reef changelog"),
        "m",
        "l",
        Some(&cal_emb(3)),
    )
    .expect("insert C");
    db.insert_node(
        "quantum entanglement review",
        "s",
        None,
        Some("physics review"),
        "m",
        "l",
        Some(&cal_emb(4)),
    )
    .expect("insert D");
    db
}

#[test]
fn incidental_single_token_overlap_must_not_fire() {
    // 4 significant tokens → min_matches = 2. Doc C shares only "module"
    // (1 hit): the keyword leg must exclude it. (With a vector supplied,
    // the vector leg may still surface C on embedding similarity — that is
    // the recall half doing its job, not the noise gate failing.)
    let db = cal_db();
    let kw_only = db
        .hybrid_search("coral module quantum entanglement", None, 5)
        .expect("hybrid");
    assert!(
        kw_only.iter().all(|r| r.name != "coral reef survey"),
        "incidental 1-of-4 overlap excluded from keyword leg: {:?}",
        kw_only.iter().map(|r| &r.name).collect::<Vec<_>>()
    );
    assert!(
        kw_only
            .iter()
            .any(|r| r.name == "quantum entanglement review"),
        "2-of-4 overlap (quantum, entanglement) fuses through"
    );
    let fused = db
        .hybrid_search("coral module quantum entanglement", Some(&cal_emb(4)), 5)
        .expect("hybrid");
    assert!(
        fused
            .iter()
            .any(|r| r.name == "quantum entanglement review"),
        "exact-embedding doc stays top through fusion"
    );
}

#[test]
fn short_query_needs_one_match_and_empty_query_is_empty() {
    let db = cal_db();
    let fused = db
        .hybrid_search("alpha beta", Some(&cal_emb(1)), 5)
        .expect("hybrid");
    assert!(fused.iter().any(|r| r.name == "alpha beta gamma"));
    let empty = db.hybrid_search("zzzqqq", None, 5).expect("no match");
    assert!(empty.is_empty(), "no keyword hit and no vector → empty");
}

#[test]
fn no_vector_path_is_keyword_only_and_k_truncates() {
    let db = cal_db();
    let kw_only = db
        .hybrid_search("alpha beta", None, 5)
        .expect("keyword-only");
    assert!(kw_only.iter().any(|r| r.name == "alpha beta gamma"));
    let one = db
        .hybrid_search("alpha beta", Some(&cal_emb(1)), 1)
        .expect("hybrid");
    assert!(one.len() <= 1, "fused output respects k");
    let again = db
        .hybrid_search("alpha beta", Some(&cal_emb(1)), 5)
        .expect("hybrid");
    let rerun = db
        .hybrid_search("alpha beta", Some(&cal_emb(1)), 5)
        .expect("hybrid");
    assert_eq!(
        again.iter().map(|r| r.id).collect::<Vec<_>>(),
        rerun.iter().map(|r| r.id).collect::<Vec<_>>(),
        "fusion is deterministic run over run"
    );
}
