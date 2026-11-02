#![forbid(unsafe_code)]

//! orator-cli shell integration: `shell-init` scripts and completions.

use std::io::Write;

use clap::CommandFactory;

use crate::Cli;

/// Shells with generated integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    /// Bourne-again shell.
    Bash,
    /// Z shell.
    Zsh,
    /// Friendly interactive shell.
    Fish,
}

impl Shell {
    /// Parse a shell name. Unknown names are rejected, never defaulted.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            _ => None,
        }
    }
}

/// Readline-style integration script: binds `Ctrl-X v` to dictation fill
/// without leaving the prompt. Fish uses its own bind syntax.
#[must_use]
pub fn init_script(shell: Shell) -> String {
    match shell {
        Shell::Bash => r#"# orator shell integration: Ctrl-X v fills the line from dictation.
_orator_fill() { READLINE_LINE="$(orator dictate --once --stdout)${READLINE_LINE}"; }
bind -x '"\C-xv":_orator_fill'
"#
        .to_string(),
        Shell::Zsh => r#"# orator shell integration.
_orator_fill() { BUFFER="$(orator dictate --once --stdout)${BUFFER}"; }
zle -N _orator_fill; bindkey '^Xv' _orator_fill
"#
        .to_string(),
        Shell::Fish => r"# orator shell integration.
function _orator_fill; commandline -i (orator dictate --once --stdout); end
bind \cxv _orator_fill
"
        .to_string(),
    }
}

/// Completions for `shell` in clap's supported shells.
pub fn write_completions(shell: &str, out: &mut dyn Write) -> Result<(), String> {
    match shell {
        "bash" => {
            clap_complete::generate(clap_complete::shells::Bash, &mut Cli::command(), "orator", out);
        }
        "zsh" => {
            clap_complete::generate(clap_complete::shells::Zsh, &mut Cli::command(), "orator", out);
        }
        "fish" => {
            clap_complete::generate(clap_complete::shells::Fish, &mut Cli::command(), "orator", out);
        }
        _ => return Err(format!("unsupported shell: {shell}")),
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/shell.rs"]
mod tests;
