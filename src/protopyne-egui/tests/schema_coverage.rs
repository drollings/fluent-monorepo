use fluent_egui::{schema_fields, FieldKind, SchemaField};
use fluent_wvr::Describable;

/// Numeric dials with no lower edge: unbounded reals by contract (any finite
/// value validates), so no schema floor can state them without lying. Every
/// other numeric dial states its floor in the schema. The panel `offset`
/// joins the slot set for the same reason.
const OPEN_DIALS: &[&str] = &["osc_trim", "osc_offset", "offset"];

/// Free-text dials: session windows are genuinely open-ended (validated
/// non-empty by contract), so no catalog can list them. Every other audited
/// string is a catalog-backed select or a color; a new free-text field trips
/// the audit on purpose until it is catalogued here with its reason.
const FREE_TEXT: &[&str] = &["session"];

fn check_schema(owner: &str, fields: &[SchemaField]) {
    assert!(!fields.is_empty(), "{owner} describes no fields at all");
    for field in fields {
        assert!(
            !field.description.is_empty(),
            "{owner}: {} carries no description",
            field.name
        );
        match field.kind {
            FieldKind::Number | FieldKind::Integer => {
                match field.minimum {
                    Some(edge) => assert!(
                        edge.is_finite(),
                        "{owner}: {} declares a non-finite floor",
                        field.name
                    ),
                    None => assert!(
                        OPEN_DIALS.contains(&field.name.as_str()),
                        "{owner}: {} is a bounded-contract numeric with no schema floor",
                        field.name
                    ),
                }
                if let Some(ceiling) = field.maximum {
                    let edge = field.minimum.expect("floor checked above");
                    assert!(
                        ceiling.is_finite() && edge < ceiling,
                        "{owner}: {} declares a disordered range",
                        field.name
                    );
                }
            }
            FieldKind::Select => assert!(
                !field.options.is_empty(),
                "{owner}: {} selects from nothing",
                field.name
            ),
            FieldKind::Boolean => assert!(
                field.minimum.is_none() && field.maximum.is_none() && field.options.is_empty(),
                "{owner}: {} is a switch carrying bounds or options",
                field.name
            ),
            FieldKind::Color => assert!(
                field.format.as_deref() == Some("color"),
                "{owner}: {} is a color without the color format",
                field.name
            ),
            FieldKind::Text => assert!(
                FREE_TEXT.contains(&field.name.as_str()),
                "{owner}: {} renders outside the catalogued widgets",
                field.name
            ),
        }
    }
}

/// Every settings schema is single-sourced: each field described, each
/// selector carrying its options, each switch bare, and each numeric dial
/// carrying its floor (or documented as an unbounded real). Kernel slots
/// iterate the whole registry, so a new kernel or a new dial joins the
/// audit the moment it renders.
#[test]
fn every_settings_schema_covers_its_fields() {
    let mut schemas: Vec<(String, Vec<SchemaField>)> = Vec::new();
    for kernel in crate::indicators::kernel_ids() {
        let config = crate::indicators::KernelIndicatorConfig::defaults_for(&kernel);
        schemas.push((
            format!("kernel:{kernel}"),
            schema_fields(&config.describe()),
        ));
    }
    assert!(
        schemas.len() >= 55,
        "the audit spans the registry, got {} kernels",
        schemas.len()
    );
    schemas.push((
        "fast_mas".to_owned(),
        schema_fields(&crate::FastMasConfig::new().build().describe()),
    ));
    schemas.push((
        "integration_lite".to_owned(),
        schema_fields(&crate::IntegrationLiteConfig::new().build().describe()),
    ));
    for (owner, fields) in &schemas {
        check_schema(owner, fields);
    }
}

/// The panel joins the audit: every kind row satisfies the same structural
/// contract as every other settings schema, and every panel kind has a
/// resolving theme key with a Style-tab override behind it.
#[test]
fn kind_defaults_and_keys_join_the_schema_audit() {
    use crate::oscillator_panel::{OscillatorPanelState, PANEL_KINDS};
    use fluent_egui::{ThemeConfig, STYLE_FIELDS};
    let panel = OscillatorPanelState::new().build();
    assert!(panel.validate().is_ok(), "defaults validate");
    for kind in PANEL_KINDS {
        let entry = panel.entry(kind).expect("every listed kind has a row");
        check_schema(
            &format!("panel:{kind}"),
            &fluent_egui::schema_fields(&entry.describe()),
        );
    }
    let theme = ThemeConfig::default();
    for kind in PANEL_KINDS {
        assert!(
            theme.style(kind).is_some(),
            "{kind} resolves a theme default"
        );
        let override_field = format!("color_{kind}");
        assert!(
            STYLE_FIELDS.iter().any(|field| *field == override_field),
            "{override_field} overrides the kind key"
        );
    }
}

/// The follow-through dials join the audit by name: every panel triple
/// field lifts its contract, and every Lite leg dial lifts its own.
#[test]
fn panel_triple_and_lite_legs_join_the_coverage() {
    use crate::oscillator_panel::{KindDefaults, PANEL_KINDS};
    use fluent_egui::{schema_fields, FieldKind};
    use fluent_wvr::Describable;
    for kind in PANEL_KINDS {
        let lifted = schema_fields(&KindDefaults::for_kind(kind).describe());
        let offset = lifted
            .iter()
            .find(|field| field.name == "offset")
            .expect("offset lifts");
        assert_eq!(offset.kind, FieldKind::Number);
        assert_eq!(
            offset.minimum, None,
            "offset stays an unbounded real by contract"
        );
        for (name, bound) in [("k", 0.0), ("mult", 0.0), ("gain", 0.0)] {
            let field = lifted
                .iter()
                .find(|field| field.name == name)
                .unwrap_or_else(|| panic!("{kind} {name} lifts"));
            assert_eq!(field.kind, FieldKind::Number);
            assert_eq!(field.minimum, Some(bound), "{kind} {name} states its floor");
        }
    }
    let lite = crate::IntegrationLiteConfig::new().build();
    let lifted = schema_fields(&lite.describe());
    for name in [
        "fast_kind",
        "fast_smooth_type",
        "slow_kind",
        "slow_smooth_type",
    ] {
        let field = lifted
            .iter()
            .find(|field| field.name == name)
            .unwrap_or_else(|| panic!("{name} lifts"));
        assert_eq!(field.kind, FieldKind::Select);
        assert!(!field.options.is_empty(), "{name} carries options");
    }
    for name in [
        "fast_period",
        "slow_period",
        "fast_smooth_period",
        "slow_smooth_period",
    ] {
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
    for name in ["fast_k", "slow_k"] {
        let field = lifted
            .iter()
            .find(|field| field.name == name)
            .unwrap_or_else(|| panic!("{name} lifts"));
        assert_eq!(field.kind, FieldKind::Number, "{name} edits a gain");
        assert_eq!(field.minimum, Some(0.0), "{name} states its floor");
    }
}

/// The backtest form joins the audit: the served composition sections are
/// present and non-empty, every leaf the form would render carries a known
/// editor kind, and every rendered path resolves in the defaults document
/// the form submits from.
///
/// This is deliberately narrower than the settings audit: the backtest form
/// performs no local validation of trading semantics (the server names any
/// refused value on submit), so descriptions and floors are hints, not
/// gates. What is gated is the three-way agreement — schema, defaults
/// document, rendered leaves — because a leaf the form paints but cannot
/// submit (or a default it submits but never paints) is a silent control.
#[test]
fn backtest_spec_sections_join_the_schema_audit() {
    use fluent_egui::FieldKind;
    use fluent_wvr::Describable as _;
    use protopyne_strategy::integration_lite::{
        IntegrationLiteSpec, IntegrationLiteStrategyConfig,
    };
    let spec = IntegrationLiteSpec::from(&IntegrationLiteStrategyConfig::default());
    let described = [
        ("indicator", spec.indicator.describe()),
        ("entry", spec.entry.describe()),
        ("exit", spec.exit.describe()),
        ("risk", spec.risk.describe()),
        ("session", spec.session.describe()),
        ("display", spec.display.describe()),
    ];
    let defaults = serde_json::to_value(&spec).expect("defaults serialize");
    for (section, schema) in &described {
        let fields = fluent_egui::schema_fields(schema);
        assert!(
            !fields.is_empty(),
            "backtest:{section} renders no fields at all"
        );
        for field in &fields {
            // The synthetic count leaf is schema machinery, not a painted
            // control: the rows carry the values, their number is the count.
            if field.name.ends_with(".count") {
                continue;
            }
            // List capacity beyond the default rows never paints: the form
            // prunes to resolving paths, and the audit holds the same line.
            if !crate::backtest::field_resolves(&defaults, &format!("{section}.{}", field.name)) {
                continue;
            }
            assert!(
                matches!(
                    field.kind,
                    FieldKind::Text
                        | FieldKind::Number
                        | FieldKind::Integer
                        | FieldKind::Boolean
                        | FieldKind::Select
                        | FieldKind::Color
                ),
                "backtest:{section}: {} has no editor kind",
                field.name
            );
            // Leaf names are relative to their section; the document path
            // is the section plus the leaf.
            let mut cursor = &defaults[section];
            for part in field.name.split('.') {
                // List rows address by index; every other segment by name.
                cursor = match (cursor, part.parse::<usize>()) {
                    (serde_json::Value::Array(rows), Ok(index)) => rows.get(index).unwrap_or_else(|| {
                        panic!(
                            "backtest:{section}: {} is painted but missing from the defaults document",
                            field.name
                        )
                    }),
                    _ => cursor.get(part).unwrap_or_else(|| {
                        panic!(
                            "backtest:{section}: {} is painted but missing from the defaults document",
                            field.name
                        )
                    }),
                };
            }
        }
    }
}
