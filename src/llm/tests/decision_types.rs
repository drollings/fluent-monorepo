//! Decision types + pure policy tests (tests-first).

use fluent_llm::decision::{decide_verdict, Decision, DecisionError, ThresholdSet, Verdict};

fn sample_distribution() -> Vec<(String, f64)> {
    // B 0.82, A 0.10, C 0.08 — sums to 1.0.
    vec![
        ("B".to_string(), 0.82),
        ("A".to_string(), 0.10),
        ("C".to_string(), 0.08),
    ]
}

#[test]
fn distribution_sums_to_one() {
    let d = Decision::from_distribution("B", sample_distribution(), "m".to_string(), 1, 12, false)
        .expect("valid distribution");
    let total: f64 = d.distribution.iter().map(|(_, p)| p).sum();
    assert!((total - 1.0).abs() < 1e-9, "total={total}");
}

#[test]
fn confidence_is_top1_and_margin_is_gap() {
    let d = Decision::from_distribution("B", sample_distribution(), "m".to_string(), 1, 12, false)
        .expect("valid");
    assert!((d.confidence - 0.82).abs() < 1e-9);
    assert!((d.margin - 0.72).abs() < 1e-9);
}

#[test]
fn unknown_label_is_rejected() {
    let err =
        Decision::from_distribution("Z", sample_distribution(), "m".to_string(), 1, 12, false)
            .expect_err("unknown label");
    assert!(matches!(err, DecisionError::Decode(_)));
}

#[test]
fn unavailable_never_converts_to_label() {
    let err = DecisionError::Unavailable("timeout".to_string());
    assert!(err.is_unavailable());
    // No constructor turns an error into a Decision: the type boundary is
    // the guarantee (this asserts the predicate the ladder matches on).
    let verdict_err = matches!(err, DecisionError::Unavailable(_));
    assert!(verdict_err);
}

#[test]
fn thresholds_are_per_point() {
    let d = Decision::from_distribution("B", sample_distribution(), "m".to_string(), 1, 12, false)
        .expect("valid");
    let strict = ThresholdSet {
        act: 0.90,
        review: 0.70,
    };
    let loose = ThresholdSet {
        act: 0.80,
        review: 0.50,
    };
    assert_eq!(decide_verdict(&d, &strict), Verdict::Review);
    assert_eq!(decide_verdict(&d, &loose), Verdict::Act);
}

#[test]
fn low_confidence_escalates() {
    let d = Decision::from_distribution(
        "B",
        vec![
            ("B".to_string(), 0.4),
            ("A".to_string(), 0.35),
            ("C".to_string(), 0.25),
        ],
        "m".to_string(),
        1,
        12,
        false,
    )
    .expect("valid");
    let t = ThresholdSet {
        act: 0.90,
        review: 0.70,
    };
    assert_eq!(decide_verdict(&d, &t), Verdict::Escalate);
}

#[test]
fn degraded_fallback_capped_below_act() {
    // Text fallback with no logprobs: invented 0.5 confidence must never
    // reach Act, even under the loosest valid thresholds.
    let body = serde_json::json!({
        "choices": [{"message": {"role": "assistant", "content": "B"}}]
    });
    let d = fluent_llm::decision::decode_choice(&body, &["A", "B"], "m", 1).expect("fallback");
    assert!(d.degraded, "fallback must be marked degraded");
    let loose = ThresholdSet {
        act: 0.5,
        review: 0.5,
    };
    assert_ne!(
        decide_verdict(&d, &loose),
        Verdict::Act,
        "degraded decisions never Act"
    );
}

#[test]
fn noul_verdict_is_directional_in_p_yes() {
    use fluent_llm::decision::{decide_noul_verdict, noul_p_yes};
    let yes = Decision::from_distribution(
        "Yes",
        vec![("Yes".to_string(), 0.95), ("No".to_string(), 0.05)],
        "m".to_string(),
        1,
        0,
        false,
    )
    .expect("yes");
    let no = Decision::from_distribution(
        "No",
        vec![("Yes".to_string(), 0.05), ("No".to_string(), 0.95)],
        "m".to_string(),
        1,
        0,
        false,
    )
    .expect("no");
    assert!((noul_p_yes(&yes).expect("p") - 0.95).abs() < 1e-9);
    assert!((noul_p_yes(&no).expect("p") - 0.05).abs() < 1e-9);
    // Both extremes act — on opposite branches — while the middle reviews.
    assert_eq!(decide_noul_verdict(&yes, 0.9, 0.1), Verdict::Act);
    assert_eq!(decide_noul_verdict(&no, 0.9, 0.1), Verdict::Act);
    let mid = Decision::from_distribution(
        "Yes",
        vec![("Yes".to_string(), 0.51), ("No".to_string(), 0.49)],
        "m".to_string(),
        1,
        0,
        false,
    )
    .expect("mid");
    assert_eq!(decide_noul_verdict(&mid, 0.9, 0.1), Verdict::Review);
    // Exact-boundary behavior: at the threshold counts as firing.
    let edge = Decision::from_distribution(
        "Yes",
        vec![("Yes".to_string(), 0.9), ("No".to_string(), 0.1)],
        "m".to_string(),
        1,
        0,
        false,
    )
    .expect("edge");
    assert_eq!(decide_noul_verdict(&edge, 0.9, 0.1), Verdict::Act);
}

#[test]
fn invalid_thresholds_rejected() {
    assert!(ThresholdSet {
        act: 1.5,
        review: 0.5
    }
    .validate()
    .is_err());
    assert!(ThresholdSet {
        act: 0.5,
        review: 0.8
    }
    .validate()
    .is_err());
    assert!(ThresholdSet {
        act: 0.8,
        review: 0.5
    }
    .validate()
    .is_ok());
}
