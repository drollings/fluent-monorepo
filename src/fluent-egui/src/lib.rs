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

pub mod annotations;
pub mod config;
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

pub use annotations::{AnnotationSet, MAX_UNDO_DEPTH, TimeIndex, annotate_at, resolve_annotation};
pub use config::{PaneConfig, ThemeConfig};
pub use error::EguiError;
pub use feed::{
    ChartFeed, CreditIntake, HistoryError, HistoryRung, PaneHandle, PaneWaveUnit,
    RefreshGate, TransportRetry, load_history, run_wave,
};
pub use fields::{
    COLOR_FORMAT, FieldEdits, FieldKind, SchemaField, SettingsTab, classify_schema_field,
    fields_for_tab, paint_schema_fields, paint_tabbed_schema_fields,
    paint_tabbed_schema_fields_compact, schema_fields, tab_fields,
};
pub use hittest::{
    Crosshair, HIT_EPSILON_PX, Hit, crosshair_data, crosshair_pixel, hit_test,
    point_to_segment_distance,
};
pub use interaction::{
    ANCHOR_DRIFT_EPS_PX, InteractionState, MAX_BAR_SPACING, MIN_BAR_SPACING, PointerInput,
    wheel_zoom_factor, zoom_range_around,
};
pub use paint::{
    CANDLE_BODY_FRACTION, CANDLE_GAP_THRESHOLD_PX, CANDLE_MIN_BODY_PX, CandleGeom,
    CrosshairLines, DeltaBarGeom, EyeIconGeom, HexBoltIconGeom, MarkerDot, PivotDot, RibbonBand,
    RibbonPolygon, SearchIconGeom, TintedCandle, TintedDeltaBar, alpha_u8, area_polygons, area_vertex_color,
    candle_body_width, candle_geoms, crosshair_lines, delta_bar_geoms, eye_icon_geom,
    hex_bolt_icon_geom, marker_dots, opacity_from_transparency, oscillator_points, parse_hex_color,
    paint_area_fill, paint_candles, paint_candles_as, paint_candles_tinted, paint_crosshair,
    paint_delta_bars, paint_delta_bars_tinted, paint_error_chip, paint_eye_icon, paint_hex_bolt_icon,
    paint_markers, paint_oscillator, paint_oscillator_styled, paint_ribbon, paint_ribbon_fill,
    paint_search_icon, paint_session_breaks, paint_thresholds, pivot_dots, pivot_price_dots,
    resolve_color, ribbon_bands, ribbon_polygons, search_icon_geom, threshold_line,
    translate_breaks, translate_candles, translate_delta_bars, translate_dots, translate_markers,
    translate_points, translate_polygons,
};
pub use modal::{CatalogAction, CatalogEntry, CatalogModal, paint_catalog_modal};
pub use plan::{CachedSeries, PaneCache, PanePlan, cache_key};
pub use replay::{ReplayController, ReplayState};
pub use slot::{
    HotSlot, HotSlotAction, HotSlotStack, ReadoutValue, SettingsCommit, SettingsOutcome,
    SlotAttachment, SlotControlHit, SlotControlRect, paint_hot_slot_stack,
    paint_slot_settings_modal,
};
pub use style::IndicatorStyle;
pub use view::{
    DeltaView, PaneKind, PaneSeries, PivotMark, PivotSide, RibbonView, affine_squash, band_squash,
    delta_candle_view, map_series_optional, pivot_marks, ribbon_fill_style_key, ribbon_style_key,
    ribbon_view, unit_squash,
};
pub use style::{LINE_WIDTH_FIELD, MAX_LINE_WIDTH, MIN_LINE_WIDTH, STYLE_FIELDS, style_key};
pub use viewport::{
    BreakLine, Viewport, axis_ticks, thin_ticks, visible_rows,
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
