//! Backtest chart overlay: trade markers, resting lines, equity pane.
//!
//! A stored run arrives as view DTOs (timestamps, decimal-text prices); the
//! overlay densifies them into per-bar series with `None` holes wherever a
//! bar carries no point, so every line breaks exactly where the run was
//! flat — the reference's line-break behaviour, never a bridged gap. The
//! run data enters at construction time (immutable evaluation input, like
//! the chart rows): the slot's configuration carries only the run id, and
//! the plan fingerprint follows it, so a new run always recomputes and a
//! configuration that names a different run than the data paints nothing
//! rather than a mislabelled run.

use std::collections::HashMap;

use fluent_egui::{
    cache_key, paint_markers, paint_oscillator_styled, sampled_line_points, translate_points,
    EguiError, MarkerDot, ReadoutValue,
};
use fluent_wvr::prelude::*;
use fluent_wvr::validate::{check_for_execute, valid_output, Validate};
use fluent_wvr::{component_downcast_ref, Describable, FieldAccess};
use protopyne_chart_model::{AnnotationStyle, ChartRow};

use crate::indicators::{ChartIndicator, IndicatorEvaluation, IndicatorSeries};

/// Slot id of the trade marker and resting-line overlay (main canvas).
pub const BACKTEST_OVERLAY_ID: &str = "backtest_overlay";
/// Slot id of the equity curve (own sub-pane).
pub const BACKTEST_EQUITY_ID: &str = "backtest_equity";
/// Custom pane the equity curve owns (auto-ranged to equity, never the
/// shared oscillator band).
pub const BACKTEST_EQUITY_PANE: &str = "backtest:equity";

/// One plotted run point: a bar timestamp plus a display price.
#[derive(Debug, Clone, Copy, PartialEq)]
/// One plotted run point: a bar timestamp plus a display price.
pub struct BacktestPoint {
    /// Bar timestamp in seconds, matching [`ChartRow::time`].
    pub at: i64,
    /// Display price (never parsed back into money).
    pub price: f64,
    /// Which series this point belongs to.
    pub kind: BacktestPointKind,
}

/// Which overlay series a point belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Which overlay series a point belongs to.
pub enum BacktestPointKind {
    /// Long/short entry marker.
    Entry,
    /// Exit marker (full close or partial slice).
    Exit,
    /// Resting stop while a position is open.
    Stop,
    /// Resting target while its zone is unfilled.
    Target,
    /// Marked equity (own pane).
    Equity,
}

/// The slot configuration: the stored run id this overlay paints. The run
/// data itself rides the plugin (construction-time evaluation input); the
/// id here keys the plan fingerprint and lets `evaluate` refuse a run it
/// was not built for rather than painting a mislabelled run.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, FieldAccess, Describable)]
/// The slot configuration: the stored run id this overlay paints. The run
/// data itself rides the plugin (construction-time evaluation input); the
/// id here keys the plan fingerprint and lets `evaluate` refuse a run it
/// was not built for rather than painting a mislabelled run.
pub struct BacktestOverlayConfig {
    /// Stored run id this overlay paints.
    #[field(desc = "Stored run id this overlay paints")]
    pub run_id: String,
}

impl BacktestOverlayConfig {
    /// Fail-closed self-check: an overlay without a run paints nothing.
    pub fn validate(&self) -> Result<(), EguiError> {
        if self.run_id.trim().is_empty() {
            return Err(EguiError::InvalidParam {
                what: "run_id".to_owned(),
                reason: "an overlay without a run paints nothing".to_owned(),
            });
        }
        Ok(())
    }
}

impl Validate for BacktestOverlayConfig {
    type Err = EguiError;
    fn validate(&self) -> Result<(), Self::Err> {
        BacktestOverlayConfig::validate(self)
    }
}

impl WorkUnit for BacktestOverlayConfig {
    fn name(&self) -> &str {
        "backtest_overlay_config"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        check_for_execute(self, "backtest overlay config invalid")?;
        Ok(valid_output(format!(
            "backtest overlay ready ({})",
            self.run_id
        )))
    }
}

impl_component!(BacktestOverlayConfig);

/// Bar timestamps to row indices, binding nothing on duplicates: a repeated
/// timestamp never resolves, exactly like the shared time index, so a
/// doubled bar cannot misplace a run marker.
fn bar_index_of(rows: &[ChartRow]) -> HashMap<i64, usize> {
    let mut counts: HashMap<i64, usize> = HashMap::new();
    for row in rows {
        *counts.entry(row.time).or_default() += 1;
    }
    let mut index_of = HashMap::new();
    for (index, row) in rows.iter().enumerate() {
        if counts.get(&row.time) == Some(&1) {
            index_of.insert(row.time, index);
        }
    }
    index_of
}

/// Marker and resting-line overlay over the main canvas.
pub struct BacktestOverlay {
    run_id: String,
    points: Vec<BacktestPoint>,
}

/// Equity curve over its own viewport.
pub struct EquityPane {
    run_id: String,
    points: Vec<BacktestPoint>,
}

fn parse_point(at: &str, price: &str, kind: BacktestPointKind) -> Option<BacktestPoint> {
    let at = at
        .parse::<chrono::DateTime<chrono::Utc>>()
        .ok()?
        .timestamp();
    let price: f64 = price.parse().ok()?;
    if !price.is_finite() {
        return None;
    }
    Some(BacktestPoint { at, price, kind })
}

/// Lift one stored run view into overlay points: entry/exit markers from
/// the trades, stop/target points from the resting levels, equity from the
/// curve. Unparseable rows are dropped (never fabricated); the series keep
/// their holes.
#[must_use]
/// Lift one stored run view into overlay points: entry/exit markers from
/// the trades, stop/target points from the resting levels, equity from the
/// curve. Unparseable rows are dropped (never fabricated); the series keep
/// their holes.
pub fn backtest_points_of(view: &protopyne_view::BacktestRunView) -> Vec<BacktestPoint> {
    let mut points = Vec::new();
    for trade in &view.trades {
        if let Some(point) = parse_point(
            &trade.entry_at,
            &trade.entry_price,
            BacktestPointKind::Entry,
        ) {
            points.push(point);
        }
        if let Some(point) = parse_point(&trade.exit_at, &trade.exit_price, BacktestPointKind::Exit)
        {
            points.push(point);
        }
    }
    for marker in &view.markers {
        let kind = match marker.kind.as_str() {
            "stop" => BacktestPointKind::Stop,
            "target" => BacktestPointKind::Target,
            // Entries and exits already ride the trades above; anything else
            // is not an overlay input.
            _ => continue,
        };
        if let Some(point) = parse_point(&marker.at, &marker.price, kind) {
            points.push(point);
        }
    }
    for point in &view.equity {
        if let Some(point) = parse_point(&point.at, &point.equity, BacktestPointKind::Equity) {
            points.push(point);
        }
    }
    points
}

impl BacktestOverlay {
    /// Build the overlay for one stored run: markers plus resting lines.
    #[must_use]
    pub fn with_run(
        run_id: &str,
        view: &protopyne_view::BacktestRunView,
    ) -> (Self, BacktestOverlayConfig) {
        let points: Vec<BacktestPoint> = backtest_points_of(view)
            .into_iter()
            .filter(|point| point.kind != BacktestPointKind::Equity)
            .collect();
        (
            Self {
                run_id: run_id.to_owned(),
                points,
            },
            BacktestOverlayConfig {
                run_id: run_id.to_owned(),
            },
        )
    }

    fn series_for(
        &self,
        rows: &[ChartRow],
        kind: BacktestPointKind,
        node: &str,
    ) -> IndicatorSeries {
        let index_of = bar_index_of(rows);
        let mut values = vec![None; rows.len()];
        for point in &self.points {
            if point.kind != kind {
                continue;
            }
            if let Some(&index) = index_of.get(&point.at) {
                values[index] = Some(point.price);
            }
        }
        let fingerprint = common_core::hash::fnv1a64(self.run_id.as_bytes());
        IndicatorSeries {
            node: node.to_owned(),
            name: node.trim_start_matches("backtest:").to_owned(),
            display_values: values.clone(),
            display_mapping: None,
            values,
            style: "fast".to_owned(),
            attachment: fluent_egui::SlotAttachment::MainOverlay,
            key: cache_key(node, fingerprint),
            pane: None,
        }
    }
}

impl EquityPane {
    /// Build the equity pane for one stored run.
    #[must_use]
    pub fn with_run(
        run_id: &str,
        view: &protopyne_view::BacktestRunView,
    ) -> (Self, BacktestOverlayConfig) {
        let points: Vec<BacktestPoint> = backtest_points_of(view)
            .into_iter()
            .filter(|point| point.kind == BacktestPointKind::Equity)
            .collect();
        (
            Self {
                run_id: run_id.to_owned(),
                points,
            },
            BacktestOverlayConfig {
                run_id: run_id.to_owned(),
            },
        )
    }
}

fn overlay_config_of(
    owner: &str,
    run_id: &str,
    config: &dyn fluent_wvr::Component,
) -> Result<(), EguiError> {
    let config = component_downcast_ref::<BacktestOverlayConfig>(config).ok_or_else(|| {
        EguiError::InvalidParam {
            what: "config".to_owned(),
            reason: format!("{owner} expects a backtest overlay configuration"),
        }
    })?;
    if config.run_id != run_id {
        return Err(EguiError::InvalidParam {
            what: "run_id".to_owned(),
            reason: "the slot names a different run than the overlay data".to_owned(),
        });
    }
    Ok(())
}

const OVERLAY_NODES: [&str; 4] = [
    "backtest:entry",
    "backtest:exit",
    "backtest:stop",
    "backtest:target",
];

impl ChartIndicator for BacktestOverlay {
    fn id(&self) -> &str {
        BACKTEST_OVERLAY_ID
    }

    fn title(&self) -> &str {
        "Backtest Overlay"
    }

    fn attachment(&self) -> fluent_egui::SlotAttachment {
        fluent_egui::SlotAttachment::MainOverlay
    }

    fn default_config(&self) -> Box<dyn fluent_wvr::Component> {
        Box::new(BacktestOverlayConfig {
            run_id: self.run_id.clone(),
        })
    }

    fn plan_nodes(&self, _config: &dyn fluent_wvr::Component) -> Result<Vec<String>, EguiError> {
        Ok(OVERLAY_NODES
            .iter()
            .map(|node| (*node).to_owned())
            .collect())
    }

    fn config_key(&self, _config: &dyn fluent_wvr::Component) -> Result<u64, EguiError> {
        Ok(common_core::hash::fnv1a64(self.run_id.as_bytes()))
    }

    fn evaluate(
        &self,
        rows: &[ChartRow],
        config: &dyn fluent_wvr::Component,
        _series: &crate::topology::SeriesCatalog,
    ) -> Result<IndicatorEvaluation, EguiError> {
        // A slot that names a different run than this overlay's data paints
        // nothing: a mislabelled run is worse than a blank one.
        if overlay_config_of(self.id(), &self.run_id, config).is_err() {
            return Ok(IndicatorEvaluation {
                series: Vec::new(),
                overlay: Ok(Vec::new()),
            });
        }
        let kinds = [
            (BacktestPointKind::Entry, OVERLAY_NODES[0]),
            (BacktestPointKind::Exit, OVERLAY_NODES[1]),
            (BacktestPointKind::Stop, OVERLAY_NODES[2]),
            (BacktestPointKind::Target, OVERLAY_NODES[3]),
        ];
        Ok(IndicatorEvaluation {
            series: kinds
                .iter()
                .map(|(kind, node)| self.series_for(rows, *kind, node))
                .collect(),
            overlay: Ok(Vec::new()),
        })
    }

    fn readouts(&self, _evaluation: &IndicatorEvaluation) -> Vec<ReadoutValue> {
        Vec::new()
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_main_overlay(
        &self,
        painter: &egui::Painter,
        evaluation: &IndicatorEvaluation,
        viewport: &fluent_egui::Viewport,
        origin: egui::Pos2,
        base: usize,
        _width_px: f32,
        theme: &fluent_egui::ThemeConfig,
        _config: &dyn fluent_wvr::Component,
        style: &fluent_egui::IndicatorStyle,
    ) -> Result<(), EguiError> {
        let series_of = |node: &str| evaluation.series.iter().find(|series| series.node == node);
        // Entry/exit markers: one dot per armed bar, Bear for exits.
        for (node, dot_style) in [
            ("backtest:entry", AnnotationStyle::Bull),
            ("backtest:exit", AnnotationStyle::Bear),
        ] {
            if let Some(series) = series_of(node) {
                let mut dots = Vec::new();
                for (offset, value) in series.values.iter().enumerate() {
                    let Some(price) = value else { continue };
                    let Some(y) = viewport.windowed_y(*price) else {
                        continue;
                    };
                    dots.push(MarkerDot {
                        index: base + offset,
                        x: viewport.index_to_x(base + offset),
                        y,
                        style: dot_style,
                    });
                }
                paint_markers(painter, &dots, theme)?;
            }
        }
        // Resting lines: one polyline per kind, breaking wherever a bar
        // carries no point for it.
        for (node, line_style) in [("backtest:stop", "bear"), ("backtest:target", "bull")] {
            if let Some(series) = series_of(node) {
                let placed = translate_points(
                    sampled_line_points(&series.values, viewport, base),
                    (origin.x, origin.y),
                );
                paint_oscillator_styled(painter, &placed, line_style, theme, style, 1.0)?;
            }
        }
        Ok(())
    }
}

impl ChartIndicator for EquityPane {
    fn id(&self) -> &str {
        BACKTEST_EQUITY_ID
    }

    fn title(&self) -> &str {
        "Backtest Equity"
    }

    fn attachment(&self) -> fluent_egui::SlotAttachment {
        fluent_egui::SlotAttachment::SubPane
    }

    fn default_config(&self) -> Box<dyn fluent_wvr::Component> {
        Box::new(BacktestOverlayConfig {
            run_id: self.run_id.clone(),
        })
    }

    fn plan_nodes(&self, _config: &dyn fluent_wvr::Component) -> Result<Vec<String>, EguiError> {
        Ok(vec!["backtest:equity".to_owned()])
    }

    fn config_key(&self, _config: &dyn fluent_wvr::Component) -> Result<u64, EguiError> {
        Ok(common_core::hash::fnv1a64(self.run_id.as_bytes()))
    }

    fn evaluate(
        &self,
        rows: &[ChartRow],
        config: &dyn fluent_wvr::Component,
        _series: &crate::topology::SeriesCatalog,
    ) -> Result<IndicatorEvaluation, EguiError> {
        if overlay_config_of(self.id(), &self.run_id, config).is_err() {
            return Ok(IndicatorEvaluation {
                series: Vec::new(),
                overlay: Ok(Vec::new()),
            });
        }
        let index_of = bar_index_of(rows);
        let mut values = vec![None; rows.len()];
        for point in &self.points {
            if let Some(&index) = index_of.get(&point.at) {
                values[index] = Some(point.price);
            }
        }
        let fingerprint = common_core::hash::fnv1a64(self.run_id.as_bytes());
        Ok(IndicatorEvaluation {
            series: vec![IndicatorSeries {
                node: "backtest:equity".to_owned(),
                name: "equity".to_owned(),
                display_values: values.clone(),
                display_mapping: None,
                values,
                style: "fast".to_owned(),
                attachment: fluent_egui::SlotAttachment::SubPane,
                key: cache_key("backtest:equity", fingerprint),
                pane: Some(BACKTEST_EQUITY_PANE.to_owned()),
            }],
            overlay: Ok(Vec::new()),
        })
    }

    fn readouts(&self, _evaluation: &IndicatorEvaluation) -> Vec<ReadoutValue> {
        Vec::new()
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_sub_pane(
        &self,
        painter: &egui::Painter,
        evaluation: &IndicatorEvaluation,
        viewport: &fluent_egui::Viewport,
        origin: egui::Pos2,
        base: usize,
        _width_px: f32,
        theme: &fluent_egui::ThemeConfig,
        _config: &dyn fluent_wvr::Component,
        style: &fluent_egui::IndicatorStyle,
    ) -> Result<(), EguiError> {
        if let Some(series) = evaluation
            .series
            .iter()
            .find(|series| series.node == "backtest:equity")
        {
            let placed = translate_points(
                sampled_line_points(&series.values, viewport, base),
                (origin.x, origin.y),
            );
            paint_oscillator_styled(painter, &placed, "fast", theme, style, 1.0)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/overlay.rs"]
mod tests;