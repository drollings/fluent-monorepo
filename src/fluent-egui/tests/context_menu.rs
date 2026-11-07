use super::*;
use crate::{flip_within, menu_anchor};
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

fn screen() -> Rect {
    Rect::new(Pos2::new(0.0, 0.0), Pos2::new(800.0, 600.0))
}

fn contains(outer: Rect, inner: Rect) -> bool {
    outer.min_x <= inner.min_x
        && outer.min_y <= inner.min_y
        && inner.max_x <= outer.max_x
        && inner.max_y <= outer.max_y
}

#[test]
fn flipped_cards_always_stay_on_screen() {
    let screens = [(800.0f32, 600.0f32), (100.5, 200.25), (64.0, 64.0)];
    let sizes = [
        (200.0f32, 100.0f32),
        (1.0, 1.0),
        (63.5, 63.5),
        (800.0, 600.0),
    ];
    let anchors = [0.0f32, 1.0, 10.0, 63.5, 399.0, 590.0, 799.0, 1000.0];
    for (sw, sh) in screens {
        let outer = Rect::new(Pos2::new(0.0, 0.0), Pos2::new(sw, sh));
        for (w, h) in sizes {
            if w > sw || h > sh {
                continue;
            }
            for x in anchors {
                for y in anchors {
                    let anchor = Pos2::new(x, y);
                    for (below, after) in
                        [(true, true), (true, false), (false, true), (false, false)]
                    {
                        let card = flip_within(outer, (w, h), anchor, below, after);
                        assert!(
                            contains(outer, card),
                            "card {w}x{h} at ({x}, {y}) below={below} after={after} stays inside: {card:?}"
                        );
                        assert!(
                            (card.max_x - card.min_x - w).abs() < 0.001
                                && (card.max_y - card.min_y - h).abs() < 0.001,
                            "the card keeps its size: {card:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn interior_triggers_never_flip() {
    let outer = screen();
    for anchor in [
        Pos2::new(10.0, 10.0),
        Pos2::new(400.0, 300.0),
        Pos2::new(100.0, 500.0),
    ] {
        let card = menu_anchor(outer, (200.0, 100.0), anchor);
        assert_eq!(
            (card.min_x, card.min_y),
            (anchor.x, anchor.y),
            "room on the preferred side keeps the anchor: {card:?}"
        );
    }
}

#[test]
fn menu_golden_three_by_three() {
    let outer = screen();
    let size = (200.0f32, 100.0f32);
    for (anchor, expect) in [
        ((10.0f32, 10.0f32), (10.0f32, 10.0f32)),
        ((400.0, 10.0), (400.0, 10.0)),
        ((790.0, 10.0), (590.0, 10.0)),
        ((10.0, 300.0), (10.0, 300.0)),
        ((400.0, 300.0), (400.0, 300.0)),
        ((790.0, 300.0), (590.0, 300.0)),
        ((10.0, 590.0), (10.0, 490.0)),
        ((400.0, 590.0), (400.0, 490.0)),
        ((790.0, 590.0), (590.0, 490.0)),
    ] {
        let card = menu_anchor(outer, size, Pos2::new(anchor.0, anchor.1));
        assert_eq!(
            (card.min_x, card.min_y, card.max_x, card.max_y),
            (expect.0, expect.1, expect.0 + size.0, expect.1 + size.1),
            "menu at {anchor:?} lands on {expect:?}"
        );
    }
}

#[test]
fn oversized_cards_pin_to_the_screen_origin() {
    let outer = screen();
    let card = menu_anchor(outer, (900.0, 700.0), Pos2::new(400.0, 300.0));
    assert_eq!(
        (card.min_x, card.min_y),
        (0.0, 0.0),
        "a card that cannot fit pins its origin: {card:?}"
    );
}

#[test]
fn menu_rows_convert_from_legacy_tuples() {
    let row = MenuRow::from(("auto", "AutoHeight"));
    assert_eq!(row.action, "auto", "the action carries over");
    assert_eq!(row.label, "AutoHeight", "the label carries over");
    assert!(row.enabled, "legacy rows start enabled");
    assert!(!row.divider_after, "legacy rows add no divider");
}

fn menu_frame<'a>(
    ctx: &egui::Context,
    input: egui::RawInput,
    id: egui::Id,
    at: egui::Pos2,
    open: &mut bool,
    items: &'a [MenuRow<&'a str>],
) -> (Option<&'a str>, egui::FullOutput) {
    let picked = std::cell::Cell::new(None);
    let mut output = ctx.run_ui(input, |ui| {
        let got: Option<&str> = paint_context_menu(ui, id, at, open, items);
        picked.set(got);
    });
    output.textures_delta.clear();
    (picked.take(), output)
}

fn menu_input(input: egui::RawInput) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 600.0),
        )),
        ..input
    }
}

fn menu_tap(pos: egui::Pos2) -> egui::RawInput {
    menu_input(egui::RawInput {
        events: vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
        ],
        ..Default::default()
    })
}

fn row_center(id: egui::Id, at: egui::Pos2, items: &[MenuRow<&str>], needle: &str) -> egui::Pos2 {
    let ctx = egui::Context::default();
    let mut open = true;
    menu_frame(
        &ctx,
        menu_input(egui::RawInput::default()),
        id,
        at,
        &mut open,
        items,
    );
    let (_, output) = menu_frame(
        &ctx,
        menu_input(egui::RawInput::default()),
        id,
        at,
        &mut open,
        items,
    );
    menu_text_center(&output, needle).unwrap_or_else(|| panic!("{needle} paints"))
}

fn menu_text_center(output: &egui::FullOutput, needle: &str) -> Option<egui::Pos2> {
    output
        .shapes
        .iter()
        .find_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) if text.galley.text() == needle => {
                Some(text.galley.rect.translate(text.pos.to_vec2()).center())
            }
            _ => None,
        })
}

fn two_rows() -> Vec<MenuRow<&'static str>> {
    vec![
        MenuRow::from(("auto", "AutoHeight")),
        MenuRow::from(("manual", "ManualHeight")),
    ]
}

#[test]
fn open_guard_suppresses_early_picks_and_admits_later_ones() {
    let items = two_rows();
    let at = egui::pos2(100.0, 100.0);
    let row = row_center(egui::Id::new("guard-probe"), at, &items, "AutoHeight");
    // Precision: a click completing on frame 0 or 1 is guarded, even though
    // the tap itself clicks (the recall run below proves the mechanism).
    let ctx = egui::Context::default();
    let mut open = true;
    let id = egui::Id::new("guard-menu");
    let (first, _) = menu_frame(&ctx, menu_tap(row), id, at, &mut open, &items);
    assert!(
        first.is_none(),
        "a click completing on frame 0 stays guarded"
    );
    let (early, _) = menu_frame(&ctx, menu_tap(row), id, at, &mut open, &items);
    assert!(
        early.is_none(),
        "a click completing on frame 1 stays guarded"
    );
    assert!(open, "a guarded click leaves the menu open");
    // Recall: the identical tap on frame 2 picks the row.
    let ctx = egui::Context::default();
    let mut open = true;
    menu_frame(
        &ctx,
        menu_input(egui::RawInput::default()),
        id,
        at,
        &mut open,
        &items,
    );
    menu_frame(
        &ctx,
        menu_input(egui::RawInput::default()),
        id,
        at,
        &mut open,
        &items,
    );
    let (late, _) = menu_frame(&ctx, menu_tap(row), id, at, &mut open, &items);
    assert_eq!(
        late,
        Some("auto"),
        "a click completing on frame 2 picks the row"
    );
    assert!(!open, "an admitted pick closes the menu");
}

#[test]
fn disabled_rows_never_yield_picks() {
    let at = egui::pos2(100.0, 100.0);
    let items = vec![
        MenuRow {
            action: "auto",
            label: "AutoHeight".to_owned(),
            enabled: false,
            divider_after: true,
        },
        MenuRow::from(("manual", "ManualHeight")),
    ];
    let row = row_center(egui::Id::new("disabled-probe"), at, &items, "AutoHeight");
    let live = row_center(egui::Id::new("live-probe"), at, &items, "ManualHeight");
    let ctx = egui::Context::default();
    let mut open = true;
    let id = egui::Id::new("disabled-menu");
    menu_frame(
        &ctx,
        menu_input(egui::RawInput::default()),
        id,
        at,
        &mut open,
        &items,
    );
    menu_frame(
        &ctx,
        menu_input(egui::RawInput::default()),
        id,
        at,
        &mut open,
        &items,
    );
    let (picked, _) = menu_frame(&ctx, menu_tap(row), id, at, &mut open, &items);
    assert!(
        picked.is_none(),
        "a disabled row picks nothing past the guard"
    );
    assert!(open, "disabled clicks leave the menu open");
    let (picked, _) = menu_frame(&ctx, menu_tap(live), id, at, &mut open, &items);
    assert_eq!(
        picked,
        Some("manual"),
        "the live row still picks past the guard"
    );
}

#[test]
fn near_edge_triggers_flip_the_menu_onto_the_screen() {
    let ctx = egui::Context::default();
    let mut open = true;
    let items = two_rows();
    let id = egui::Id::new("edge-menu");
    let at = egui::pos2(790.0, 590.0);
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
    let (_, _) = menu_frame(
        &ctx,
        menu_input(egui::RawInput::default()),
        id,
        at,
        &mut open,
        &items,
    );
    let (_, settled) = menu_frame(
        &ctx,
        menu_input(egui::RawInput::default()),
        id,
        at,
        &mut open,
        &items,
    );
    for clipped in &settled.shapes {
        if let egui::Shape::Text(text) = &clipped.shape {
            let center = text.galley.rect.translate(text.pos.to_vec2()).center();
            assert!(
                screen.contains(center),
                "settled menu text stays on screen: {center:?}"
            );
        }
    }
    let mut roomy = true;
    let roomy_id = egui::Id::new("roomy-menu");
    let roomy_at = egui::pos2(100.0, 100.0);
    menu_frame(
        &ctx,
        menu_input(egui::RawInput::default()),
        roomy_id,
        roomy_at,
        &mut roomy,
        &items,
    );
    let (_, roomy_out) = menu_frame(
        &ctx,
        menu_input(egui::RawInput::default()),
        roomy_id,
        roomy_at,
        &mut roomy,
        &items,
    );
    assert!(
        menu_text_center(&roomy_out, "AutoHeight").is_some(),
        "an interior trigger paints its rows"
    );
}
