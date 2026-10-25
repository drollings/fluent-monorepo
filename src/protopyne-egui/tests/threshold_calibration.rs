use super::*;
use std::path::PathBuf;

fn flat_rows() -> Vec<ChartRow> {
    (0..40)
        .map(|i| ChartRow {
            time: i64::from(i),
            open: 99.5,
            high: 100.5,
            low: 99.0,
            close: 100.0,
            volume: 10,
        })
        .collect()
}

fn rise_rows() -> Vec<ChartRow> {
    (0..60)
        .map(|i| {
            let close = 100.0 + f64::from(i);
            ChartRow {
                time: i64::from(i),
                open: close - 0.5,
                high: close + 0.5,
                low: close - 1.0,
                close,
                volume: 10,
            }
        })
        .collect()
}

fn fall_rows() -> Vec<ChartRow> {
    (0..60)
        .map(|i| {
            let close = 160.0 - f64::from(i);
            ChartRow {
                time: i64::from(i),
                open: close - 0.5,
                high: close + 0.5,
                low: close - 1.0,
                close,
                volume: 10,
            }
        })
        .collect()
}

fn synthetic_chop() -> Vec<Option<f64>> {
    (0..40)
        .map(|i| Some(if i % 2 == 0 { 60.0 } else { -60.0 }))
        .collect()
}

fn synthetic_flat() -> Vec<Option<f64>> {
    vec![Some(5.0); 40]
}

fn display_for_kind(rows: &[ChartRow], slot: &str) -> Vec<Option<f64>> {
    let mut workspace = ChartWorkspace::with_rows(rows.to_vec()).expect("workspace boots");
    workspace.slot_indicator(slot).expect("kernel slots");
    workspace.recompute().expect("evaluations build");
    workspace
        .evaluations
        .get(slot)
        .expect("slot evaluates")
        .series
        .first()
        .expect("one series")
        .display_values
        .clone()
}

fn crossings(display: &[Option<f64>], threshold: f64) -> usize {
    display
        .iter()
        .filter(|value| value.is_some_and(|v| v.abs() >= threshold))
        .count()
}

fn mapped_slots() -> [&'static str; 4] {
    ["rsi", "cci", "cmo", "mfi"]
}

#[test]
fn flat_and_chop_never_cross_at_the_frozen_threshold() {
    assert_eq!(
        crossings(&synthetic_flat(), 100.0),
        0,
        "flat must not cross at 100"
    );
    assert_eq!(
        crossings(&synthetic_chop(), 100.0),
        0,
        "chop must not cross at 100"
    );
    assert!(
        crossings(&synthetic_chop(), 50.0) > 0,
        "chop fires at 50, which is why 50 cannot be trusted"
    );
    for slot in mapped_slots() {
        let flat = display_for_kind(&flat_rows(), slot);
        assert_eq!(
            crossings(&flat, 100.0),
            0,
            "{slot} flat must not cross at 100"
        );
    }
}

#[test]
fn known_swings_cross_at_the_frozen_threshold() {
    for slot in mapped_slots() {
        let rise = display_for_kind(&rise_rows(), slot);
        let fall = display_for_kind(&fall_rows(), slot);
        assert!(crossings(&rise, 100.0) > 0, "{slot} rise must cross at 100");
        assert!(crossings(&fall, 100.0) > 0, "{slot} fall must cross at 100");
        assert_eq!(crossings(&rise, 2000.0), 0, "{slot} rise sleeps at 2000");
        assert_eq!(crossings(&fall, 2000.0), 0, "{slot} fall sleeps at 2000");
    }
}

#[test]
fn raising_gain_never_removes_a_crossing() {
    let mut fixtures: Vec<Vec<Option<f64>>> = vec![synthetic_flat(), synthetic_chop()];
    for slot in mapped_slots() {
        fixtures.push(display_for_kind(&flat_rows(), slot));
        fixtures.push(display_for_kind(&rise_rows(), slot));
        fixtures.push(display_for_kind(&fall_rows(), slot));
    }
    for display in fixtures {
        let mut previous = 0;
        for gain in [0.5, 1.0, 1.5, 2.0] {
            let scaled: Vec<Option<f64>> = display
                .iter()
                .map(|value| value.map(|v| v * gain))
                .collect();
            let count = crossings(&scaled, 100.0);
            assert!(
                count >= previous,
                "gain {gain} removed a crossing: {previous} -> {count}"
            );
            previous = count;
        }
    }
}

#[test]
fn calibration_note_matches_the_frozen_threshold() {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "data",
        "oscillator_threshold_calibration.json",
    ]
    .iter()
    .collect();
    let text = std::fs::read_to_string(&path).expect("calibration note loads");
    let note: serde_json::Value = serde_json::from_str(&text).expect("note parses");
    assert_eq!(
        note.get("frozen_threshold")
            .and_then(serde_json::Value::as_f64),
        Some(100.0),
        "the note freezes 100"
    );
    let floors = note.get("floors").expect("floors recorded");
    assert_eq!(
        floors
            .get("controls_at_100")
            .and_then(serde_json::Value::as_str),
        Some("0 marks on flat and chop for every mapped kind"),
        "controls stay quiet at the frozen threshold"
    );
}

fn wobble_rows() -> Vec<ChartRow> {
    (0..60)
        .map(|i| {
            let close = 100.0 + f64::from(i) * 0.5 + if i % 2 == 0 { 1.0 } else { -1.0 };
            ChartRow {
                time: i64::from(i),
                open: close - 0.25,
                high: close + 0.6,
                low: close - 0.6,
                close,
                volume: 10,
            }
        })
        .collect()
}

fn chop_rows() -> Vec<ChartRow> {
    (0..40)
        .map(|i| {
            let close = if i % 2 == 0 { 100.0 } else { 101.0 };
            ChartRow {
                time: i64::from(i),
                open: close - 0.5,
                high: close + 0.5,
                low: close - 1.0,
                close,
                volume: 10,
            }
        })
        .collect()
}

/// Per-kind calibration designations: which fixtures control (must stay
/// quiet at 100) and which must fire. Measured, not assumed — chop
/// sensitivity differs by arm (CCI and stochastics read alternating bars as
/// signal), the SRSI relay saturates on monotonic ramps, VOSS lives in a
/// ±2 band the frozen threshold never reaches, and aroon breaks extremum
/// ties to the most recent bar (parity with Pine) so flat reads extreme.
/// Everything not listed is recorded but not floored.
fn kind_designation(kind: &str) -> (&'static [&'static str], &'static [&'static str]) {
    match kind {
        "rsi" | "cci" => (&["flat_40"], &["rise_60", "fall_60"]),
        "cmo" | "mfi" => (&["flat_40", "chop_40"], &["rise_60", "fall_60"]),
        "stoch" => (&["flat_40"], &["rise_60", "fall_60", "wobble_60"]),
        "trendsi" => (
            &["flat_40", "chop_40"],
            &["rise_60", "fall_60", "wobble_60"],
        ),
        "srsi" => (&["flat_40"], &["wobble_60"]),
        "voss" => (
            &["flat_40", "chop_40", "rise_60", "fall_60", "wobble_60"],
            &[],
        ),
        "aroon" => (&[], &["rise_60", "fall_60", "wobble_60"]),
        _ => panic!("unknown kind {kind}"),
    }
}

fn calibration_kinds() -> [&'static str; 9] {
    [
        "rsi", "cci", "cmo", "mfi", "stoch", "trendsi", "srsi", "voss", "aroon",
    ]
}

fn fixture_rows(name: &str) -> Vec<ChartRow> {
    match name {
        "flat_40" => flat_rows(),
        "chop_40" => chop_rows(),
        "rise_60" => rise_rows(),
        "fall_60" => fall_rows(),
        "wobble_60" => wobble_rows(),
        _ => panic!("unknown fixture {name}"),
    }
}

fn per_kind_report_path() -> PathBuf {
    [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "data",
        "per_kind_threshold_calibration.json",
    ]
    .iter()
    .collect()
}

/// Per-kind threshold calibration (task value: alert fitness per arm).
/// Recomputes every (kind, fixture, threshold) crossing count and compares
/// against the checked-in report number-for-number; floors pin the
/// designations above, so a behavior change fails before the report does.
/// Regenerate only with `PROTOPYNE_RECORD=1`.
#[test]
fn per_kind_thresholds_match_the_checked_in_report() {
    let mut counts = serde_json::Map::new();
    for kind in calibration_kinds() {
        let mut kind_counts = serde_json::Map::new();
        for fixture in ["flat_40", "chop_40", "rise_60", "fall_60", "wobble_60"] {
            let display = display_for_kind(&fixture_rows(fixture), kind);
            let mut fixture_counts = serde_json::Map::new();
            for (threshold, is_absurd) in [(100.0, false), (2000.0, true)] {
                let count = crossings(&display, threshold);
                fixture_counts.insert(
                    format!("{threshold}"),
                    serde_json::Value::from(count as u64),
                );
                // Absurd sleeps for every kind on every fixture: 2000 is
                // beyond every arm's display band, so nothing may fire.
                if is_absurd {
                    assert_eq!(count, 0, "{kind} {fixture} sleeps at 2000");
                }
            }
            kind_counts.insert(
                fixture.to_owned(),
                serde_json::Value::Object(fixture_counts),
            );
        }
        counts.insert(kind.to_owned(), serde_json::Value::Object(kind_counts));
    }
    // Floors pin the designations: controls quiet, must-fires fire.
    for kind in calibration_kinds() {
        let (controls, must_fire) = kind_designation(kind);
        let kind_counts = counts.get(kind).expect("kind counted");
        for fixture in controls {
            assert_eq!(
                kind_counts[fixture]["100"].as_u64().expect("count"),
                0,
                "{kind} control {fixture} must stay quiet at 100"
            );
        }
        for fixture in must_fire {
            assert!(
                kind_counts[fixture]["100"].as_u64().expect("count") > 0,
                "{kind} must-fire {fixture} must cross at 100"
            );
        }
    }
    let record = serde_json::Value::Object(counts);
    if std::env::var("PROTOPYNE_RECORD").is_ok() {
        let mut full = serde_json::Map::new();
        full.insert(
            "scope".to_owned(),
            serde_json::Value::from(
                "per-kind alert fitness at the frozen pivot threshold: every (kind, fixture) crossing count at 100 and 2000. Outcome calibration for strategy triggers lands in the equity-sweep milestone with the native signal.",
            ),
        );
        full.insert("counts".to_owned(), record);
        std::fs::write(
            per_kind_report_path(),
            serde_json::to_string_pretty(&serde_json::Value::Object(full)).expect("serializes")
                + "\n",
        )
        .expect("record writes");
        return;
    }
    let text = std::fs::read_to_string(per_kind_report_path()).expect("per-kind report loads");
    let report: serde_json::Value = serde_json::from_str(&text).expect("report parses");
    assert_eq!(
        record, report["counts"],
        "recomputed per-kind counts must match the checked-in report number-for-number"
    );
}

fn gain_report_path() -> PathBuf {
    [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "data",
        "gain_threshold_mapping.json",
    ]
    .iter()
    .collect()
}

/// Gain-aware threshold mapping (task value: the composed
/// `squash_to_100(value * gain)` mapping preserves crossing sets).
/// Records per-fixture crossing counts across gains and thresholds so the
/// mapping evidence is checked in beside the fixtures; floors pin
/// monotonicity (raising gain never removes a crossing) and the absurd
/// sleep. Regenerate only with `PROTOPYNE_RECORD=1`.
#[test]
fn gain_threshold_mapping_matches_the_checked_in_report() {
    let mut fixtures: Vec<(&str, Vec<Option<f64>>)> = vec![
        ("synthetic_flat", synthetic_flat()),
        ("synthetic_chop", synthetic_chop()),
    ];
    for kind in ["rsi", "cci", "cmo", "mfi", "stoch", "trendsi"] {
        for (name, rows) in [
            ("flat_40", flat_rows()),
            ("rise_60", rise_rows()),
            ("fall_60", fall_rows()),
        ] {
            fixtures.push((
                Box::leak(format!("{kind}_{name}").into_boxed_str()) as &str,
                display_for_kind(&rows, kind),
            ));
        }
    }
    let mut table = serde_json::Map::new();
    for (name, display) in &fixtures {
        let mut rows = serde_json::Map::new();
        for gain in [0.5, 1.0, 1.5, 2.0] {
            let scaled: Vec<Option<f64>> = display
                .iter()
                .map(|value| value.map(|v| v * gain))
                .collect();
            let mut gains = serde_json::Map::new();
            for threshold in [50.0, 100.0, 2000.0] {
                gains.insert(
                    format!("{threshold}"),
                    serde_json::Value::from(crossings(&scaled, threshold) as u64),
                );
            }
            rows.insert(format!("{gain}"), serde_json::Value::Object(gains));
        }
        table.insert((*name).to_owned(), serde_json::Value::Object(rows));
    }
    // Floors: monotonicity in gain at every threshold, absurd sleeps.
    for (name, display) in &fixtures {
        for threshold in [50.0, 100.0, 2000.0] {
            let mut previous = 0;
            for gain in [0.5, 1.0, 1.5, 2.0] {
                let scaled: Vec<Option<f64>> = display
                    .iter()
                    .map(|value| value.map(|v| v * gain))
                    .collect();
                let count = crossings(&scaled, threshold);
                assert!(
                    count >= previous,
                    "{name} at {threshold}: gain {gain} removed a crossing"
                );
                previous = count;
            }
        }
        let full: Vec<Option<f64>> = display.iter().map(|value| value.map(|v| v * 2.0)).collect();
        assert_eq!(
            crossings(&full, 2000.0),
            0,
            "{name} sleeps at 2000 at every gain"
        );
    }
    let record = serde_json::Value::Object(table);
    if std::env::var("PROTOPYNE_RECORD").is_ok() {
        let mut full = serde_json::Map::new();
        full.insert(
            "scope".to_owned(),
            serde_json::Value::from(
                "gain-aware threshold mapping: crossing counts per fixture across gains [0.5, 1, 1.5, 2] and thresholds [50, 100, 2000]. Raising gain never removes a crossing; 2000 sleeps everywhere.",
            ),
        );
        full.insert("counts".to_owned(), record);
        std::fs::write(
            gain_report_path(),
            serde_json::to_string_pretty(&serde_json::Value::Object(full)).expect("serializes")
                + "\n",
        )
        .expect("record writes");
        return;
    }
    let text = std::fs::read_to_string(gain_report_path()).expect("gain report loads");
    let report: serde_json::Value = serde_json::from_str(&text).expect("report parses");
    assert_eq!(
        record, report["counts"],
        "recomputed gain mapping must match the checked-in report number-for-number"
    );
}
