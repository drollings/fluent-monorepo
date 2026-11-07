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
    assert!(
        theme.set_field("no_such_field", "x").is_err(),
        "unknown fields refuse"
    );
}

#[test]
fn constraints_refuse_with_reasons() {
    assert!(
        PaneConfig::new()
            .name(String::new())
            .build()
            .validate()
            .is_err(),
        "empty pane name refuses"
    );
    for share in [0.0, -0.5, 1.5, f64::NAN] {
        assert!(
            PaneConfig::new()
                .name("price".to_owned())
                .height_share(share)
                .build()
                .validate()
                .is_err(),
            "share {share} refuses"
        );
    }

    assert!(
        ThemeConfig::new()
            .bull("red".to_owned())
            .build()
            .validate()
            .is_err(),
        "non-hex color refuses"
    );
    assert!(
        ThemeConfig::new()
            .name(String::new())
            .build()
            .validate()
            .is_err(),
        "empty theme name refuses"
    );
    assert!(
        ThemeConfig::default().validate().is_ok(),
        "default theme validates"
    );
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
    assert_eq!(theme.style("rsi"), Some("#20C8F0"));
    assert_eq!(theme.style("cci"), Some("#B4F028"));
    assert_eq!(theme.style("cmo"), Some("#8CF050"));
    assert_eq!(theme.style("mfi"), Some("#F020F0"));
    assert_eq!(theme.style("stoch"), Some("#B4F0C8"));
    assert_eq!(theme.style("aroon"), Some("#FAF028"));
    assert_eq!(theme.style("adx"), Some("#20C8F0"));
}

#[test]
fn configs_are_components_without_hand_rolled_downcasts() {
    let pane: Arc<dyn Component> = Arc::new(PaneConfig::new().name("price".to_owned()).build());
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
    assert!(
        ThemeConfig::default()
            .execute(&ctx)
            .expect("valid theme")
            .success
    );
    let broken = PaneConfig::new()
        .name("x".to_owned())
        .height_share(0.0)
        .build();
    assert!(
        broken.execute(&ctx).is_err(),
        "invalid pane refuses at execute"
    );
}

#[test]
fn unknown_theme_key_refuses() {
    let theme = ThemeConfig::default();
    assert!(
        theme.style("no_such_key").is_none(),
        "unknown keys resolve to nothing"
    );
    let error =
        crate::paint::resolve_color(&theme, "no_such_key").expect_err("unknown style keys refuse");
    assert!(
        error.to_string().contains("no_such_key"),
        "the refusal names the offender: {error}"
    );
    assert!(
        theme.validate().is_ok(),
        "a valid theme stays valid after a refused lookup"
    );
}

#[test]
fn old_theme_json_loads_with_defaulted_chrome_slots() {
    let old = serde_json::json!({
        "name": "legacy",
        "bull": "#20B46C",
        "bear": "#B42020",
        "fast": "#20C8F0",
        "slow": "#B4F028",
        "background": "#000000",
        "ribbon_bull": "#00BCD4",
        "ribbon_bear": "#7B1FA2",
        "bull_down": "#FF9800",
        "bear_up": "#2196F3",
        "rsi": "#20C8F0",
        "cci": "#B4F028",
        "cmo": "#8CF050",
        "mfi": "#F020F0",
        "stoch": "#B4F0C8",
        "aroon": "#FAF028",
        "adx": "#20C8F0",
        "grid": "#2A2A2A"
    });
    let theme = crate::config::theme_from_json(&old.to_string())
        .expect("old JSON without chrome slots loads");
    assert_eq!(theme.name, "legacy");
    assert_eq!(theme.bull, "#20B46C");
    assert!(theme.validate().is_ok(), "defaulted slots validate");
    let fresh = ThemeConfig::default();
    assert_eq!(theme.surface, fresh.surface, "surface defaults");
    assert_eq!(theme.on_surface, fresh.on_surface, "on-surface defaults");
    assert_eq!(
        theme.outline_variant, fresh.outline_variant,
        "outline-variant defaults"
    );
}

#[test]
fn chrome_slots_refuse_malformed_hex_naming_the_slot() {
    for slot in ["surface", "on_surface", "outline_variant"] {
        for bad in ["red", "#FFF", "#GGGGGG", "", "#12345G", "20B46C"] {
            let mut theme = ThemeConfig::default();
            theme
                .set_field(slot, bad)
                .expect("any text writes through the text contract");
            let error = theme.validate().expect_err("malformed chrome slots refuse");
            assert!(
                error.to_string().contains(slot),
                "the refusal names {slot}: {error}"
            );
        }
        let mut theme = ThemeConfig::default();
        theme
            .set_field(slot, "#1A2B3C")
            .expect("well-formed hex writes");
        assert!(theme.validate().is_ok(), "a well-formed {slot} validates");
    }
}

#[test]
fn style_keys_lists_every_chrome_slot_exactly_once() {
    let keys = ThemeConfig::style_keys();
    for slot in ["surface", "on_surface", "outline_variant"] {
        assert_eq!(
            keys.iter().copied().filter(|key| *key == slot).count(),
            1,
            "{slot} appears exactly once: {keys:?}"
        );
        assert!(
            ThemeConfig::default().style(slot).is_some(),
            "{slot} resolves"
        );
    }
}

#[tokio::test]
async fn theme_file_round_trips_through_the_filesystem() {
    let dir = std::env::temp_dir().join("fluent_egui_theme_probe");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join("theme.json");
    std::fs::write(&path, r##"{"name": "file", "bull": "#123456"}"##).expect("theme writes");
    let theme = crate::config::load_theme_file(&path)
        .await
        .expect("theme file loads");
    assert_eq!(theme.name, "file", "the file body deserializes");
    assert_eq!(
        theme.bull, "#123456",
        "explicit slots survive the round trip"
    );
    assert!(theme.validate().is_ok(), "the loaded theme validates");
    std::fs::remove_file(&path).ok();
    let missing = dir.join("absent.json");
    let error = crate::config::load_theme_file(&missing)
        .await
        .expect_err("a missing file refuses");
    assert!(
        error.to_string().contains("absent.json"),
        "the refusal names the path: {error}"
    );
}
