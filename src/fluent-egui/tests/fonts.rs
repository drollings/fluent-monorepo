use super::*;

use egui::epaint::text::FontInsert;

const BYTES: &[u8] = b"fake-font-bytes";

fn highest_families(insert: &FontInsert) -> Vec<(String, String)> {
    insert
        .families
        .iter()
        .map(|entry| {
            (
                format!("{:?}", entry.family),
                format!("{:?}", entry.priority),
            )
        })
        .collect()
}

#[test]
fn bundled_insert_is_highest_for_both_families() {
    let insert = bundled_font_insert("NotoSans", BYTES).expect("valid insert");
    assert_eq!(insert.name, "NotoSans");
    assert_eq!(
        highest_families(&insert),
        vec![
            ("Proportional".to_owned(), "Highest".to_owned()),
            ("Monospace".to_owned(), "Highest".to_owned()),
        ]
    );
    assert!(!insert.data.font.is_empty());
}

#[test]
fn per_family_inserts_cover_exactly_one_family() {
    let proportional = proportional_font_insert("NotoSans", BYTES).expect("valid insert");
    assert_eq!(highest_families(&proportional).len(), 1);
    assert_eq!(
        highest_families(&proportional)[0].0,
        format!("{:?}", egui::FontFamily::Proportional)
    );
    let monospace = monospace_font_insert("NotoSansMono", BYTES).expect("valid insert");
    assert_eq!(highest_families(&monospace).len(), 1);
    assert_eq!(
        highest_families(&monospace)[0].0,
        format!("{:?}", egui::FontFamily::Monospace)
    );
}

#[test]
fn blank_name_is_refused() {
    for name in ["", "   "] {
        assert_eq!(
            bundled_font_insert(name, BYTES).unwrap_err(),
            EguiError::InvalidParam {
                what: "font name".to_owned(),
                reason: "must not be blank".to_owned(),
            }
        );
        assert_eq!(
            with_bundled_font(egui::FontDefinitions::empty(), name, BYTES),
            Err(EguiError::InvalidParam {
                what: "font name".to_owned(),
                reason: "must not be blank".to_owned(),
            })
        );
    }
}

#[test]
fn empty_bytes_are_refused() {
    assert_eq!(
        bundled_font_insert("NotoSans", b"").unwrap_err(),
        EguiError::InvalidParam {
            what: "font bytes".to_owned(),
            reason: "must not be empty".to_owned(),
        }
    );
}

#[test]
fn empty_family_list_is_refused() {
    assert_eq!(
        font_insert("NotoSans", BYTES, Vec::new()).unwrap_err(),
        EguiError::InvalidParam {
            what: "font families".to_owned(),
            reason: "at least one family is required".to_owned(),
        }
    );
}

#[test]
fn split_priority_insert_names_both_legs() {
    let insert = primary_with_fallback_insert(
        "NotoSansMono",
        BYTES,
        egui::FontFamily::Monospace,
        egui::FontFamily::Proportional,
    )
    .expect("valid insert");
    assert_eq!(
        highest_families(&insert),
        vec![
            ("Monospace".to_owned(), "Highest".to_owned()),
            ("Proportional".to_owned(), "Lowest".to_owned()),
        ]
    );
}

#[test]
fn split_priority_insert_refuses_identical_families() {
    assert_eq!(
        primary_with_fallback_insert(
            "NotoSansMono",
            BYTES,
            egui::FontFamily::Monospace,
            egui::FontFamily::Monospace,
        )
        .unwrap_err(),
        EguiError::InvalidParam {
            what: "font families".to_owned(),
            reason: "primary and fallback must differ".to_owned(),
        }
    );
}

#[test]
fn with_bundled_font_prepends_and_keeps_fallbacks() {
    let base = egui::FontDefinitions::default();
    let proportional_len = base
        .families
        .get(&egui::FontFamily::Proportional)
        .map_or(0, Vec::len);
    let with_bundle = with_bundled_font(base, "NotoSans", BYTES).expect("valid prepend");
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        let chain = with_bundle
            .families
            .get(&family)
            .expect("family stays bound");
        assert_eq!(
            chain.first().map(String::as_str),
            Some("NotoSans"),
            "bundle is the primary for {family:?}"
        );
        assert!(
            chain.len() > 1,
            "previous defaults stay behind as fallbacks for {family:?}"
        );
    }
    let chain = with_bundle
        .families
        .get(&egui::FontFamily::Proportional)
        .expect("proportional stays bound");
    assert_eq!(chain.len(), proportional_len + 1);
}

#[test]
fn with_bundled_font_repeat_install_never_stacks() {
    let once = with_bundled_font(egui::FontDefinitions::default(), "NotoSans", BYTES)
        .expect("first install");
    let twice = with_bundled_font(once, "NotoSans", BYTES).expect("repeat install");
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        let count = twice.families.get(&family).map_or(0, |chain| {
            chain
                .iter()
                .filter(|name| name.as_str() == "NotoSans")
                .count()
        });
        assert_eq!(count, 1, "repeat install dedupes for {family:?}");
    }
}

#[test]
fn install_wrapper_queues_without_a_window() {
    // `add_font` only queues in memory; parsing happens at the next pass,
    // so fake bytes are safe here — the wrapper's contract is validation
    // plus queueing, and validation is what these asserts pin.
    let ctx = egui::Context::default();
    install_bundled_font(&ctx, "NotoSans", BYTES).expect("queues");
    assert_eq!(
        install_bundled_font(&ctx, "", BYTES),
        Err(EguiError::InvalidParam {
            what: "font name".to_owned(),
            reason: "must not be blank".to_owned(),
        })
    );
    assert_eq!(
        install_bundled_font(&ctx, "NotoSans", b""),
        Err(EguiError::InvalidParam {
            what: "font bytes".to_owned(),
            reason: "must not be empty".to_owned(),
        })
    );
}
