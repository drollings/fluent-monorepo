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
