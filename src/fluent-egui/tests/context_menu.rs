use super::*;
use protopyne_chart_model::ChartRow;
use protopyne_chart_scale::PriceScaleMode;

/// Price pane `[90, 110]` on 400 px with 8 px bars, 60 rows: slot `s` sits
/// at x `8s`, so the axis strip (x 300..368) maps past the last bar.
fn viewport() -> Viewport {
    Viewport::new(PriceScaleMode::Normal, 90.0, 110.0, 400.0, 8.0, 90.0, false)
        .expect("valid test viewport")
}

fn plot() -> Rect {
    Rect::new(Pos2::new(0.0, 0.0), Pos2::new(300.0, 382.0))
}

fn axis() -> Rect {
    Rect::new(Pos2::new(300.0, 0.0), Pos2::new(368.0, 382.0))
}

fn rows(count: usize) -> Vec<ChartRow> {
    (0..count as i64)
        .map(|i| ChartRow {
            time: i,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.5,
            volume: 10,
        })
        .collect()
}

#[test]
fn axis_hover_raises_a_trigger_with_the_nearest_bar() {
    let view = viewport();
    let history = rows(60);
    let trigger =
        detect_context_trigger(0, axis(), plot(), &view, &history, Pos2::new(334.0, 120.0))
            .expect("the axis strip answers");
    assert_eq!(trigger.pane, 0, "the caller names the pane");
    assert_eq!(
        trigger.bar_index, 38,
        "x 300 reads slot 37.5, ties round up"
    );
    assert_eq!(
        trigger.plot_pos,
        (300.0, 120.0),
        "pane-local anchor, x clamped into the plot"
    );
}

#[test]
fn plot_legend_and_off_canvas_hover_raise_nothing() {
    let view = viewport();
    let history = rows(60);
    for point in [
        Pos2::new(120.0, 200.0),
        Pos2::new(0.0, 0.0),
        Pos2::new(400.0, 200.0),
        Pos2::new(334.0, 500.0),
        Pos2::new(f64::NAN as f32, 120.0),
    ] {
        assert!(
            detect_context_trigger(0, axis(), plot(), &view, &history, point).is_none(),
            "a click at {point:?} raises nothing"
        );
    }
    assert!(
        detect_context_trigger(0, axis(), plot(), &view, &[], Pos2::new(334.0, 120.0)).is_none(),
        "no rows name no bar"
    );
}

#[test]
fn closed_menu_paints_nothing_and_picks_nothing() {
    let mut open = false;
    egui::__run_test_ui(|ui| {
        let picked: Option<&str> = paint_context_menu(
            ui,
            egui::Id::new("menu"),
            egui::Pos2::ZERO,
            &mut open,
            &[("auto", "AutoHeight")],
        );
        assert_eq!(picked, None, "a closed menu picks nothing");
    });
    assert!(!open, "painting a closed menu never opens it");
}

#[test]
fn open_menu_paints_without_a_spurious_pick() {
    let mut open = true;
    egui::__run_test_ui(|ui| {
        let picked: Option<&str> = paint_context_menu(
            ui,
            egui::Id::new("menu"),
            egui::Pos2::new(10.0, 10.0),
            &mut open,
            &[("auto", "AutoHeight")],
        );
        assert_eq!(picked, None, "an unclicked menu picks nothing");
    });
    assert!(open, "an unclicked menu stays open");
}
