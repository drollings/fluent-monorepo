//! Native Delta Volume Profile: binning, POC, value area, VWAP, the plugin's
//! per-bar primitives, and a headless overlay paint.

use super::*;
use crate::indicators::ChartIndicator;
use protopyne_chart_scale::PriceScaleMode;

fn bar(high: f64, low: f64, close: f64, volume: f64) -> ProfileBar {
    ProfileBar { high, low, close, volume, buy: volume * 0.5 }
}

fn row(open: f64, high: f64, low: f64, close: f64, volume: u64) -> ChartRow {
    ChartRow { time: 0, open, high, low, close, volume }
}

#[test]
fn build_profile_bins_by_price_and_finds_the_poc() {
    let bars = [
        bar(101.0, 100.0, 100.5, 10.0),
        bar(102.0, 101.0, 101.5, 30.0),
        bar(103.0, 102.0, 102.5, 20.0),
    ];
    let profile = build_profile(&bars, 3, 0.7);
    assert_eq!(profile.bins.len(), 3);
    let total: f64 = profile.bins.iter().map(|bin| bin.volume).sum();
    assert!((total - 60.0).abs() < 1e-6, "all volume is distributed: {total}");
    let poc = profile.poc.expect("a non-empty profile has a POC");
    assert!(
        (profile.bins[poc].volume - profile.max_volume).abs() < 1e-9,
        "the POC holds the peak bin"
    );
    assert!(profile.vah.expect("value area high") >= profile.poc_price().expect("poc price"));
    assert!(profile.val.expect("value area low") <= profile.poc_price().expect("poc price"));
    assert!(profile.vwap.expect("vwap").is_finite());
}

#[test]
fn build_profile_refuses_degenerate_inputs_without_guessing() {
    assert!(build_profile(&[], 10, 0.7).bins.is_empty(), "no bars, no profile");
    assert!(build_profile(&[bar(100.0, 100.0, 100.0, 5.0)], 0, 0.7).bins.is_empty(), "no bins");
    assert!(
        build_profile(&[bar(100.0, 100.0, 100.0, 5.0)], 10, 0.7).bins.is_empty(),
        "a flat range has no profile"
    );
    assert!(
        build_profile(&[bar(101.0, 100.0, 100.5, 10.0)], 10, f64::NAN).bins.is_empty(),
        "a non-finite value-area share refuses"
    );
}

#[test]
fn config_refuses_values_outside_its_declared_bounds() {
    let mut config = DeltaVolumeProfileConfig::default();
    assert!(config.validate().is_ok(), "defaults validate");
    config.bins = 4;
    assert!(config.validate().is_err(), "too few bins refuse");
    config.bins = 60;
    config.value_area_pct = 1.5;
    assert!(config.validate().is_err(), "an out-of-band value area refuses");
    config.value_area_pct = 0.7;
    config.lookback = 1;
    assert!(config.validate().is_err(), "a one-bar lookback refuses");
}

#[test]
fn profile_series_are_holes_before_the_lookback_start() {
    let rows: Vec<ChartRow> =
        (0..20_usize).map(|i| row(100.0 + i as f64, 101.0 + i as f64, 99.0 + i as f64, 100.5 + i as f64, 10)).collect();
    let config = DeltaVolumeProfileConfig::new().lookback(10).build();
    let series = profile_series(&rows, &config).expect("series build");
    assert_eq!(series.len(), PROFILE_NODES.len());
    assert!(series[0][0].is_none(), "a bar before the lookback start is a hole");
    assert!(series[0][19].is_some(), "a bar inside the lookback reads");
}

#[test]
fn plugin_evaluates_reads_out_and_paints_headlessly() {
    let rows: Vec<ChartRow> = (0..60_usize)
        .map(|i| {
            let base = 100.0 + (i as f64 * 0.2).sin() * 2.0;
            row(base, base + 1.0, base - 1.0, base + 0.3, 1_000 + i as u64 * 5)
        })
        .collect();
    let plugin = DeltaVolumeProfileIndicator::new();
    assert_eq!(plugin.id(), DELTA_VOLUME_PROFILE_ID);
    assert_eq!(plugin.attachment(), SlotAttachment::MainOverlay);
    let config = plugin.default_config();
    let evaluation = plugin.evaluate(&rows, config.as_ref()).expect("the plugin evaluates");
    assert_eq!(evaluation.series.len(), PROFILE_NODES.len());
    let readouts = plugin.readouts(&evaluation);
    assert!(readouts.iter().any(|readout| readout.label == "POC"), "the POC reads out");
    assert!(
        readouts.iter().any(|readout| readout.label == "VWAP" && readout.value.is_some()),
        "the VWAP reads out"
    );
    let viewport =
        Viewport::new(PriceScaleMode::Normal, 90.0, 110.0, 300.0, 8.0, 100.0, false)
            .expect("valid viewport");
    egui::__run_test_ui(|ui| {
        plugin
            .paint_main_overlay(
                ui.painter(),
                &evaluation,
                &viewport,
                egui::Pos2::ZERO,
                &ThemeConfig::default(),
                config.as_ref(),
            )
            .expect("the overlay paints");
    });
}

#[test]
fn profile_boxes_skip_off_window_bins_and_scale_with_volume() {
    let bars = [
        bar(101.0, 100.0, 100.5, 10.0),
        bar(102.0, 101.0, 101.5, 40.0),
    ];
    let profile = build_profile(&bars, 4, 0.7);
    let viewport =
        Viewport::new(PriceScaleMode::Normal, 95.0, 105.0, 300.0, 8.0, 100.0, false)
            .expect("valid viewport");
    let boxes = profile_boxes(&profile, &viewport, egui::Pos2::ZERO, 10, 30);
    assert!(!boxes.is_empty(), "in-window bins paint");
    assert!(boxes.iter().any(|box_| box_.poc), "the POC bin is marked");
    let widest = boxes.iter().map(|box_| box_.rect.width()).fold(0.0_f32, f32::max);
    let narrowest = boxes.iter().map(|box_| box_.rect.width()).fold(f32::INFINITY, f32::min);
    assert!(widest > narrowest, "a thicker bin paints a wider bar");
}

#[test]
fn catalog_lists_and_instantiates_the_native_profile() {
    let catalog = crate::indicators::build_indicator_catalog().expect("catalog builds");
    let ids: Vec<String> = catalog.filtered().into_iter().map(|entry| entry.id().to_owned()).collect();
    assert!(ids.contains(&DELTA_VOLUME_PROFILE_ID.to_owned()), "the profile is in the catalog");
    let plugin = crate::indicators::instantiate_indicator(DELTA_VOLUME_PROFILE_ID)
        .expect("the profile instantiates");
    assert_eq!(plugin.title(), "Delta Volume Profile");
}
