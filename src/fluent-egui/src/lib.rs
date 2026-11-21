#![forbid(unsafe_code)]

//! Generic paint scaffolding over headless display data.
//!
//! This crate owns the display-side machinery every native chart UI needs —
//! pane view-models, DAG-ordered compute plans, fingerprint-keyed series
//! caches, layout/theme control-plane components, headless paint geometry
//! plus thin `egui` emitters, supervised feed/wave plumbing, and replay
//! cursors — composed from the workspace primitives (`fluent-wvr`,
//! `fluent-concurrency`, `fluent-dag`, `common-core`) with no
//! domain-crate dependencies at all. Chart-domain geometry (bars to pixels
//! through a viewport, annotations, crosshairs, interaction) lives in the
//! terminal crate built on top.
//!
//! The numeric boundary is one-way: `Decimal` values become display `f64`
//! upstream of this crate, and no `f64` held here is ever parsed back into
//! a decision. Non-finite display legs are skipped and counted, never
//! substituted.
//!
//! Paint-edge confinement: `egui` (and the optional `eframe` demo shell) may
//! be depended on only by this crate and the terminal crate. No trading or
//! headless chart crate may gain a paint dependency in either direction of
//! the decision path.

pub mod config;
pub mod dialog;
pub mod error;
pub mod feed;
pub mod fields;
pub mod fonts;
pub mod icons;
pub mod images;
pub mod modal;
pub mod notice;
pub mod paint;
pub mod plan;
pub mod replay;
pub mod slot;
pub mod style;
pub mod table;
pub mod tokens;
pub mod wheel;

pub use config::{load_theme_file, theme_from_json, PaneConfig, ThemeConfig};
pub use dialog::{DialogCommit, DialogOutcome, DialogState};
pub use error::EguiError;
pub use feed::{
    run_wave, submit_rebuild, AsyncBind, BindState, CreditIntake, PaneHandle, PaneWaveUnit,
    RefreshGate, TransportRetry,
};
pub use fields::{
    classify_schema_field, fields_for_tab, paint_schema_fields, paint_tabbed_schema_fields,
    paint_tabbed_schema_fields_compact, project_style_fields, schema_fields, tab_fields,
    FieldEdits, FieldKind, SchemaField, SettingsTab, COLOR_FORMAT,
};
pub use fonts::{
    bundled_font_insert, font_insert, install_bundled_font, monospace_font_insert,
    primary_with_fallback_insert, proportional_font_insert, with_bundled_font,
};
pub use icons::{IconCache, IconKey};
pub use images::{
    net_fetch, placeholder_rect, validate_image_bytes, ImageError, ImageFetch, ImageLoader,
};
pub use modal::{paint_catalog_modal, CatalogAction, CatalogEntry, CatalogModal, TagFilter};
pub use notice::{offsets, Notice, NoticeCenter};
pub use paint::{
    alpha_u8, area_vertex_color, candle_body_width, column_runs, disabled_content, disabled_outline,
    eye_icon_geom, hex_bolt_icon_geom, needs_column_aggregation, opacity_from_transparency,
    paint_area_fill, paint_candles, paint_candles_as, paint_candles_tinted, paint_crosshair,
    paint_delta_bars, paint_delta_bars_styled, paint_delta_bars_tinted, paint_error_chip,
    paint_eye_icon, paint_hex_bolt_icon, paint_oscillator, paint_oscillator_styled, paint_ribbon,
    paint_ribbon_fill, paint_search_icon, parse_hex_color, resolve_color, ribbon_fill_style_key,
    ribbon_style_key, sample_column_extremes, sample_column_last, search_icon_geom, state_layer,
    translate_bands, translate_candles, translate_delta_bars, translate_points, translate_polygons,
    CandleGeom, CrosshairLines, DeltaBarGeom, EyeIconGeom, HexBoltIconGeom, RibbonBand,
    RibbonPolygon, SearchIconGeom, TintedCandle, TintedDeltaBar, ACTION_PILL_PX,
    CANDLE_BODY_FRACTION, CANDLE_GAP_THRESHOLD_PX, CANDLE_MIN_BODY_PX, HOVER_FRAC, MODAL_CORNER_PX,
    PRESS_FRAC,
};
pub use plan::{cache_key, CachedSeries, PaneCache, PanePlan};
pub use replay::{ReplayController, ReplayState};
pub use slot::{
    badge_dot, paint_hot_slot_stack, paint_slot_settings_modal, seed_fields, seed_pending, HotSlot,
    HotSlotAction, HotSlotStack, PendingSettings, ReadoutValue, SettingsCommit, SettingsOutcome,
    SlotAttachment, SlotControlHit, SlotControlRect, StackDensity, StagedFields, STACK_CARD_GAP,
    STACK_CARD_HEIGHT, STACK_TOUCH_MIN_PX,
};
pub use style::IndicatorStyle;
pub use style::{style_key, LINE_WIDTH_FIELD, MAX_LINE_WIDTH, MIN_LINE_WIDTH, STYLE_FIELDS};
pub use table::{ColWidth, SortDirection, TablePlan};
pub use tokens::{apply_visuals, chart_roles, known_roles, MaterialTokens, ResolvedScheme};
pub use wheel::{
    flip_within, menu_anchor, readout_anchor, PaneZones, Pos2 as WheelPos2, Rect as WheelRect,
    WheelTarget, WheelZones,
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
