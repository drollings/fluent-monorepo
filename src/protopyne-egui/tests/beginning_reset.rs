use super::*;

fn dated_rows() -> Vec<ChartRow> {
    let base = chrono::NaiveDate::from_ymd_opt(2025, 1, 1)
        .expect("fixture date builds")
        .and_hms_opt(0, 0, 0)
        .map_or(0, |dt| dt.and_utc().timestamp());
    (0..120)
        .map(|i| {
            let close = 100.0 + f64::from(i);
            ChartRow {
                time: base + i64::from(i) * 86_400,
                open: close - 0.5,
                high: close + 0.5,
                low: close - 1.0,
                close,
                volume: 10,
            }
        })
        .collect()
}

#[test]
fn clear_beginning_restores_two_month_default() {
    let mut workspace = ChartWorkspace::with_rows(dated_rows()).expect("workspace boots");
    workspace
        .set_beginning("2025-02-01", None)
        .expect("date pick applies");
    assert_eq!(
        workspace.range(),
        DateRange::All,
        "a pick widens to the full span"
    );
    workspace.set_beginning("", None).expect("clear applies");
    assert_eq!(
        workspace.range(),
        DateRange::Month2,
        "clear restores the default preset"
    );
    assert_eq!(
        workspace.visible_months(),
        2,
        "clear restores the default width"
    );
    assert!(
        workspace.custom_start.is_none(),
        "clear releases the custom start"
    );
    workspace
        .set_beginning("2025-02-01", None)
        .expect("date pick applies");
    workspace
        .set_beginning("   ", None)
        .expect("whitespace clear applies");
    assert_eq!(
        workspace.range(),
        DateRange::Month2,
        "whitespace restores the default preset"
    );
    assert_eq!(
        workspace.visible_months(),
        2,
        "whitespace restores the default width"
    );
    assert!(
        workspace.custom_start.is_none(),
        "whitespace releases the custom start"
    );
}

#[test]
fn beginning_pick_sets_all() {
    let mut workspace = ChartWorkspace::with_rows(dated_rows()).expect("workspace boots");
    workspace
        .set_beginning("2025-02-01", None)
        .expect("date pick applies");
    assert_eq!(
        workspace.range(),
        DateRange::All,
        "a pick widens to the full span"
    );
    assert!(
        workspace.custom_start.is_some(),
        "a pick pins the custom start"
    );
}

/// The Range button names the loaded window, not the preset a custom pick
/// replaced: a Beginning pick clears the preset highlight and shows the date.
#[test]
fn range_button_reflects_a_beginning_pick() {
    let mut workspace = ChartWorkspace::with_rows(dated_rows()).expect("workspace boots");
    assert_eq!(
        workspace.active_range(),
        Some(DateRange::Month2),
        "a preset is highlighted"
    );
    assert_eq!(workspace.range_label(), "2M");
    workspace
        .set_beginning("2025-02-01", None)
        .expect("date pick applies");
    assert_eq!(
        workspace.active_range(),
        None,
        "a custom window is no preset"
    );
    assert_eq!(
        workspace.range_label(),
        "2025-02-01",
        "the button names the picked start"
    );
    workspace.set_beginning("", None).expect("clear applies");
    assert_eq!(workspace.active_range(), Some(DateRange::Month2));
    assert_eq!(workspace.range_label(), "2M");
}
