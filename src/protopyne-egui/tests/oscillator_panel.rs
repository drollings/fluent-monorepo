use super::*;
use fluent_egui::schema_fields;
use fluent_wvr::Describable;

use crate::oscillator_panel::{KindDefaults, OscillatorPanelState, PANEL_KINDS};

fn ramp_rows(count: i64) -> Vec<ChartRow> {
    (0..count)
        .map(|i| {
            let close = 100.0 + i as f64;
            ChartRow {
                time: i,
                open: close - 0.5,
                high: close + 0.5,
                low: close - 0.5,
                close,
                volume: 10,
            }
        })
        .collect()
}

/// The panel opens neutral: the registry length (14 on every panel kind),
/// unarmed smoothing, unit gain, and today's hues. Neutral means unevaluated
/// slots cannot move when the panel appears.
#[test]
fn kind_defaults_carry_the_neutral_defaults() {
    let panel = OscillatorPanelState::new().build();
    for kind in PANEL_KINDS {
        let entry = panel.entry(kind).expect("every listed kind has an entry");
        assert_eq!(entry.period, 14, "{kind} templates the registry length");
        assert_eq!(entry.smooth_type, "na", "{kind} smoothing starts disarmed");
        assert_eq!(
            entry.smooth_period, 3,
            "{kind} smoothing window matches the leg standard"
        );
        assert!(
            (entry.gain - 1.0).abs() < f64::EPSILON,
            "{kind} gain starts neutral"
        );
    }
    assert_eq!(PANEL_KINDS.len(), 7, "rsi cci cmo mfi stoch aroon adx");
}

/// Length dials lift the shared 2..200 window (slider-eligible), smoothing
/// types lift the catalog (dropdown), and every refusal names its field.
#[test]
fn panel_schema_bounds_name_the_fields() {
    use fluent_egui::FieldKind;
    let entry = KindDefaults::new().build();
    assert!(entry.validate().is_ok(), "defaults validate");
    let lifted = schema_fields(&entry.describe());
    for name in ["period", "smooth_period"] {
        let field = lifted
            .iter()
            .find(|field| field.name == name)
            .unwrap_or_else(|| panic!("{name} lifts"));
        assert_eq!(field.kind, FieldKind::Integer, "{name} edits whole bars");
        assert_eq!(
            (field.minimum, field.maximum),
            (Some(2.0), Some(200.0)),
            "{name} shares the MA window"
        );
    }
    let smooth = lifted
        .iter()
        .find(|field| field.name == "smooth_type")
        .expect("smoothing lifts");
    assert_eq!(smooth.kind, FieldKind::Select);
    assert!(!smooth.options.is_empty(), "the catalog backs the dropdown");
    let gain = lifted
        .iter()
        .find(|field| field.name == "gain")
        .expect("gain lifts");
    assert_eq!(gain.kind, FieldKind::Number);
    assert_eq!(gain.minimum, Some(0.0), "the gain floor lifts");
    for period in [1, 201] {
        for (label, candidate) in [
            ("period", KindDefaults::new().period(period).build()),
            (
                "smooth_period",
                KindDefaults::new().smooth_period(period).build(),
            ),
        ] {
            let error = format!("{}", candidate.validate().expect_err("refuses"));
            assert!(
                error.contains(label),
                "{label}={period} names the field: {error}"
            );
        }
    }
    for smooth in ["not-a-smoothing", "eit"] {
        let candidate = KindDefaults::new().smooth_type(smooth.to_owned()).build();
        let error = format!("{}", candidate.validate().expect_err("refuses"));
        assert!(
            error.contains("smooth_type"),
            "{smooth} names the field: {error}"
        );
    }
    for gain in [0.0, -2.0, f64::NAN, f64::INFINITY] {
        let candidate = KindDefaults::new().gain(gain).build();
        let error = format!("{}", candidate.validate().expect_err("refuses"));
        assert!(
            error.contains("gain"),
            "gain {gain} names the field: {error}"
        );
    }
    for gain in [0.2, 1.0, 1.2] {
        assert!(
            KindDefaults::new().gain(gain).build().validate().is_ok(),
            "gain {gain} validates"
        );
    }
}

/// Panel edits persist: a mutated entry round-trips through the prefs
/// document onto a fresh workspace.
#[test]
fn panel_state_round_trips_through_prefs() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(40)).expect("workspace boots");
    workspace
        .oscillator_panel
        .entry_mut("rsi")
        .expect("rsi entry")
        .period = 21;
    let prefs = workspace.prefs();
    assert_eq!(prefs.oscillators.rsi.period, 21, "the prefs carry the edit");
    let mut fresh = ChartWorkspace::with_rows(ramp_rows(40)).expect("fresh workspace boots");
    fresh.apply_prefs(&prefs).expect("prefs apply");
    assert_eq!(
        fresh
            .oscillator_panel
            .entry("rsi")
            .expect("rsi entry")
            .period,
        21,
        "a fresh workspace restores the edit"
    );
}

/// The dock renders the Oscillators section headlessly without refusing.
#[test]
fn dock_paints_the_oscillators_section() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(40)).expect("workspace boots");
    workspace.dock_open = true;
    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(egui::RawInput::default(), |root| {
        egui::CentralPanel::default().show(root, |ui| {
            workspace.paint_dock_contents(ui, None);
        });
    });
    output.textures_delta.clear();
    assert!(!output.shapes.is_empty(), "the dock paints its sections");
}

/// A blank gain follows the kind default; an explicit gain wins. The frozen
/// catalog stays the last resort only through the kind neutral.
#[test]
fn blank_gain_follows_the_kind_default() {
    use crate::oscillator_panel::KindDefaults;
    let slot = crate::indicators::KernelIndicatorConfig::defaults_for("rsi");
    assert_eq!(slot.osc_gain, None, "gain defaults to following");
    let neutral = KindDefaults::new().build();
    let resolved = slot.resolve_panel_defaults(&neutral);
    assert_eq!(resolved.osc_gain, Some(1.0), "neutral kind means parity");
    let boosted = KindDefaults::new().gain(1.5).build();
    let resolved = slot.resolve_panel_defaults(&boosted);
    assert_eq!(resolved.osc_gain, Some(1.5), "blank gain follows the kind");
    let mut explicit = crate::indicators::KernelIndicatorConfig::defaults_for("rsi");
    explicit
        .set_field("osc_gain", "2")
        .expect("explicit gain writes");
    let resolved = explicit.resolve_panel_defaults(&boosted);
    assert_eq!(resolved.osc_gain, Some(2.0), "explicit gain wins");
}

/// Slot `NA` smoothing follows an armed kind; an armed slot always wins;
/// mutual `NA` stays off (reproducing today).
#[test]
fn slot_na_smoothing_follows_armed_kind_smoothing() {
    use crate::oscillator_panel::KindDefaults;
    let slot = crate::indicators::KernelIndicatorConfig::defaults_for("rsi");
    let off = KindDefaults::new().build();
    let resolved = slot.resolve_panel_defaults(&off);
    assert_eq!(resolved.smooth_type, "na", "mutual NA stays off");
    assert_eq!(resolved.smooth_period, 3);
    let armed = KindDefaults::new()
        .smooth_type("sma".to_owned())
        .smooth_period(7)
        .build();
    let resolved = slot.resolve_panel_defaults(&armed);
    assert_eq!(resolved.smooth_type, "sma", "NA follows the armed kind");
    assert_eq!(resolved.smooth_period, 7);
    let mut explicit = crate::indicators::KernelIndicatorConfig::defaults_for("rsi");
    explicit.set_field("smooth_type", "wma").expect("writes");
    explicit.set_field("smooth_period", "5").expect("writes");
    let resolved = explicit.resolve_panel_defaults(&armed);
    assert_eq!(resolved.smooth_type, "wma", "an armed slot wins");
    assert_eq!(resolved.smooth_period, 5);
}

/// Live follow: a kind edit rebuilds following slots and spares overriding
/// ones — values and cache keys both.
#[test]
fn kind_edits_rebuild_followers_and_spare_overriders() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    workspace.slot_indicator("rsi").expect("rsi slots");
    workspace.recompute().expect("evaluations build");
    let read = |workspace: &ChartWorkspace| {
        let evaluation = workspace.evaluations.get("rsi").expect("rsi evaluated");
        let series = evaluation.series.first().expect("one series");
        (series.key, series.display_values.clone())
    };
    let (key_before, values_before) = read(&workspace);
    workspace
        .oscillator_panel
        .entry_mut("rsi")
        .expect("rsi entry")
        .gain = 1.5;
    workspace.recompute().expect("rebuilds");
    let (key_after, values_after) = read(&workspace);
    assert_ne!(key_after, key_before, "a kind edit moves the follower key");
    assert_ne!(
        values_after, values_before,
        "a kind edit moves the follower picture"
    );
    workspace
        .stack
        .set_slot_field("rsi", "osc_gain", "2")
        .expect("explicit gain writes");
    workspace.recompute().expect("rebuilds");
    let (key_explicit, values_explicit) = read(&workspace);
    workspace
        .oscillator_panel
        .entry_mut("rsi")
        .expect("rsi entry")
        .gain = 2.0;
    workspace.recompute().expect("rebuilds");
    let (key_final, values_final) = read(&workspace);
    assert_eq!(key_final, key_explicit, "overriders keep their key");
    assert_eq!(
        values_final, values_explicit,
        "overriders keep their picture"
    );
}

/// The panel period templates new slots of that kind.
#[test]
fn panel_period_templates_new_slots() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(60)).expect("workspace boots");
    workspace
        .oscillator_panel
        .entry_mut("rsi")
        .expect("rsi entry")
        .period = 21;
    workspace.slot_indicator("rsi").expect("rsi slots");
    let config = workspace.stack.config("rsi").expect("config lives");
    let kernel_config: &crate::indicators::KernelIndicatorConfig =
        fluent_wvr::component_downcast_ref(config).expect("kernel config");
    assert_eq!(
        kernel_config.params.get("length").map(String::as_str),
        Some("21"),
        "new slots template the panel period"
    );
}

/// Blanks survive prefs: a following gain serializes empty and restores
/// empty, never a fabricated value.
#[test]
fn blanks_round_trip_through_prefs() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(60)).expect("workspace boots");
    workspace.slot_indicator("rsi").expect("rsi slots");
    let prefs = workspace.prefs();
    let ticker = prefs.tickers.get("SPY").expect("active ticker saved");
    let slot = ticker
        .slots
        .iter()
        .find(|slot| slot.id == "rsi")
        .expect("rsi slot saved");
    assert_eq!(
        slot.config.get("osc_gain").map(String::as_str),
        Some(""),
        "a following gain persists as blank"
    );
    let mut fresh = ChartWorkspace::with_rows(ramp_rows(60)).expect("fresh boots");
    fresh.apply_prefs(&prefs).expect("prefs apply");
    let config = fresh.stack.config("rsi").expect("config lives");
    let kernel_config: &crate::indicators::KernelIndicatorConfig =
        fluent_wvr::component_downcast_ref(config).expect("kernel config");
    assert_eq!(
        kernel_config.osc_gain, None,
        "a fresh workspace restores the blank"
    );
}

/// Panel triple defaults equal the frozen catalog rows per normalized kind;
/// unmapped kinds carry valid but hidden placeholders (never resolved).
#[test]
fn panel_triple_defaults_match_the_catalog_rows() {
    use crate::oscillator_panel::KindDefaults;
    for (kind, offset, k, mult) in [
        ("rsi", -50.0, 0.0084, 740.0),
        ("cci", -50.0, 0.0035, 500.0),
        ("cmo", 0.0, 0.002, 1000.0),
        ("mfi", -50.0, 0.0084, 740.0),
        ("stoch", -50.0, 0.0005, 7000.0),
        ("aroon", -50.0, 0.005, 800.0),
    ] {
        let entry = KindDefaults::for_kind(kind);
        assert!(
            (entry.offset.expect("triple set") - offset).abs() < f64::EPSILON,
            "{kind} offset defaults to the catalog row"
        );
        assert!(
            (entry.k.expect("triple set") - k).abs() < f64::EPSILON,
            "{kind} steepness defaults to the catalog row"
        );
        assert!(
            (entry.mult.expect("triple set") - mult).abs() < f64::EPSILON,
            "{kind} band defaults to the catalog row"
        );
        assert!(
            (entry.gain - 1.0).abs() < f64::EPSILON,
            "{kind} gain stays neutral"
        );
    }
    {
        let entry = KindDefaults::for_kind("adx");
        assert!(entry.validate().is_ok(), "adx placeholders validate");
        assert!(
            crate::oscillator_panel::hides_dial("adx", "offset")
                && crate::oscillator_panel::hides_dial("adx", "k")
                && crate::oscillator_panel::hides_dial("adx", "mult")
                && crate::oscillator_panel::hides_dial("adx", "gain"),
            "adx hides the inapplicable triple and gain"
        );
        assert!(
            !crate::oscillator_panel::hides_dial("rsi", "mult"),
            "normalized kinds show the whole row"
        );
    }
}

/// A blank slot triple follows a panel edit bar-for-bar; an explicit triple
/// never moves; unmapped kernels never inherit placeholder zeros.
#[test]
fn blank_slot_triple_follows_the_panel() {
    use crate::oscillator_panel::KindDefaults;
    let slot = crate::indicators::KernelIndicatorConfig::defaults_for("rsi");
    assert_eq!(slot.osc_offset, None);
    assert_eq!(slot.osc_k, None);
    assert_eq!(slot.osc_mult, None);
    let mut shaped = KindDefaults::for_kind("rsi");
    shaped.offset = Some(-40.0);
    shaped.k = Some(0.01);
    shaped.mult = Some(800.0);
    let resolved = slot.resolve_panel_defaults(&shaped);
    assert_eq!(resolved.osc_offset, Some(-40.0));
    assert_eq!(resolved.osc_k, Some(0.01));
    assert_eq!(resolved.osc_mult, Some(800.0));
    let mut explicit = crate::indicators::KernelIndicatorConfig::defaults_for("rsi");
    explicit.set_field("osc_offset", "-10").expect("writes");
    explicit.set_field("osc_k", "0.02").expect("writes");
    explicit.set_field("osc_mult", "900").expect("writes");
    let resolved = explicit.resolve_panel_defaults(&shaped);
    assert_eq!(resolved.osc_offset, Some(-10.0), "explicit offset wins");
    assert_eq!(resolved.osc_k, Some(0.02), "explicit steepness wins");
    assert_eq!(resolved.osc_mult, Some(900.0), "explicit band wins");
    let unmapped = crate::indicators::KernelIndicatorConfig::defaults_for("mom");
    let resolved = unmapped.resolve_panel_defaults(&KindDefaults::for_kind("rsi"));
    assert_eq!(resolved.osc_offset, None, "unmapped slots keep blanks");
    assert_eq!(resolved.osc_k, None, "unmapped slots keep blanks");
    assert_eq!(resolved.osc_mult, None, "unmapped slots keep blanks");
}

/// Triple refusals name the field; valid triples validate.
#[test]
fn triple_refusals_name_the_field() {
    use crate::oscillator_panel::KindDefaults;
    for (field, value) in [
        ("offset", f64::NAN),
        ("offset", f64::INFINITY),
        ("k", 0.0),
        ("k", -0.01),
        ("k", f64::NAN),
        ("mult", 0.0),
        ("mult", -700.0),
    ] {
        let mut candidate = KindDefaults::for_kind("rsi");
        match field {
            "offset" => candidate.offset = Some(value),
            "k" => candidate.k = Some(value),
            _ => candidate.mult = Some(value),
        }
        let error = format!("{}", candidate.validate().expect_err("refuses"));
        assert!(
            error.contains(field),
            "{field}={value} names the field: {error}"
        );
    }
}

/// Switching a Lite leg kind re-evaluates that leg through the new kind and
/// repaints under its theme key; the other leg never moves.
#[test]
fn lite_leg_kind_switch_rebuilds_that_leg_only() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    assert!(workspace
        .stack
        .set_slot_field("integration_lite", "fast_kind", "mfi")
        .is_ok());
    // An unknown kind refuses at the contract, never substitutes.
    assert!(workspace
        .stack
        .set_slot_field("integration_lite", "fast_kind", "nope")
        .is_ok());
    assert!(
        workspace.recompute().is_err(),
        "unknown kinds refuse loudly"
    );
    assert!(workspace
        .stack
        .set_slot_field("integration_lite", "fast_kind", "mfi")
        .is_ok());
    workspace.recompute().expect("rebuilds");
    let pane = workspace.sub_pane.as_ref().expect("sub pane builds");
    assert!(!pane.fast_bounded.iter().all(Option::is_none));
    let mut keys = workspace.painted_style_keys("integration_lite");
    keys.sort();
    assert!(
        keys.contains(&"mfi".to_owned()),
        "legs paint through the kind key: {keys:?}"
    );
    assert!(
        keys.contains(&"cci".to_owned()),
        "the slow leg keeps its key: {keys:?}"
    );
}

/// Panel smoothing and gain reach the Lite legs: an armed kind smoothing
/// smooths the fast leg bar-for-bar against the smoothing kernel, and a
/// kind gain scales it.
#[test]
fn panel_smoothing_and_gain_reach_the_lite_legs() {
    use crate::integration_lite::lite_native_series;
    use crate::oscillators::OscillatorKind;
    use fluent_egui::band_squash;
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    workspace
        .oscillator_panel
        .entry_mut("rsi")
        .expect("rsi entry")
        .smooth_type = "sma".to_owned();
    workspace
        .oscillator_panel
        .entry_mut("rsi")
        .expect("rsi entry")
        .smooth_period = 7;
    workspace
        .oscillator_panel
        .entry_mut("rsi")
        .expect("rsi entry")
        .gain = 1.5;
    workspace.recompute().expect("rebuilds");
    let pane = workspace.sub_pane.as_ref().expect("sub pane builds");
    // Manual pipeline: RSI raw, SMA(7) smoothed, gain-scaled, squashed.
    let rows = ramp_rows(120);
    let native = lite_native_series(&rows, 0.14).expect("native builds");
    let raw = &native.fast_raw;
    let leading = raw.iter().take_while(|slot| slot.is_none()).count();
    let expected: Vec<Option<f64>> = crate::topology::series_frame(&rows, raw)
        .ok()
        .and_then(|frame| {
            use protopyne_kernels::smoothing::SmoothingFamily;
            let family = SmoothingFamily::parse("sma").expect("parses");
            family
                .evaluate(
                    &frame,
                    &protopyne_kernels::smoothing::SmoothingArgs::length(7),
                )
                .ok()
        })
        .map(|smoothed| {
            let mut out: Vec<Option<f64>> = smoothed
                .iter()
                .map(|slot| {
                    slot.and_then(|value| {
                        crate::oscillators::normalize_oscillator(value, OscillatorKind::Rsi)
                            .ok()
                            .and_then(|norm| {
                                let shaped = band_squash(norm * 1.5, 100.0, 100.0);
                                shaped.is_finite().then_some(shaped)
                            })
                    })
                })
                .collect();
            for value in out.iter_mut().take(leading + 7) {
                *value = None;
            }
            out
        })
        .expect("manual pipeline builds");
    assert_eq!(
        pane.fast_bounded, expected,
        "the fast leg equals the smoothing kernel, the gain, then the band"
    );
    assert!(pane.fast_bounded.iter().any(Option::is_some));
}

/// A panel edit moves following Lite legs (and their cache keys) while the
/// frozen calibration windows keep their verdicts.
#[test]
fn lite_panel_edits_move_follower_keys() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    workspace.recompute().expect("evaluations build");
    let read = |workspace: &ChartWorkspace| {
        workspace
            .evaluations
            .get("integration_lite")
            .expect("lite evaluated")
            .series
            .first()
            .expect("one series")
            .key
    };
    let before = read(&workspace);
    workspace
        .oscillator_panel
        .entry_mut("rsi")
        .expect("rsi entry")
        .gain = 1.5;
    workspace.recompute().expect("rebuilds");
    assert_ne!(
        read(&workspace),
        before,
        "a kind gain moves the follower key"
    );
    workspace
        .stack
        .set_slot_field("integration_lite", "fast_k", "2")
        .expect("explicit gain writes");
    workspace.recompute().expect("rebuilds");
    let pinned = read(&workspace);
    workspace
        .oscillator_panel
        .entry_mut("rsi")
        .expect("rsi entry")
        .gain = 3.0;
    workspace.recompute().expect("rebuilds");
    assert_eq!(read(&workspace), pinned, "explicit leg gains never move");
}

/// Aroon left the unshaped set: its panel row shows the full triple and
/// gain, and only adx stays hidden.
#[test]
fn aroon_row_shows_the_full_triple() {
    use crate::oscillator_panel::{hides_dial, PANEL_UNSHAPED_KINDS};
    assert_eq!(
        PANEL_UNSHAPED_KINDS.to_vec(),
        vec!["adx"],
        "only adx stays unshaped"
    );
    for field in ["gain", "offset", "k", "mult"] {
        assert!(
            !hides_dial("aroon", field),
            "aroon shows {field} now that it normalizes"
        );
        assert!(hides_dial("adx", field), "adx still hides {field}");
    }
    for field in ["period", "smooth_type", "smooth_period"] {
        assert!(!hides_dial("aroon", field), "aroon keeps {field}");
    }
}

/// A blank aroon triple follows the panel; an explicit triple never moves.
#[test]
fn aroon_slots_follow_the_panel_triple() {
    use crate::indicators::KernelIndicatorConfig;
    use crate::oscillator_panel::OscillatorPanelState;
    let panel = OscillatorPanelState::new().build();
    let slot = KernelIndicatorConfig::defaults_for("aroon");
    assert_eq!(slot.osc_offset, None);
    assert_eq!(slot.osc_k, None);
    assert_eq!(slot.osc_mult, None);
    let resolved = slot.resolve_panel_defaults(panel.entry("aroon").expect("aroon row"));
    assert_eq!(resolved.osc_offset, Some(-50.0));
    assert_eq!(resolved.osc_k, Some(0.005));
    assert_eq!(resolved.osc_mult, Some(800.0));
    let mut edited = panel;
    edited.entry_mut("aroon").expect("aroon row").offset = Some(-40.0);
    let resolved = slot.resolve_panel_defaults(edited.entry("aroon").expect("aroon row"));
    assert_eq!(
        resolved.osc_offset,
        Some(-40.0),
        "a panel edit moves followers"
    );
    assert_eq!(resolved.osc_gain, Some(1.0), "gain still follows neutral");
    let mut explicit = KernelIndicatorConfig::defaults_for("aroon");
    explicit.set_field("osc_offset", "-10").expect("writes");
    let resolved = explicit.resolve_panel_defaults(edited.entry("aroon").expect("aroon row"));
    assert_eq!(
        resolved.osc_offset,
        Some(-10.0),
        "explicit triples never move"
    );
}

/// A panel triple edit moves following aroon slots and leaves overriding
/// ones untouched — values and cache keys both.
#[test]
fn aroon_panel_triple_moves_followers_only() {
    let mut workspace = ChartWorkspace::with_rows(ramp_rows(120)).expect("workspace boots");
    workspace.slot_indicator("aroon").expect("aroon slots");
    workspace.recompute().expect("evaluations build");
    let read = |workspace: &ChartWorkspace| {
        let evaluation = workspace.evaluations.get("aroon").expect("aroon evaluates");
        let series = evaluation.series.first().expect("one series");
        (series.key, series.display_values.clone())
    };
    let (key_before, values_before) = read(&workspace);
    assert!(
        values_before.iter().any(Option::is_some),
        "aroon paints past warmup"
    );
    workspace
        .oscillator_panel
        .entry_mut("aroon")
        .expect("aroon row")
        .mult = Some(900.0);
    workspace.recompute().expect("rebuilds");
    let (key_after, values_after) = read(&workspace);
    assert_ne!(key_after, key_before, "a panel edit moves the follower key");
    assert_ne!(
        values_after, values_before,
        "a panel edit moves the follower picture"
    );
    workspace
        .stack
        .set_slot_field("aroon", "osc_mult", "740")
        .expect("explicit triple writes");
    workspace.recompute().expect("rebuilds");
    let (key_pinned, values_pinned) = read(&workspace);
    workspace
        .oscillator_panel
        .entry_mut("aroon")
        .expect("aroon row")
        .mult = Some(700.0);
    workspace.recompute().expect("rebuilds");
    assert_eq!(read(&workspace).0, key_pinned, "overriders keep their key");
    assert_eq!(
        read(&workspace).1,
        values_pinned,
        "overriders keep their picture"
    );
}
