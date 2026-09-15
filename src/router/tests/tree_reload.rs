use super::*;
use crate::config::classification::{ClassificationChild, ClassificationNode, ClassificationTree};

fn fixture_tree() -> ClassificationTree {
    ClassificationTree {
        root: ClassificationNode::Classifier {
            description: "root router".into(),
            model: None,
            model_group: None,
            coherence_threshold: None,
            safety_threshold: None,
            children: vec![
                ClassificationChild {
                    key: "plan".into(),
                    description: "planning".into(),
                    node: ClassificationNode::Terminal {
                        route: "plan".into(),
                        group: None,
                        role: None,
                        description: "plan things".into(),
                        always_route: false,
                    },
                },
                ClassificationChild {
                    key: "sub".into(),
                    description: "sub router".into(),
                    node: ClassificationNode::Classifier {
                        description: "nested".into(),
                        model: Some("m".into()),
                        model_group: None,
                        coherence_threshold: Some(0.7),
                        safety_threshold: None,
                        children: vec![ClassificationChild {
                            key: "deep".into(),
                            description: "deep terminal".into(),
                            node: ClassificationNode::Terminal {
                                route: "deep".into(),
                                group: Some("g".into()),
                                role: None,
                                description: "deep things".into(),
                                always_route: false,
                            },
                        }],
                    },
                },
            ],
        },
    }
}

#[test]
fn regenerate_root_description_flows_into_prompt() {
    let tree = regenerate_description(&fixture_tree(), &[], "root router v2".into()).expect("regen");
    let prompt = tree.derive_system_prompt().expect("prompt derives");
    assert!(prompt.contains("root router v2"), "prompt rebuilt from regen: {prompt}");
    // Topology untouched: same children, same routes.
    assert_eq!(topology_fingerprint(&tree), topology_fingerprint(&fixture_tree()));
}

#[test]
fn regenerate_nested_terminal_description() {
    let tree = regenerate_description(&fixture_tree(), &["sub", "deep"], "deeper things".into())
        .expect("regen");
    let views = tree.terminal_views();
    let deep = views.iter().find(|(route, _, _)| route == "deep").expect("deep terminal");
    assert_eq!(deep.2, "deeper things");
}

#[test]
fn regenerate_thresholds_render_in_prompt() {
    let tree =
        regenerate_thresholds(&fixture_tree(), &["sub"], Some(0.9), Some(0.42)).expect("regen");
    let prompt = prompt_for_path(&tree, &["sub"]).expect("nested prompt");
    assert!(prompt.contains("coherence < 0.90"), "new coherence renders: {prompt}");
    assert!(prompt.contains("safety < 0.42"), "new safety renders: {prompt}");
    // Unset fields stay untouched (node had no safety threshold; coherence
    // was 0.7 → now 0.9 only where asked).
    let untouched = regenerate_thresholds(&fixture_tree(), &["sub"], None, None).expect("noop");
    assert_eq!(untouched, fixture_tree(), "all-None changes nothing");
}

#[test]
fn regenerate_rejects_bad_targets_and_ranges() {
    assert!(matches!(
        regenerate_thresholds(&fixture_tree(), &["plan"], Some(0.9), None),
        Err(ReloadError::WrongNodeType { .. })
    ));
    assert!(matches!(
        regenerate_thresholds(&fixture_tree(), &["sub"], Some(1.5), None),
        Err(ReloadError::ThresholdOutOfRange { .. })
    ));
    assert!(matches!(
        regenerate_thresholds(&fixture_tree(), &["sub"], None, Some(-0.1)),
        Err(ReloadError::ThresholdOutOfRange { .. })
    ));
    assert!(matches!(
        regenerate_description(&fixture_tree(), &["nope"], "x".into()),
        Err(ReloadError::NotFound { .. })
    ));
    // Boundary values are legal.
    assert!(regenerate_thresholds(&fixture_tree(), &["sub"], Some(0.0), Some(1.0)).is_ok());
}

#[test]
fn reload_versions_and_rejects_no_change_and_topology_edits() {
    let holder = VersionedTree::new(fixture_tree());
    assert_eq!(holder.version(), 1);
    let v2 = regenerate_description(&fixture_tree(), &[], "v2".into()).expect("regen");
    assert_eq!(holder.reload(v2, "operator tune").expect("reload"), 2);
    assert_eq!(holder.version(), 2);

    // Identical content: no new version, no audit noise.
    let same = holder.current().1;
    assert!(matches!(
        holder.reload(same, "noop"),
        Err(ReloadError::NoChange)
    ));
    assert_eq!(holder.version(), 2);

    // Topology edits are excluded, however small: added child, renamed
    // route, changed group, changed model each fail the fingerprint check.
    let mut topo = holder.current().1;
    if let ClassificationNode::Classifier { children, .. } = &mut topo.root {
        children.push(ClassificationChild {
            key: "extra".into(),
            description: String::new(),
            node: ClassificationNode::Terminal {
                route: "extra".into(),
                group: None,
                role: None,
                description: String::new(),
                always_route: false,
            },
        });
    }
    assert!(matches!(
        holder.reload(topo, "add route"),
        Err(ReloadError::TopologyMismatch { .. })
    ));
    let mut renamed = holder.current().1;
    rename_route(&mut renamed, "deep", "deeper");
    assert!(matches!(
        holder.reload(renamed, "rename route"),
        Err(ReloadError::TopologyMismatch { .. })
    ));
    assert_eq!(holder.version(), 2, "rejected reloads never version");
}

fn rename_route(tree: &mut ClassificationTree, from: &str, to: &str) {
    fn walk(node: &mut ClassificationNode, from: &str, to: &str) {
        match node {
            ClassificationNode::Terminal { route, .. } if route == from => *route = to.to_string(),
            ClassificationNode::Classifier { children, .. } => {
                for c in children {
                    walk(&mut c.node, from, to);
                }
            }
            ClassificationNode::Fallback { node, .. } => walk(node, from, to),
            _ => {}
        }
    }
    walk(&mut tree.root, from, to);
}

#[test]
fn rollback_restores_one_generation_then_exhausts() {
    let holder = VersionedTree::new(fixture_tree());
    assert!(matches!(holder.rollback(), Err(ReloadError::NoHistory)));
    let v2 = regenerate_description(&fixture_tree(), &[], "v2".into()).expect("regen");
    holder.reload(v2, "tune").expect("reload");
    let back = holder.rollback().expect("rollback");
    assert_eq!(back, 3, "rollback mints a new version (monotonic)");
    assert_eq!(holder.current().1, fixture_tree(), "content restored");
    assert!(matches!(holder.rollback(), Err(ReloadError::NoHistory)), "one-reload depth");
}

#[test]
fn rebootless_reload_never_tears_readers() {
    use std::sync::Arc;
    let holder = Arc::new(VersionedTree::new(fixture_tree()));
    let v1_print = holder.current().1.derive_system_prompt().expect("v1 prompt");
    let v2 = regenerate_description(&fixture_tree(), &[], "live v2".into()).expect("regen");
    let v2_print = v2.derive_system_prompt().expect("v2 prompt");
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let holder = Arc::clone(&holder);
            let (v1_print, v2_print) = (&v1_print, &v2_print);
            scope.spawn(move || {
                for _ in 0..200 {
                    let prompt = holder.current().1.derive_system_prompt().expect("prompt");
                    assert!(
                        prompt == *v1_print || prompt == *v2_print,
                        "readers see whole versions only"
                    );
                }
            });
        }
        holder.reload(v2, "live").expect("reload");
    });
    assert_eq!(holder.version(), 2);
}

#[test]
fn fingerprint_ignores_tuning_but_catches_structure() {
    let base = topology_fingerprint(&fixture_tree());
    let tuned = regenerate_thresholds(&fixture_tree(), &["sub"], Some(0.1), Some(0.2))
        .map(|t| regenerate_description(&t, &["plan"], "new".into()).expect("desc"))
        .expect("thresholds");
    assert_eq!(topology_fingerprint(&tuned), base, "tuning is fingerprint-silent");
    let mut added_filter = fixture_tree();
    if let ClassificationNode::Classifier { children, .. } = &mut added_filter.root {
        children.push(ClassificationChild {
            key: "f".into(),
            description: String::new(),
            node: ClassificationNode::Filter {
                description: String::new(),
                patterns: vec!["x".into()],
                outcome: crate::config::filters::FilterOutcome::default(),
                redirect_to: None,
                match_interlingua: None,
            },
        });
    }
    assert_ne!(topology_fingerprint(&added_filter), base);
}
