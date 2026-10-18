use crate::indicator::{OwnedOhlcv, ParamSet};
use crate::smoothing::{params_for_kernel, SmoothingArgs, SmoothingFamily};

fn trend_frame(len: usize) -> OwnedOhlcv {
    let mut open = Vec::with_capacity(len);
    let mut high = Vec::with_capacity(len);
    let mut low = Vec::with_capacity(len);
    let mut close = Vec::with_capacity(len);
    let mut prev = 100.0;
    for i in 0..len {
        let shut = 100.0 + i as f64 * 0.7 + ((i * 13) % 5) as f64 * 0.9;
        let gap = if i > 0 && i % 10 == 0 { 2.5 } else { 0.0 };
        let unlocked = if i == 0 { shut } else { prev + gap + 0.1 };
        open.push(unlocked);
        high.push(unlocked.max(shut) + 0.8);
        low.push(unlocked.min(shut) - 0.8);
        close.push(shut);
        prev = shut;
    }
    OwnedOhlcv {
        open,
        high,
        low,
        close,
        volume: vec![1_000.0; len],
    }
}

#[test]
fn catalog_covers_every_factory_arm_with_kernel_or_recipe() {
    let rows: Vec<(&str, Option<&str>, bool, bool)> = vec![
        ("na", None, true, false),
        ("alma", Some("alma"), false, false),
        ("eit", Some("eit"), false, false),
        ("ema", Some("ema"), false, false),
        ("hma", Some("hma"), false, false),
        ("kalman", Some("kalman"), false, false),
        ("rma", Some("rma"), false, false),
        ("sma", Some("sma"), false, false),
        ("vwma", Some("vwma"), false, false),
        ("wma", Some("wma"), false, false),
        ("zlema", Some("ema"), false, true),
    ];
    assert_eq!(rows.len(), SmoothingFamily::all().len());
    for (id, kernel, passthrough, recipe) in rows {
        let family = SmoothingFamily::parse(id).expect("catalog covers the arm");
        assert_eq!(family.kernel_id(), kernel);
        assert_eq!(family.is_passthrough(), passthrough);
        assert_eq!(family.is_recipe(), recipe);
        // Pine `Smoothings` spelling and canonical id agree case-insensitively.
        assert_eq!(SmoothingFamily::parse(family.pine_name()), Some(family));
    }
    assert!(SmoothingFamily::parse("not-a-smoothing").is_none());
    assert!(SmoothingFamily::parse("").is_none());
}

#[test]
fn catalog_pine_names_match_the_shipped_factory_dispatch() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/protopyne-rs/libraries/_libMAs.pine");
    let text = std::fs::read_to_string(&path).expect("shipped mas library loads");
    let factory = text
        .split("export factorySmoothing")
        .nth(1)
        .expect("factory dispatch present");
    let mut arms = std::collections::BTreeSet::new();
    for piece in factory.split("UTIL.Smoothings.").skip(1) {
        let end = piece
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(piece.len());
        arms.insert(piece[..end].to_owned());
    }
    let catalog: std::collections::BTreeSet<String> = SmoothingFamily::all()
        .iter()
        .map(|family| family.pine_name().to_owned())
        .collect();
    assert_eq!(arms, catalog, "a pine arm edit fails here first");
}

#[test]
fn legacy_param_builders_match_catalog_for_all_seven_kinds() {
    // The hardcoded twin builders this catalog replaces: length everywhere,
    // plus reference ALMA offset/sigma. Fingerprints must agree exactly.
    for kind in ["sma", "ema", "wma", "hma", "rma", "vwma", "alma"] {
        let mut legacy = ParamSet::new().with_int("length", 14);
        if kind == "alma" {
            legacy = legacy.with_float("offset", 0.85).with_float("sigma", 6.0);
        }
        let family = SmoothingFamily::parse(kind).expect("legacy kind in catalog");
        let catalog_params = family.params_for_length(14).expect("length builds");
        assert_eq!(
            legacy.fingerprint(),
            catalog_params.fingerprint(),
            "{kind} catalog output pins the legacy builder"
        );
        let shared = params_for_kernel(kind, 14).expect("shared builder covers it");
        assert_eq!(legacy.fingerprint(), shared.fingerprint());
    }
    // Length-only arms cannot supply EIT's alpha.
    assert!(SmoothingFamily::Eit.params_for_length(14).is_err());
    assert!(params_for_kernel("eit", 14).is_err());
}

#[test]
fn param_bounds_refuse_before_any_kernel_runs() {
    for family in SmoothingFamily::all() {
        if family.takes_length() {
            assert!(family.params_for_length(0).is_err());
            assert!(family.params_for_length(-3).is_err());
            assert!(family
                .evaluate(&trend_frame(8), &SmoothingArgs::length(0))
                .is_err());
        }
    }
    let frame = trend_frame(16);
    // EIT with Pine-default args evaluates (n1 defaults to 0.85, a valid
    // alpha); only explicit out-of-range alphas refuse.
    assert!(SmoothingFamily::Eit
        .evaluate(&frame, &SmoothingArgs::length(14))
        .is_ok());
    assert!(SmoothingFamily::Eit
        .evaluate(&frame, &SmoothingArgs::length(14).with_n1(0.0))
        .is_err());
    assert!(SmoothingFamily::Eit
        .evaluate(&frame, &SmoothingArgs::length(14).with_n1(1.5))
        .is_err());
    assert!(SmoothingFamily::Alma
        .evaluate(&frame, &SmoothingArgs::length(14).with_n1(1.5))
        .is_err());
    assert!(SmoothingFamily::Alma
        .evaluate(&frame, &SmoothingArgs::length(14).with_param2(0.0))
        .is_err());
}

#[test]
fn catalog_evaluation_matches_direct_kernels_on_trend_frame() {
    use crate::indicator::builtin_registry;
    let frame = trend_frame(60);
    let registry = builtin_registry().expect("builtins register");
    let args = SmoothingArgs::length(14);
    for family in SmoothingFamily::all() {
        if family == SmoothingFamily::Eit {
            continue;
        }
        let got = family.evaluate(&frame, &args).expect("catalog evaluates");
        assert_eq!(got.len(), frame.len());
        match family {
            SmoothingFamily::Na => {
                let want: Vec<Option<f64>> = frame
                    .close
                    .iter()
                    .map(|v| crate::indicator::fin_or_none(*v))
                    .collect();
                assert_eq!(got, want, "pass-through is the close leg");
            }
            SmoothingFamily::Zlema => {
                let length = 14_usize;
                let lag = (length - 1) / 2;
                let derived: Vec<f64> = frame
                    .close
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        if i < lag {
                            f64::NAN
                        } else {
                            2.0 * c - frame.close[i - lag]
                        }
                    })
                    .collect();
                let ema = registry.get("ema").expect("ema registered");
                let params = ParamSet::new().with_int("length", 14);
                let want = ema
                    .batch(&crate::indicator::Ohlcv::close_only(&derived), &params)
                    .expect("ema evaluates")
                    .values;
                assert_eq!(got, want, "zlema composes ema over the derived series");
            }
            _ => {
                let kernel = registry
                    .get(family.kernel_id().expect("kernel arm"))
                    .expect("registered");
                let params = family.params_for_length(14).expect("params build");
                let want = kernel
                    .batch(&frame.views(), &params)
                    .expect("kernel evaluates")
                    .values;
                assert_eq!(got, want, "{family:?} catalog equals direct batch");
            }
        }
    }
    // EIT evaluates through its alpha arm.
    let eit = SmoothingFamily::Eit
        .evaluate(&frame, &SmoothingArgs::length(14).with_n1(0.12))
        .expect("eit evaluates");
    let kernel = registry.get("eit").expect("eit registered");
    let want = kernel
        .batch(&frame.views(), &ParamSet::new().with_float("alpha", 0.12))
        .expect("kernel evaluates")
        .values;
    assert_eq!(eit, want);
}

#[test]
fn warmup_delegates_to_kernels_and_composes_for_the_recipe() {
    use crate::indicator::builtin_registry;
    let registry = builtin_registry().expect("builtins register");
    for family in SmoothingFamily::all() {
        let warmup = family.warmup_for_length(14).expect("warmup reads");
        match family {
            SmoothingFamily::Zlema => assert_eq!(warmup, 14 + (14 - 1) / 2),
            SmoothingFamily::Na | SmoothingFamily::Eit | SmoothingFamily::Kalman => {
                assert_eq!(warmup, 1);
            }
            _ => {
                let kernel = registry
                    .get(family.kernel_id().expect("kernel arm"))
                    .expect("registered");
                let params = family.params_for_length(14).expect("params build");
                assert_eq!(warmup, kernel.warmup(&params).expect("kernel warmup"));
            }
        }
    }
    assert!(SmoothingFamily::Sma.warmup_for_length(0).is_err());
}

#[test]
fn catalog_describes_every_family_with_param_specs() {
    use fluent_wvr::Describable as _;
    let doc = crate::smoothing::SmoothingCatalog.describe();
    let families = doc
        .get("families")
        .expect("families key")
        .as_array()
        .expect("array");
    assert_eq!(families.len(), SmoothingFamily::all().len());
    for family in SmoothingFamily::all() {
        let entry = families
            .iter()
            .find(|row| row.get("id").and_then(|id| id.as_str()) == Some(family.id()));
        let entry = entry.unwrap_or_else(|| panic!("{family:?} described"));
        assert_eq!(
            entry.get("pine").and_then(|pine| pine.as_str()),
            Some(family.pine_name())
        );
        assert!(entry
            .get("params")
            .and_then(|params| params.as_array())
            .is_some());
    }
    let single = SmoothingFamily::Alma.describe();
    assert_eq!(single.get("id").and_then(|id| id.as_str()), Some("alma"));
}
