//! Pipeline order: dependency depth first, blast radius second, path
//! last; plus the witness rule and the early syntactic index. The
//! naive flat reorder (move the timestamp call last and sort by input
//! order) fixes the crash window but regresses convergence: dependents
//! verify before their dependencies settle, and the witness can pass
//! while a dependent is still dirty. Depth-first plus blast-radius
//! ordering is what makes the witness honest.

use std::path::PathBuf;

use crate::pipeline::{lite_index, order_for_waves, witness_clean};

fn chain(tmp: &std::path::Path, names: &[&str]) -> Vec<PathBuf> {
    names.iter().map(|name| tmp.join(name)).collect()
}

#[test]
fn out_of_order_input_still_converges_leaf_first() {
    let order = order_for_waves(
        &chain(std::path::Path::new("/w"), &["c.rs", "a.rs", "b.rs"]),
        &|path| {
            if path.ends_with("c.rs") {
                vec![PathBuf::from("/w/b.rs")]
            } else if path.ends_with("b.rs") {
                vec![PathBuf::from("/w/a.rs")]
            } else {
                Vec::new()
            }
        },
        &|_| Vec::new(),
    );
    assert_eq!(
        order,
        chain(std::path::Path::new("/w"), &["a.rs", "b.rs", "c.rs"]),
        "depth primary: leaves before dependents regardless of input order"
    );
}

#[test]
fn blast_radius_breaks_depth_ties() {
    let order = order_for_waves(
        &chain(std::path::Path::new("/w"), &["hot.rs", "cold.rs"]),
        &|_| Vec::new(),
        &|path| {
            if path.ends_with("hot.rs") {
                vec![PathBuf::from("/w/u1.rs"), PathBuf::from("/w/u2.rs")]
            } else {
                Vec::new()
            }
        },
    );
    assert_eq!(
        order,
        chain(std::path::Path::new("/w"), &["cold.rs", "hot.rs"]),
        "same depth: smaller blast radius first"
    );
}

#[test]
fn cycles_do_not_hang_ordering() {
    let order = order_for_waves(
        &chain(std::path::Path::new("/w"), &["a.rs", "b.rs"]),
        &|path| {
            if path.ends_with("a.rs") {
                vec![PathBuf::from("/w/b.rs")]
            } else {
                vec![PathBuf::from("/w/a.rs")]
            }
        },
        &|_| Vec::new(),
    );
    assert_eq!(order.len(), 2, "cycle resolves by visit guard, never hangs");
}

#[test]
fn witness_requires_quiet_final_wave_and_owned_remainder() {
    assert!(witness_clean(false, 0, false));
    assert!(!witness_clean(true, 0, false), "kept edit moves the tree");
    assert!(
        !witness_clean(false, 1, false),
        "unowned remainder refuses done"
    );
    assert!(
        !witness_clean(false, 0, true),
        "breaker trip is halted, not done"
    );
}

#[test]
fn injected_late_diagnostic_refuses_done() {
    assert!(
        !witness_clean(false, 1, false),
        "a diagnostic arriving after the last fix wave must refuse done"
    );
}

#[test]
fn lite_index_parses_without_building() {
    let dir = fluent_wvr_testutil::tempdir();
    std::fs::write(dir.path().join("a.rs"), "pub fn a() {}\n").expect("write");
    let mut parser = crate::ast_parser::AstParser::new();
    let items = lite_index(&[dir.path().join("a.rs")], &mut parser);
    assert_eq!(items.len(), 1);
    assert!(!items[0].content_hash.is_empty());
}
