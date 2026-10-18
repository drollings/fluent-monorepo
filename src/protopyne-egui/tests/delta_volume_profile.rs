//! Native Delta Volume Profile: binning, POC, value area, VWAP, the plugin's
//! per-bar primitives, and a headless overlay paint.

use super::*;
use crate::indicators::ChartIndicator;
use crate::topology::SeriesCatalog;
use fluent_egui::IndicatorStyle;
use protopyne_chart_scale::PriceScaleMode;

/// The endpoints the ported gradient interpolates between: pure red on the
/// sell end and pure green on the buy end, so a channel comparison reads the
/// polarity unambiguously.
fn palette() -> BinPalette {
    BinPalette {
        support: [0, 255, 0],
        resistance: [255, 0, 0],
    }
}

/// A bin with an explicit volume and imbalance.
fn bin(volume: f64, imbalance: f64) -> VolumeBin {
    VolumeBin {
        price_low: 0.0,
        price_high: 1.0,
        volume,
        imbalance,
    }
}

/// A one-bin profile around `bin`, whose mean volume is `mean` (the bin's own
/// volume unless the test is exercising the volume-driven shading).
fn one_bin_profile(bin: VolumeBin) -> DeltaVolumeProfile {
    let mean = bin.volume;
    profile_with_mean(bin, mean)
}

/// A one-bin profile whose mean bin volume is held separately from the bin, so
/// the volume-driven shading can be exercised at any ratio.
fn profile_with_mean(bin: VolumeBin, mean: f64) -> DeltaVolumeProfile {
    DeltaVolumeProfile {
        bins: vec![bin],
        poc: Some(0),
        vah: None,
        val: None,
        vwap: None,
        max_volume: bin.volume,
        avg_volume: mean,
    }
}

/// Ratio comparisons are exact by construction (every operand is exactly
/// representable), but a tolerance keeps a failure about the rule rather than
/// about float representation.
fn close_to(actual: f64, expected: f64) -> bool {
    (actual - expected).abs() <= 1e-9 * expected.abs().max(1.0)
}

/// The fill one full bin reads at imbalance `ratio` (in `[-1, +1]`).
fn fill_at(imbalance_ratio: f64) -> BinFill {
    bin_fill(
        &one_bin_profile(bin(100.0, 50.0 * imbalance_ratio)),
        0,
        &palette(),
    )
    .expect("a full bin always fills")
}

fn bar(high: f64, low: f64, close: f64, volume: f64) -> ProfileBar {
    ProfileBar {
        high,
        low,
        close,
        volume,
        buy: volume * 0.5,
    }
}

fn row(open: f64, high: f64, low: f64, close: f64, volume: u64) -> ChartRow {
    ChartRow {
        time: 0,
        open,
        high,
        low,
        close,
        volume,
    }
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
    assert!(
        (total - 60.0).abs() < 1e-6,
        "all volume is distributed: {total}"
    );
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
    assert!(
        build_profile(&[], 10, 0.7).bins.is_empty(),
        "no bars, no profile"
    );
    assert!(
        build_profile(&[bar(100.0, 100.0, 100.0, 5.0)], 0, 0.7)
            .bins
            .is_empty(),
        "no bins"
    );
    assert!(
        build_profile(&[bar(100.0, 100.0, 100.0, 5.0)], 10, 0.7)
            .bins
            .is_empty(),
        "a flat range has no profile"
    );
    assert!(
        build_profile(&[bar(101.0, 100.0, 100.5, 10.0)], 10, f64::NAN)
            .bins
            .is_empty(),
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
    assert!(
        config.validate().is_err(),
        "an out-of-band value area refuses"
    );
    config.value_area_pct = 0.7;
    config.lookback = 1;
    assert!(config.validate().is_err(), "a one-bar lookback refuses");
}

#[test]
fn profile_series_are_holes_before_the_lookback_start() {
    let rows: Vec<ChartRow> = (0..20_usize)
        .map(|i| {
            row(
                100.0 + i as f64,
                101.0 + i as f64,
                99.0 + i as f64,
                100.5 + i as f64,
                10,
            )
        })
        .collect();
    let config = DeltaVolumeProfileConfig::new().lookback(10).build();
    let series = profile_series(&rows, &config).expect("series build");
    assert_eq!(series.len(), PROFILE_NODES.len());
    assert!(
        series[0][0].is_none(),
        "a bar before the lookback start is a hole"
    );
    assert!(series[0][19].is_some(), "a bar inside the lookback reads");
}

#[test]
fn plugin_evaluates_reads_out_and_paints_headlessly() {
    let rows: Vec<ChartRow> = (0..60_usize)
        .map(|i| {
            let base = 100.0 + (i as f64 * 0.2).sin() * 2.0;
            row(
                base,
                base + 1.0,
                base - 1.0,
                base + 0.3,
                1_000 + i as u64 * 5,
            )
        })
        .collect();
    let plugin = DeltaVolumeProfileIndicator::new();
    assert_eq!(plugin.id(), DELTA_VOLUME_PROFILE_ID);
    assert_eq!(plugin.attachment(), SlotAttachment::MainOverlay);
    let config = plugin.default_config();
    let evaluation = plugin
        .evaluate(&rows, config.as_ref(), &SeriesCatalog::empty())
        .expect("the plugin evaluates");
    assert_eq!(evaluation.series.len(), PROFILE_NODES.len());
    let readouts = plugin.readouts(&evaluation);
    assert!(
        readouts.iter().any(|readout| readout.label == "POC"),
        "the POC reads out"
    );
    assert!(
        readouts
            .iter()
            .any(|readout| readout.label == "VWAP" && readout.value.is_some()),
        "the VWAP reads out"
    );
    let viewport = Viewport::new(
        PriceScaleMode::Normal,
        90.0,
        110.0,
        300.0,
        8.0,
        100.0,
        false,
    )
    .expect("valid viewport");
    egui::__run_test_ui(|ui| {
        plugin
            .paint_main_overlay(
                ui.painter(),
                &evaluation,
                &viewport,
                egui::Pos2::ZERO,
                0,
                640.0,
                &ThemeConfig::default(),
                config.as_ref(),
                &IndicatorStyle::default(),
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
    let viewport = Viewport::new(
        PriceScaleMode::Normal,
        95.0,
        105.0,
        300.0,
        8.0,
        100.0,
        false,
    )
    .expect("valid viewport");
    let boxes = profile_boxes(&profile, &viewport, egui::Pos2::ZERO, 10, 30, &palette());
    assert!(!boxes.is_empty(), "in-window bins paint");
    assert!(boxes.iter().any(|box_| box_.poc), "the POC bin is marked");
    let widest = boxes
        .iter()
        .map(|box_| box_.rect.width())
        .fold(0.0_f32, f32::max);
    let narrowest = boxes
        .iter()
        .map(|box_| box_.rect.width())
        .fold(f32::INFINITY, f32::min);
    assert!(widest > narrowest, "a thicker bin paints a wider bar");
}

#[test]
fn catalog_lists_and_instantiates_the_native_profile() {
    let catalog = crate::indicators::build_indicator_catalog().expect("catalog builds");
    let ids: Vec<String> = catalog
        .filtered()
        .into_iter()
        .map(|entry| entry.id().to_owned())
        .collect();
    assert!(
        ids.contains(&DELTA_VOLUME_PROFILE_ID.to_owned()),
        "the profile is in the catalog"
    );
    let plugin = crate::indicators::instantiate_indicator(DELTA_VOLUME_PROFILE_ID)
        .expect("the profile instantiates");
    assert_eq!(plugin.title(), "Delta Volume Profile");
}

#[test]
fn the_bin_imbalance_ratio_is_a_share_of_the_most_a_bin_could_hold() {
    // Fully one-sided volume reads as the maximum imbalance; the neutral
    // split reads as exactly zero.
    assert!(
        close_to(imbalance_ratio(&bin(100.0, 50.0)), 1.0),
        "all-buy volume saturates"
    );
    assert!(
        close_to(imbalance_ratio(&bin(100.0, -50.0)), -1.0),
        "all-sell volume saturates"
    );
    assert!(
        close_to(imbalance_ratio(&bin(100.0, 0.0)), 0.0),
        "an even split is neutral"
    );
    assert!(
        close_to(imbalance_ratio(&bin(100.0, 25.0)), 0.5),
        "a quarter is half-saturated"
    );
    // Beyond the possible imbalance the ratio clamps rather than exceeding one.
    assert!(
        close_to(imbalance_ratio(&bin(100.0, 200.0)), 1.0),
        "over-imbalance clamps to the rail"
    );
}

#[test]
fn an_empty_or_unmeasurable_bin_reads_as_neutral_rather_than_a_guess() {
    for empty in [
        bin(0.0, 5.0),
        bin(VOLUME_EPSILON / 2.0, 1.0),
        bin(f64::NAN, 1.0),
        bin(f64::INFINITY, 1.0),
        bin(100.0, f64::NAN),
    ] {
        assert!(
            close_to(imbalance_ratio(&empty), 0.0),
            "{empty:?} is not a measurement"
        );
    }
    assert_eq!(
        bin_fill(&one_bin_profile(bin(0.0, 0.0)), 0, &palette()),
        None,
        "an empty bin paints nothing at all, as the reference returns COLOR_NONE"
    );
    assert_eq!(
        bin_fill(&one_bin_profile(bin(10.0, 5.0)), 7, &palette()),
        None,
        "and so does an index past the last bin"
    );
}

#[test]
fn bin_color_follows_the_imbalance_not_the_sign_of_a_single_leg() {
    // The point of the port: bins near balance move continuously along the
    // gradient instead of alternating between two flat colors. The reference
    // maps a narrow band (0.2..0.8), so conviction beyond it saturates on
    // purpose — a profile reads as "mostly one side, softly turning near
    // balance", not as a binary flip.
    let steps: usize = 30;
    let fills: Vec<BinFill> = (0..=steps)
        .map(|step| fill_at((step as f64 - 15.0) / 100.0))
        .collect();
    assert_eq!(
        fills[0].red, 255,
        "a sell-side bin reads the resistance end"
    );
    assert_eq!(fills[0].green, 0, "with no support in it");
    assert_eq!(
        fills[steps].green, 255,
        "a buy-side bin reads the support end"
    );
    assert_eq!(fills[steps].red, 0, "with no resistance in it");
    // Every step between the ends is its own shade: no two adjacent readings
    // collapse onto one color the way a hard green/red alternation did.
    let ramp: Vec<(u8, u8)> = fills.iter().map(|fill| (fill.red, fill.green)).collect();
    // Inside the transition band every step is its own shade: no two adjacent
    // readings collapse onto one color the way a hard green/red alternation
    // did. Past the band the ramp is deliberately flat (that is the rail).
    let (first_transition, last_transition) = (6, steps - 6);
    let blending: Vec<(u8, u8)> = ramp[first_transition..=last_transition].to_vec();
    assert!(
        blending.windows(2).all(|pair| pair[0] != pair[1]),
        "the gradient never repeats a step inside the band: {blending:?}"
    );
    for pair in fills.windows(2) {
        assert!(pair[1].red <= pair[0].red, "red falls as buys rise");
        assert!(pair[1].green >= pair[0].green, "green rises as buys rise");
    }
    // The transition really is a blend, not a hard flip at the midpoint: the
    // bins flanking balance straddle it instead of jumping over it.
    let middle = steps / 2;
    assert!(
        (i16::from(fills[middle].red) - i16::from(fills[middle].green)).abs() <= 1,
        "an exactly balanced bin is mid-band: {:?}",
        fills[middle]
    );
    assert!(
        fills[middle - 1].red > fills[middle].red && fills[middle + 1].red < fills[middle].red,
        "so its neighbours straddle it: {ramp:?}"
    );
    // Conviction past the band saturates, which is the reference's own rail.
    assert_eq!(
        fill_at(1.0).green,
        255,
        "a saturated buy bin is fully support"
    );
    assert_eq!(
        fill_at(-1.0).red,
        255,
        "a saturated sell bin is fully resistance"
    );
}

#[test]
fn a_mildly_lopsided_bin_reads_as_a_blend_rather_than_a_flip() {
    // The exponent is below one precisely so a small imbalance is visibly off
    // neutral; a linear map would leave it almost indistinguishable from zero.
    let neutral = fill_at(0.0);
    let mild = fill_at(0.05);
    let saturated = fill_at(1.0);
    assert!(
        mild.red < neutral.red && mild.red > saturated.red,
        "a 5%-imbalanced bin sits strictly between neutral and saturated: \
         neutral {neutral:?}, mild {mild:?}, saturated {saturated:?}"
    );
    assert!(
        mild.green > neutral.green,
        "and has moved just as far the other way"
    );
    // Its opposite sign lands symmetrically on the other side of the band.
    let mirrored = fill_at(-0.05);
    assert!(
        (i16::from(mild.red) - i16::from(mirrored.green)).abs() <= 1,
        "buy and sell mirrors about the midpoint: {mild:?} vs {mirrored:?}"
    );
}

#[test]
fn bin_transparency_follows_volume_while_color_follows_imbalance() {
    // Volume against the mean drives opacity, so thickness reads as density
    // rather than as a different color.
    let thin = bin_fill(&profile_with_mean(bin(100.0, 5.0), 800.0), 0, &palette())
        .expect("a full bin fills");
    let mean = bin_fill(&profile_with_mean(bin(100.0, 5.0), 100.0), 0, &palette())
        .expect("a full bin fills");
    let thick = bin_fill(&profile_with_mean(bin(100.0, 5.0), 12.5), 0, &palette())
        .expect("a full bin fills");
    assert_eq!(
        thin.red, thick.red,
        "the same imbalance paints the same color"
    );
    assert_eq!(thin.green, thick.green, "on every channel");
    assert!(
        thick.transparency < mean.transparency && mean.transparency < thin.transparency,
        "a thicker bin is more opaque: {} / {} / {}",
        thick.transparency,
        mean.transparency,
        thin.transparency
    );
    // The reference rails: never fully opaque, never invisible, at any ratio.
    for fill in [thin, mean, thick] {
        assert!(
            (MIN_TRANSPARENCY..=MAX_TRANSPARENCY).contains(&fill.transparency),
            "transparency stays inside the reference band: {fill:?}"
        );
    }
    assert!(close_to(MIN_TRANSPARENCY, 5.0));
    assert!(close_to(MAX_TRANSPARENCY, 95.0));
    // A bin four times the mean saturates the shading formula at its floor.
    assert!(
        close_to(thick.transparency, MIN_TRANSPARENCY),
        "4x the mean hits the rail"
    );
    // A bin an eighth of the mean barely moves the shading at all, so a long
    // tail of thin bins stays a faint wash instead of turning into blocks.
    assert!(
        thin.transparency >= TRANSPARENCY_BASE - 1.0,
        "a hair-thin bin sits at the shading base: {}",
        thin.transparency
    );
}

#[test]
fn the_transparency_formula_is_the_references_own() {
    // `int(75 - (vol_ratio / 4)^2 * 70)` at the three ratios that make the
    // arithmetic checkable by hand.
    let at = |ratio: f64| {
        let fill = bin_fill(
            &profile_with_mean(bin(100.0, 0.0), 100.0 / ratio),
            0,
            &palette(),
        )
        .expect("a full bin fills");
        fill.transparency
    };
    assert!(
        close_to(at(1.0), 70.0),
        "at the mean: 75 - (1/4)^2 * 70 = 70.625, truncated"
    );
    assert!(
        close_to(at(2.0), 57.0),
        "at twice the mean: 75 - (2/4)^2 * 70 = 57.5"
    );
    assert!(
        close_to(at(4.0), MIN_TRANSPARENCY),
        "at four times the mean the formula goes negative"
    );

    // With no mean to compare against, the reference falls back to a ratio of
    // 1.0 rather than dividing by zero.
    let no_mean = bin_fill(&profile_with_mean(bin(100.0, 0.0), 0.0), 0, &palette())
        .expect("a full bin fills");
    assert!(
        close_to(no_mean.transparency, at(1.0)),
        "an absent mean reads as the mean bin"
    );
}

#[test]
fn the_palette_is_the_indicators_own_bull_and_bear_slots() {
    let theme = ThemeConfig::default();
    let palette = bin_palette(&IndicatorStyle::default(), &theme).expect("the theme resolves");
    for (key, endpoint) in [("bull", palette.support), ("bear", palette.resistance)] {
        let themed = IndicatorStyle::default()
            .color(&theme, key)
            .expect("a known key resolves");
        assert_eq!(
            endpoint,
            [themed.r(), themed.g(), themed.b()],
            "the {key} end is that slot, so the profile shades in the chart\'s own colors"
        );
    }
}

#[test]
fn a_style_override_restyles_the_profile_through_the_same_two_slots() {
    let theme = ThemeConfig::default();
    let mut style = IndicatorStyle::default();
    style
        .set_field("color_bull", "#010203")
        .expect("a well-formed override writes");
    let palette = bin_palette(&style, &theme).expect("the override resolves");
    assert_eq!(
        palette.support,
        [1, 2, 3],
        "the Style tab restyles the buy end"
    );
    assert_eq!(
        palette.resistance,
        bin_palette(&IndicatorStyle::default(), &theme)
            .expect("themed")
            .resistance,
        "and leaves the sell end themed"
    );
}

#[test]
fn the_profile_paints_its_boxes_through_the_ported_shading() {
    let rows: Vec<ChartRow> = (0..60_usize)
        .map(|i| {
            let base = 100.0 + (i as f64 * 0.2).sin() * 2.0;
            row(
                base,
                base + 1.0,
                base - 1.0,
                base + 0.3,
                1_000 + i as u64 * 5,
            )
        })
        .collect();
    let plugin = DeltaVolumeProfileIndicator::new();
    let config = plugin.default_config();
    let evaluation = plugin
        .evaluate(&rows, config.as_ref(), &SeriesCatalog::empty())
        .expect("the plugin evaluates");
    let viewport = Viewport::new(
        PriceScaleMode::Normal,
        90.0,
        110.0,
        300.0,
        8.0,
        100.0,
        false,
    )
    .expect("valid viewport");
    let profile = build_profile(
        &[ProfileBar {
            high: 101.0,
            low: 100.0,
            close: 100.5,
            volume: 40.0,
            buy: 35.0,
        }],
        4,
        0.7,
    );
    let boxes = profile_boxes(&profile, &viewport, egui::Pos2::ZERO, 30, 30, &palette());
    assert!(!boxes.is_empty(), "the boxes lay out");
    for box_ in &boxes {
        assert!(
            box_.fill.green > box_.fill.red,
            "a buy-heavy box leans to the support end: {:?}",
            box_.fill
        );
        assert!(
            (MIN_TRANSPARENCY..=MAX_TRANSPARENCY).contains(&box_.fill.transparency),
            "and carries a usable alpha: {:?}",
            box_.fill
        );
    }
    // And the whole overlay paints headlessly, themed and overridden.
    let mut style = IndicatorStyle::default();
    style
        .set_field("color_bull", "#00FF00")
        .expect("an override writes");
    egui::__run_test_ui(|ui| {
        for style in [&IndicatorStyle::default(), &style] {
            plugin
                .paint_main_overlay(
                    ui.painter(),
                    &evaluation,
                    &viewport,
                    egui::Pos2::ZERO,
                    0,
                    640.0,
                    &ThemeConfig::default(),
                    config.as_ref(),
                    style,
                )
                .expect("the overlay paints");
        }
    });
}
