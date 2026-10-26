//! Expansion tests for the fluent-wvr derive/macro surface.
//!
//! `fluent-wvr-macros` is a proc-macro crate, so its own `#[cfg(test)]` unit
//! tests cannot invoke the proc macros (a proc macro is only usable from a
//! separate crate). These integration tests are that separate crate: they
//! depend on `fluent-wvr` and expand `#[derive(FieldAccess, Describable)]`,
//! `fluent_wvr::impl_fieldless!`, and `fluent_wvr::impl_component!`
//! (concrete + generic) on real fixture types, then assert the generated
//! behavior.

use fluent_wvr::prelude::*;
use fluent_wvr::FieldError;
use internment::ArcIntern;
use std::sync::Arc;

// ── FieldAccess + Describable derive on a struct with the full attribute
//    surface ───────────────────────────────────────────────────────────────

#[derive(FieldAccess, Describable)]
struct ToolConfig {
    #[field(desc = "TCP listen port", min = 1, max = 65535)]
    pub port: u16,
    #[field(desc = "Host address", max_len = 255)]
    pub host: String,
    #[field(desc = "Enable verbose logging")]
    pub verbose: bool,
    #[field(desc = "API endpoint URL", pattern = "https://")]
    pub endpoint: String,
    #[field(desc = "Display name", sanitize = "lowercase")]
    pub name: String,
    // Never read by design: exists to pin `skip` behavior in the expansion.
    #[allow(dead_code)]
    #[field(skip)]
    pub internal: u64,
}

#[test]
fn derive_field_access_set_and_get() {
    let mut cfg = ToolConfig {
        port: 8080,
        host: "localhost".into(),
        verbose: false,
        endpoint: "https://api.example.com".into(),
        name: "Server".into(),
        internal: 7,
    };
    cfg.set_field("port", "9000").unwrap();
    assert_eq!(cfg.get_field("port").unwrap(), "9000");
    cfg.set_field("verbose", "true").unwrap();
    assert_eq!(cfg.get_field("verbose").unwrap(), "true");
    cfg.set_field("host", "router.local").unwrap();
    assert_eq!(cfg.get_field("host").unwrap(), "router.local");
}

#[test]
fn derive_field_access_field_names() {
    let cfg = ToolConfig {
        port: 1,
        host: "h".into(),
        verbose: false,
        endpoint: "https://e".into(),
        name: "n".into(),
        internal: 0,
    };
    // The `skip` field is excluded from field_names.
    let names = cfg.field_names();
    assert!(names.contains(&"port"));
    assert!(names.contains(&"host"));
    assert!(names.contains(&"verbose"));
    assert!(names.contains(&"endpoint"));
    assert!(names.contains(&"name"));
    assert!(!names.contains(&"internal"));
}

#[test]
fn derive_field_access_enforces_constraints() {
    let mut cfg = ToolConfig {
        port: 1,
        host: "h".into(),
        verbose: false,
        endpoint: "https://e".into(),
        name: "n".into(),
        internal: 0,
    };
    // min/max on port.
    assert!(matches!(
        cfg.set_field("port", "0"),
        Err(FieldError::Constraint(_))
    ));
    assert!(matches!(
        cfg.set_field("port", "65536"),
        Err(FieldError::Constraint(_))
    ));
    // non-numeric parse error.
    assert!(matches!(
        cfg.set_field("port", "abc"),
        Err(FieldError::Parse(_))
    ));
    // max_len on host.
    let long = "x".repeat(300);
    assert!(matches!(
        cfg.set_field("host", &long),
        Err(FieldError::Constraint(_))
    ));
    // pattern (substring) on endpoint.
    assert!(matches!(
        cfg.set_field("endpoint", "http://plain"),
        Err(FieldError::Constraint(_))
    ));
    cfg.set_field("endpoint", "https://ok").unwrap();
    // unknown field.
    assert!(matches!(
        cfg.set_field("nope", "x"),
        Err(FieldError::NotFound(_))
    ));
}

#[test]
fn derive_field_access_sanitizes() {
    let mut cfg = ToolConfig {
        port: 1,
        host: "h".into(),
        verbose: false,
        endpoint: "https://e".into(),
        name: "n".into(),
        internal: 0,
    };
    // sanitize = "lowercase".
    cfg.set_field("name", "MixedCase").unwrap();
    assert_eq!(cfg.get_field("name").unwrap(), "mixedcase");
}

#[test]
fn derive_describable_emits_schema() {
    let cfg = ToolConfig {
        port: 1,
        host: "h".into(),
        verbose: false,
        endpoint: "https://e".into(),
        name: "n".into(),
        internal: 0,
    };
    let schema = cfg.describe();
    let props = &schema["properties"];
    assert_eq!(props["port"]["type"], "integer");
    assert_eq!(props["host"]["type"], "string");
    assert_eq!(props["verbose"]["type"], "boolean");
    // The `skip` field does not appear in the schema.
    assert!(props.get("internal").is_none());
}

// ── impl_fieldless! on a fieldless unit ──────────────────────────────────

struct FieldlessUnit;
impl WorkUnit for FieldlessUnit {
    fn name(&self) -> &str {
        "fieldless"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _: &WorkContext) -> Result<WorkOutput, WorkError> {
        Ok(WorkOutput::ok("fieldless"))
    }
}
impl Describable for FieldlessUnit {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({})
    }
}
fluent_wvr::impl_fieldless!(FieldlessUnit);
fluent_wvr::impl_component!(FieldlessUnit);

#[test]
fn impl_fieldless_rejects_all_fields() {
    let mut unit = FieldlessUnit;
    assert!(matches!(
        unit.set_field("x", "y"),
        Err(FieldError::NotFound(_))
    ));
    assert!(matches!(unit.get_field("x"), Err(FieldError::NotFound(_))));
    assert!(unit.field_names().is_empty());
    // The type is still a full Component: it can be erased and downcast.
    let comp: Arc<dyn Component> = Arc::new(FieldlessUnit);
    assert!(fluent_wvr::component_downcast_ref::<FieldlessUnit>(&*comp).is_some());
}

// ── impl_component!(generic ...) on a wrapper ─────────────────────────────

struct Wrapper<U>(U);
impl<U: Component> WorkUnit for Wrapper<U> {
    fn name(&self) -> &str {
        self.0.name()
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        self.0.depends()
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        self.0.provides()
    }
    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        self.0.execute(ctx)
    }
}
impl<U: Component> FieldAccess for Wrapper<U> {
    fn set_field(&mut self, n: &str, v: &str) -> Result<(), FieldError> {
        self.0.set_field(n, v)
    }
    fn get_field(&self, n: &str) -> Result<String, FieldError> {
        self.0.get_field(n)
    }
    fn field_names(&self) -> &'static [&'static str] {
        self.0.field_names()
    }
}
impl<U: Component> Describable for Wrapper<U> {
    fn describe(&self) -> serde_json::Value {
        self.0.describe()
    }
}
fluent_wvr::impl_component!(generic (U: Component + 'static) for Wrapper<U>);

#[test]
fn impl_component_generic_expands() {
    let inner: Arc<dyn Component> = Arc::new(FieldlessUnit);
    let comp: Arc<dyn Component> = Arc::new(Wrapper(Arc::clone(&inner)));
    assert!(fluent_wvr::component_downcast_ref::<Wrapper<Arc<dyn Component>>>(&*comp).is_some());
    assert_eq!(comp.name(), "fieldless");
}

// ── Closed-vocabulary control: every documented #[field(...)] argument ─────
// still expands and round-trips set_field/describe. The unknown-argument
// fence must not become a blanket refusal.

fn kind_options() -> Vec<String> {
    vec!["fast".to_string(), "slow".to_string()]
}

#[derive(FieldAccess, Describable)]
struct ClosedVocab {
    #[field(desc = "TCP listen port", min = 1, max = 65535)]
    port: u16,
    #[field(desc = "Clamped dial", min = 0, max = 10, clamp)]
    clamped: u32,
    #[field(desc = "Host address", max_len = 64)]
    host: String,
    #[field(desc = "Display name", sanitize = "trim")]
    trimmed: String,
    #[field(desc = "API endpoint", pattern = "https://")]
    endpoint: String,
    #[field(desc = "Mode flag", coerce = "trim, strip_quotes")]
    mode: String,
    #[field(desc = "Smoothing ratio", parse = "number")]
    ratio: f64,
    #[field(desc = "Smoothing kind", variants = "kind_options")]
    kind: String,
    #[field(desc = "Endpoint URL", format = "url")]
    url: String,
    #[field(desc = "Nickname", required = false)]
    nick: Option<String>,
    #[field(desc = "Contact mail", required = false, empty_is_none = false)]
    email: Option<String>,
    #[allow(dead_code)]
    #[field(skip)]
    internal: u64,
}

#[test]
fn closed_vocabulary_still_expands_and_round_trips() {
    let mut cfg = ClosedVocab {
        port: 8080,
        clamped: 5,
        host: "localhost".into(),
        trimmed: "x".into(),
        endpoint: "https://api.example.com".into(),
        mode: "fast".into(),
        ratio: 0.5,
        kind: "fast".into(),
        url: "https://example.com".into(),
        nick: None,
        email: None,
        internal: 7,
    };
    cfg.set_field("port", "9000").unwrap();
    assert_eq!(cfg.get_field("port").unwrap(), "9000");
    cfg.set_field("clamped", "999").unwrap();
    assert_eq!(cfg.clamped, 10);
    cfg.set_field("host", "router.local").unwrap();
    assert_eq!(cfg.get_field("host").unwrap(), "router.local");
    cfg.set_field("trimmed", "  padded  ").unwrap();
    assert_eq!(cfg.get_field("trimmed").unwrap(), "padded");
    cfg.set_field("endpoint", "https://ok.example.com").unwrap();
    assert!(cfg.set_field("endpoint", "http://plain").is_err());
    cfg.set_field("mode", "  \"slow\"  ").unwrap();
    assert_eq!(cfg.get_field("mode").unwrap(), "slow");
    cfg.set_field("ratio", "0.75").unwrap();
    assert_eq!(cfg.get_field("ratio").unwrap(), "0.75");
    cfg.set_field("kind", "slow").unwrap();
    assert_eq!(cfg.get_field("kind").unwrap(), "slow");
    cfg.set_field("url", "https://example.com").unwrap();
    cfg.set_field("nick", "bob").unwrap();
    assert_eq!(cfg.get_field("nick").unwrap(), "bob");
    cfg.set_field("nick", "").unwrap();
    assert_eq!(cfg.get_field("nick").unwrap(), "");
    cfg.set_field("email", "").unwrap();
    assert_eq!(cfg.get_field("email").unwrap(), "");
    assert!(!cfg.field_names().contains(&"internal"));

    let schema = cfg.describe();
    assert_eq!(schema["properties"]["port"]["type"], "integer");
    assert!(schema["properties"]["port"]["minimum"].is_number());
    assert_eq!(schema["properties"]["url"]["format"], "url");
    let kinds = schema["properties"]["kind"]["enum"]
        .as_array()
        .expect("variants list");
    assert!(kinds.iter().any(|v| v == "fast"));
    let required = schema["required"].as_array().expect("required");
    assert!(!required.iter().any(|v| v == "nick"));
    assert!(!required.iter().any(|v| v == "internal"));
    assert!(schema["properties"].get("internal").is_none());
}

// ── Display hints ride the schema as x-* ──────────────────────────────────

#[derive(FieldAccess, Describable)]
struct HintedConfig {
    #[field(
        desc = "Low",
        min = 0.03,
        max = 1.0,
        tab = "inputs",
        group = "Trend Ribbon",
        inline = "adaptive",
        tooltip = "Weak-leg shading",
        step = 0.02
    )]
    pub alpha_low: f64,
    #[field(desc = "Plain")]
    pub plain: f64,
}

#[test]
fn derive_describable_emits_display_hints() {
    let cfg = HintedConfig {
        alpha_low: 0.14,
        plain: 1.0,
    };
    let props = &cfg.describe()["properties"];
    assert_eq!(props["alpha_low"]["x-tab"], "inputs");
    assert_eq!(props["alpha_low"]["x-group"], "Trend Ribbon");
    assert_eq!(props["alpha_low"]["x-inline"], "adaptive");
    assert_eq!(props["alpha_low"]["x-tooltip"], "Weak-leg shading");
    assert_eq!(props["alpha_low"]["x-step"], 0.02);
    // Unhinted fields carry no x-* keys, so schemas without the hints
    // describe exactly as before.
    for key in ["x-tab", "x-group", "x-inline", "x-tooltip", "x-step"] {
        assert!(
            props["plain"].get(key).is_none(),
            "no {key} without the hint"
        );
    }
}

// ── Nested component and list members ─────────────────────────────────────

#[derive(FieldAccess, Describable, Default, Clone, PartialEq, Debug)]
struct Leaf {
    #[field(desc = "Leaf amount")]
    pub amount: u32,
    pub label: String,
}

#[derive(FieldAccess, Describable, Default, Clone)]
struct NestedOwner {
    #[field(nested)]
    pub inner: Leaf,
    #[field(items = 3)]
    pub slots: Vec<Leaf>,
}

#[test]
fn nested_members_route_dotted_paths() {
    let mut owner = NestedOwner::default();
    owner.set_field("inner.amount", "7").expect("nested set");
    assert_eq!(owner.get_field("inner.amount").expect("nested get"), "7");
    let names = owner.field_names();
    assert!(names.contains(&"inner.amount"), "names: {names:?}");
    assert!(names.contains(&"inner.label"), "names: {names:?}");
    assert!(
        owner.set_field("inner.missing", "x").is_err(),
        "an unknown inner leaf refuses"
    );
}

#[test]
fn list_members_grow_and_truncate_by_count() {
    let mut owner = NestedOwner::default();
    assert_eq!(owner.get_field("slots.count").expect("count"), "0");
    owner.set_field("slots.count", "2").expect("grow");
    assert_eq!(owner.slots.len(), 2);
    owner.set_field("slots.0.amount", "11").expect("leaf set");
    assert_eq!(owner.get_field("slots.0.amount").expect("leaf get"), "11");
    assert_eq!(
        owner.get_field("slots.count").expect("count"),
        "2",
        "the count reflects the grown list"
    );
    owner.set_field("slots.count", "1").expect("truncate");
    assert_eq!(owner.slots.len(), 1, "the tail truncates");
    assert!(
        owner.set_field("slots.2.amount", "1").is_err(),
        "an index past the current length refuses"
    );
    assert!(
        owner.set_field("slots.count", "4").is_err(),
        "an open list cannot exceed its declared maximum"
    );
    let names = owner.field_names();
    assert!(names.contains(&"slots.0.amount"), "names: {names:?}");
    assert!(names.contains(&"slots.2.label"), "names: {names:?}");
    assert!(names.contains(&"slots.count"), "names: {names:?}");
}

#[test]
fn nested_and_list_members_describe_their_shape() {
    let owner = NestedOwner::default();
    let schema = owner.describe();
    let properties = schema["properties"].as_object().expect("properties");
    assert_eq!(properties["inner"]["type"], "object");
    assert_eq!(properties["inner"]["x-flatten"], true);
    assert_eq!(
        properties["inner.amount"]["description"], "Leaf amount",
        "a nested leaf keeps its own metadata"
    );
    assert_eq!(properties["slots"]["type"], "array");
    assert_eq!(properties["slots"]["x-items"], 3);
    assert_eq!(properties["slots.0.amount"]["x-item-of"], "slots");
    assert_eq!(properties["slots.0.amount"]["x-item-index"], 0);
    assert_eq!(properties["slots.count"]["maximum"], 3);
    assert_eq!(
        schema["required"],
        serde_json::json!([]),
        "a defaulted struct requires nothing"
    );
}

// ── Tagged union ──────────────────────────────────────────────────────────

#[derive(FieldAccess, Describable, Clone, Debug, PartialEq)]
enum Tagged {
    Breakout {
        #[field(desc = "Lookback", min = 1)]
        period: u32,
        level: f64,
    },
    Cross {
        fast: u32,
        slow: u32,
    },
    Off,
}

impl Default for Tagged {
    fn default() -> Self {
        Self::Breakout {
            period: 0,
            level: 0.0,
        }
    }
}

#[test]
fn a_tagged_union_switches_its_legal_leaf_set() {
    let mut union = Tagged::default();
    assert_eq!(union.get_field("kind").expect("kind"), "breakout");
    assert!(union.field_names().contains(&"kind"));
    assert!(union.field_names().contains(&"breakout.period"));
    union
        .set_field("breakout.period", "9")
        .expect("active leaf");
    assert_eq!(union.get_field("breakout.period").expect("leaf"), "9");

    union.set_field("kind", "cross").expect("switch arm");
    assert_eq!(union, Tagged::Cross { fast: 0, slow: 0 });
    assert!(
        !union.field_names().contains(&"breakout.period"),
        "the previous arm's leaves are gone: {:?}",
        union.field_names()
    );
    assert!(union.field_names().contains(&"cross.fast"));
    assert!(
        union.set_field("breakout.period", "3").is_err(),
        "an inactive leaf refuses"
    );
    assert_eq!(
        union.get_field("cross.fast").expect("active leaf"),
        "0",
        "the refusal stored nothing"
    );

    union.set_field("kind", "off").expect("unit arm");
    assert_eq!(union, Tagged::Off);
    assert_eq!(union.get_field("kind").expect("kind"), "off");
    assert_eq!(union.field_names(), &["kind"][..]);
}

#[test]
fn a_tagged_union_describes_its_arms() {
    let schema = Tagged::default().describe();
    assert_eq!(
        schema["properties"]["kind"]["enum"],
        serde_json::json!(["breakout", "cross", "off"])
    );
    assert_eq!(
        schema["properties"]["breakout.period"]["x-visible-when"],
        "kind=breakout"
    );
    assert_eq!(
        schema["properties"]["breakout.period"]["description"],
        "Lookback"
    );
    assert_eq!(schema["oneOf"].as_array().expect("arms").len(), 3);
}
