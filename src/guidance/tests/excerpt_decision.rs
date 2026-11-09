use super::{excerpt_spec, relevance_gate};

#[test]
fn spec_pins_score_shape() {
    let spec = excerpt_spec();
    assert!(spec.validate().is_ok());
    assert_eq!(spec.kind, "score");
    assert!(spec.options().len() >= 2);
}

#[test]
fn gate_filters_without_reordering() {
    // The gate is a filter, never a rerank: survivors keep input order.
    let candidates = vec![
        ("b_low", 0.2),
        ("a_high", 0.9),
        ("c_mid", 0.6),
        ("d_low", 0.49),
    ];
    let kept = relevance_gate(&candidates, 0.5);
    assert_eq!(kept, vec!["a_high".to_string(), "c_mid".to_string()]);
}

#[test]
fn gate_edge_cases() {
    // At-threshold passes (pinned operating point, matches the legacy
    // 0.5 gate); empty input stays empty.
    let kept = relevance_gate(&[("edge", 0.5)], 0.5);
    assert_eq!(kept, vec!["edge".to_string()]);
    let kept: Vec<String> = relevance_gate(&[], 0.5);
    assert!(kept.is_empty());
}

#[test]
fn gate_preserves_legacy_noop_operating_point() {
    // Noop scores 0.9 hits / 0.1 misses: the 0.5 gate keeps exactly the
    // substring hits, in order.
    let candidates = vec![("fn hello() void", 0.9), ("fn greet() void", 0.1)];
    let kept = relevance_gate(&candidates, 0.5);
    assert_eq!(kept, vec!["fn hello() void".to_string()]);
}

#[test]
fn seed_corpus_passes_gate_at_legacy_threshold() {
    struct Case {
        score: f64,
        label: bool,
    }
    // Positives fire at 0.5; controls (near-miss, single-token, empty
    // proxies) must not.
    let cases = vec![
        Case {
            score: 0.90,
            label: true,
        },
        Case {
            score: 0.85,
            label: true,
        },
        Case {
            score: 0.75,
            label: true,
        },
        Case {
            score: 0.60,
            label: true,
        },
        Case {
            score: 0.95,
            label: true,
        },
        Case {
            score: 0.80,
            label: true,
        },
        Case {
            score: 0.70,
            label: true,
        },
        Case {
            score: 0.55,
            label: true,
        },
        Case {
            score: 0.40,
            label: false,
        },
        Case {
            score: 0.30,
            label: false,
        },
        Case {
            score: 0.20,
            label: false,
        },
        Case {
            score: 0.10,
            label: false,
        },
        Case {
            score: 0.05,
            label: false,
        },
        Case {
            score: 0.45,
            label: false,
        },
        Case {
            score: 0.35,
            label: false,
        },
        Case {
            score: 0.15,
            label: false,
        },
    ];
    let report =
        common_core::calibration::calibrate_threshold(&cases, |c| c.score, |c| c.label, 0.5);
    assert!(
        report.passes_gate(),
        "excerpt seed must pass: precision {} fpr {}",
        report.precision,
        report.fpr
    );
    assert!(report.precision >= 0.90, "precision floor");
    assert!(report.fpr <= 0.05, "fpr ceiling");
}
