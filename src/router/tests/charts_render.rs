use super::*;

fn ctx(request: &str) -> RenderContext {
    RenderContext {
        request: request.to_string(),
        deps: HashMap::new(),
        upstream: HashMap::new(),
        chart: "test_chart".into(),
    }
}

#[test]
fn renders_request_and_for_loop() {
    let mut c = ctx("hello world");
    c.deps.insert(
        "files".into(),
        vec![
            BoundEntity {
                id: "f1".into(),
                kind: "file".into(),
                value: serde_json::json!({"name": "a.rs"}),
            },
            BoundEntity {
                id: "f2".into(),
                kind: "file".into(),
                value: serde_json::json!({"name": "b.rs"}),
            },
        ],
    );
    let out = render(
        "{{ request }}\n{% for e in deps.files %}- {{ e.value.name }}\n{% endfor %}",
        &c,
    )
    .unwrap();
    assert!(out.starts_with("hello world"));
    assert!(out.contains("- a.rs"));
    assert!(out.contains("- b.rs"));
}

#[test]
fn unknown_attribute_is_render_error() {
    let c = ctx("x");
    let err = render("{{ request }} {{ bogus }}", &c).unwrap_err();
    assert!(matches!(err, ChartError::Render { .. }));
}

#[test]
fn injection_string_neutralized() {
    let payload = "<script>alert(1)</script> {{ request }} {% if true %}x{% endif %}";
    let sanitized = sanitize_entity_value(serde_json::json!(payload));
    let s = sanitized.as_str().unwrap();
    // The template-open tokens must be broken by a zero-width space.
    assert!(!s.contains("{{"), "template markers must be escaped: {s:?}");
    assert!(!s.contains("{%"), "block markers must be escaped: {s:?}");
    // And it must not open a template block when rendered.
    let mut c = ctx("safe");
    c.deps.insert(
        "e".into(),
        vec![BoundEntity {
            id: "x".into(),
            kind: "e".into(),
            value: serde_json::json!(payload),
        }],
    );
    let out = render("{% for e in deps.e %}{{ e.value }}{% endfor %}", &c).unwrap();
    assert!(
        !out.contains("alert(1)"),
        "script must be neutralized: {out:?}"
    );
    // The zero-width escape is preserved as literal data (characters are
    // kept; only the template token is broken).
    assert!(out.contains('\u{200B}'));
}

#[test]
fn nul_and_control_chars_stripped() {
    let payload = "a\u{0}b\u{1}c\x1f\n   d";
    let sanitized = sanitize_entity_value(serde_json::json!(payload));
    assert_eq!(sanitized.as_str().unwrap(), "a b c d");
}

#[test]
fn over_cap_rendered_rejected() {
    let c = ctx("x");
    // A template that repeats the request more times than the cap.
    let template = format!(
        "{{% for i in range({CHART_RENDERED_MAX_CHARS} + 1) %}}{{ request }}{{% endfor %}}"
    );
    let err = render(&template, &c).unwrap_err();
    assert!(matches!(err, ChartError::Render { .. }));
}

#[test]
fn long_entity_truncated() {
    let long = "y".repeat(ENTITY_VALUE_MAX_CHARS * 2);
    let sanitized = sanitize_entity_value(serde_json::json!(long));
    let s = sanitized.as_str().unwrap();
    assert_eq!(s.chars().count(), ENTITY_VALUE_MAX_CHARS);
}

#[test]
fn upstream_output_tojson_pastes_canonical_json() {
    // Prior-target outputs are structured values; whole-value interpolation
    // must paste canonical JSON (`true`/`null`), not the engine's
    // Python-repr scalar rendering (`True`/`None`).
    let mut c = ctx("x");
    c.upstream.insert(
        "reproduce".into(),
        serde_json::json!({"output": {
            "ok": true,
            "retry": false,
            "cause": null,
            "tags": ["a", 1],
            "nested": {"depth": 2},
        }}),
    );
    let out = render("{{ upstream.reproduce.output | tojson }}", &c).unwrap();
    // Keys sort: the engine holds maps sorted, matching the workspace
    // canonical-JSON convention (`serde_json::Map` without `preserve_order`).
    assert_eq!(
        out,
        r#"{"cause":null,"nested":{"depth":2},"ok":true,"retry":false,"tags":["a",1]}"#
    );
}

#[test]
fn upstream_output_drill_stays_structured() {
    // Attribute drills into an output keep working: structured access is
    // the narrow view, `tojson` is the paste-whole view.
    let mut c = ctx("x");
    c.upstream.insert(
        "reproduce".into(),
        serde_json::json!({"output": {"plan": "minimal repro"}}),
    );
    let out = render("{{ upstream.reproduce.output.plan }}", &c).unwrap();
    assert_eq!(out, "minimal repro");
}

#[test]
fn upstream_output_scalar_tojson_is_json_scalar() {
    // Scalars through `tojson` render as JSON scalars (strings quoted).
    let mut c = ctx("x");
    c.upstream.insert(
        "reproduce".into(),
        serde_json::json!({"output": {"plan": "minimal repro", "steps": 3}}),
    );
    let out = render(
        "{{ upstream.reproduce.output.plan | tojson }}|{{ upstream.reproduce.output.steps | tojson }}",
        &c,
    )
    .unwrap();
    assert_eq!(out, "\"minimal repro\"|3");
}

#[test]
fn unknown_filter_is_render_error() {
    // A misspelled filter must fail closed, not paste raw.
    let c = ctx("x");
    let err = render("{{ request | nosuchfilter }}", &c).unwrap_err();
    assert!(matches!(err, ChartError::Render { .. }));
}

#[test]
fn golden_renders_appendix_a_target() {
    // Appendix A bug_triage "reproduce" target template with a fixed
    // request + one bound entity.
    let template = "Given the bug report {{ request }}, write a minimal reproduction plan.\n{% for e in deps.report %}Report entity: {{ e.value.title }}\n{% endfor %}";
    let mut c = RenderContext {
        request: "crash on startup when loading large projects".into(),
        deps: HashMap::new(),
        upstream: HashMap::new(),
        chart: "bug_triage".into(),
    };
    c.deps.insert(
        "report".into(),
        vec![BoundEntity {
            id: "issue-42".into(),
            kind: "report".into(),
            value: serde_json::json!({"title": "Segfault on project load"}),
        }],
    );
    let out = render(template, &c).unwrap();
    assert_eq!(
        out,
        "Given the bug report crash on startup when loading large projects, write a minimal reproduction plan.\nReport entity: Segfault on project load\n"
    );
}
