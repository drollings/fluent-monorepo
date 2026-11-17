use bon::Builder;
use fluent_wvr::prelude::*;
use internment::ArcIntern;

#[derive(Builder)]
#[builder(start_fn = new)]
pub struct CommandUnit {
    name: String,
    command: String,
    #[builder(default)]
    depends: Vec<ArcIntern<str>>,
    #[builder(default)]
    provides: Vec<ArcIntern<str>>,
}

/// Shared shell-execution semantics for `CommandUnit` and `TargetWorkUnit`.
///
/// Single source of truth for the `dry_run` / no-op / `run_shell_capture`
/// contract so the two shell-backed work units never diverge.
pub(crate) fn run_shell_command(
    name: &str,
    command: &str,
    ctx: &WorkContext,
) -> Result<WorkOutput, WorkError> {
    if ctx.dry_run {
        return Ok(WorkOutput::ok(format!(
            "[DRY-RUN] would execute: {command}"
        )));
    }
    if command.is_empty() {
        return Ok(WorkOutput::ok(format!("no-op: {name}")));
    }
    let output = common_core::shell::run_shell_capture(command)
        .map_err(|e| WorkError::Execution(format!("command failed: {e}")))?;
    if output.success {
        Ok(WorkOutput::ok_with_data(
            format!("{name} completed"),
            serde_json::json!({"stdout": output.stdout}),
        ))
    } else {
        Err(WorkError::Execution(format!(
            "{name} failed: {}",
            output.stderr
        )))
    }
}

impl WorkUnit for CommandUnit {
    fn name(&self) -> &str {
        &self.name
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &self.depends
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &self.provides
    }
    fn execute(&self, ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        run_shell_command(&self.name, &self.command, ctx)
    }
}

/// Shared `name`/`command` [`FieldAccess`] + [`Describable`] glue for the
/// shell-backed units (`CommandUnit`, `TargetWorkUnit`), following the
/// [`delegate_component_fields`](fluent_wvr::delegate_component_fields) shape.
/// The `command` lane is a plain `String` on both; `$set_name`/`$get_name`
/// adapt the `name` representation (`String` vs `ArcIntern<str>`), and
/// `$name_desc` names the unit kind in the schema.
macro_rules! impl_name_command_fields {
    ($type:ty, $set_name:expr, $get_name:expr, $name_desc:literal) => {
        impl FieldAccess for $type {
            fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
                match name {
                    "name" => {
                        $set_name(self, value);
                        Ok(())
                    }
                    "command" => {
                        self.command = value.to_string();
                        Ok(())
                    }
                    _ => Err(FieldError::NotFound(name.into())),
                }
            }
            fn get_field(&self, name: &str) -> Result<String, FieldError> {
                match name {
                    "name" => Ok($get_name(self)),
                    "command" => Ok(self.command.clone()),
                    _ => Err(FieldError::NotFound(name.into())),
                }
            }
            fn field_names(&self) -> &'static [&'static str] {
                &["name", "command"]
            }
        }

        impl fluent_wvr::Describable for $type {
            fn describe(&self) -> serde_json::Value {
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": $name_desc },
                        "command": { "type": "string", "description": "Shell command to execute" }
                    },
                    "required": ["name", "command"]
                })
            }
        }
    };
}
pub(crate) use impl_name_command_fields;

impl_name_command_fields!(
    CommandUnit,
    |unit: &mut CommandUnit, value: &str| unit.name = value.to_string(),
    |unit: &CommandUnit| unit.name.clone(),
    "Task name"
);

impl_component!(CommandUnit);

#[cfg(test)]
#[path = "../tests/work_unit.rs"]
mod tests;
