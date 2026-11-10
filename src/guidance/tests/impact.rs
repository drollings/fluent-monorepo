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
    assert_eq!(report.dependents, graph.dependents_closure(&["/repo/b.rs".to_string()]));
    assert!(report.dependents.contains(&"/repo/b.rs".to_string()));
    for (path, gate) in &report.owners {
        assert_eq!(*gate, component_gate_for_path(path));
    }
    let mut expected: Vec<String> =
        report.owners.iter().map(|(_, gate)| gate.gate.clone()).collect();
    expected.sort();
    expected.dedup();
    assert_eq!(report.gates, expected);
}

#[test]
fn unknown_paths_fall_back_to_workspace_gate() {
    let graph = two_file_graph();
    let report: ImpactReport = impact_report(&graph, &["/repo/zzz_missing.rs".to_string()]);
    assert!(report.dependents.contains(&"/repo/zzz_missing.rs".to_string()));
    assert!(report.gates.iter().any(|gate| gate == "make smoke"));
}
