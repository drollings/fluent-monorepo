//! Subprocess helpers: `run_capture`, `run_shell_capture`, `run_command`, `CommandOutput`.

use std::process::{Command, Output};

/// Returns the platform-specific shell program and argument.
///
/// On Unix: `("sh", "-c")`. On Windows: `("cmd", "/C")`.
pub fn shell_cmd() -> (&'static str, &'static str) {
    if cfg!(target_os = "windows") {
        ("cmd", "/C")
    } else {
        ("sh", "-c")
    }
}

/// Captured output from a subprocess, including exit status, stdout, and stderr.
#[derive(Debug)]
pub struct CommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    fn from_output(output: &Output) -> Self {
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Self {
            success: output.status.success(),
            stdout,
            stderr,
        }
    }
}

/// Runs a command with the given argv and captures stdout/stderr.
///
/// Returns `Err` only if the process fails to spawn (e.g. binary not found).
/// Check `CommandOutput::success` for exit status.
pub fn run_capture(argv: &[&str]) -> std::io::Result<CommandOutput> {
    spawn_capture(argv, &[])
}

/// Runs a command with explicit environment pairs and captures
/// stdout/stderr. Shares the single spawning implementation with
/// [`run_capture`]; prefer it over hand-rolled `Command` assembly
/// whenever a child needs caller-provided env.
pub fn run_capture_with_env(
    argv: &[&str],
    env: &[(String, String)],
) -> std::io::Result<CommandOutput> {
    spawn_capture(argv, env)
}

fn spawn_capture(argv: &[&str], env: &[(String, String)]) -> std::io::Result<CommandOutput> {
    if argv.is_empty() {
        return Ok(CommandOutput {
            success: false,
            stdout: String::new(),
            stderr: String::new(),
        });
    }
    let mut command = Command::new(argv[0]);
    command.args(&argv[1..]);
    for (key, value) in env {
        command.env(key, value);
    }
    command
        .output()
        .map(|output| CommandOutput::from_output(&output))
}

/// Runs a shell command string using the platform shell (`sh -c` / `cmd /C`).
///
/// Convenience wrapper over the shared capture implementation that
/// prepends the shell prefix.
pub fn run_shell_capture(command: &str) -> std::io::Result<CommandOutput> {
    let (prog, arg) = shell_cmd();
    spawn_capture(&[prog, arg, command], &[])
}

pub fn run_command(argv: &[&str]) -> bool {
    if argv.is_empty() {
        return false;
    }
    Command::new(argv[0])
        .args(&argv[1..])
        .status()
        .is_ok_and(|s| s.success())
}

pub fn add_unique_path(list: &mut Vec<String>, path: &str, project_root: Option<&str>) -> bool {
    if list.iter().any(|p| p == path) {
        return false;
    }
    if let Some(root) = project_root {
        if !root.is_empty() {
            let full_path = if root.ends_with('/') {
                format!("{root}{path}")
            } else {
                format!("{root}/{path}")
            };
            if !std::path::Path::new(&full_path).exists() {
                return false;
            }
        }
    }
    list.push(path.to_string());
    true
}
