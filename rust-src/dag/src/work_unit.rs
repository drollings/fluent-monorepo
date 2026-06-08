use std::process::Command;

use guidance_common::traits::{WorkContext, WorkError, WorkOutput, WorkUnit};
use internment::ArcIntern;

pub struct CommandUnit {
    name: String,
    command: String,
    depends: Vec<ArcIntern<str>>,
    provides: Vec<ArcIntern<str>>,
}

impl CommandUnit {
    pub fn new(
        name: impl Into<String>,
        command: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            depends: Vec::new(),
            provides: Vec::new(),
        }
    }

    pub fn with_depends(mut self, deps: &[ArcIntern<str>]) -> Self {
        self.depends = deps.to_vec();
        self
    }

    pub fn with_provides(mut self, prov: &[ArcIntern<str>]) -> Self {
        self.provides = prov.to_vec();
        self
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
        if ctx.dry_run {
            return Ok(WorkOutput::ok(format!("[DRY-RUN] would execute: {}", self.command)));
        }

        if self.command.is_empty() {
            return Ok(WorkOutput::ok(format!("no-op: {}", self.name)));
        }

        let shell_cmd = if cfg!(target_os = "windows") {
            "cmd"
        } else {
            "sh"
        };
        let shell_arg = if cfg!(target_os = "windows") {
            "/C"
        } else {
            "-c"
        };

        let output = Command::new(shell_cmd)
            .arg(shell_arg)
            .arg(&self.command)
            .output()
            .map_err(|e| WorkError::Execution(format!("command failed: {e}")))?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            Ok(WorkOutput::ok_with_data(
                format!("{} completed", self.name),
                serde_json::json!({ "stdout": stdout }),
            ))
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Err(WorkError::Execution(format!("{} failed: {stderr}", self.name)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_unit_noop() {
        let unit = CommandUnit::new("noop", "");
        let ctx = WorkContext::default();
        let result = unit.execute(&ctx).unwrap();
        assert!(result.success);
    }

    #[test]
    fn test_command_unit_dry_run() {
        let unit = CommandUnit::new("dry", "echo hello");
        let ctx = WorkContext {
            dry_run: true,
            ..WorkContext::default()
        };
        let result = unit.execute(&ctx).unwrap();
        assert!(result.success);
        assert!(result.message.contains("DRY-RUN"));
    }

    #[test]
    fn test_command_unit_true() {
        let unit = CommandUnit::new("true_cmd", "true");
        let ctx = WorkContext::default();
        let result = unit.execute(&ctx).unwrap();
        assert!(result.success);
    }

    #[test]
    fn test_command_unit_false() {
        let unit = CommandUnit::new("false_cmd", "false");
        let ctx = WorkContext::default();
        let result = unit.execute(&ctx);
        assert!(result.is_err());
    }

    #[test]
    fn test_command_unit_name_and_deps() {
        let unit = CommandUnit::new("build", "make")
            .with_depends(&[ArcIntern::from("compile")])
            .with_provides(&[ArcIntern::from("artifact")]);
        assert_eq!(unit.name(), "build");
        assert_eq!(unit.depends().len(), 1);
        assert_eq!(&*unit.depends()[0], "compile");
        assert_eq!(unit.provides().len(), 1);
    }
}
