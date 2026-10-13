use super::*;
use std::sync::Arc;

#[test]
fn pane_and_theme_fields_round_trip() {
    let mut pane = PaneConfig::new().name("oscillator".to_owned()).build();
    for name in ["name", "visible", "height_share"] {
        assert!(pane.get_field(name).is_ok(), "pane field {name} reads");
        assert!(pane.describe()["properties"].get(name).is_some());
    }
    pane.set_field("visible", "false").expect("visible writes");
    assert!(!pane.visible);
    pane.set_field("height_share", "0.5").expect("share writes");
    assert!((pane.height_share - 0.5).abs() < 1e-12);

    let mut theme = ThemeConfig::default();
    for name in [
        "name",
        "bull",
        "bear",
        "fast",
        "slow",
        "background",
        "ribbon_bull",
        "ribbon_bear",
        "bull_down",
        "bear_up",
        "grid",
    ] {
        assert!(theme.get_field(name).is_ok(), "theme field {name} reads");
        assert!(theme.describe()["properties"].get(name).is_some());
    }
    theme.set_field("bull", "#FFFFFF").expect("color writes");
    assert_eq!(theme.bull, "#FFFFFF");
    assert!(theme.set_field("no_such_field", "x").is_err(), "unknown fields refuse");
}

#[test]
fn constraints_refuse_with_reasons() {
    assert!(
        PaneConfig::new().name(String::new()).build().validate().is_err(),
        "empty pane name refuses"
    );
    for share in [0.0, -0.5, 1.5, f64::NAN] {
        assert!(
            PaneConfig::new().name("price".to_owned()).height_share(share).build().validate().is_err(),
            "share {share} refuses"
        );
    }

    assert!(
        ThemeConfig::new().bull("red".to_owned()).build().validate().is_err(),
        "non-hex color refuses"
    );
    assert!(
        ThemeConfig::new().name(String::new()).build().validate().is_err(),
        "empty theme name refuses"
    );
    assert!(ThemeConfig::default().validate().is_ok(), "default theme validates");
}

#[test]
fn theme_resolves_style_keys() {
    let theme = ThemeConfig::default();
    assert_eq!(theme.style("bull"), Some("#20B46C"));
    assert_eq!(theme.style("bear"), Some("#B42020"));
    assert_eq!(theme.style("fast"), Some("#20C8F0"));
    assert_eq!(theme.style("slow"), Some("#B4F028"));
    assert_eq!(theme.style("background"), Some("#000000"));
    assert_eq!(theme.style("ribbon_bull"), Some("#00BCD4"));
    assert_eq!(theme.style("ribbon_bear"), Some("#7B1FA2"));
    assert_eq!(theme.style("bull_down"), Some("#FF9800"));
    assert_eq!(theme.style("bear_up"), Some("#2196F3"));
    assert_eq!(theme.style("grid"), Some("#2A2A2A"));
    assert_eq!(theme.style("rsi"), None, "domain names are not style keys");
}

#[test]
fn configs_are_components_without_hand_rolled_downcasts() {
    let pane: Arc<dyn Component> =
        Arc::new(PaneConfig::new().name("price".to_owned()).build());
    assert!(fluent_wvr::component_downcast_ref::<PaneConfig>(&*pane).is_some());
    let theme: Arc<dyn Component> = Arc::new(ThemeConfig::default());
    assert!(fluent_wvr::component_downcast_ref::<ThemeConfig>(&*theme).is_some());
    assert!(fluent_wvr::component_downcast_ref::<PaneConfig>(&*theme).is_none());
}

#[test]
fn pane_and_theme_execute_when_valid() {
    let ctx = WorkContext::default();
    let pane = PaneConfig::new().name("price".to_owned()).build();
    assert!(pane.execute(&ctx).expect("valid pane").success);
    assert!(ThemeConfig::default().execute(&ctx).expect("valid theme").success);
    let broken = PaneConfig::new().name("x".to_owned()).height_share(0.0).build();
    assert!(broken.execute(&ctx).is_err(), "invalid pane refuses at execute");
}
