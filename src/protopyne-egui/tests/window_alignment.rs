use super::*;
use crate::chart::TimeframeKey;
use protopyne_chart_model::{ChartRow, DateRange};
use protopyne_view::{
    ArmResponse, ChartBar, ChartBars, GatewayError, HealthView, JournalRowView, OverlaySeries,
    PortfolioView, ProposalView, ReplayView, WorstCase,
};
use std::collections::BTreeMap;

fn h1_rows(count: usize) -> Vec<ChartRow> {
    let base = 1_700_000_000_i64;
    (0..count)
        .map(|i| {
            let close = 100.0 + i as f64 * 0.01;
            ChartRow {
                time: base + i as i64 * 3_600,
                open: close - 0.25,
                high: close + 0.5,
                low: close - 0.5,
                close,
                volume: 10,
            }
        })
        .collect()
}

struct LongGateway {
    rows: Vec<ChartRow>,
}

impl LongGateway {
    fn new(count: usize) -> Self {
        Self {
            rows: h1_rows(count),
        }
    }
}

fn unavailable() -> GatewayError {
    GatewayError::Unavailable("long gateway".to_owned())
}

impl protopyne_view::TerminalGateway for LongGateway {
    fn health(&self) -> Result<HealthView, GatewayError> {
        Err(unavailable())
    }
    fn proposals(&self) -> Result<Vec<ProposalView>, GatewayError> {
        Err(unavailable())
    }
    fn portfolio(&self) -> Result<PortfolioView, GatewayError> {
        Err(unavailable())
    }
    fn journal(&self) -> Result<Vec<JournalRowView>, GatewayError> {
        Err(unavailable())
    }
    fn chart_bars(&self) -> Result<ChartBars, GatewayError> {
        Ok(ChartBars {
            rows: self
                .rows
                .iter()
                .map(|row| ChartBar {
                    time: row.time,
                    open: row.open,
                    high: row.high,
                    low: row.low,
                    close: row.close,
                    volume: row.volume,
                })
                .collect(),
            dropped: 0,
        })
    }
    fn chart_overlays(&self, _kind: &str, _period: i64) -> Result<OverlaySeries, GatewayError> {
        Err(unavailable())
    }
    fn replay(&self) -> Result<ReplayView, GatewayError> {
        Err(unavailable())
    }
    fn preview_arm(&self, _patch: serde_json::Value) -> Result<WorstCase, GatewayError> {
        Err(unavailable())
    }
    fn grant_tiers(&self) -> Result<BTreeMap<String, String>, GatewayError> {
        Err(unavailable())
    }
    fn arm(&self, _grant: serde_json::Value) -> Result<ArmResponse, GatewayError> {
        Err(unavailable())
    }
}

fn boot_h1_long() -> ChartWorkspace {
    let gateway = LongGateway::new(5_000);
    let mut workspace =
        ChartWorkspace::with_gateway(&gateway).expect("gateway boots the workspace");
    workspace
        .switch_timeframe(TimeframeKey::H1, &gateway)
        .expect("H1 switch pulls the long history");
    workspace
}

#[test]
fn trailing_tail_stays_visible_when_history_exceeds_window() {
    let workspace = boot_h1_long();
    let rows = workspace.rows();
    assert_eq!(rows.len(), 5_000, "the long history loads");
    let window = crate::chart::window_for_range(rows, DateRange::Month3, 3_600);
    assert_eq!(window.len(), 2_160, "Month3 at H1 needs 2160 bars");
    let window_start = rows.len() - window.len();
    assert!(window_start > 0, "the painted window starts mid-history");
    let main = workspace.main_viewport().expect("viewports bind on boot");
    let (start, end) = main.culled_bar_range(640.0, rows.len());
    assert!(end > start, "the culled tail is non-empty");
    assert_eq!(end, rows.len(), "the culled tail reaches the newest bar");
}

#[test]
fn newest_bar_maps_inside_plot_without_panning() {
    let workspace = boot_h1_long();
    let rows = workspace.rows();
    let main = workspace.main_viewport().expect("viewports bind on boot");
    let x = main.index_to_x(rows.len() - 1);
    assert!(
        (0.0..=640.0).contains(&x),
        "newest bar maps inside the plot without panning: {x}"
    );
}

#[test]
fn zero_offset_window_slice_matches_full_rows() {
    let rows = h1_rows(100);
    // Month3 at H1 needs 2160 bars, so 100 rows sit wholly inside the preset.
    let window = crate::chart::window_for_range(&rows, DateRange::Month3, 3_600);
    assert_eq!(window.len(), rows.len(), "the window covers the history");
    let viewport = fluent_egui::Viewport::new(
        protopyne_chart_scale::PriceScaleMode::Normal,
        90.0,
        120.0,
        400.0,
        8.0,
        100.0,
        false,
    )
    .expect("viewport builds")
    .with_x_offset(640.0 - rows.len() as f32 * 8.0)
    .expect("offset applies");
    let trend = vec![true; rows.len()];
    let full = fluent_egui::column_candle_geoms(&rows, &trend, &viewport, 640.0)
        .expect("full-history geoms build");
    let sliced = fluent_egui::column_candle_geoms(window, &trend, &viewport, 640.0)
        .expect("window-slice geoms build");
    assert!(!full.is_empty(), "the anchored tail paints");
    assert_eq!(full, sliced, "zero offset: both spellings agree");
    for geom in &full {
        assert_eq!(
            geom.x.to_bits(),
            viewport.index_to_x(geom.index).to_bits(),
            "slots ride the shared mapping"
        );
    }
}

#[test]
fn anchor_roundtrip_keeps_newest_time() {
    let mut workspace = boot_h1_long();
    let last = workspace
        .rows()
        .last()
        .map(|row| row.time)
        .expect("rows load");
    let anchor = workspace
        .rightmost_time(640.0)
        .expect("a bound viewport answers a rightmost time");
    assert_eq!(anchor, last, "the rightmost time is the newest bar");
    workspace
        .restore_anchor(Some(anchor), 640.0)
        .expect("anchor restores");
    let again = workspace
        .rightmost_time(640.0)
        .expect("a bound viewport answers again");
    assert_eq!(again, last, "the roundtrip keeps the newest time");
}
