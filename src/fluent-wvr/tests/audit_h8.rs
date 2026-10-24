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
    std::sync::Arc::new(E { n: name.to_string() })
}

#[test]
fn h8_cascade_empty_returns_descriptive_error() {
    let cascade = ComponentCascade::new();
    let err = cascade.execute_first_ok(&WorkContext::default()).unwrap_err();
    assert!(err.to_string().contains("no units"));
}

#[test]
fn h8_cascade_ok_fail_short_circuits_characterization() {
    // Docs (SKILL Pattern 10) say Err/WorkOutput::fail advances; code only advances on Err.
    let mut cascade = ComponentCascade::new();
    cascade.push(ok_unit_msg("a", "FAIL", false)); // Ok but success=false
    cascade.push(ok_unit_msg("b", "good", true));
    let out = cascade.execute_first_ok(&WorkContext::default()).unwrap();
    assert_eq!(out.message, "FAIL", "characterization: Ok(fail) wins, does not advance");
}

#[test]
#[ignore = "AUDIT-H8: decide whether Ok(fail) should advance like Err"]
fn h8_cascade_ok_fail_should_advance_desired() {
    let mut cascade = ComponentCascade::new();
    cascade.push(ok_unit_msg("a", "FAIL", false));
    cascade.push(ok_unit_msg("b", "good", true));
    let out = cascade.execute_first_ok(&WorkContext::default()).unwrap();
    assert_eq!(out.message, "good");
}

#[test]
fn h8_cascade_all_err_returns_last() {
    let mut cascade = ComponentCascade::new();
    cascade.push(err_unit("a"));
    cascade.push(err_unit("b"));
    let err = cascade.execute_first_ok(&WorkContext::default()).unwrap_err();
    assert!(matches!(err, WorkError::Execution(_)));
}

#[test]
fn h8_cascade_execute_delegates_to_first_ok() {
    let mut cascade = ComponentCascade::new();
    cascade.push(err_unit("a"));
    cascade.push(ok_unit_msg("b", "good", true));
    let out = cascade.execute(&WorkContext::default()).unwrap();
    assert_eq!(out.message, "good");
}
