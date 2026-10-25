//! Display-flag dials behind the IntegrationLite pane: every new knob
//! defaults to today's picture, validates fail-closed, and renders in its
//! Pine group.

use super::*;
use crate::oscillators::OscillatorKind;
use fluent_egui::SettingsTab;

fn ramp_rows(count: usize) -> Vec<ChartRow> {
    (0..count)
        .map(|i| {
            let close = 100.0 + i as f64;
            ChartRow {
                time: i as i64,
                open: close - 0.5,
                high: close + 0.5,
                low: close - 1.0,
                close,
                volume: 10,
            }
        })
        .collect()
}

/// Rows with varying bar widths, so derived columns (hl2, hlc3, …) are not
/// constant shifts of close and single-leg arms genuinely move. The wobble
/// is one-sided so it survives the high/low average.
fn wobble_rows(count: usize) -> Vec<ChartRow> {
    (0..count)
        .map(|i| {
            // Jagged climbs: mixed-sign gains keep RSI off saturation so a
            // resynthesized column genuinely moves the arm.
            let close = 100.0 + i as f64 * 0.5 + if i % 2 == 0 { 1.0 } else { -1.0 };
            let span = (i % 5) as f64 * 0.4;
            ChartRow {
                time: i as i64,
                open: close - 0.5,
                high: close + 0.5 + span,
                low: close - 1.0,
                close,
                volume: 10,
            }
        })
        .collect()
}

#[test]
fn display_flags_default_to_todays_picture() {
    let config = IntegrationLiteConfig::default();
    assert!(config.delta_show, "delta candles paint today");
    assert!(config.ribbon_health_show, "ribbon health paints today");
    assert!(config.ma_health_show, "ma health paints today");
    assert!(config.pivots_show, "pivots fire today");
    assert!(config.osc_show, "oscillator legs paint today");
    assert_eq!(config.brightness, 30, "reference brightness input");
    assert!(
        !config.adaptive,
        "legacy close-frame legs stay the default; Pine-true adaptive is opt-in"
    );
    assert_eq!(config.price_source, "close", "legacy price source");
    assert!(config.color_code, "candle tinting paints today");
}

#[test]
fn new_dials_render_in_their_pine_groups() {
    let fields = fluent_egui::schema_fields(&IntegrationLiteConfig::default().describe());
    let group_of = |name: &str| {
        fields
            .iter()
            .find(|field| field.name == name)
            .unwrap_or_else(|| panic!("{name} lifts"))
            .group
            .clone()
    };
    assert_eq!(
        group_of("delta_show").as_deref(),
        Some("Delta Volume Candles")
    );
    assert_eq!(
        group_of("ribbon_health_show").as_deref(),
        Some("Trend Ribbon")
    );
    assert_eq!(group_of("ma_health_show").as_deref(), Some("Trend Ribbon"));
    assert_eq!(group_of("pivots_show").as_deref(), Some("Trend Ribbon"));
    assert_eq!(
        group_of("osc_show").as_deref(),
        Some("Oscillator 1 Settings")
    );
    assert_eq!(
        group_of("brightness").as_deref(),
        Some("Oscillator 1 Settings")
    );
    assert_eq!(
        group_of("adaptive").as_deref(),
        Some("Oscillator 1 Settings")
    );
    assert_eq!(
        group_of("price_source").as_deref(),
        Some("Oscillator 1 Settings")
    );
    assert_eq!(group_of("color_code").as_deref(), Some("Colors"));
    // Show flags route to Visibility through the explicit pin, not the
    // name heuristic (which only recognizes leading `show_`).
    for name in [
        "delta_show",
        "ribbon_health_show",
        "ma_health_show",
        "pivots_show",
        "osc_show",
        "color_code",
    ] {
        let field = fields
            .iter()
            .find(|field| field.name == name)
            .unwrap_or_else(|| panic!("{name} lifts"));
        assert_eq!(
            fluent_egui::classify_schema_field(field),
            SettingsTab::Visibility,
            "{name} is a Visibility toggle"
        );
    }
}

#[test]
fn brightness_bounds_refuse() {
    let mut config = IntegrationLiteConfig::default();
    assert!(config.validate().is_ok());
    config.brightness = 101;
    assert!(config.validate().is_err(), "101 exceeds the Pine maxval");
    config.brightness = 30;
    assert!(config.set_field("brightness", "101").is_err());
    assert!(config.set_field("brightness", "-1").is_err());
    assert!(config.set_field("brightness", "60").is_ok());
    assert_eq!(config.get_field("brightness").unwrap(), "60");
}

#[test]
fn unknown_source_refuses() {
    let config = IntegrationLiteConfig {
        price_source: "hl3x".to_owned(),
        ..IntegrationLiteConfig::default()
    };
    assert!(
        config.validate().is_err(),
        "an unlisted source refuses with its reason"
    );
}

#[test]
fn multileg_arms_refuse_resynthesis() {
    // MFI and Stochastic read high/low/volume legs the single-column
    // resynthesis cannot supply, so non-close sources and the adaptive
    // relay refuse on those arms instead of miscomputing.
    for kind in ["mfi", "stoch"] {
        let config = IntegrationLiteConfig {
            fast_kind: kind.to_owned(),
            price_source: "hl2".to_owned(),
            ..IntegrationLiteConfig::default()
        };
        assert!(
            config.validate().is_err(),
            "{kind} with a non-close source refuses"
        );
        let config = IntegrationLiteConfig {
            slow_kind: kind.to_owned(),
            adaptive: true,
            ..IntegrationLiteConfig::default()
        };
        assert!(
            config.validate().is_err(),
            "adaptive {kind} refuses until the exact-arms milestone"
        );
    }
    for kind in ["rsi", "cci", "cmo"] {
        let config = IntegrationLiteConfig {
            fast_kind: kind.to_owned(),
            price_source: "hl2".to_owned(),
            ..IntegrationLiteConfig::default()
        };
        assert!(
            config.validate().is_ok(),
            "single-leg {kind} follows the source column"
        );
        let config = IntegrationLiteConfig {
            fast_kind: kind.to_owned(),
            adaptive: true,
            ..IntegrationLiteConfig::default()
        };
        assert!(
            config.validate().is_ok(),
            "single-leg {kind} follows the adaptive relay"
        );
    }
}

#[test]
fn legacy_documents_decode_and_evaluate_byte_identically() {
    // Old documents carry no display-flag keys; they must decode through
    // the new defaults and evaluate exactly like the current default.
    let rows = ramp_rows(60);
    let current = integration_lite_sub_pane(&rows, &IntegrationLiteConfig::default())
        .expect("default evaluates");
    let mut legacy_json =
        serde_json::to_value(IntegrationLiteConfig::default()).expect("default serializes");
    let object = legacy_json.as_object_mut().expect("config is an object");
    for key in [
        "delta_show",
        "ribbon_health_show",
        "ma_health_show",
        "pivots_show",
        "osc_show",
        "brightness",
        "adaptive",
        "price_source",
        "color_code",
        "delta_bull",
        "delta_bear",
        "signal_type",
        "signal_period",
        "signal_param1",
        "mean_type",
        "mean_period",
        "mean_param1",
        "ltf_timeframe",
        "fetch_timeframe",
        "htf_timeframe",
        "session_filter",
        "session",
        "mtf_table_show",
        "mtf_table_position",
        "mtf_table_size",
        "mtf_table_text",
        "mtf_table_bg",
        "mtf_timeframe_1",
        "mtf_timeframe_2",
        "mtf_timeframe_3",
        "mtf_timeframe_4",
        "ribbon_alpha_low",
        "ribbon_alpha_high",
        "ribbon_bull",
        "ribbon_bear",
        "pivots_lookback",
    ] {
        assert!(object.remove(key).is_some(), "legacy fixture drops {key}");
    }
    let legacy: IntegrationLiteConfig =
        serde_json::from_value(legacy_json).expect("legacy document decodes");
    let rerun = integration_lite_sub_pane(&rows, &legacy).expect("legacy evaluates");
    assert_eq!(rerun.fast_bounded, current.fast_bounded);
    assert_eq!(rerun.slow_bounded, current.slow_bounded);
    assert_eq!(rerun.ribbon_health_bounded, current.ribbon_health_bounded);
    assert_eq!(rerun.ma_health_bounded, current.ma_health_bounded);
    assert_eq!(rerun.pivots, current.pivots);
    assert_eq!(rerun.show_ribbon, current.show_ribbon);
}

#[test]
fn adaptive_and_source_dials_move_single_leg_arms() {
    let rows = ramp_rows(60);
    let base = integration_lite_sub_pane(&rows, &IntegrationLiteConfig::default())
        .expect("default evaluates");
    let adaptive = IntegrationLiteConfig {
        adaptive: true,
        ..IntegrationLiteConfig::default()
    };
    let moved = integration_lite_sub_pane(&rows, &adaptive).expect("adaptive evaluates");
    assert_ne!(
        moved.fast_bounded, base.fast_bounded,
        "the adaptive relay moves the fast arm"
    );
    let resourced = IntegrationLiteConfig {
        price_source: "hl2".to_owned(),
        ..IntegrationLiteConfig::default()
    };
    let rows = wobble_rows(60);
    let base = integration_lite_sub_pane(&rows, &IntegrationLiteConfig::default())
        .expect("default evaluates");
    let moved = integration_lite_sub_pane(&rows, &resourced).expect("hl2 evaluates");
    assert_ne!(
        moved.fast_bounded, base.fast_bounded,
        "the source column moves the fast arm"
    );
    // Kind colors seed from the catalog, so the oscillator taxonomy still
    // names both arms.
    assert!(OscillatorKind::Rsi.is_evaluable());
    assert!(OscillatorKind::Cci.is_evaluable());
}

#[test]
fn paint_only_edits_keep_the_key_and_the_values() {
    use crate::indicators::{ChartIndicator, IntegrationLiteIndicator};
    let plugin = IntegrationLiteIndicator::new();
    let rows = ramp_rows(60);
    let base = IntegrationLiteConfig::default();
    let base_key = plugin.config_key(&base).expect("default fingerprints");
    let base_values = integration_lite_sub_pane(&rows, &base).expect("default evaluates");
    let mut painted = base.clone();
    painted.delta_show = false;
    painted.ribbon_health_show = false;
    painted.ma_health_show = false;
    painted.pivots_show = false;
    painted.osc_show = false;
    painted.brightness = 80;
    painted.color_code = false;
    painted.show_ribbon = false;
    assert!(painted.validate().is_ok(), "paint dials validate");
    assert_eq!(
        plugin.config_key(&painted).expect("painted fingerprints"),
        base_key,
        "paint-only dials never move the plan fingerprint"
    );
    let rerun = integration_lite_sub_pane(&rows, &painted).expect("painted evaluates");
    assert_eq!(rerun.fast_bounded, base_values.fast_bounded);
    assert_eq!(rerun.slow_bounded, base_values.slow_bounded);
    assert_eq!(
        rerun.ribbon_health_bounded, base_values.ribbon_health_bounded,
        "health gains untouched by visibility"
    );
}

#[test]
fn value_edits_move_the_key() {
    use crate::indicators::{ChartIndicator, IntegrationLiteIndicator};
    let plugin = IntegrationLiteIndicator::new();
    let base = IntegrationLiteConfig::default();
    let base_key = plugin.config_key(&base).expect("default fingerprints");
    for (mutate, moved) in [
        ("pivot_threshold", "200"),
        ("ma_health_k", "2.4"),
        ("ribbon_health_k", "3.6"),
        ("delta_unit", "160"),
        ("eit_alpha", "0.28"),
        ("fast_kind", "cci"),
        ("adaptive", "true"),
        ("price_source", "hl2"),
    ] {
        let mut edited = base.clone();
        edited.get_field(mutate).expect("field reads");
        edited.set_field(mutate, moved).expect("value edit writes");
        assert!(
            edited.validate().is_ok(),
            "{mutate} edit stays valid ({moved})"
        );
        assert_ne!(
            plugin.config_key(&edited).expect("edited fingerprints"),
            base_key,
            "{mutate} moves the key"
        );
    }
}

#[test]
fn ribbon_leg_dials_default_to_the_script_rows() {
    let config = IntegrationLiteConfig::default();
    assert_eq!(config.signal_type, "eit");
    assert_eq!(config.signal_period, 10);
    assert!(
        (config.signal_param1 - 0.12).abs() < 1e-12,
        "signal alpha default"
    );
    assert_eq!(config.mean_type, "ema");
    assert_eq!(config.mean_period, 21);
    assert!(
        (config.mean_param1 - 0.06).abs() < 1e-12,
        "mean alpha default"
    );
}

#[test]
fn ribbon_leg_dials_validate_through_the_family_contract() {
    let mut config = IntegrationLiteConfig::default();
    assert!(config.validate().is_ok());
    config.signal_type = "nope".to_owned();
    assert!(config.validate().is_err(), "unknown family refuses");
    config.signal_type = "eit".to_owned();
    config.signal_param1 = 1.5;
    assert!(
        config.validate().is_err(),
        "EIT alpha above one refuses through the kernel contract"
    );
    config.signal_param1 = 0.12;
    config.mean_period = 0;
    assert!(config.validate().is_err(), "non-positive window refuses");
}

#[test]
fn ribbon_leg_dials_move_the_band_and_the_fold() {
    let rows = ramp_rows(60);
    let base = integration_lite_sub_pane(&rows, &IntegrationLiteConfig::default())
        .expect("default evaluates");
    let smoothed = IntegrationLiteConfig {
        signal_type: "sma".to_owned(),
        ..IntegrationLiteConfig::default()
    };
    let moved = integration_lite_sub_pane(&rows, &smoothed).expect("sma signal evaluates");
    assert_ne!(
        moved.ribbon.upper, base.ribbon.upper,
        "the signal dial moves the ribbon band"
    );
    let meaned = IntegrationLiteConfig {
        mean_type: "alma".to_owned(),
        ..IntegrationLiteConfig::default()
    };
    let moved = integration_lite_sub_pane(&rows, &meaned).expect("alma mean evaluates");
    assert_ne!(
        moved.ribbon_health_bounded, base.ribbon_health_bounded,
        "the mean dial moves the health fold"
    );
}

#[test]
fn volume_colors_default_to_the_script_hex() {
    let config = IntegrationLiteConfig::default();
    assert_eq!(config.delta_bull, "#00ff00");
    assert_eq!(config.delta_bear, "#ff0000");
}

#[test]
fn volume_colors_validate_as_hex() {
    let mut config = IntegrationLiteConfig::default();
    assert!(config.validate().is_ok());
    for bad in ["red", "#12", "", "#zzzzzz", "00ff00"] {
        config.delta_bull = bad.to_owned();
        assert!(
            config.validate().is_err(),
            "{bad:?} refuses naming delta_bull"
        );
    }
}

#[test]
fn configuration_session_and_mtf_dials_default_to_script() {
    let config = IntegrationLiteConfig::default();
    assert_eq!(config.ltf_timeframe, "m1");
    assert_eq!(config.fetch_timeframe, "c");
    assert_eq!(config.htf_timeframe, "w1");
    assert!(config.session_filter);
    assert_eq!(config.session, "0930-1600");
    assert!(config.mtf_table_show);
    assert_eq!(config.mtf_table_position, "middle_left");
    assert_eq!(config.mtf_table_size, "small");
    assert_eq!(config.mtf_table_text, "#dbdbdb");
    assert_eq!(config.mtf_table_bg, "#000000");
    assert_eq!(config.mtf_timeframe_1, "m5");
    assert_eq!(config.mtf_timeframe_2, "m15");
    assert_eq!(config.mtf_timeframe_3, "h1");
    assert_eq!(config.mtf_timeframe_4, "h4");
    assert!(config.validate().is_ok());
}

#[test]
fn configuration_dials_validate_against_their_catalogs() {
    let config = IntegrationLiteConfig {
        ltf_timeframe: "weekly".to_owned(),
        ..IntegrationLiteConfig::default()
    };
    assert!(config.validate().is_err(), "unlisted timeframe refuses");
    let config = IntegrationLiteConfig {
        mtf_table_position: "everywhere".to_owned(),
        ..IntegrationLiteConfig::default()
    };
    assert!(config.validate().is_err(), "unlisted location refuses");
    let config = IntegrationLiteConfig {
        session: "   ".to_owned(),
        ..IntegrationLiteConfig::default()
    };
    assert!(config.validate().is_err(), "empty session refuses");
    let config = IntegrationLiteConfig {
        mtf_table_text: "dbdbdb".to_owned(),
        ..IntegrationLiteConfig::default()
    };
    assert!(config.validate().is_err(), "unhashed color refuses");
}

#[test]
fn configuration_groups_render() {
    let fields = fluent_egui::schema_fields(&IntegrationLiteConfig::default().describe());
    let group_of = |name: &str| {
        fields
            .iter()
            .find(|field| field.name == name)
            .unwrap_or_else(|| panic!("{name} lifts"))
            .group
            .clone()
    };
    assert_eq!(group_of("ltf_timeframe").as_deref(), Some("Configuration"));
    assert_eq!(group_of("session").as_deref(), Some("Session Filter"));
    assert_eq!(
        group_of("mtf_table_show").as_deref(),
        Some("Multi-Timeframe Table")
    );
    assert_eq!(
        group_of("mtf_timeframe_1").as_deref(),
        Some("Multi-Timeframe Selection")
    );
}

#[test]
fn ribbon_span_colors_and_lookback_default_to_script() {
    let config = IntegrationLiteConfig::default();
    assert!(
        (config.ribbon_alpha_low - 0.14).abs() < 1e-12,
        "alpha low default"
    );
    assert!(
        (config.ribbon_alpha_high - 0.8).abs() < 1e-12,
        "alpha high default"
    );
    assert_eq!(config.ribbon_bull, "#00bcd4");
    assert_eq!(config.ribbon_bear, "#7b1fa2");
    assert_eq!(config.pivots_lookback, 8);
    assert!(config.validate().is_ok());
}

#[test]
fn ribbon_span_colors_and_lookback_validate() {
    let config = IntegrationLiteConfig {
        ribbon_alpha_low: 0.01,
        ..IntegrationLiteConfig::default()
    };
    assert!(config.validate().is_err(), "low below its floor refuses");
    let config = IntegrationLiteConfig {
        ribbon_alpha_high: 1.5,
        ..IntegrationLiteConfig::default()
    };
    assert!(config.validate().is_err(), "high above one refuses");
    let config = IntegrationLiteConfig {
        ribbon_alpha_low: 0.9,
        ribbon_alpha_high: 0.8,
        ..IntegrationLiteConfig::default()
    };
    assert!(config.validate().is_err(), "inverted span refuses");
    let config = IntegrationLiteConfig {
        ribbon_bull: "cyan".to_owned(),
        ..IntegrationLiteConfig::default()
    };
    assert!(config.validate().is_err(), "unhashed ribbon color refuses");
    let config = IntegrationLiteConfig {
        pivots_lookback: 1,
        ..IntegrationLiteConfig::default()
    };
    assert!(config.validate().is_err(), "lookback below two refuses");
    let config = IntegrationLiteConfig {
        pivots_lookback: 41,
        ..IntegrationLiteConfig::default()
    };
    assert!(config.validate().is_err(), "lookback above forty refuses");
}

#[test]
fn ribbon_span_grouping_renders_in_trend_ribbon() {
    let fields = fluent_egui::schema_fields(&IntegrationLiteConfig::default().describe());
    let group_of = |name: &str| {
        fields
            .iter()
            .find(|field| field.name == name)
            .unwrap_or_else(|| panic!("{name} lifts"))
            .group
            .clone()
    };
    for name in [
        "ribbon_alpha_low",
        "ribbon_alpha_high",
        "ribbon_bull",
        "ribbon_bear",
        "pivots_lookback",
    ] {
        assert_eq!(
            group_of(name).as_deref(),
            Some("Trend Ribbon"),
            "{name} renders in the ribbon group"
        );
    }
}

#[test]
fn ribbon_colors_are_paint_only_and_span_moves_the_key() {
    use crate::indicators::{ChartIndicator, IntegrationLiteIndicator};
    let plugin = IntegrationLiteIndicator::new();
    let base = IntegrationLiteConfig::default();
    let base_key = plugin.config_key(&base).expect("default fingerprints");
    let mut painted = base.clone();
    painted.ribbon_bull = "#ffffff".to_owned();
    painted.ribbon_bear = "#000000".to_owned();
    assert!(painted.validate().is_ok());
    assert_eq!(
        plugin.config_key(&painted).expect("painted fingerprints"),
        base_key,
        "ribbon colors never move the plan fingerprint"
    );
    let mut spanned = base.clone();
    spanned.ribbon_alpha_low = 0.2;
    assert_ne!(
        plugin.config_key(&spanned).expect("spanned fingerprints"),
        base_key,
        "alpha span moves the key"
    );
    let mut looked = base.clone();
    looked.pivots_lookback = 12;
    assert_ne!(
        plugin.config_key(&looked).expect("looked fingerprints"),
        base_key,
        "lookback moves the key"
    );
}
