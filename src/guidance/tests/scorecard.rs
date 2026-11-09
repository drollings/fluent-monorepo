use super::{render_human, score_metrics, Baseline, Measured, MetricStatus};

// Scorecard math over miniature inputs — no fixture, no subprocess:
// deltas, ratchet pass/fail, exit codes, and the 40-line cap.

fn measured(name: &str, value: f64) -> Measured {
    Measured {
        name: name.to_string(),
        value,
        repro: "repro-cmd".to_string(),
        higher_is_better: true,
    }
}

fn baseline(pairs: &[(&str, f64)]) -> Baseline {
    Baseline {
        metrics: pairs
            .iter()
            .map(|(name, value)| (name.to_string(), *value))
            .collect(),
        ..Baseline::default()
    }
}

#[test]
fn deltas_and_status_against_baseline() {
    let card = score_metrics(
        &[measured("m.up", 1.0), measured("m.down", 0.5)],
        &Some(baseline(&[("m.up", 0.8), ("m.down", 0.9)])),
    );
    assert_eq!(card.lines.len(), 2);
    let up = card
        .lines
        .iter()
        .find(|line| line.name == "m.up")
        .expect("m.up");
    assert!((up.delta.expect("delta") - 0.2).abs() < 1e-6);
    assert_eq!(up.status, MetricStatus::Pass);
    let down = card
        .lines
        .iter()
        .find(|line| line.name == "m.down")
        .expect("m.down");
    assert!((down.delta.expect("delta") + 0.4).abs() < 1e-6);
    assert_eq!(down.status, MetricStatus::Violation);
    assert_eq!(card.exit_code, 2);
    assert_eq!(card.regressions.len(), 1);
    assert_eq!(card.regressions[0].name, "m.down");
}

#[test]
fn lower_is_better_metrics_violate_upward() {
    let mut slow = measured("speed.ms", 50.0);
    slow.higher_is_better = false;
    let card = score_metrics(&[slow], &Some(baseline(&[("speed.ms", 40.0)])));
    assert_eq!(card.lines[0].status, MetricStatus::Violation);
    assert_eq!(card.exit_code, 2);
}

#[test]
fn missing_baseline_is_not_a_violation() {
    let card = score_metrics(&[measured("m.new", 1.0)], &None);
    assert_eq!(card.lines[0].status, MetricStatus::NoBaseline);
    assert_eq!(card.exit_code, 0);
}

#[test]
fn fast_tier_passes_on_fixtures() {
    // End-to-end: the real measurement over the checked-in fixtures is
    // green without a baseline (every status `no-baseline`, exit 0).
    // Perfect-by-construction metrics read exactly 1.0; MRR is pinned for
    // stability (path-substring matches can outrank exact symbols — real
    // retrieval behavior the baseline holds, not fixture noise).
    let first = common_core::runtime::block_on_shared(super::run_fast_tier());
    let second = common_core::runtime::block_on_shared(super::run_fast_tier());
    assert!(
        first.len() >= 15,
        "tier covers every suite, got {}",
        first.len()
    );
    assert_eq!(first.len(), second.len());
    for metric in &first {
        if metric.higher_is_better {
            if metric.name.contains(".mrr.") {
                assert!((0.0..=1.0).contains(&metric.value), "{}", metric.name);
            } else {
                assert!(
                    (metric.value - 1.0).abs() <= f64::EPSILON,
                    "{}",
                    metric.name
                );
            }
        } else {
            assert!(
                metric.value.is_finite() && metric.value >= 0.0,
                "{}",
                metric.name
            );
        }
    }
    for (left, right) in first.iter().zip(second.iter()) {
        assert_eq!(left.name, right.name);
        // Timing samples vary run to run; everything else is deterministic.
        if left.higher_is_better {
            assert!((left.value - right.value).abs() < 1e-9, "{}", left.name);
        }
    }
    let card = super::score_metrics(&first, &None);
    assert_eq!(card.exit_code, 0);
    assert!(card.regressions.is_empty());
}

#[test]
fn render_caps_at_forty_lines() {
    let many: Vec<Measured> = (0..45)
        .map(|i| measured(&format!("m.{i:02}"), 1.0))
        .collect();
    let card = score_metrics(&many, &None);
    let text = render_human(&card);
    assert!(
        text.lines().count() <= 40,
        "got {} lines",
        text.lines().count()
    );
    assert!(text.contains("in JSON"), "{text}");
}
