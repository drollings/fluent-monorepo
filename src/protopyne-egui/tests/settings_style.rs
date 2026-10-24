use super::*;
use crate::indicators::KernelIndicatorConfig;
use fluent_egui::{fields_for_tab, schema_fields, IndicatorStyle, SettingsTab, ThemeConfig};
use fluent_wvr::Describable;

fn style_names(kernel: &str) -> Vec<String> {
    let config = KernelIndicatorConfig::defaults_for(kernel);
    let fields = schema_fields(&config.describe());
    let mut names: Vec<String> = fields_for_tab(&fields, SettingsTab::Style)
        .into_iter()
        .map(|field| field.name)
        .collect();
    names.sort();
    names
}

#[test]
fn oscillator_display_dials_render_under_the_style_tab() {
    let names = style_names("rsi");
    for dial in [
        "normalize",
        "osc_gain",
        "osc_trim",
        "osc_threshold",
        "style",
    ] {
        assert!(
            names.contains(&dial.to_owned()),
            "the Style tab carries the {dial} display knob: {names:?}"
        );
    }
    assert!(
        !names.contains(&"length".to_owned()),
        "calculation fields stay out of the Style tab: {names:?}"
    );
}

#[test]
fn painted_style_keys_are_the_slots_own_series_keys() {
    let mut workspace = ChartWorkspace::with_rows(
        (0..60)
            .map(|i| ChartRow {
                time: i64::from(i),
                open: 99.5,
                high: 100.5,
                low: 99.0,
                close: 100.0,
                volume: 10,
            })
            .collect(),
    )
    .expect("workspace boots");
    workspace.slot_indicator("rsi").expect("rsi slots");
    workspace.recompute().expect("evaluations build");
    let keys = workspace.painted_style_keys("rsi");
    assert_eq!(
        keys,
        vec!["rsi".to_owned()],
        "the Style tab covers exactly the keys the slot paints: {keys:?}"
    );
    assert!(
        workspace.painted_style_keys("nope").is_empty(),
        "unknown slots project to nothing"
    );
}

/// Color audit against the shipped references: every theme default the Pine
/// scripts pin matches hex-for-hex, and a slot override wins over the theme
/// (clearing it restores the theme). Pine sources: the Integration Lite
/// candle/ribbon inputs, the Fast MAs leg colors, and the oscillator table's
/// RSI/CCI arms behind the Lite fast/slow legs. Terminal chrome
/// (`background`, `grid`) carries no Pine input and stays documented here
/// rather than drifted.
#[test]
fn theme_defaults_match_the_reference_hex_values() {
    use fluent_wvr::FieldAccess as _;
    let theme = ThemeConfig::default();
    for (key, hex) in [
        ("bull", "#20B46C"),
        ("bear", "#B42020"),
        ("bull_down", "#FF9800"),
        ("bear_up", "#2196F3"),
        ("ribbon_bull", "#00BCD4"),
        ("ribbon_bear", "#7B1FA2"),
        ("fast", "#20C8F0"),
        ("slow", "#B4F028"),
        ("background", "#000000"),
        ("grid", "#2A2A2A"),
    ] {
        assert_eq!(
            theme.style(key),
            Some(hex),
            "theme default for {key} matches the reference"
        );
    }
    let mut style = IndicatorStyle::default();
    for key in ThemeConfig::style_keys() {
        if *key == "background" || *key == "grid" {
            continue;
        }
        assert_eq!(
            style.color(&theme, key).expect("resolves"),
            fluent_egui::resolve_color(&theme, key).expect("resolves"),
            "{key} inherits the theme with no override set"
        );
    }
    style
        .set_field("color_fast", "#123456")
        .expect("override writes");
    assert_eq!(
        style.color(&theme, "fast").expect("resolves"),
        egui::Color32::from_rgb(0x12, 0x34, 0x56),
        "a set override wins over the theme"
    );
    style.set_field("color_fast", "").expect("clears");
    assert_eq!(
        style.color(&theme, "fast").expect("resolves"),
        fluent_egui::resolve_color(&theme, "fast").expect("resolves"),
        "clearing the override restores the theme"
    );
}

#[test]
fn lite_style_tab_offers_the_ribbon_overrides_with_the_leg_keys() {
    let mut workspace = ChartWorkspace::with_rows(
        (0..60)
            .map(|i| ChartRow {
                time: i64::from(i),
                open: 99.5,
                high: 100.5,
                low: 99.0,
                close: 100.0,
                volume: 10,
            })
            .collect(),
    )
    .expect("workspace boots");
    workspace.recompute().expect("evaluations build");
    let mut keys = workspace.painted_style_keys("integration_lite");
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "cci".to_owned(),
            "fast".to_owned(),
            "ribbon_bear".to_owned(),
            "ribbon_bull".to_owned(),
            "rsi".to_owned(),
            "slow".to_owned(),
        ],
        "the Lite Style tab covers the kind legs, the health legs, and the ribbon it paints: {keys:?}"
    );
}

/// Slots whose kernel does not normalize omit the seven normalization dials:
/// on a moving average they cannot move the picture, so the Style tab stops
/// offering them. Normalizing kernels keep every dial.
#[test]
fn slots_without_normalization_omit_the_inert_dials() {
    const INERT: [&str; 7] = [
        "normalize",
        "osc_gain",
        "osc_trim",
        "osc_threshold",
        "osc_offset",
        "osc_k",
        "osc_mult",
    ];
    for kernel in ["sma", "ema", "alma", "wma"] {
        let names = style_names(kernel);
        for dial in INERT {
            assert!(
                !names.contains(&dial.to_owned()),
                "{kernel} must not offer {dial}: {names:?}"
            );
        }
        assert!(
            names.contains(&"smooth_type".to_owned()),
            "{kernel} keeps its working smoothing dials: {names:?}"
        );
    }
    for kernel in ["rsi", "cci", "stoch"] {
        let names = style_names(kernel);
        for dial in INERT {
            assert!(
                names.contains(&dial.to_owned()),
                "{kernel} keeps {dial}: {names:?}"
            );
        }
    }
}

/// Oscillator legs resolve through kind keys carrying the Pine hues: each
/// mapped kind defaults its slot style to its own key, and anything else
/// keeps the role default. Hue sources: the shipped oscillator color table
/// (`rsi`/`cci` arms already matched `fast`/`slow`); `adx` has no Pine row
/// and keeps its current effective hue, so its default changes nothing.
#[test]
fn oscillator_kind_keys_carry_the_pine_hues() {
    let theme = ThemeConfig::default();
    for (key, hex) in [
        ("rsi", "#20C8F0"),
        ("cci", "#B4F028"),
        ("cmo", "#8CF050"),
        ("mfi", "#F020F0"),
        ("stoch", "#B4F0C8"),
        ("aroon", "#FAF028"),
        ("adx", "#20C8F0"),
    ] {
        assert_eq!(
            theme.style(key),
            Some(hex),
            "theme default for {key} matches the Pine hue"
        );
    }
    for (kernel, key) in [
        ("rsi", "rsi"),
        ("cci", "cci"),
        ("cmo", "cmo"),
        ("mfi", "mfi"),
        ("stoch", "stoch"),
        ("aroon", "aroon"),
        ("adx", "adx"),
    ] {
        assert_eq!(
            crate::indicators::KernelIndicatorConfig::defaults_for(kernel).style,
            key,
            "{kernel} slots default to the kind key"
        );
    }
    for kernel in ["sma", "ema", "macd", "bb"] {
        assert_eq!(
            crate::indicators::KernelIndicatorConfig::defaults_for(kernel).style,
            "fast",
            "{kernel} keeps the role default"
        );
    }
}

#[test]
fn kind_overrides_repaint_the_leg_and_clearing_restores_the_hue() {
    use fluent_wvr::FieldAccess as _;
    let theme = ThemeConfig::default();
    let mut style = IndicatorStyle::default();
    assert_eq!(
        style.color(&theme, "mfi").expect("resolves"),
        fluent_egui::resolve_color(&theme, "mfi").expect("resolves"),
        "the kind key inherits the Pine hue with no override set"
    );
    style
        .set_field("color_mfi", "#F020F0")
        .expect("override writes");
    assert_eq!(
        style.color(&theme, "mfi").expect("resolves"),
        egui::Color32::from_rgb(0xF0, 0x20, 0xF0),
        "a set kind override wins over the theme"
    );
    style.set_field("color_mfi", "").expect("clears");
    assert_eq!(
        style.color(&theme, "mfi").expect("resolves"),
        fluent_egui::resolve_color(&theme, "mfi").expect("resolves"),
        "clearing the kind override restores the Pine hue"
    );
}
