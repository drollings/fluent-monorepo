//! Unit tests for [`crate::traits`]: engines are `Component`s.
//!
//! The bound is load-bearing: tunables stay runtime-editable and lifecycle
//! stays supervised without a second interface. This test pins it with a
//! stub so removing the bound breaks the build here first.

use fluent_concurrency::stream::StreamAbort;
use fluent_wvr::prelude::*;
use internment::ArcIntern;

use crate::error::SttError;
use crate::traits::SttStream;
use crate::types::{Frame, WordUpdate};

struct StubStt {
    pushes: usize,
}

impl WorkUnit for StubStt {
    fn name(&self) -> &str {
        "stub_stt"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        Ok(WorkOutput::ok(format!("pushes={}", self.pushes)))
    }
}

impl FieldAccess for StubStt {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        match name {
            "pushes" => {
                self.pushes = value
                    .parse()
                    .map_err(|_| FieldError::Parse("pushes must be a number".to_string()))?;
                Ok(())
            }
            _ => Err(FieldError::NotFound(name.into())),
        }
    }
    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        match name {
            "pushes" => Ok(self.pushes.to_string()),
            _ => Err(FieldError::NotFound(name.into())),
        }
    }
    fn field_names(&self) -> &'static [&'static str] {
        &["pushes"]
    }
}

impl Describable for StubStt {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
}

fluent_wvr::impl_component!(StubStt);

impl SttStream for StubStt {
    fn push(&mut self, _frame: &Frame) -> Result<Vec<WordUpdate>, SttError> {
        self.pushes += 1;
        Ok(Vec::new())
    }
    fn finalize(&mut self) -> Result<Vec<WordUpdate>, SttError> {
        Ok(Vec::new())
    }
    fn reset(&mut self) {
        self.pushes = 0;
    }
}

fn assert_component<T: Component>() {}

#[test]
fn engines_present_the_component_interface() {
    assert_component::<StubStt>();
    fn assert_stt<T: SttStream>() {}
    assert_stt::<StubStt>();

    let mut stub = StubStt { pushes: 0 };
    stub.set_field("pushes", "3").expect("field set works");
    let ctx = WorkContext::default();
    let output = stub.execute(&ctx).expect("health executes");
    assert!(output.is_accept());
    assert!(output.message.contains("pushes=3"));

    let _abort = StreamAbort::new();
    stub.reset();
    assert_eq!(stub.pushes, 0);
}
