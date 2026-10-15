#![forbid(unsafe_code)]

//! Trading terminal over the generic paint scaffolding.
//!
//! This crate owns every display decision that knows trading semantics —
//! oscillator identity and its frozen squash table, the standard pane
//! topology, chart configuration, session breaks, toolbar presets, the
//! multi-timeframe widget, oscillator recompute, regime labels, the
//! IntegrationLite parity pane, and server-shaped export — while the generic
//! pane/viewport/plan/paint/feed/replay machinery lives in `fluent-egui`
//! and is reused, never reimplemented.
//!
//! The numeric boundary stays one-way: `Decimal` money becomes display `f64`
//! through the chart-model conversion, and no `f64` held here is ever parsed
//! back into a decision. The compute truth stays in `protopyne-kernels`
//! (indicators, regimes, Pine engine); this crate only reads it for display.
//!
//! Paint-edge confinement: `egui` (and the optional `eframe` demo shell) may
//! be depended on only by `fluent-egui` and this crate. No trading or
//! headless chart crate may gain a paint dependency.

pub mod annotations;
pub mod chart;
pub mod delta_volume_profile;
pub mod export;
pub mod fast_mas;
pub mod feed;
pub mod grant_form;
pub mod indicators;
pub mod integration_lite;
pub mod oscillators;
pub mod pine_overlay;
pub mod regime_labels;
pub mod replay;
pub mod session;
pub mod topology;
pub mod views;
pub mod widgets;
pub mod workspace;

pub use annotations::{annotations_of, annotations_of_counted, append_new};
pub use chart::{
    AreaView, ChartConfig, DEFAULT_SYMBOL, PictureSource, TimeframeKey, area_view, chart_price_bars,
    default_symbol, line_values, picture_rows, picture_source, refuse_chart_type, timed_plan_build,
    window_for_range,
};
pub use export::{
    EXPORT_CSV_HEADER, OverlayExport, export_overlay_json, export_rows_csv, export_rows_json,
};
pub use fast_mas::{
    FastMasConfig, FastMasLine, FastMasView, evaluate_fast_mas, paint_fast_mas,
};
pub use feed::{RecomputeJob, RecomputePool};
pub use grant_form::{GrantFormError, GrantFormField, GrantFormState, paint_grant_form};
pub use indicators::{
    ChartIndicator, FastMasIndicator, IndicatorCategory, IndicatorEvaluation, IndicatorSeries,
    IntegrationLiteIndicator, KernelIndicator, KernelIndicatorConfig, build_indicator_catalog,
    instantiate_indicator,
};
pub use integration_lite::{
    ATR_LENGTH, BOUNDED_DELTA_UNIT, CALIBRATED_PIVOT_THRESHOLD, CandleMomentum, FreezePair,
    FreezeReport, IntegrationLiteConfig, IntegrationLiteSubPane, IntegrationLiteView, LiteFrozen,
    LiteNative, MA_HEALTH_K, MomentumPalette, MomentumTint, OSCILLATOR_BRIGHTNESS,
    RAW_OSCILLATOR_LENGTH, RIBBON_HEALTH_K, RIBBON_MEAN_LENGTH, SIGNAL_EIT_ALPHA, VOL_SMA_LENGTH,
    VOLUME_TRANSPARENCY, candle_momentum_keys, candle_momentum_series, compare_freeze,
    integration_lite_sub_pane, integration_lite_view, integration_lite_view_calibrated,
    momentum_tint, momentum_tints, native_lite_series, oscillator_transparency, precision_recall,
    ribbon_fill_transparency, series_max_diff,
};
pub use oscillators::{
    BOUNDED_OSC_SCALE, CALIBRATED_BOUNDED_PIVOT_THRESHOLD, OscParams, OscillatorKind,
    SQUASH_100_DIVISOR, normalize_oscillator, normalize_oscillator_bounded, normalize_series,
    normalize_series_bounded, osc_params, oscillator_kind_name, parse_oscillator_kind,
    squash_series_to_100, squash_to_100,
};
pub use pine_overlay::{
    OverlayGeoms, PlacedLabel, SeriesGeom, overlay_geoms, paint_overlay, paint_unsupported_chip,
    translate_overlay,
};
pub use replay::{ReplayScrub, ScrubOutcome, ScrubRefusal, paint_replay_scrub};
pub use regime_labels::{
    STABILITY_BAND, QuietLabel, display_label, label_series, parity_label, regime_style_key,
    stability_band,
};
pub use session::{break_lines, time_grid_lines};
pub use topology::{
    ADAPTIVE_NODE, HEALTH_MA_NODE, HEALTH_RIBBON_NODE, OSC_FAST_NODE, OSC_SLOW_NODE,
    PRICE_NODE, RIBBON_NODE, SIGNAL_NODE, VOLUME_NODE, cache_key_for, kernel_order,
    standard_plan, standard_replay,
};
pub use widgets::{
    INDICATOR_SEARCH_LABEL, MtfRow, SYMBOL_SEARCH_LABEL, TOP_BAR_CHART_TYPES, TopBarAction,
    chart_type_label, mtf_table_shape, paint_mtf_table, paint_timeframe_toolbar, paint_toolbar,
    paint_top_bar, timeframe_labels, toolbar_labels,
};
pub use workspace::{AssetClass, ChartWorkspace, build_symbol_catalog, paint_workspace};

/// Terminal marker; touches the linked paint dependency so wiring breaks
/// loudly if the dependency stops resolving.
#[must_use]
pub fn is_terminal_edge() -> bool {
    let _ = egui::Color32::WHITE;
    true
}

#[cfg(test)]
#[path = "../tests/lib.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/budget.rs"]
mod budget_tests;

#[cfg(test)]
#[path = "../tests/gateway.rs"]
mod gateway_tests;
