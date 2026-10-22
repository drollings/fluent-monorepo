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
pub mod fonts;
pub mod grant_form;
pub mod indicators;
pub mod integration_lite;
pub mod oscillators;
pub mod pine_overlay;
pub mod prefs;
pub mod regime_labels;
pub mod replay;
pub mod session;
pub mod topology;
pub mod views;
pub mod widgets;
pub mod workspace;

pub use annotations::{annotations_of, annotations_of_counted, append_new};
pub use chart::{
    area_view, chart_price_bars, default_symbol, line_values, picture_rows, picture_source,
    prefold_levels, prefold_pick, prefold_pick_hysteretic, refuse_chart_type,
    resolve_prefold_level, timed_plan_build, window_for_range, AreaView, ChartConfig,
    PictureSource, TimeframeKey, DEFAULT_SYMBOL, PREFOLD_ENABLED, PREFOLD_HYSTERESIS_PX,
    PREFOLD_TARGET_PX_PER_BAR,
};
pub use export::{export_overlay_json, export_rows_csv, export_rows_json, EXPORT_CSV_HEADER};
pub use fast_mas::{evaluate_fast_mas, paint_fast_mas, FastMasConfig, FastMasLine, FastMasView};
pub use feed::{RecomputeJob, RecomputePool};
pub use fonts::{install_noto_sans, NOTO_SANS_MONO_NAME, NOTO_SANS_NAME};
pub use grant_form::{paint_grant_form, GrantFormError, GrantFormField, GrantFormState};
pub use indicators::{
    build_indicator_catalog, build_indicator_catalog_with_manifest, instantiate_indicator,
    instantiate_indicator_with_manifest, kernel_attachment, kernel_ids, kernel_title,
    ChartIndicator, FastMasIndicator, IndicatorCategory, IndicatorEvaluation, IndicatorSeries,
    IntegrationLiteIndicator, KernelIndicator, KernelIndicatorConfig,
};
pub use integration_lite::{
    candle_momentum_keys, candle_momentum_series, compare_freeze, integration_lite_sub_pane,
    integration_lite_view, integration_lite_view_calibrated, momentum_key_for, momentum_tint,
    momentum_tints, native_lite_series, oscillator_transparency, precision_recall,
    ribbon_fill_transparency, series_max_diff, CandleMomentum, FreezePair, FreezeReport,
    IntegrationLiteConfig, IntegrationLiteSubPane, IntegrationLiteView, LiteFrozen, LiteNative,
    MomentumPalette, MomentumTint, ATR_LENGTH, BOUNDED_DELTA_UNIT, CALIBRATED_PIVOT_THRESHOLD,
    MA_HEALTH_K, OSCILLATOR_BRIGHTNESS, RAW_OSCILLATOR_LENGTH, RIBBON_HEALTH_K, RIBBON_MEAN_LENGTH,
    SIGNAL_EIT_ALPHA, VOLUME_TRANSPARENCY, VOL_SMA_LENGTH,
};
pub use oscillators::{
    evaluable_oscillator_ids, normalize_oscillator, normalize_oscillator_bounded, normalize_series,
    normalize_series_bounded, oscillator_kind_name, oscillator_options, parse_oscillator_kind,
    squash_series_to_100, squash_to_100, OscParams, OscillatorKind, OscillatorOption, WhichLeg,
    BOUNDED_OSC_SCALE, CALIBRATED_BOUNDED_PIVOT_THRESHOLD, SQUASH_100_DIVISOR,
};
pub use pine_overlay::{
    overlay_geoms, paint_overlay, paint_unsupported_chip, translate_overlay, OverlayGeoms,
    PlacedLabel, SeriesGeom,
};
pub use prefs::{
    prefs_path, BookmarkEntry, Favorites, SlotPrefs, TerminalPrefs, TickerLayout, PREFS_FILE,
};
pub use protopyne_view::AssetClass;
pub use regime_labels::{
    display_label, label_series, parity_label, regime_style_key, stability_band, QuietLabel,
    STABILITY_BAND,
};
pub use replay::{paint_replay_scrub, ReplayScrub, ScrubOutcome, ScrubRefusal};
pub use session::{break_lines, break_lines_times, time_grid_lines, SessionBreakCache};
pub use topology::{
    cache_key_for, evaluation_window, kernel_order, kernel_warmup, standard_plan, standard_replay,
    ADAPTIVE_NODE, HEALTH_MA_NODE, HEALTH_RIBBON_NODE, OSC_FAST_NODE, OSC_SLOW_NODE, PRICE_NODE,
    RIBBON_NODE, SIGNAL_NODE, VOLUME_NODE,
};
pub use widgets::{
    chart_type_label, mtf_table_shape, paint_mtf_table, paint_range_dialog,
    paint_timeframe_toolbar, paint_top_bar, timeframe_labels, toolbar_labels, CalendarState,
    MtfRow, PlayPressState, RangeChoice, ReplaySpeed, TopBarAction, TopBarMenuState,
    BOOKMARK_SEARCH_LABEL, INDICATOR_SEARCH_LABEL, PLAY_LONG_PRESS_SECS, PULLDOWN_CHEVRON,
    SYMBOL_SEARCH_LABEL, TOP_BAR_CHART_TYPES,
};
pub use workspace::{
    build_symbol_catalog, live_update_of_frame, paint_workspace, symbol_modal_for, ChartWorkspace,
};

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

#[cfg(test)]
#[path = "../tests/demo_shell.rs"]
mod demo_shell_tests;
