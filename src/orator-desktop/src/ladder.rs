//! Injection ladder: ordered fallback across input rungs.
//!
//! Wayland offers no universal synthetic-input API, so the daemon tries
//! rungs in order and remembers the first that works. The ladder runs on
//! `first_accept_in_order`: terminal errors abort, clean exhaustion yields
//! the output-only fallback. Rungs that need unwired backends report
//! `Unavailable` (skip), never fake success.

use std::sync::Arc;

use fluent_wvr::ladder::first_accept_in_order_sync;
use thiserror::Error;

use crate::clipboard::{paste_with_restore, ChordSender, Clipboard, PasteOutcome};

/// Injection failures. Only `Fatal` stops the ladder; everything else
/// tries the next rung.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
/// Injection failures. Only `Fatal` stops the ladder; everything else
/// tries the next rung.
pub enum InjectError {
    /// Rung cannot run here (unwired backend, missing tool). Skips.
    #[error("injection rung unavailable: {0}")]
    Unavailable(String),
    /// Rung failed transiently (chord lost, clipboard busy). Skips.
    #[error("injection failed: {0}")]
    Failed(String),
    /// Rung must not be retried or skipped past (hard permission denial).
    #[error("injection fatal: {0}")]
    Fatal(String),
}

impl InjectError {
    /// Whether the ladder must stop instead of trying the next rung.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Fatal(_))
    }
}

/// Injection rungs, strongest first.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Injection rungs, strongest first.
pub enum Rung {
    /// Portal RemoteDesktop with libei emulation. Unwired: later work.
    RemoteDesktop,
    /// Clipboard write plus synthetic paste chord.
    ClipboardPaste,
    /// Compositor virtual-keyboard protocol (wlroots family only).
    VirtualKeyboard,
    /// Kernel uinput device (privileged).
    Uinput,
    /// Stdout/clipboard only, with a user notification.
    OutputOnly,
}

impl Rung {
    /// Full ladder order.
    pub const ORDER: [Self; 5] = [
        Self::RemoteDesktop,
        Self::ClipboardPaste,
        Self::VirtualKeyboard,
        Self::Uinput,
        Self::OutputOnly,
    ];
}

/// How the text reached the user.
#[derive(Debug, Clone, PartialEq, Eq)]
/// How the text reached the user.
pub enum InjectOutcome {
    /// A rung injected the text (which one).
    Injected(Rung),
    /// No rung could inject; the text went to stdout instead.
    OutputOnly(String),
}

/// Remembers the first working rung per session type. The next injection
/// tries the remembered rung first, then the full order — a stale memory
/// costs one failed attempt, never correctness. Also stamps the last
/// successful injection time so consecutive dictation in the same window
/// joins with a leading space (Murmur's 10 s continuation rule).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
/// Remembers the first working rung per session type. The next injection
/// tries the remembered rung first, then the full order — a stale memory
/// costs one failed attempt, never correctness. Also stamps the last
/// successful injection time so consecutive dictation in the same window
/// joins with a leading space (Murmur's 10 s continuation rule).
pub struct LadderMemory {
    /// Last rung that reported success, if any.
    pub last_working: Option<Rung>,
    /// When the last injection succeeded, if any.
    pub last_inject: Option<std::time::Instant>,
}

/// Inject `text` using `clip` and `chord`, honoring `memory`. Returns how
/// the text reached the user; terminal rung errors propagate. Text
/// injected within the continuation window of the previous success
/// gains a leading space so `"One." + "Two."` lands as `"One. Two."`;
/// separate processes start with empty memory (no prefix).
pub fn inject(
    clip: &dyn Clipboard,
    chord: &dyn ChordSender,
    text: &str,
    memory: &mut LadderMemory,
) -> Result<InjectOutcome, InjectError> {
    use crate::continuation::continuation_prefix;
    let now = std::time::Instant::now();
    // Single-session callers share one window; cross-window callers
    // would pass `false` here once focus tracking lands.
    let spoken = format!(
        "{}{text}",
        continuation_prefix(memory.last_inject, true, now)
    );
    let mut order = Vec::with_capacity(Rung::ORDER.len() + 1);
    if let Some(remembered) = memory.last_working.clone() {
        if Rung::ORDER.contains(&remembered) {
            order.push(remembered);
        }
    }
    for rung in Rung::ORDER {
        if !order.contains(&rung) {
            order.push(rung);
        }
    }
    let outcome = first_accept_in_order_sync(
        order,
        |rung| attempt_rung(&rung, clip, chord, &spoken).map(|fired| fired.map(|()| rung)),
        InjectError::is_terminal,
    );
    match outcome {
        Ok(Some(rung)) => {
            memory.last_working = Some(rung.clone());
            memory.last_inject = Some(now);
            if rung == Rung::OutputOnly {
                Ok(InjectOutcome::OutputOnly(spoken))
            } else {
                Ok(InjectOutcome::Injected(rung))
            }
        }
        Ok(None) => Ok(InjectOutcome::OutputOnly(spoken)),
        Err(error) => Err(error),
    }
}

/// Try one rung: `Ok(Some(()))` injected, `Ok(None)` unavailable (skip),
/// `Err` terminal-or-skippable per [`InjectError::is_terminal`].
fn attempt_rung(
    rung: &Rung,
    clip: &dyn Clipboard,
    chord: &dyn ChordSender,
    text: &str,
) -> Result<Option<()>, InjectError> {
    match rung {
        Rung::RemoteDesktop => Err(InjectError::Unavailable(
            "portal RemoteDesktop unwired".to_string(),
        )),
        Rung::ClipboardPaste => match paste_with_restore(clip, chord, text, 180) {
            Ok(PasteOutcome::Pasted | PasteOutcome::PastedRestored) => Ok(Some(())),
            Err(InjectError::Unavailable(reason)) => {
                Err(InjectError::Unavailable(reason))
            }
            Err(other) => Err(other),
        },
        Rung::VirtualKeyboard => Err(InjectError::Unavailable(
            "virtual keyboard needs a wlroots compositor".to_string(),
        )),
        Rung::Uinput => Err(InjectError::Unavailable(
            "uinput needs device-group membership".to_string(),
        )),
        Rung::OutputOnly => {
            // No print here: the caller owns the single print per
            // `--stdout`/sink policy, so headless and headed paths
            // print exactly once through the same gate.
            Ok(Some(()))
        },
    }
}

/// Shared clipboard behind one handle for session tasks.
pub type SharedClipboard = Arc<dyn Clipboard>;

#[cfg(test)]
#[path = "../tests/ladder.rs"]
mod tests;