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
    let tree =
        regenerate_description(&fixture_tree(), &[], "root router v2".into()).expect("regen");
    let prompt = tree.derive_system_prompt().expect("prompt derives");
    assert!(
        prompt.contains("root router v2"),
        "prompt rebuilt from regen: {prompt}"
    );
    // Topology untouched: same children, same routes.
    assert_eq!(
        topology_fingerprint(&tree),
        topology_fingerprint(&fixture_tree())
    );
}

#[test]
fn regenerate_nested_terminal_description() {
    let tree = regenerate_description(&fixture_tree(), &["sub", "deep"], "deeper things".into())
        .expect("regen");
    let views = tree.terminal_views();
    let deep = views
        .iter()
        .find(|(route, _, _)| route == "deep")
        .expect("deep terminal");
    assert_eq!(deep.2, "deeper things");
}

#[test]
fn regenerate_thresholds_render_in_prompt() {
    let tree =
        regenerate_thresholds(&fixture_tree(), &["sub"], Some(0.9), Some(0.42)).expect("regen");
    let prompt = prompt_for_path(&tree, &["sub"]).expect("nested prompt");
    assert!(
        prompt.contains("coherence < 0.90"),
        "new coherence renders: {prompt}"
    );
    assert!(
        prompt.contains("safety < 0.42"),
        "new safety renders: {prompt}"
    );
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
    assert!(
        matches!(holder.rollback(), Err(ReloadError::NoHistory)),
        "one-reload depth"
    );
}

#[test]
fn rebootless_reload_never_tears_readers() {
    use std::sync::Arc;
    let holder = Arc::new(VersionedTree::new(fixture_tree()));
    let v1_print = holder
        .current()
        .1
        .derive_system_prompt()
        .expect("v1 prompt");
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

/// A reload publishes a new compiled tree alongside the raw one, and
/// in-flight readers of the compiled snapshot never observe a torn version
/// (version and content move together).
#[test]
fn reload_publishes_a_new_compiled_tree_without_tearing_readers() {
    use std::sync::Arc;
    let holder = Arc::new(VersionedTree::new(fixture_tree()));
    let (v1, c1) = holder.current_compiled();
    assert_eq!(v1, 1);
    let v1_prompt = c1.render_prompt(&[], 0.6, 0.4).expect("v1 prompt");
    let v2 = regenerate_description(&fixture_tree(), &[], "live v2".into()).expect("regen");
    let v2_prompt = v2
        .compile()
        .expect("compile")
        .render_prompt(&[], 0.6, 0.4)
        .expect("v2 prompt");
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let holder = Arc::clone(&holder);
            let (v1_prompt, v2_prompt) = (&v1_prompt, &v2_prompt);
            scope.spawn(move || {
                for _ in 0..200 {
                    let (version, compiled) = holder.current_compiled();
                    let prompt = compiled.render_prompt(&[], 0.6, 0.4).expect("prompt");
                    assert_eq!(
                        prompt == *v1_prompt,
                        version == 1,
                        "reader saw a torn version"
                    );
                    assert!(
                        prompt == *v1_prompt || prompt == *v2_prompt,
                        "readers see whole versions only"
                    );
                }
            });
        }
        holder.reload(v2, "live").expect("reload");
    });
    assert_eq!(holder.version(), 2);
    let (v2_live, c2) = holder.current_compiled();
    assert_eq!(v2_live, 2);
    assert!(
        !Arc::ptr_eq(&c1, &c2),
        "reload publishes a new compiled tree"
    );
}

#[test]
fn fingerprint_ignores_tuning_but_catches_structure() {
    let base = topology_fingerprint(&fixture_tree());
    let tuned = regenerate_thresholds(&fixture_tree(), &["sub"], Some(0.1), Some(0.2))
        .map(|t| regenerate_description(&t, &["plan"], "new".into()).expect("desc"))
        .expect("thresholds");
    assert_eq!(
        topology_fingerprint(&tuned),
        base,
        "tuning is fingerprint-silent"
    );
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

// ── M11: opt-in topology reload ──────────────────────────────────────────
// Path-qualified child keys ("sub/deep"); aliases map removed old paths to
// live new paths.

fn without_child(tree: &ClassificationTree, parent: &[&str], key: &str) -> ClassificationTree {
    let mut next = tree.clone();
    let node = if parent.is_empty() {
        &mut next.root
    } else {
        panic!("test helper only handles root-level removal");
    };
    let ClassificationNode::Classifier { children, .. } = node else {
        panic!("root must be a classifier");
    };
    children.retain(|c| c.key != key);
    next
}

fn renamed_child(
    tree: &ClassificationTree,
    parent: &[&str],
    old: &str,
    new: &str,
) -> ClassificationTree {
    let mut next = tree.clone();
    let node = if parent.is_empty() {
        &mut next.root
    } else {
        panic!("test helper only handles root-level rename");
    };
    let ClassificationNode::Classifier { children, .. } = node else {
        panic!("root must be a classifier");
    };
    for child in children.iter_mut() {
        if child.key == old {
            child.key = new.to_string();
        }
    }
    next
}

fn with_broken_threshold(tree: &ClassificationTree) -> ClassificationTree {
    let mut next = tree.clone();
    let ClassificationNode::Classifier { children, .. } = &mut next.root else {
        panic!("root must be a classifier");
    };
    for child in children.iter_mut() {
        if child.key == "sub" {
            let ClassificationNode::Classifier {
                coherence_threshold,
                ..
            } = &mut child.node
            else {
                panic!("sub must be a classifier");
            };
            *coherence_threshold = Some(5.0);
        }
    }
    next
}

fn with_emptied_sub(tree: &ClassificationTree) -> ClassificationTree {
    let mut next = tree.clone();
    let ClassificationNode::Classifier { children, .. } = &mut next.root else {
        panic!("root must be a classifier");
    };
    for child in children.iter_mut() {
        if child.key == "sub" {
            let ClassificationNode::Classifier {
                children: nested, ..
            } = &mut child.node
            else {
                panic!("sub must be a classifier");
            };
            nested.clear();
        }
    }
    next
}

fn no_aliases() -> std::collections::HashMap<String, String> {
    std::collections::HashMap::new()
}

fn aliases(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
    pairs
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
}

#[test]
fn topology_guard_accept_reject_table() {
    // Calibration table for the guard decision: the control group
    // (tuning-only, pure addition) must NOT fire; uncovered removals and
    // renames must fire; covered ones must NOT fire.
    let live = fixture_tree();
    let holder = VersionedTree::new(live.clone());

    // Control: tuning-only change needs no alias and is accepted.
    let tuned = regenerate_description(&live, &[], "root router v2".into()).expect("regen");
    assert!(holder
        .reload_topology(tuned, "tuning", &no_aliases())
        .is_ok());

    // Control: pure addition (no live key removed) needs no alias.
    let mut added = live.clone();
    let ClassificationNode::Classifier { children, .. } = &mut added.root else {
        panic!("root must be a classifier");
    };
    children.push(ClassificationChild {
        key: "extra".into(),
        description: "new branch".into(),
        node: ClassificationNode::Terminal {
            route: "extra".into(),
            group: None,
            role: None,
            description: "extra things".into(),
            always_route: false,
        },
    });
    assert!(holder
        .reload_topology(added, "addition", &no_aliases())
        .is_ok());

    // Must fire: key removal without an alias is rejected.
    // (Fresh holder per case: each swap changes the live tree the guard
    // compares against.)
    let holder = VersionedTree::new(live.clone());
    let removed = without_child(&live, &[], "plan");
    assert!(
        matches!(
            holder.reload_topology(removed.clone(), "drop plan", &no_aliases()),
            Err(ReloadError::TopologyMismatch { .. })
        ),
        "uncovered removal must be rejected"
    );
    // Covered removal is accepted, and the alias resolves at read time.
    holder
        .reload_topology(removed, "drop plan aliased", &aliases(&[("plan", "sub")]))
        .expect("aliased removal accepted");
    assert_eq!(holder.resolve_alias("plan"), "sub");
    assert_eq!(holder.resolve_alias("sub"), "sub");

    // Must fire: rename without an alias is rejected (old key uncovered).
    let holder2 = VersionedTree::new(live.clone());
    let renamed = renamed_child(&live, &[], "plan", "project");
    assert!(
        matches!(
            holder2.reload_topology(renamed.clone(), "rename", &no_aliases()),
            Err(ReloadError::TopologyMismatch { .. })
        ),
        "uncovered rename must be rejected"
    );
    assert!(holder2
        .reload_topology(renamed, "rename aliased", &aliases(&[("plan", "project")]))
        .is_ok());

    // Unknown alias targets are rejected: aliases must point at live keys.
    let holder3 = VersionedTree::new(live.clone());
    let removed2 = without_child(&live, &[], "plan");
    assert!(
        matches!(
            holder3.reload_topology(removed2, "bad alias", &aliases(&[("plan", "ghost")])),
            Err(ReloadError::TopologyMismatch { .. })
        ),
        "alias to a nonexistent key must be rejected"
    );
}

#[test]
fn topology_reload_validates_prompts_and_thresholds() {
    let live = fixture_tree();
    let holder = VersionedTree::new(live.clone());
    // Out-of-range threshold in the payload is rejected with its path.
    let broken = with_broken_threshold(&live);
    assert!(matches!(
        holder.reload_topology(broken, "bad threshold", &no_aliases()),
        Err(ReloadError::ThresholdOutOfRange { .. })
    ));
    // A classifier left with no routeable children cannot regenerate its
    // prompt: rejected as invalid topology, not swapped.
    let emptied = with_emptied_sub(&live);
    assert!(matches!(
        holder.reload_topology(emptied, "emptied", &no_aliases()),
        Err(ReloadError::TopologyInvalid { .. })
    ));
    assert_eq!(holder.version(), 1, "rejected payloads mint no version");
}

#[test]
fn topology_swap_is_atomic_under_concurrent_readers() {
    use std::sync::Arc;
    let v1 = fixture_tree();
    let tuned = regenerate_description(&v1, &[], "v2".into()).expect("regen");
    let v3 = without_child(&v1, &[], "plan");
    let holder = Arc::new(VersionedTree::new(v1.clone()));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let snapshots: Arc<std::sync::Mutex<Vec<(u64, ClassificationTree)>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let (h, s, stop) = (
                Arc::clone(&holder),
                Arc::clone(&snapshots),
                Arc::clone(&stop),
            );
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let snap = h.current();
                    s.lock().unwrap().push(snap);
                }
            })
        })
        .collect();
    holder.reload(tuned.clone(), "tuning").expect("v2");
    holder
        .reload_topology(v3.clone(), "drop plan", &aliases(&[("plan", "sub")]))
        .expect("v3");
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    for r in readers {
        r.join().unwrap();
    }
    // Every snapshot is wholly one version: no torn mix of v1/v2/v3 trees.
    for (version, tree) in snapshots.lock().unwrap().iter() {
        match version {
            1 => assert_eq!(*tree, v1),
            2 => assert_eq!(*tree, tuned),
            3 => assert_eq!(*tree, v3),
            other => panic!("unexpected version {other}"),
        }
    }
    assert_eq!(holder.version(), 3);
}

#[test]
fn topology_reload_emits_audit_record() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::registry;
    use tracing_subscriber::Layer;
    let dir = tempfile::tempdir().unwrap();
    let audit_path = dir.path().join("audit.log");
    let audit_appender = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::NEVER)
        .filename_prefix("audit")
        .filename_suffix("log")
        .build(dir.path())
        .unwrap();
    let (audit_nb, audit_guard) = tracing_appender::non_blocking(audit_appender);
    let subscriber = registry().with(
        tracing_subscriber::fmt::layer()
            .json()
            .with_writer(audit_nb)
            .with_filter(tracing_subscriber::filter::EnvFilter::new(
                "router.audit=info",
            )),
    );
    let holder = VersionedTree::new(fixture_tree());
    tracing::subscriber::with_default(subscriber, || {
        let next = without_child(&fixture_tree(), &[], "plan");
        holder
            .reload_topology(next, "drop plan", &aliases(&[("plan", "sub")]))
            .expect("reload");
    });
    drop(audit_guard);
    let content = std::fs::read_to_string(&audit_path).unwrap_or_default();
    let logged: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap_or_default())
        .collect();
    let all = logged.join("\n") + &content;
    assert!(
        all.contains("tree_topology_reload"),
        "audit record missing:\n{all}"
    );
    assert!(all.contains("drop plan"), "reason missing:\n{all}");
    assert!(all.contains(r#"\"version\":2"#), "version missing:\n{all}");
    assert!(
        all.contains(r#"\"removed_keys\":[\"plan\"]"#),
        "guard decision missing:\n{all}"
    );
}
