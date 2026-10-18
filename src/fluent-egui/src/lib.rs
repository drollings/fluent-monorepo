#![forbid(unsafe_code)]

//! Generic paint scaffolding over headless chart data.
//!
//! This crate owns the display-side machinery every native chart UI needs —
//! pane view-models, viewport geometry, DAG-ordered compute plans,
//! fingerprint-keyed series caches, layout/theme control-plane components,
//! headless paint geometry plus thin `egui` emitters, supervised feed/wave
//! plumbing, and replay cursors — composed from the workspace primitives
//! (`fluent-wvr`, `fluent-concurrency`, `fluent-dag`, `common-core`) plus
//! three chart-domain dependencies, stated explicitly: `protopyne-chart-model`
//! (`ChartRow`/`ChartBars` rows, `Annotation` overlays, `to_display_float`),
//! `protopyne-chart-scale` (`PriceRange`/`PriceScale` geometry, `nice_ticks`),
//! and `protopyne-types` (`Validate`/`check_for_execute`/`valid_output`).
//! "Domain-blind" means blind to indicator, regime, Pine, and trading-path
//! logic — never dependency-free of chart display types. Trading-domain
//! display truth lives in the terminal crate built on top.
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

pub mod annotations;
pub mod config;
pub mod context_menu;
pub mod error;
pub mod feed;
pub mod fields;
pub mod hittest;
pub mod interaction;
pub mod modal;
pub mod paint;
pub mod plan;
pub mod replay;
pub mod slot;
pub mod style;
pub mod view;
pub mod viewport;
pub mod wheel;

pub use annotations::{annotate_at, resolve_annotation, AnnotationSet, TimeIndex, MAX_UNDO_DEPTH};
pub use config::{PaneConfig, ThemeConfig};
pub use context_menu::{detect_context_trigger, paint_context_menu, ContextTrigger};
pub use error::EguiError;
pub use feed::{
    load_history, run_wave, ChartFeed, CreditIntake, HistoryError, HistoryRung, PaneHandle,
    PaneWaveUnit, RefreshGate, TransportRetry,
};
pub use fields::{
    classify_schema_field, fields_for_tab, paint_schema_fields, paint_tabbed_schema_fields,
    paint_tabbed_schema_fields_compact, schema_fields, tab_fields, FieldEdits, FieldKind,
    SchemaField, SettingsTab, COLOR_FORMAT,
};
pub use hittest::{
    crosshair_data, crosshair_pixel, hit_test, point_to_segment_distance, Crosshair, Hit,
    HIT_EPSILON_PX,
};
pub use interaction::{
    anchor_window, wheel_zoom_factor, zoom_range_around, InteractionState, PointerInput,
    ANCHOR_DRIFT_EPS_PX, MAX_BAR_SPACING, MIN_BAR_SPACING,
};
pub use modal::{paint_catalog_modal, CatalogAction, CatalogEntry, CatalogModal};
pub use paint::{
    aggregate_column, alpha_u8, area_polygons, area_vertex_color, candle_body_width, candle_geoms,
    column_candle_geoms, column_delta_geoms, column_runs, crosshair_lines, culled_candle_geoms,
    delta_bar_geoms, delta_bar_geoms_sliced, eye_icon_geom, hex_bolt_icon_geom, marker_dots,
    needs_column_aggregation, opacity_from_transparency, oscillator_points,
    oscillator_points_sliced, paint_area_fill, paint_candles, paint_candles_as,
    paint_candles_tinted, paint_crosshair, paint_delta_bars, paint_delta_bars_tinted,
    paint_error_chip, paint_eye_icon, paint_hex_bolt_icon, paint_markers, paint_oscillator,
    paint_oscillator_styled, paint_ribbon, paint_ribbon_fill, paint_search_icon,
    paint_session_breaks, paint_thresholds, parse_hex_color, pivot_dots, pivot_price_dots,
    proportional_line_points, proportional_ribbon_polygons, resolve_color, ribbon_bands,
    ribbon_polygons, ribbon_polygons_based, ribbon_polygons_sliced, sample_column_extremes,
    sample_column_last, sampled_band_spans, sampled_line_points, sampled_ribbon_bands,
    search_icon_geom, threshold_line, translate_bands, translate_breaks, translate_candles,
    translate_delta_bars, translate_dots, translate_markers, translate_points, translate_polygons,
    CandleGeom, CrosshairLines, DeltaBarGeom, EyeIconGeom, HexBoltIconGeom, MarkerDot, PivotDot,
    RibbonBand, RibbonPolygon, SearchIconGeom, TintedCandle, TintedDeltaBar, CANDLE_BODY_FRACTION,
    CANDLE_GAP_THRESHOLD_PX, CANDLE_MIN_BODY_PX,
};
pub use plan::{cache_key, CachedSeries, PaneCache, PanePlan};
pub use replay::{ReplayController, ReplayState};
pub use slot::{
    paint_hot_slot_stack, paint_slot_settings_modal, HotSlot, HotSlotAction, HotSlotStack,
    ReadoutValue, SettingsCommit, SettingsOutcome, SlotAttachment, SlotControlHit, SlotControlRect,
};
pub use style::IndicatorStyle;
pub use style::{style_key, LINE_WIDTH_FIELD, MAX_LINE_WIDTH, MIN_LINE_WIDTH, STYLE_FIELDS};
pub use view::{
    affine_squash, band_squash, delta_candle_view, map_series_optional, pivot_marks,
    ribbon_fill_style_key, ribbon_style_key, ribbon_view, unit_squash, DeltaView, PaneKind,
    PaneSeries, PivotMark, PivotSide, RibbonView,
};
pub use viewport::{
    auto_height_range, axis_ticks, thin_ticks, visible_rows, BreakLine, Viewport,
    CULL_OVERSCAN_BARS, PROPORTIONAL_CULL_FACTOR,
};
pub use wheel::{PaneZones, Pos2 as WheelPos2, Rect as WheelRect, WheelTarget, WheelZones};

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
