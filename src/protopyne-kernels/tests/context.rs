use super::*;
use chrono::TimeZone as _;
use protopyne_types::{Bar, BarWindow, Timeframe};

use crate::{drawing::DrawingKind, value::PineValue};

fn bar_at_15m(step: i64) -> Bar {
    let ts = chrono::Utc
        .with_ymd_and_hms(2026, 1, 2, 9, 0, 0)
        .single()
        .expect("valid test timestamp")
        + chrono::Duration::minutes(15 * step);
    Bar::new()
        .ts(ts)
        .o("100.00".parse().expect("decimal"))
        .h("101.00".parse().expect("decimal"))
        .l("99.00".parse().expect("decimal"))
        .c("100.50".parse().expect("decimal"))
        .volume(1_000_u64)
        .maybe_adj(None)
        .build()
}

fn three_bars() -> Vec<Bar> {
    vec![bar_at_15m(0), bar_at_15m(1), bar_at_15m(2)]
}

#[test]
fn cursor_starts_at_first_closed_bar_and_advances() {
    let bars = three_bars();
    let mut ctx = RunCtx::new(&bars, InputSet::new());
    assert_eq!(ctx.cursor(), 0);
    assert_eq!(ctx.current(), Some(&bars[0]));
    assert_eq!(ctx.bars_series().at(0), Some(&bars[0]));
    assert_eq!(ctx.bars_series().at(1), None);

    ctx.advance();
    assert_eq!(ctx.cursor(), 1);
    assert_eq!(ctx.bars_series().at(0), Some(&bars[1]));
    assert_eq!(ctx.bars_series().at(1), Some(&bars[0]));
    assert_eq!(ctx.bars_series().at(2), None);
}

/// One cell identity for the store-level assertions below.
fn cell(decl: u32) -> crate::vars::ScopeKey {
    crate::vars::ScopeKey::new(
        Some("local".to_owned()),
        Vec::new(),
        crate::vars::NodeId::new(decl),
    )
}

#[test]
fn two_concurrent_contexts_are_isolated() {
    let bars = three_bars();
    let mut first = RunCtx::new(&bars, InputSet::new());
    let second = RunCtx::new(&bars, InputSet::new());

    first.advance();
    first
        .vars_mut()
        .set_var(cell(1), "trend", PineValue::Float(1.5));
    first.draw(
        Drawing::new()
            .kind(DrawingKind::Label)
            .bar_index(1)
            .price(100.0)
            .text("a".to_owned())
            .build(),
    );

    assert_eq!(second.cursor(), 0);
    assert_eq!(second.vars().get_var(&cell(1)), None);
    assert!(second.drawings().is_empty());
    assert_eq!(first.vars().get_var(&cell(1)), Some(PineValue::Float(1.5)));
    assert_eq!(first.drawings().len(), 1);
}

#[test]
fn advance_resets_intrabar_state_only() {
    let bars = three_bars();
    let mut ctx = RunCtx::new(&bars, InputSet::new());
    ctx.vars_mut().set_var(cell(1), "v", PineValue::Int(1));
    ctx.vars_mut().set_varip(cell(2), "w", PineValue::Int(2));
    ctx.advance();
    assert_eq!(ctx.vars().get_var(&cell(1)), Some(PineValue::Int(1)));
    assert_eq!(ctx.vars().get_varip(&cell(2)), None);
}

#[test]
fn from_window_agrees_with_slice_constructor() {
    let mut window = BarWindow::new(Timeframe::M15);
    for step in 0..3 {
        window.push(bar_at_15m(step)).expect("in order");
    }
    let ctx = RunCtx::from_window(&window, InputSet::new());
    assert_eq!(ctx.bar_count(), 3);
    assert_eq!(ctx.cursor(), 0);
}
