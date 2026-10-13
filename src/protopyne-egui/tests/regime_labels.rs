use super::*;
use std::path::{Path, PathBuf};

use fluent_egui::ThemeConfig;
use protopyne_kernels::market_context::{MarketRegime, REGIME_HYSTERESIS};

/// Calibration control (producer confidence: label stability): the stability
/// band and its holder wire to label display only — no alert, gate, cache,
/// or persistence path may name them. The scan keeps that wiring checked:
/// the symbols may appear in the label module, the crate re-export, and
/// this control file; anywhere else fails loudly until the wiring is
/// re-justified.
#[test]
fn stability_band_wires_to_no_alert_gate_cache_or_persistence() {
    assert_eq!(
        stability_band().to_bits(),
        REGIME_HYSTERESIS.to_bits(),
        "the band resolves to the kernel truth"
    );
    const SYMBOLS: [&str; 4] =
        ["stability_band", "STABILITY_BAND", "QuietLabel", "REGIME_HYSTERESIS"];
    // Allowed naming sites: the label module, the crate re-export, this
    // control, and the ledger (which pins the constant in its own literal).
    forbid_symbols(&["src"], &["regime_labels.rs", "lib.rs"], &SYMBOLS);
    forbid_symbols(&["tests"], &["regime_labels.rs", "budget.rs"], &SYMBOLS);
}

/// Fail when any scanned file outside `allowed` names a symbol in code
/// (comment lines document but never wire, so the scan skips them).
fn forbid_symbols(dirs: &[&str], allowed: &[&str], symbols: &[&str]) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut stack: Vec<PathBuf> = dirs.iter().map(|dir| root.join(dir)).collect();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("list {}: {e}", dir.display())) {
            let path = entry.expect("scan entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let name = path.file_name().expect("file name").to_string_lossy().into_owned();
            if allowed.contains(&name.as_str()) {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("scan reads");
            assert_no_symbol(&path, &text, symbols);
        }
    }
}

fn assert_no_symbol(path: &Path, text: &str, symbols: &[&str]) {
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('/')) {
        for symbol in symbols {
            assert!(
                !line.contains(symbol),
                "{} must not wire {symbol} (line: {line})",
                path.display()
            );
        }
    }
}

/// One pinned transition: held label, three surfaces, expected screen label.
type TransitionRow = (MarketRegime, Option<f64>, Option<f64>, Option<f64>, MarketRegime);

/// Pinned transition table: each row routes through the display wrapper and
/// must read exactly. Rows mirror the kernel stability probes so the paint
/// edge can never drift from the frozen detector + band.
fn transition_table() -> Vec<TransitionRow> {
    use MarketRegime::{LowVolume, Ranging, TrendingBear, TrendingBull, Volatile};
    vec![
        // Fresh agrees: stability never overrides the detector.
        (TrendingBull, Some(30.0), Some(0.5), Some(0.5), TrendingBull),
        (Volatile, Some(5.0), Some(4.375), Some(2.6), Volatile),
        (Ranging, Some(5.0), Some(1.0), Some(3.0), Ranging),
        // Quiet never vetoes a fresh signal.
        (LowVolume, Some(5.0), Some(4.375), Some(2.6), Volatile),
        // Same-surface sign flip is decisive, never held.
        (TrendingBull, Some(-30.0), Some(0.5), Some(0.5), TrendingBear),
        // Held surface decayed below floor minus band: release to quiet.
        (TrendingBull, Some(10.0), Some(1.5), Some(2.0), LowVolume),
        // Held surface inside the band: hold through the chop.
        (TrendingBull, Some(16.0), Some(2.2), Some(0.5), TrendingBull),
        // Cross-surface rotation by margin: fresh 35 beats held 26 + 5.
        (Ranging, Some(5.0), Some(4.375), Some(2.6), Volatile),
        // Cross-surface challenge inside the band: hold.
        (Ranging, Some(5.0), Some(3.75), Some(2.6), Ranging),
        // Cross-surface rotation the other way: trend 30 beats held 20 + 5.
        (Ranging, Some(30.0), Some(0.5), Some(2.0), TrendingBull),
        // Held evidence gone (all absent): never hold a stale label.
        (TrendingBear, None, None, None, LowVolume),
    ]
}

#[test]
fn transition_table_pinned_through_display_wrapper() {
    for (prev, health, volatility, volume, want) in transition_table() {
        assert_eq!(
            display_label(prev, health, volatility, volume),
            want,
            "display transition from {prev}"
        );
    }
}

#[test]
fn parity_probe_holds_exact_detector_semantics() {
    use MarketRegime::{LowVolume, Ranging, TrendingBear, TrendingBull, Volatile};
    // Frozen detector cases, read through the probe wrapper only.
    let cases = [
        (Some(30.0), Some(0.5), Some(0.5), TrendingBull),
        (Some(-30.0), Some(0.5), Some(0.5), TrendingBear),
        (Some(5.0), Some(4.375), Some(2.6), Volatile),
        (Some(5.0), Some(1.0), Some(3.0), Ranging),
        (Some(5.0), Some(1.0), Some(2.0), LowVolume),
        (None, None, None, LowVolume),
    ];
    for (health, volatility, volume, want) in cases {
        assert_eq!(parity_label(health, volatility, volume), want);
    }
}

#[test]
fn routing_holds_where_the_probe_flickers() {
    // Inside-band challenge: the probe fires Volatile, the display holds
    // Ranging. This divergence is the whole point of the wrapper.
    let (health, volatility, volume) = (Some(5.0), Some(3.75), Some(2.6));
    assert_eq!(parity_label(health, volatility, volume), MarketRegime::Volatile);
    assert_eq!(
        display_label(MarketRegime::Ranging, health, volatility, volume),
        MarketRegime::Ranging
    );
    // Decayed-but-inside-band quiet: the probe reads LowVolume, the display
    // still holds the trend.
    let (health, volatility, volume) = (Some(16.0), Some(2.2), Some(0.5));
    assert_eq!(parity_label(health, volatility, volume), MarketRegime::LowVolume);
    assert_eq!(
        display_label(MarketRegime::TrendingBull, health, volatility, volume),
        MarketRegime::TrendingBull
    );
}

#[test]
fn chop_hold_controls_never_flip() {
    // Trend chop inside the band: eight identical bars hold Bull.
    let bars = 8_usize;
    let out = label_series(
        MarketRegime::TrendingBull,
        &vec![Some(16.0); bars],
        &vec![Some(2.2); bars],
        &vec![Some(0.5); bars],
    )
    .expect("equal lengths fold");
    assert!(out.iter().all(|label| *label == MarketRegime::TrendingBull));
    // Cross-surface chop inside the band holds Ranging the same way.
    let out = label_series(
        MarketRegime::Ranging,
        &vec![Some(5.0); bars],
        &vec![Some(3.75); bars],
        &vec![Some(2.6); bars],
    )
    .expect("equal lengths fold");
    assert!(out.iter().all(|label| *label == MarketRegime::Ranging));
}

#[test]
fn genuine_rotation_switches_promptly() {
    // Surfaces separate by tens: one bar is enough, both directions.
    assert_eq!(
        display_label(MarketRegime::Ranging, Some(5.0), Some(4.375), Some(2.6)),
        MarketRegime::Volatile
    );
    assert_eq!(
        display_label(MarketRegime::Ranging, Some(30.0), Some(0.5), Some(2.0)),
        MarketRegime::TrendingBull
    );
    let out = label_series(
        MarketRegime::Ranging,
        &[Some(5.0)],
        &[Some(4.375)],
        &[Some(2.6)],
    )
    .expect("single bar folds");
    assert_eq!(out, vec![MarketRegime::Volatile]);
}

#[test]
fn same_surface_sign_flip_switches_at_once() {
    let mut held = QuietLabel::new(MarketRegime::TrendingBull);
    assert_eq!(held.update(Some(-30.0), Some(0.5), Some(0.5)), MarketRegime::TrendingBear);
    assert_eq!(held.current(), MarketRegime::TrendingBear);
}

#[test]
fn vanished_evidence_never_holds_a_stale_label() {
    for prev in [
        MarketRegime::TrendingBull,
        MarketRegime::TrendingBear,
        MarketRegime::Volatile,
        MarketRegime::Ranging,
    ] {
        assert_eq!(display_label(prev, None, None, None), MarketRegime::LowVolume);
    }
    // A quiet tail collapses and stays quiet through the fold.
    let out = label_series(
        MarketRegime::TrendingBear,
        &[Some(30.0), None, None],
        &[Some(0.5), None, None],
        &[Some(0.5), None, None],
    )
    .expect("equal lengths fold");
    assert_eq!(
        out,
        vec![
            MarketRegime::TrendingBull,
            MarketRegime::LowVolume,
            MarketRegime::LowVolume
        ]
    );
}

#[test]
fn decayed_hold_releases_to_quiet() {
    assert_eq!(
        display_label(MarketRegime::TrendingBull, Some(10.0), Some(1.5), Some(2.0)),
        MarketRegime::LowVolume
    );
}

#[test]
fn style_keys_feed_the_existing_theme() {
    let theme = ThemeConfig::default();
    for label in [
        MarketRegime::Ranging,
        MarketRegime::TrendingBull,
        MarketRegime::TrendingBear,
        MarketRegime::Volatile,
        MarketRegime::LowVolume,
    ] {
        let key = regime_style_key(label);
        assert!(
            theme.style(key).is_some(),
            "{label} must resolve against the existing theme, got key {key:?}"
        );
    }
}

#[test]
fn band_constant_is_shared_never_copied() {
    assert!((stability_band() - REGIME_HYSTERESIS).abs() < 1e-12);
    assert!((STABILITY_BAND - REGIME_HYSTERESIS).abs() < 1e-12);
    assert!((STABILITY_BAND - 5.0).abs() < 1e-12);
}

#[test]
fn series_fold_preserves_length_and_refuses_mismatch() {
    let out = label_series(
        MarketRegime::Ranging,
        &[Some(5.0), Some(5.0)],
        &[Some(1.0), Some(1.0)],
        &[Some(3.0), Some(3.0)],
    )
    .expect("equal lengths fold");
    assert_eq!(out.len(), 2);
    assert!(label_series(MarketRegime::Ranging, &[Some(1.0)], &[], &[Some(1.0)]).is_err());
    assert!(label_series(MarketRegime::Ranging, &[], &[], &[]).expect("empty folds").is_empty());
}

#[test]
fn tracker_reset_moves_the_hold() {
    let mut held = QuietLabel::new(MarketRegime::TrendingBull);
    held.reset(MarketRegime::Ranging);
    assert_eq!(held.current(), MarketRegime::Ranging);
    // From the reset hold, a genuine rotation still switches at once.
    assert_eq!(held.update(Some(5.0), Some(4.375), Some(2.6)), MarketRegime::Volatile);
}
