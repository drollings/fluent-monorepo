#![forbid(unsafe_code)]

//! Generic paint scaffolding over headless chart data.
//!
//! This crate owns the domain-blind machinery every native chart UI needs —
//! pane view-models, viewport geometry, DAG-ordered compute plans,
//! fingerprint-keyed series caches, layout/theme control-plane components,
//! headless paint geometry plus thin `egui` emitters, supervised feed/wave
//! plumbing, and replay cursors — composed from the workspace primitives
//! (`fluent-wvr`, `fluent-concurrency`, `fluent-dag`, `common-core`) plus
//! display vocabulary (chart-model rows), pure scale math (chart-scale), and
//! the shared validation bridge (types). It knows no indicator, no regime,
//! no Pine, and no trading path: trading-domain display truth lives in the
//! terminal crate built on top.
//!
//! The numeric boundary is one-way: `Decimal` values become display `f64`
//! through the chart-model conversion (`rows_of`/`row_of`/`to_display_float`),
//! and no `f64` held here is ever parsed back into a decision. Non-finite
//! display legs are skipped and counted, never substituted.
//!
//! Paint-edge confinement: `egui` (and the optional `eframe` demo shell) may
//! be depended on only by this crate and the terminal crate. No trading or
//! headless chart crate may gain a paint dependency in either direction of
//! the decision path.

pub mod config;
pub mod error;
pub mod feed;
pub mod paint;
pub mod plan;
pub mod replay;
pub mod view;
pub mod viewport;

pub use config::{PaneConfig, ThemeConfig};
pub use error::EguiError;
pub use feed::{
    ChartFeed, CreditIntake, HistoryError, HistoryRung, PaneHandle, PaneWaveUnit,
    RefreshGate, TransportRetry, load_history, run_wave,
};
pub use paint::{
    CandleGeom, DeltaBarGeom, PivotDot, RibbonBand, RibbonPolygon, candle_geoms,
    delta_bar_geoms, opacity_from_transparency, oscillator_points, paint_candles,
    paint_candles_as, paint_delta_bars, paint_oscillator, paint_pivot_dots, paint_ribbon,
    paint_ribbon_fill, paint_session_breaks, paint_thresholds, pivot_dots, pivot_price_dots,
    ribbon_bands, ribbon_polygons, theme_color, threshold_line,
};
pub use plan::{CachedSeries, PaneCache, PanePlan, cache_key};
pub use replay::{ReplayController, ReplayState};
pub use view::{
    DeltaView, PaneKind, PaneSeries, PivotMark, PivotSide, RibbonView, delta_candle_view,
    pivot_marks, ribbon_style_key, ribbon_view,
};
pub use viewport::{
    BreakLine, Viewport, axis_ticks, thin_ticks, visible_rows,
};

/// Paint-edge marker; touches the linked paint dependency so wiring breaks
/// loudly if the dependency stops resolving.
#[must_use]
pub fn is_paint_edge() -> bool {
    let _ = egui::Color32::WHITE;
    true
}

#[cfg(test)]
#[path = "../tests/lib.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/budget.rs"]
mod budget_tests;
