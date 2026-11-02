//! Preset tests: every preset resolves, sorts acyclically, and orders
//! capture before inference before injection.

use crate::preset::{assistant_preset, dictation_preset, duplex_preset, meeting_preset};

fn position(order: &[&str], node: &str) -> usize {
    order.iter().position(|name| *name == node).expect("node present")
}

#[test]
fn dictation_resolves_and_sorts() {
    for with_aec in [false, true] {
        let graph = dictation_preset(with_aec).expect("preset builds");
        assert!(
            graph.unresolved_deps().is_empty(),
            "no dangling assets"
        );
        let order = graph.topo_sort().expect("preset is acyclic");
        assert_eq!(order.len(), if with_aec { 5 } else { 4 });
        assert!(position(&order, "capture.mic") < position(&order, "stt.mic"));
        assert!(position(&order, "stt.mic") < position(&order, "inject"));
        assert!(position(&order, "vad.mic") < position(&order, "stt.mic"));
        if with_aec {
            assert!(position(&order, "aec") < position(&order, "vad.mic"));
        }
    }
}

#[test]
fn sink_root_covers_the_whole_preset() {
    let graph = dictation_preset(false).expect("preset builds");
    let order = graph.topo_sort_from(&["inject"]).expect("subgraph sorts");
    assert_eq!(order.len(), 4);
    assert!(position(&order, "capture.mic") < position(&order, "inject"));
}

#[test]
fn assistant_sorts_with_the_cascade_in_order() {
    let graph = assistant_preset().expect("preset builds");
    assert!(graph.unresolved_deps().is_empty(), "no dangling assets");
    let order = graph.topo_sort().expect("preset is acyclic");
    assert_eq!(order.len(), 9);
    for (before, after) in [
        ("capture.mic", "stt.mic"),
        ("stt.mic", "turn"),
        ("turn", "llm"),
        ("llm", "tts"),
        ("llm", "tools"),
    ] {
        assert!(
            position(&order, before) < position(&order, after),
            "{before} must precede {after}"
        );
    }
}

#[test]
fn killing_stt_cancels_dependents_but_never_capture() {
    use std::collections::HashSet;
    let graph = assistant_preset().expect("preset builds");
    let cancelled: HashSet<&str> = graph.dependents_of(&"stt.mic").into_iter().collect();
    for node in ["turn", "llm", "tools", "tts", "transcript"] {
        assert!(cancelled.contains(node), "{node} must cancel");
    }
    assert!(!cancelled.contains("capture.mic"));
    assert!(!cancelled.contains("aec"));
    assert!(!cancelled.contains("vad.mic"));
}

#[test]
fn meeting_has_two_lanes_archive_and_no_voice() {
    let graph = meeting_preset().expect("preset builds");
    assert!(graph.unresolved_deps().is_empty(), "no dangling assets");
    let order = graph.topo_sort().expect("preset is acyclic");
    assert_eq!(order.len(), 9);
    assert!(position(&order, "capture.mic") < position(&order, "transcript"));
    assert!(position(&order, "capture.sys") < position(&order, "transcript"));
    assert!(position(&order, "stt.mic") < position(&order, "transcript"));
    assert!(position(&order, "stt.sys") < position(&order, "transcript"));
    assert!(position(&order, "transcript") < position(&order, "notes.live"));
    assert!(!order.contains(&"tts"), "meeting never speaks");
    assert!(!order.contains(&"tools"), "meeting holds no effectful capability");
    assert!(order.contains(&"archive"), "record-first: archive always present");
}

#[test]
fn duplex_collapses_the_cascade_behind_one_node() {
    let graph = duplex_preset().expect("preset builds");
    assert!(graph.unresolved_deps().is_empty(), "no dangling assets");
    let order = graph.topo_sort().expect("preset is acyclic");
    assert_eq!(order.len(), 5);
    assert!(position(&order, "capture.mic") < position(&order, "duplex"));
    assert!(position(&order, "duplex") < position(&order, "inject"));
    for node in ["stt.mic", "llm", "tools", "tts", "turn"] {
        assert!(!order.contains(&node), "{node} collapses into duplex");
    }
    assert!(order.contains(&"archive"), "record-first holds in duplex too");
}
