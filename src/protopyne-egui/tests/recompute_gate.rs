use super::*;

fn gate_rows(count: i64) -> Vec<ChartRow> {
    (0..count)
        .map(|i| {
            let close = 100.0 + (i as f64);
            ChartRow {
                time: 1_700_000_000 + i * 900,
                open: close - 1.0,
                high: close + 1.0,
                low: close - 2.0,
                close,
                volume: 10,
            }
        })
        .collect()
}

fn rows_shape_of(rows: &[ChartRow]) -> (usize, Option<i64>, Option<i64>) {
    (
        rows.len(),
        rows.first().map(|row| row.time),
        rows.last().map(|row| row.time),
    )
}

fn shadow_inputs(workspace: &ChartWorkspace) -> (Vec<ChartRow>, Vec<(String, String)>) {
    let mut slots: Vec<(String, String)> = workspace
        .stack
        .slot_ids()
        .into_iter()
        .map(|id| {
            let describe = workspace
                .stack
                .config(&id)
                .map(|config| config.describe().to_string())
                .unwrap_or_default();
            (id, describe)
        })
        .collect();
    slots.sort();
    (workspace.rows.clone(), slots)
}

#[test]
fn identical_replay_skips_the_evaluate_wave() {
    let mut workspace = ChartWorkspace::with_rows(gate_rows(40)).expect("workspace boots");
    let builds = workspace.derived_builds();
    let evaluations = workspace.evaluations.clone();
    workspace.recompute().expect("recompute runs");
    assert_eq!(
        workspace.derived_builds(),
        builds,
        "a byte-identical replay skips"
    );
    assert_eq!(
        workspace.evaluations, evaluations,
        "a skip reproduces identical evaluations"
    );
}

#[test]
fn config_edit_forces_rebuild() {
    let mut workspace = ChartWorkspace::with_rows(gate_rows(40)).expect("workspace boots");
    // A pure threshold edit: no node changes, only the value moves.
    let current: i64 = workspace
        .stack
        .config("fast_mas")
        .expect("fast slot lives")
        .get_field("first_ma_length")
        .expect("length reads")
        .parse()
        .expect("length parses");
    workspace
        .stack
        .set_slot_field("fast_mas", "first_ma_length", &(current + 1).to_string())
        .expect("threshold writes");
    let builds = workspace.derived_builds();
    workspace.recompute().expect("recompute runs");
    assert_eq!(
        workspace.derived_builds(),
        builds + 1,
        "an edited threshold recomputes"
    );
    // A structural edit (new nodes) recomputes too.
    workspace
        .stack
        .set_slot_field("fast_mas", "show_smas", "true")
        .expect("toggle writes");
    let builds = workspace.derived_builds();
    workspace.recompute().expect("recompute runs");
    assert_eq!(
        workspace.derived_builds(),
        builds + 1,
        "an added slot arm recomputes"
    );
}

#[test]
fn slot_add_and_remove_force_rebuild() {
    let mut workspace = ChartWorkspace::with_rows(gate_rows(40)).expect("workspace boots");
    workspace.slot_indicator("sma").expect("sma slots");
    let builds = workspace.derived_builds();
    workspace.recompute().expect("recompute runs");
    assert_eq!(
        workspace.derived_builds(),
        builds + 1,
        "an added slot recomputes"
    );
    let builds = workspace.derived_builds();
    workspace.remove_indicator("sma").expect("sma removes");
    assert_eq!(
        workspace.derived_builds(),
        builds + 1,
        "a removal rebuilds directly"
    );
    workspace.recompute().expect("recompute runs");
    assert_eq!(
        workspace.derived_builds(),
        builds + 1,
        "the post-removal replay holds the gate"
    );
}

#[test]
fn row_extension_forces_rebuild() {
    let mut workspace = ChartWorkspace::with_rows(gate_rows(40)).expect("workspace boots");
    let last = workspace.rows.last().copied().expect("rows seed");
    workspace.rows.push(ChartRow {
        time: last.time + 900,
        open: last.close,
        high: last.close + 1.0,
        low: last.close - 1.0,
        close: last.close + 0.5,
        volume: 10,
    });
    let builds = workspace.derived_builds();
    workspace.recompute().expect("recompute runs");
    assert_eq!(
        workspace.derived_builds(),
        builds + 1,
        "one more bar recomputes"
    );
}

#[test]
fn fuzz_skip_iff_exact_key_match() {
    let mut workspace = ChartWorkspace::with_rows(gate_rows(30)).expect("workspace boots");
    let mut state: u64 = 0x1234_5678_9abc_def0;
    let mut next = || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        state
    };
    let mut sma_on = false;
    let mut smas_on = false;
    for round in 0..200 {
        let builds = workspace.derived_builds();
        let stored = (workspace.last_rows_shape, workspace.last_plan_fingerprint);
        let shadow_before = shadow_inputs(&workspace);
        match next() % 6 {
            0 => {
                let last = workspace.rows.last().copied().expect("rows hold");
                workspace.rows.push(ChartRow {
                    time: last.time + 900,
                    open: last.close,
                    high: last.close + 1.0,
                    low: last.close - 1.0,
                    close: last.close + 0.5,
                    volume: 10,
                });
            }
            1 => {
                if workspace.rows.len() > 10 {
                    workspace.rows.pop();
                }
            }
            2 => {
                smas_on = !smas_on;
                workspace
                    .stack
                    .set_slot_field(
                        "fast_mas",
                        "show_smas",
                        if smas_on { "true" } else { "false" },
                    )
                    .expect("toggle writes");
            }
            3 => {
                let current: i64 = workspace
                    .stack
                    .config("fast_mas")
                    .expect("fast slot lives")
                    .get_field("first_ma_length")
                    .expect("length reads")
                    .parse()
                    .expect("length parses");
                workspace
                    .stack
                    .set_slot_field("fast_mas", "first_ma_length", &(current + 1).to_string())
                    .expect("length writes");
            }
            4 => {
                if sma_on {
                    workspace.remove_indicator("sma").expect("sma removes");
                    sma_on = false;
                } else {
                    workspace.slot_indicator("sma").expect("sma slots");
                    sma_on = true;
                }
            }
            _ => {}
        }
        let key_now = (
            rows_shape_of(&workspace.rows),
            workspace.plan_fingerprint().expect("key builds"),
        );
        let dirty = shadow_inputs(&workspace) != shadow_before;
        workspace.recompute().expect("recompute runs");
        let built = workspace.derived_builds() != builds;
        assert_eq!(
            built,
            key_now != stored,
            "round {round}: skip iff the exact key matches"
        );
        if dirty {
            assert!(
                built,
                "round {round}: dirty inputs must rebuild, never skip"
            );
        }
    }
}

#[test]
fn revised_forming_bar_recomputes_without_a_shape_change() {
    let mut workspace = ChartWorkspace::with_rows(gate_rows(40)).expect("workspace boots");
    let builds = workspace.derived_builds();
    let before = workspace.evaluations.clone();
    let mut revised = workspace.rows.clone();
    let tail = revised.last_mut().expect("rows hold");
    tail.close += 5.0;
    tail.high += 5.0;
    workspace.rows = revised;
    workspace.recompute().expect("recompute runs");
    assert_eq!(
        workspace.derived_builds(),
        builds + 1,
        "a revised forming bar re-evaluates at the same shape"
    );
    assert_ne!(
        workspace.evaluations, before,
        "the overlay reflects the revised content"
    );
}

#[test]
fn identical_shape_symbol_switch_recomputes() {
    let mut workspace = ChartWorkspace::with_rows(gate_rows(40)).expect("workspace boots");
    let builds = workspace.derived_builds();
    let other: Vec<ChartRow> = (0..40)
        .map(|i| {
            let close = 200.0 + (i as f64);
            ChartRow {
                time: 1_700_000_000 + i * 900,
                open: close - 1.0,
                high: close + 1.0,
                low: close - 2.0,
                close,
                volume: 10,
            }
        })
        .collect();
    assert_eq!(
        rows_shape_of(&other),
        rows_shape_of(&workspace.rows),
        "the switch fixture keeps the shape identical"
    );
    workspace.rows = other;
    workspace.recompute().expect("recompute runs");
    assert_eq!(
        workspace.derived_builds(),
        builds + 1,
        "identical-shape content still re-evaluates"
    );
}

#[test]
fn config_edit_moves_the_consumer_key() {
    let mut workspace = ChartWorkspace::with_rows(gate_rows(40)).expect("workspace boots");
    workspace.recompute().expect("evaluations build");
    let keys_before: Vec<u64> = workspace
        .evaluations
        .get("fast_mas")
        .expect("fast_mas evaluates")
        .series
        .iter()
        .map(|series| series.key)
        .collect();
    assert!(!keys_before.is_empty(), "fast_mas publishes keys");
    let current: i64 = workspace
        .stack
        .config("fast_mas")
        .expect("fast slot lives")
        .get_field("first_ma_length")
        .expect("length reads")
        .parse()
        .expect("length parses");
    workspace
        .stack
        .set_slot_field("fast_mas", "first_ma_length", &(current + 1).to_string())
        .expect("threshold writes");
    workspace.recompute().expect("recompute runs");
    let after = workspace
        .evaluations
        .get("fast_mas")
        .expect("fast_mas evaluates");
    let keys_after: Vec<u64> = after.series.iter().map(|series| series.key).collect();
    assert_ne!(
        keys_before, keys_after,
        "a value-dial edit moves the consumer key"
    );
    assert!(
        after
            .series
            .iter()
            .any(|series| series.values.iter().any(Option::is_some)),
        "the rebuilt evaluation still carries values"
    );
}

#[test]
fn paint_only_edit_keeps_the_gate() {
    let mut workspace = ChartWorkspace::with_rows(gate_rows(40)).expect("workspace boots");
    workspace.recompute().expect("evaluations build");
    let builds = workspace.derived_builds();
    let before = workspace.evaluations.clone();
    let current: String = workspace
        .stack
        .config("integration_lite")
        .expect("lite slot lives")
        .get_field("brightness")
        .expect("brightness reads");
    let flipped = if current == "50" { "60" } else { "50" }.to_owned();
    workspace
        .stack
        .set_slot_field("integration_lite", "brightness", &flipped)
        .expect("paint dial writes");
    assert_eq!(
        workspace
            .stack
            .config("integration_lite")
            .expect("lite slot lives")
            .get_field("brightness")
            .expect("brightness reads"),
        flipped,
        "the paint dial actually moved"
    );
    workspace.recompute().expect("recompute runs");
    assert_eq!(
        workspace.derived_builds(),
        builds,
        "a paint-only dial skips the derived rebuild"
    );
    assert_eq!(
        workspace.evaluations, before,
        "paint-only edits leave every evaluated series untouched"
    );
}
