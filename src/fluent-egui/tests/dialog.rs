use super::*;
use crate::config::ThemeConfig;
use fluent_wvr::{Describable, FieldAccess};

#[derive(Debug, Clone, PartialEq, FieldAccess, Describable)]
struct DialogProbe {
    #[field(desc = "Lookback length", min = 1)]
    length: i64,
    #[field(desc = "Display name", max_len = 8)]
    name: String,
}

#[derive(Debug, Clone, PartialEq, FieldAccess, Describable)]
struct DialogStyle {
    #[field(desc = "Stroke width", min = 1.0, max = 10.0)]
    line_width: f64,
}

fn probe() -> (DialogProbe, DialogStyle) {
    (
        DialogProbe {
            length: 14,
            name: "ribbon".to_owned(),
        },
        DialogStyle { line_width: 2.0 },
    )
}

fn opened() -> (DialogState, DialogProbe, DialogStyle) {
    let (config, style) = probe();
    let mut dialog = DialogState::new("Ribbon settings");
    dialog.open_staged(&config, &style);
    assert!(dialog.is_open(), "open_staged opens the dialog");
    (dialog, config, style)
}

#[test]
fn malformed_fields_never_commit_and_the_dialog_stays_open() {
    let (mut dialog, mut config, mut style) = opened();
    dialog.set_staged("length", "-5");
    dialog.set_staged("line_width", "99.0");
    dialog.set_staged("name", "way too long");
    let snapshot = (config.clone(), style.clone());
    let refusals = dialog.apply(&mut config, &mut style);
    assert_eq!(
        refusals.len(),
        3,
        "every malformed field refuses: {refusals:?}"
    );
    for field in ["length", "line_width", "name"] {
        assert!(
            refusals.iter().any(|refusal| refusal.starts_with(field)),
            "{field} names itself: {refusals:?}"
        );
    }
    assert!(dialog.is_open(), "a refused Apply never closes");
    assert_eq!((config, style), snapshot, "live state untouched");
}

#[test]
fn cancel_discards_without_touching_live_state() {
    let (mut dialog, config, style) = opened();
    let snapshot = (config.clone(), style.clone());
    dialog.set_staged("length", "42");
    dialog.set_staged("line_width", "3.0");
    dialog.cancel();
    assert!(!dialog.is_open(), "cancel closes");
    assert_eq!((config, style), snapshot, "live state byte-identical");
}

#[test]
fn apply_commits_valid_fields_while_refusing_invalid_ones() {
    let (mut dialog, mut config, mut style) = opened();
    dialog.set_staged("length", "42");
    dialog.set_staged("line_width", "99.0");
    let refusals = dialog.apply(&mut config, &mut style);
    assert_eq!(refusals.len(), 1, "one field refuses: {refusals:?}");
    assert!(
        refusals[0].starts_with("line_width"),
        "the refusal names its field: {refusals:?}"
    );
    assert_eq!(config.length, 42, "the valid field lands");
    assert!(
        (style.line_width - 2.0).abs() < f64::EPSILON,
        "the refused field keeps its live value"
    );
    assert!(dialog.is_open(), "Apply keeps the dialog open");
    assert_eq!(
        dialog.staged("length"),
        Some("42"),
        "applied drafts reseed from live"
    );
}

#[test]
fn untouched_dialog_paints_without_committing() {
    let (mut dialog, _, _) = opened();
    let theme = ThemeConfig::default();
    egui::__run_test_ui(|ui| {
        let outcome = dialog.show(ui, &theme, &|_| {}).expect("the dialog paints");
        assert_eq!(
            outcome.commit,
            DialogCommit::None,
            "an untouched frame commits nothing"
        );
    });
    assert!(dialog.is_open(), "painting never closes");
}
