use super::*;
use crate::config::ThemeConfig;

fn token_set() -> MaterialTokens {
    let mut tokens = MaterialTokens::new()
        .mode("dark".to_owned())
        .contrast(0.0)
        .build();
    tokens
        .insert_role("primary", "#6750A4")
        .expect("primary registers");
    tokens
        .insert_role("surface", "#141218")
        .expect("surface registers");
    tokens
}

#[test]
fn unknown_roles_refuse_and_well_formed_roles_resolve() {
    let mut tokens = token_set();
    for bad in ["nope", "", "Primary", "primary "] {
        let error = tokens
            .insert_role(bad, "#FFFFFF")
            .expect_err("unknown roles refuse");
        assert!(
            error.to_string().contains(bad.trim()),
            "the refusal names the role: {error}"
        );
    }
    for key in known_roles() {
        tokens
            .insert_role(key, "#1A2B3C")
            .unwrap_or_else(|error| panic!("{key} is a known role: {error}"));
    }
    assert!(
        tokens.resolve(true).is_ok(),
        "every well-formed role resolves"
    );
    let smuggled: MaterialTokens = serde_json::from_str(
        r##"{"mode": "dark", "contrast": 0.0, "roles": {"smuggled": "#FFFFFF"}}"##,
    )
    .expect("serde reads the map");
    assert!(
        smuggled.validate().is_err(),
        "serde-smuggled roles still refuse at validate"
    );
}

#[test]
fn malformed_hex_refuses_naming_the_role() {
    let mut tokens = token_set();
    for bad in ["red", "#FFF", "#GGGGGG", "", "#12345G", "20B46C"] {
        let error = tokens
            .insert_role("primary", bad)
            .expect_err("malformed hex refuses");
        assert!(
            error.to_string().contains("primary"),
            "the refusal names the role: {error}"
        );
    }
    assert!(
        tokens.resolve(true).is_ok(),
        "refused writes never land, so the set still resolves"
    );
}

#[test]
fn identical_inputs_resolve_to_identical_schemes() {
    let first = token_set().resolve(true).expect("resolves");
    let second = token_set().resolve(true).expect("resolves");
    assert_eq!(first, second, "bit-identical input resolves identically");
    assert_eq!(
        first.fingerprint, second.fingerprint,
        "the fingerprint follows the input"
    );
    assert!(first.dark, "the dark flag carries through");
    assert_eq!(
        first.colors.get("primary"),
        Some(&egui::Color32::from_rgb(0x67, 0x50, 0xA4)),
        "roles parse to bytes"
    );
}

#[test]
fn auto_resolves_through_the_injected_detector() {
    let auto = MaterialTokens::new()
        .mode("auto".to_owned())
        .contrast(0.0)
        .build();
    assert!(
        auto.resolve_auto(|| true).expect("resolves").dark,
        "the detector picks dark"
    );
    assert!(
        !auto.resolve_auto(|| false).expect("resolves").dark,
        "the detector picks light"
    );
    let fixed = MaterialTokens::new()
        .mode("dark".to_owned())
        .contrast(0.0)
        .build();
    assert!(
        fixed.resolve_auto(|| false).expect("resolves").dark,
        "an explicit mode wins over the detector"
    );
    let broken = MaterialTokens::new()
        .mode("midnight".to_owned())
        .contrast(0.0)
        .build();
    assert!(
        broken.resolve(true).is_err(),
        "an unknown mode refuses instead of guessing"
    );
}

#[test]
fn chart_projection_agrees_with_the_theme_on_overlap_keys() {
    let theme = ThemeConfig::default();
    let mut tokens = MaterialTokens::new()
        .mode("light".to_owned())
        .contrast(0.0)
        .build();
    for key in ThemeConfig::style_keys() {
        tokens
            .insert_role(key, theme.style(key).expect("a theme key resolves"))
            .unwrap_or_else(|error| panic!("{key} is a known role: {error}"));
    }
    let scheme = tokens.resolve(false).expect("resolves");
    let projected = chart_roles(&scheme);
    for key in ThemeConfig::style_keys() {
        let expected = crate::paint::parse_hex_color(key, theme.style(key).expect("resolves"))
            .expect("parses");
        assert_eq!(
            projected.get(*key),
            Some(&expected),
            "{key} projects to the theme value"
        );
    }
    let mut shelled = MaterialTokens::new()
        .mode("dark".to_owned())
        .contrast(0.0)
        .build();
    shelled
        .insert_role("primary", "#6750A4")
        .expect("shell roles register");
    let shell_projected = chart_roles(&shelled.resolve(true).expect("resolves"));
    assert!(
        !shell_projected.contains_key("primary"),
        "shell-only roles stay out of the chart projection"
    );
}

#[test]
fn visuals_apply_the_surface_and_primary_slots() {
    let scheme = token_set().resolve(true).expect("resolves");
    let visuals = apply_visuals(&scheme);
    assert_eq!(
        visuals.window_fill,
        egui::Color32::from_rgb(0x14, 0x12, 0x18),
        "the surface slot fills the window"
    );
}
