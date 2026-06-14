use std::sync::Arc;

use fluent_wvr::{
    Component, Describable, FieldAccess, FieldError, WorkContext, WorkError, WorkOutput, WorkUnit,
};
use internment::ArcIntern;

type ExecuteFn = Arc<dyn Fn(&WorkContext) -> Result<WorkOutput, WorkError> + Send + Sync>;

pub struct ComponentAdapter {
    inner: Arc<dyn Component>,
    name_override: Option<String>,
    execute_override: Option<ExecuteFn>,
    field_overrides: Vec<(String, String)>,
}

impl ComponentAdapter {
    pub fn new(inner: Arc<dyn Component>) -> Self {
        Self {
            inner,
            name_override: None,
            execute_override: None,
            field_overrides: Vec::new(),
        }
    }
    #[must_use]
    pub fn with_name_override(mut self, name: impl Into<String>) -> Self {
        self.name_override = Some(name.into());
        self
    }
    #[must_use]
    pub fn with_execute_override(mut self, f: ExecuteFn) -> Self {
        self.execute_override = Some(f);
        self
    }
    #[must_use]
    pub fn with_field_override(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.field_overrides.push((name.into(), value.into()));
        self
    }
}

impl WorkUnit for ComponentAdapter {
    fn name(&self) -> &str {
        self.name_override
            .as_deref()
            .unwrap_or_else(|| self.inner.name())
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        self.inner.depends()
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        self.inner.provides()
    }
    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        if let Some(ref f) = self.execute_override {
            f(ctx)
        } else {
            self.inner.execute(ctx)
        }
    }
}

impl FieldAccess for ComponentAdapter {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        self.field_overrides.push((name.into(), value.into()));
        Ok(())
    }
    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        self.field_overrides
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .ok_or_else(|| FieldError::NotFound(name.into()))
    }
    fn field_names(&self) -> &'static [&'static str] {
        &[]
    }
}

impl Describable for ComponentAdapter {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({"name": self.name(), "adapted": true, "field_overrides": self.field_overrides})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestUnit {
        name: String,
    }
    impl WorkUnit for TestUnit {
        fn name(&self) -> &str {
            &self.name
        }
        fn depends(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn provides(&self) -> &[ArcIntern<str>] {
            &[]
        }
        fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
            Ok(WorkOutput::ok("original"))
        }
    }
    impl FieldAccess for TestUnit {
        fn set_field(&mut self, _name: &str, _value: &str) -> Result<(), FieldError> {
            Ok(())
        }
        fn get_field(&self, _name: &str) -> Result<String, FieldError> {
            Err(FieldError::NotFound("no fields".into()))
        }
        fn field_names(&self) -> &'static [&'static str] {
            &[]
        }
    }
    impl Describable for TestUnit {
        fn describe(&self) -> serde_json::Value {
            serde_json::json!({"name": self.name})
        }
    }

    #[test]
    fn test_adapter_delegates_by_default() {
        let adapter = ComponentAdapter::new(Arc::new(TestUnit {
            name: "inner".into(),
        }));
        assert_eq!(adapter.name(), "inner");
        let result = adapter.execute(&WorkContext::default()).unwrap();
        assert_eq!(result.message, "original");
    }
    #[test]
    fn test_adapter_name_override() {
        let adapter = ComponentAdapter::new(Arc::new(TestUnit {
            name: "inner".into(),
        }))
        .with_name_override("renamed");
        assert_eq!(adapter.name(), "renamed");
    }
    #[test]
    fn test_adapter_execute_override() {
        let adapter = ComponentAdapter::new(Arc::new(TestUnit {
            name: "inner".into(),
        }))
        .with_execute_override(Arc::new(|_| Ok(WorkOutput::ok("overridden"))));
        assert_eq!(
            adapter.execute(&WorkContext::default()).unwrap().message,
            "overridden"
        );
    }
    #[test]
    fn test_adapter_field_override() {
        let mut adapter = ComponentAdapter::new(Arc::new(TestUnit {
            name: "inner".into(),
        }))
        .with_field_override("port", "8080");
        assert_eq!(adapter.get_field("port").unwrap(), "8080");
        adapter.set_field("port", "9090").unwrap();
        assert_eq!(adapter.get_field("port").unwrap(), "9090");
    }
}
