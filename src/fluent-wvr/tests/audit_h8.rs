//! AUDIT-H8: cascade first-Ok vs ladder (fluent-wvr side: cascade only).

use fluent_wvr::prelude::*;

fn ok_unit_msg(name: &str, msg: &str, success: bool) -> std::sync::Arc<dyn Component> {
    struct U {
        n: String,
        m: String,
        s: bool,
    }
    impl WorkUnit for U {
        fn name(&self) -> &str {
            &self.n
        }
        fn depends(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn provides(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
            Ok(WorkOutput {
                success: self.s,
                message: self.m.clone(),
                data: serde_json::Value::Null,
            })
        }
    }
    impl FieldAccess for U {
        fn set_field(&mut self, _: &str, _: &str) -> Result<(), FieldError> {
            Err(FieldError::NotFound("none".into()))
        }
        fn get_field(&self, _: &str) -> Result<String, FieldError> {
            Err(FieldError::NotFound("none".into()))
        }
        fn field_names(&self) -> &'static [&'static str] {
            &[]
        }
    }
    impl Describable for U {
        fn describe(&self) -> serde_json::Value {
            serde_json::json!({})
        }
    }
    fluent_wvr::impl_component!(U);
    std::sync::Arc::new(U {
        n: name.to_string(),
        m: msg.to_string(),
        s: success,
    })
}

fn err_unit(name: &str) -> std::sync::Arc<dyn Component> {
    struct E {
        n: String,
    }
    impl WorkUnit for E {
        fn name(&self) -> &str {
            &self.n
        }
        fn depends(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn provides(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
            Err(WorkError::Execution("boom".into()))
        }
    }
    impl FieldAccess for E {
        fn set_field(&mut self, _: &str, _: &str) -> Result<(), FieldError> {
            Err(FieldError::NotFound("none".into()))
        }
        fn get_field(&self, _: &str) -> Result<String, FieldError> {
            Err(FieldError::NotFound("none".into()))
        }
        fn field_names(&self) -> &'static [&'static str] {
            &[]
        }
    }
    impl Describable for E {
        fn describe(&self) -> serde_json::Value {
            serde_json::json!({})
        }
    }
    fluent_wvr::impl_component!(E);
    std::sync::Arc::new(E {
        n: name.to_string(),
    })
}

#[test]
fn h8_cascade_empty_returns_descriptive_error() {
    let cascade = ComponentCascade::new();
    let err = cascade
        .execute_first_ok(&WorkContext::default())
        .unwrap_err();
    assert!(err.to_string().contains("no units"));
}

#[test]
fn h8_cascade_ok_fail_advances_to_win() {
    let mut cascade = ComponentCascade::new();
    cascade.push(ok_unit_msg("a", "FAIL", false)); // Ok but success=false
    cascade.push(ok_unit_msg("b", "good", true));
    let out = cascade.execute_first_ok(&WorkContext::default()).unwrap();
    assert_eq!(out.message, "good");
}

#[test]
fn h8_cascade_all_err_returns_last() {
    let mut cascade = ComponentCascade::new();
    cascade.push(err_unit("a"));
    cascade.push(err_unit("b"));
    let err = cascade
        .execute_first_ok(&WorkContext::default())
        .unwrap_err();
    assert!(matches!(err, WorkError::Execution(_)));
}

fn err_unit_msg(name: &str, msg: &str) -> std::sync::Arc<dyn Component> {
    struct E {
        n: String,
        m: String,
    }
    impl WorkUnit for E {
        fn name(&self) -> &str {
            &self.n
        }
        fn depends(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn provides(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
            Err(WorkError::Execution(self.m.clone()))
        }
    }
    impl FieldAccess for E {
        fn set_field(&mut self, _: &str, _: &str) -> Result<(), FieldError> {
            Err(FieldError::NotFound("none".into()))
        }
        fn get_field(&self, _: &str) -> Result<String, FieldError> {
            Err(FieldError::NotFound("none".into()))
        }
        fn field_names(&self) -> &'static [&'static str] {
            &[]
        }
    }
    impl Describable for E {
        fn describe(&self) -> serde_json::Value {
            serde_json::json!({})
        }
    }
    fluent_wvr::impl_component!(E);
    std::sync::Arc::new(E {
        n: name.to_string(),
        m: msg.to_string(),
    })
}

// ── M4: the cascade walks the ladder with an explicit acceptance predicate ──
// `WorkOutput::is_accept` (task-value) is orthogonal to `Result::is_ok`
// (producer-confidence). The cascade wins on the first accepted output,
// continues past `Ok(fail)` remembering the last failure verdict, and walks
// `Err` through the ladder's stop walk. Each row below runs the same rungs
// against both the cascade and `first_accept_in_order_sync`.

/// Map one rung result onto the ladder's three-way shape, per the spec:
/// accepted output wins, a failure verdict skips with memory, a transport
/// error walks.
fn rung_step(
    result: Result<WorkOutput, WorkError>,
    last_fail: &mut Option<WorkOutput>,
) -> Result<Option<WorkOutput>, WorkError> {
    match result {
        Ok(output) if output.is_accept() => Ok(Some(output)),
        Ok(output) => {
            *last_fail = Some(output);
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

fn ladder_outcome(
    units: &[std::sync::Arc<dyn Component>],
    stop: impl FnMut(&WorkError) -> bool,
    last_fail: &mut Option<WorkOutput>,
) -> Result<Option<WorkOutput>, WorkError> {
    let ctx = WorkContext::default();
    fluent_wvr::ladder::first_accept_in_order_sync(
        units,
        |unit| rung_step(unit.execute(&ctx), last_fail),
        stop,
    )
}

fn cascade_outcome(units: Vec<std::sync::Arc<dyn Component>>) -> Result<WorkOutput, WorkError> {
    let mut cascade = ComponentCascade::new();
    for unit in units {
        cascade.push(unit);
    }
    cascade.execute_first_ok(&WorkContext::default())
}

#[test]
fn m4_empty_shapes_differ_by_design() {
    let err = cascade_outcome(vec![]).unwrap_err();
    assert!(err.to_string().contains("no units"));
    let mut last_fail = None;
    let out: Result<Option<WorkOutput>, WorkError> =
        ladder_outcome(&[], |_: &WorkError| false, &mut last_fail);
    assert!(
        matches!(out, Ok(None)),
        "empty ladder skips cleanly: {out:?}"
    );
}

#[test]
fn m4_all_err_returns_last_on_both() {
    let units = || vec![err_unit_msg("a", "first"), err_unit_msg("b", "second")];
    let err = cascade_outcome(units()).unwrap_err();
    assert!(err.to_string().contains("second"), "got {err:?}");
    let mut last_fail = None;
    let err = ladder_outcome(&units(), |_: &WorkError| false, &mut last_fail).unwrap_err();
    assert!(err.to_string().contains("second"), "got {err:?}");
}

#[test]
fn m4_first_win_short_circuits_both() {
    let units = || vec![ok_unit_msg("a", "good", true)];
    let out = cascade_outcome(units()).unwrap();
    assert_eq!(out.message, "good");
    let mut last_fail = None;
    let out = ladder_outcome(&units(), |_: &WorkError| false, &mut last_fail)
        .unwrap()
        .expect("win");
    assert_eq!(out.message, "good");
}

#[test]
fn m4_fail_continues_to_win_on_both() {
    let units = || {
        vec![
            ok_unit_msg("a", "FAIL", false),
            ok_unit_msg("b", "good", true),
        ]
    };
    let out = cascade_outcome(units()).unwrap();
    assert_eq!(out.message, "good", "Ok(fail) must advance, not win");
    let mut last_fail = None;
    let out = ladder_outcome(&units(), |_: &WorkError| false, &mut last_fail)
        .unwrap()
        .expect("win");
    assert_eq!(out.message, "good");
}

#[test]
fn m4_mixed_fail_err_continues_on_both() {
    let units = || {
        vec![
            ok_unit_msg("a", "FAIL", false),
            err_unit_msg("b", "boom"),
            ok_unit_msg("c", "good", true),
        ]
    };
    let out = cascade_outcome(units()).unwrap();
    assert_eq!(out.message, "good");
    let mut last_fail = None;
    let out = ladder_outcome(&units(), |_: &WorkError| false, &mut last_fail)
        .unwrap()
        .expect("win");
    assert_eq!(out.message, "good");
}

#[test]
fn m4_all_fail_returns_last_verdict_not_error() {
    let units = || {
        vec![
            ok_unit_msg("a", "first", false),
            ok_unit_msg("b", "second", false),
        ]
    };
    // The cascade preserves the failure verdict on the wire as `Ok(fail)`;
    // it must never become `WorkError::Execution`.
    let out = cascade_outcome(units()).unwrap();
    assert!(!out.is_accept());
    assert_eq!(out.message, "second", "last failure verdict wins: {out:?}");
    // The ladder reports clean exhaustion; the stashed verdict is separate.
    let mut last_fail = None;
    let out = ladder_outcome(&units(), |_: &WorkError| false, &mut last_fail);
    assert!(matches!(out, Ok(None)), "got {out:?}");
    assert_eq!(last_fail.expect("stashed verdict").message, "second");
}

#[test]
fn m4_stop_terminal_aborts_before_later_win() {
    let units = || vec![err_unit_msg("a", "boom"), ok_unit_msg("b", "good", true)];
    // A terminal stop aborts the ladder before the later win.
    let mut last_fail = None;
    let err = ladder_outcome(&units(), |_: &WorkError| true, &mut last_fail).unwrap_err();
    assert!(err.to_string().contains("boom"), "got {err:?}");
    // The cascade has no stop predicate: it always walks like `stop = false`.
    let out = cascade_outcome(units()).unwrap();
    assert_eq!(out.message, "good");
    let mut last_fail = None;
    let out = ladder_outcome(&units(), |_: &WorkError| false, &mut last_fail)
        .unwrap()
        .expect("win");
    assert_eq!(out.message, "good");
}

#[test]
fn m4_stop_never_fires_on_fail() {
    let units = || {
        vec![
            ok_unit_msg("a", "FAIL", false),
            ok_unit_msg("b", "good", true),
        ]
    };
    // Task-value negatives are not confidence terminals: even an
    // always-terminal stop must not fire on `Ok(fail)`.
    let mut last_fail = None;
    let out = ladder_outcome(&units(), |_: &WorkError| true, &mut last_fail)
        .unwrap()
        .expect("win");
    assert_eq!(out.message, "good");
}

#[test]
fn m4_fail_never_becomes_execution() {
    // `Ok(fail)` mapped to `Err(Execution)` would corrupt `is_retryable`
    // (Execution is permanent) and the supervisor's failed/cancelled
    // accounting. The wire keeps `Ok(fail)` distinct from `Err`.
    let out = cascade_outcome(vec![ok_unit_msg("a", "FAIL", false)]).unwrap();
    assert!(!out.is_accept());
    assert!(!WorkError::Execution("boom".into()).is_retryable());
    assert!(WorkError::Dependency("wait".into()).is_retryable());
    assert!(WorkError::Timeout {
        duration_ms: 1,
        unit: "u".into()
    }
    .is_retryable());
}

#[test]
fn m4_is_accept_predicate() {
    assert!(WorkOutput::ok("good").is_accept());
    assert!(!WorkOutput::fail("nope").is_accept());
}

#[test]
fn h8_cascade_execute_delegates_to_first_ok() {
    let mut cascade = ComponentCascade::new();
    cascade.push(err_unit("a"));
    cascade.push(ok_unit_msg("b", "good", true));
    let out = cascade.execute(&WorkContext::default()).unwrap();
    assert_eq!(out.message, "good");
}
