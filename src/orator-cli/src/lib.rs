#![forbid(unsafe_code)]

//! orator-cli: the `orator` command surface.
//!
//! The clap surface is generated from `COMMANDS`, and every socket request
//! the CLI builds is validated against the wire contract in `orator-core`.
//! The parity test pins both, so neither side can drift alone.

use clap::{CommandFactory, Parser, Subcommand};
use orator_core::protocol::{validate_method, METHOD_NAMES};

/// Shell integration surface.
pub mod doctor;

/// One error envelope for every CLI failure line.
pub mod error;

/// Configuration helpers: backend resolution and dotted-key access.
pub mod config;

/// Honest `models verify` reporting.
pub mod models;

/// Shell scripts.
pub mod shell;

/// Daemon socket client.
pub mod socket;

/// Chunked playout orchestration (synth-ahead while the device drains).
pub mod stream;

/// Voice fallback decisions shared by the `say` path.
pub mod say_chain;

/// Terminal duplex client (levels, transcript, barge key, sessions).
pub mod tui;

/// Offline transcription rendering.
pub mod transcribe;

/// Local stat-file rendering (same bytes as the daemon path).
pub mod stats;

/// Dictation capture bounds (seconds ceiling, deadline, device choice).
pub mod dictate;

/// Local-first voice fallback (daemon first, local specialized second).
pub mod fallback;

/// Scrolling-log events for `chat` (text + JSON renderers).
pub mod chatlog;

/// `chat` turn helpers (slash parsing, voice/text modes, labels).
pub mod chat;

/// Ask preflight (strict ports, fail-fast weights, speak notes).
pub mod ask;

/// Canonical CLI command names. The clap surface must equal exactly this
/// set (pinned by test); every entry is implemented in `run` below.
pub const COMMANDS: [&str; 19] = [
    "say",
    "listen",
    "transcribe",
    "ask",
    "chat",
    "dictate",
    "ptt",
    "meeting",
    "models",
    "cmd",
    "approve",
    "status",
    "stats",
    "doctor",
    "config",
    "daemon",
    "shell-init",
    "completions",
    "tui",
];

/// Stable process exit codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    /// Success.
    Success,
    /// Runtime error.
    Error,
    /// Usage error (clap exits 2 on its own for parse failures).
    Usage,
    /// Daemon unavailable.
    DaemonUnavailable,
    /// Denied by policy.
    Denied,
    /// Interrupted (SIGINT).
    Interrupted,
}

impl ExitCode {
    /// Numeric code for process exit.
    #[must_use]
    pub const fn code(self) -> i32 {
        match self {
            Self::Success => 0,
            Self::Error => 1,
            Self::Usage => 2,
            Self::DaemonUnavailable => 3,
            Self::Denied => 4,
            Self::Interrupted => 130,
        }
    }
}

/// A validated outbound socket request under construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundRequest {
    /// Wire-contract method name.
    pub method: String,
}

impl OutboundRequest {
    /// Build a request for `method`, rejecting anything outside the
    /// contract shared with the daemon.
    pub fn new(method: &str) -> Result<Self, orator_core::ProtocolError> {
        validate_method(method)?;
        Ok(Self {
            method: method.to_string(),
        })
    }
}

/// The `orator` command line.
#[derive(Debug, Parser)]
#[command(name = "orator", about = "Local voice assistant")]
pub struct Cli {
    /// Daemon transport: `socket` (default) or `ws`.
    #[arg(long, default_value = "socket", global = true)]
    pub transport: String,
    /// Bypass the socket preflight (wrong uid/mode). Explicit only:
    /// without this the CLI refuses a foreign socket instead of dialing.
    #[arg(long, global = true)]
    pub insecure: bool,
    /// Subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// One variant per entry in `COMMANDS`, pinned by the parity test.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Speak text or stdin; returns when playback ends.
    Say {
        /// Text to speak. Empty reads stdin.
        #[arg(default_values_t = Vec::<String>::new())]
        text: Vec<String>,
        /// TTS backend: daemon, kokoro, audio-server. Empty follows config.
        #[arg(long, default_value = "")]
        backend: String,
    },
    /// Stream the live transcript to stdout.
    Listen {
        /// Capture system audio.
        #[arg(long)]
        sys: bool,
        /// Capture the microphone.
        #[arg(long)]
        mic: bool,
        /// Emit JSON lines.
        #[arg(long)]
        json: bool,
    },
    /// Offline transcription of a file.
    Transcribe {
        /// Audio file (WAV) to transcribe.
        file: std::path::PathBuf,
        /// Output format: txt, srt, vtt, json.
        #[arg(long, default_value = "txt")]
        format: String,
        /// STT backend: whisper, moonshine, audio-server. Empty follows config.
        #[arg(long, default_value = "")]
        backend: String,
    },
    /// One-shot question to the local LLM.
    Ask {
        /// The question to ask.
        question: String,
        /// Speak the reply when playback is available.
        #[arg(long)]
        speak: bool,
    },
    /// Interactive assistant in the terminal.
    Chat {
        /// Log wire shape: human text on stderr, JSON lines on stdout.
        /// Line input sends; the live mic (when a capture device
        /// answers) endpoints VAD-gated turns in auto mode or on /talk
        /// toggle-off in ptt. /quit exits, /reset clears, /wav + /input
        /// voice a file, /mic auto|ptt gates file and mic turns, /talk
        /// toggles, /voice off|short|full, /text short|full, /barge
        /// on|off (voice barge default on; key barge always works).
        /// Session starts come from the chat.* config leaves
        /// (chat.voice/text/mic/barge); slash commands override them
        /// per session. Chords match the TUI (Ctrl+B barge, Ctrl+Q
        /// quit, Ctrl+G mic mode, Ctrl+T talk) typed as the control
        /// character plus Enter; Ctrl+C aborts, Ctrl+D ends input.
        /// Barge cuts the live turn and stops playout at once; the
        /// interrupting utterance becomes the next turn. Key barge
        /// (Ctrl+B) always cuts; voice barge cuts speech the VAD
        /// sustains past turn.barge_in_ms. With barge off the mic
        /// sheds while a turn runs.
        #[arg(long, default_value = "text")]
        log_format: String,
        /// Verbosity: 0 hides agent tool-call detail, 1+ reveals it.
        #[arg(long, default_value_t = 0)]
        verbosity: u8,
    },
    /// Dictation into the focused window.
    Dictate {
        /// Record once and exit instead of holding to talk.
        #[arg(long)]
        once: bool,
        /// Seconds to record in `--once` mode (1..=120).
        #[arg(long, default_value_t = 30)]
        seconds: u64,
        /// Print the transcript to stdout as well (shell fill).
        #[arg(long)]
        stdout: bool,
        /// STT backend: whisper, moonshine, audio-server. Empty follows config.
        #[arg(long, default_value = "")]
        backend: String,
    },
    /// Push-to-talk session control.
    Ptt {
        /// Action: start, stop.
        #[arg(default_value = "")]
        action: String,
    },
    /// Meeting lifecycle and re-runs.
    Meeting {
        /// Action: start, stop, status, list, show, export.
        #[arg(default_value = "")]
        action: String,
        /// Meeting id for id-addressed actions.
        #[arg(default_value = "")]
        id: String,
    },
    /// Model management.
    Models {
        /// Models action: list, pull, rm, verify.
        #[command(subcommand)]
        action: ModelsAction,
    },
    /// Propose a shell command (never runs it).
    Cmd {
        /// What should be done, in words.
        #[arg(default_values_t = Vec::<String>::new())]
        request: Vec<String>,
    },
    /// Answer a pending approval.
    Approve {
        /// Approval id.
        #[arg(default_value = "")]
        id: String,
        /// Deny instead of approving.
        #[arg(long)]
        deny: bool,
    },
    /// Daemon state.
    Status {
        /// Reserved for future selectors.
        #[arg(default_value = "")]
        selector: String,
    },
    /// Metrics and counters.
    Stats {
        /// Stat file to render locally. Empty dials the daemon.
        #[arg(default_value = "")]
        file: String,
        /// Reserved for future selectors.
        #[arg(default_value = "")]
        selector: String,
    },
    /// Environment diagnosis.
    Doctor,
    /// Configuration administration.
    Config {
        /// Action: get, set, path.
        #[arg(default_value = "")]
        action: String,
        /// Key for get/set.
        #[arg(default_value = "")]
        key: String,
        /// Value for set.
        #[arg(default_value = "")]
        value: String,
    },
    /// Daemon lifecycle.
    Daemon {
        /// Action: run, stop, reload.
        #[arg(default_value = "")]
        action: String,
    },
    /// Emit shell integration scripts.
    ShellInit {
        /// Shell: bash, zsh, fish.
        shell: String,
    },
    /// Emit shell completions.
    Completions {
        /// Shell: bash, zsh, fish.
        shell: String,
    },
    /// Terminal duplex client (levels, transcript, barge key, sessions).
    Tui,
}

/// Model-management actions.
#[derive(Debug, Subcommand)]
pub enum ModelsAction {
    /// List catalog entries with presence and verification state.
    List,
    /// Download a pinned model through the artifact plane.
    Pull {
        /// Catalog tag (`tiny`, `liquidai/lfm2.5-2.6b`, `kokoro-82m`).
        name: String,
    },
    /// Remove a local model file.
    Rm {
        /// Catalog tag.
        name: String,
    },
    /// Re-hash local model files.
    Verify {
        /// Catalog tag. Empty means all present files.
        #[arg(default_value = "")]
        name: String,
    },
}

/// Render a `doctor` report for humans. One section per probe, then
/// actionable notes. Empty notes line means green.
#[must_use]
pub fn render_doctor(report: &orator_audio::DoctorReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("orator doctor\n");
    let _ = writeln!(out, "devices: {}", report.devices.len());
    for device in &report.devices {
        let _ = writeln!(out, "  - {device}");
    }
    let _ = writeln!(
        out,
        "opus: {}",
        if report.opus_ok { "ok" } else { "FAILED" }
    );
    let _ = writeln!(
        out,
        "models: {}",
        report.models_dir.as_ref().map_or_else(
            || "unknown (HOME and XDG_DATA_HOME are unset)".to_string(),
            |dir| dir.display().to_string()
        )
    );
    if report.notes.is_empty() {
        out.push_str("notes: none — green\n");
    } else {
        for note in &report.notes {
            let _ = writeln!(out, "note: {note}");
        }
    }
    out
}

impl Cli {
    /// Subcommand names the parser accepts, in declaration order.
    #[must_use]
    pub fn surface() -> Vec<String> {
        Self::command()
            .get_subcommands()
            .map(|sub| sub.get_name().to_string())
            .collect()
    }

    /// Every wire-contract method the CLI can address.
    #[must_use]
    pub fn methods() -> Vec<&'static str> {
        METHOD_NAMES.to_vec()
    }
}

/// Await `task` unless a shutdown signal (SIGTERM/SIGINT) arrives first.
/// On a signal, `on_signal` runs — release the endpoint, stop the child —
/// and `None` returns so the caller exits with `Interrupted` instead of
/// hanging on work nobody will hear.
pub async fn cancel_on_signal<T>(
    task: impl std::future::Future<Output = T>,
    on_signal: impl FnOnce(),
) -> Option<T> {
    tokio::select! {
        out = task => Some(out),
        () = orator_core::shutdown_signal() => {
            on_signal();
            None
        }
    }
}

/// Watch for a shutdown signal on a background thread while blocking work
/// runs on the caller. When the signal arrives, `release` runs and
/// [`SignalWatch::signalled`] starts reporting true. Dropping the watch
/// disarms it: a later signal no longer releases.
///
/// A parked thread per armed watch; the CLI arms at most one per command,
/// and the thread exits with the process, so nothing accumulates.
pub struct SignalWatch {
    armed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    signalled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl SignalWatch {
    /// Arm the watch: `release` runs once if the signal arrives first.
    pub fn arm(release: impl FnOnce() + Send + 'static) -> Self {
        use std::sync::atomic::Ordering;
        let armed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let signalled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (armed_task, signalled_task) = (
            std::sync::Arc::clone(&armed),
            std::sync::Arc::clone(&signalled),
        );
        std::thread::spawn(move || {
            common_core::runtime::block_on(orator_core::shutdown_signal());
            signalled_task.store(true, Ordering::SeqCst);
            if armed_task.swap(false, Ordering::SeqCst) {
                release();
            }
        });
        Self { armed, signalled }
    }

    /// Whether the signal arrived while armed.
    #[must_use]
    pub fn signalled(&self) -> bool {
        self.signalled.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for SignalWatch {
    fn drop(&mut self) {
        self.armed.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
#[path = "../tests/commands.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/signal.rs"]
mod signal_tests;
