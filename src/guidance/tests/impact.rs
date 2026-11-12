//! Impact agreement with the graph closure and the component gate map.

use super::{impact_report, ImpactReport};
use crate::component_map::component_gate_for_path;
use crate::graph_index::{GraphIndex, HarvestInput};

fn two_file_graph() -> GraphIndex {
    GraphIndex::build(
        &[
            HarvestInput {
                path: "/repo/a.rs".to_string(),
                format: "rust".to_string(),
                text: "fn foo() { bar(); }\nfn bar() {}\n".to_string(),
            },
            HarvestInput {
                path: "/repo/b.rs".to_string(),
                format: "rust".to_string(),
                text: "fn bar() {}\n".to_string(),
            },
        ],
        &["/repo".to_string()],
    )
    .expect("graph builds")
}

#[test]
fn report_joins_closure_owners_and_gates() {
    let graph = two_file_graph();
    let report = impact_report(&graph, &["/repo/b.rs".to_string()]);
    assert_eq!(report.changed, vec!["/repo/b.rs".to_string()]);
    assert_eq!(
        report.dependents,
        graph.dependents_closure(&["/repo/b.rs".to_string()])
    );
    assert!(report.dependents.contains(&"/repo/b.rs".to_string()));
    for (path, gate) in &report.owners {
        assert_eq!(*gate, component_gate_for_path(path));
    }
    let mut expected: Vec<String> = report
        .owners
        .iter()
        .map(|(_, gate)| gate.gate.clone())
        .collect();
    expected.sort();
    expected.dedup();
    assert_eq!(report.gates, expected);
}

#[test]
fn unknown_paths_fall_back_to_workspace_gate() {
    let graph = two_file_graph();
    let report: ImpactReport = impact_report(&graph, &["/repo/zzz_missing.rs".to_string()]);
    assert!(report
        .dependents
        .contains(&"/repo/zzz_missing.rs".to_string()));
    assert!(report.gates.iter().any(|gate| gate == "make smoke"));
}

#[test]
fn report_lists_tests_touching_changed_files() {
    // Recorded refs via the hydrated graph: the test names an item
    // defined in a.rs and nothing in b.rs.
    let graph = GraphIndex::build(
        &[
            HarvestInput {
                path: "/repo/src/a.rs".to_string(),
                format: "rust".to_string(),
                text: "pub fn target_a() -> u64 {\n    1\n}\n".to_string(),
            },
            HarvestInput {
                path: "/repo/src/b.rs".to_string(),
                format: "rust".to_string(),
                text: "pub fn other_b() -> u64 {\n    2\n}\n".to_string(),
            },
            HarvestInput {
                path: "/repo/tests/test_t.rs".to_string(),
                format: "rust".to_string(),
                text: "use crate::a::target_a;\n#[test]\nfn t() {\n    assert_eq!(target_a(), 1);\n}\n"
                    .to_string(),
            },
        ],
        &["/repo".to_string()],
    )
    .expect("graph builds");
    let report = impact_report(&graph, &["/repo/src/a.rs".to_string()]);
    assert_eq!(report.tests, vec!["/repo/tests/test_t.rs".to_string()]);
    let quiet = impact_report(&graph, &["/repo/src/b.rs".to_string()]);
    assert!(quiet.tests.is_empty(), "nothing exercises b.rs: {quiet:?}");
}
