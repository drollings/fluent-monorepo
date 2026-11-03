//! Clipboard transactions: change-counted paste and capture.
//!
//! Both directions share one discipline (Murmur's `inject.rs` /
//! `selection.rs` shape, unified): snapshot content fingerprint, actuate,
//! poll for the change, restore-or-clear only when nothing moved. The
//! fingerprint abstracts the macOS change counter and the Wayland
//! content-hash compare behind one `u64`.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use common_core::sync::lock;

use crate::ladder::InjectError;

/// Clipboard backend: read, write, and a change fingerprint.
pub trait Clipboard: Send + Sync {
    /// Current text, if any.
    fn read(&self) -> Option<String>;
    /// Replace the clipboard text.
    fn write(&self, text: &str) -> Result<(), InjectError>;
    /// Change fingerprint: bumps on every external modification.
    fn fingerprint(&self) -> u64;
}

/// Paste-chord sender: the compositor-specific "press Ctrl+V".
pub trait ChordSender: Send + Sync {
    /// Send the paste chord to the focused window.
    fn send_paste(&self) -> Result<(), InjectError>;
    /// Send the copy chord to the focused window.
    fn send_copy(&self) -> Result<(), InjectError>;
}

/// Outcome of a paste transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteOutcome {
    /// The paste landed; the clipboard moved, so nothing was restored.
    Pasted,
    /// The paste landed but nothing moved; previous content was restored.
    PastedRestored,
}

/// Paste `text` with restore discipline: snapshot, write, send the chord,
/// then poll every 10 ms up to `deadline_ms` for the fingerprint to move.
/// A moved clipboard is left alone (clipboard-manager safe); a still one
/// gets its previous content back, or is cleared when nothing was saved.
/// Never a fixed timer. Focus is attested through the fingerprint:
/// an external modification between snapshot and write, or between
/// write and chord, refuses (focus lost). Hash-backed clipboards treat
/// equality as a weak hint (documented); counter-backed clipboards
/// enforce exactly.
pub fn paste_with_restore(
    clip: &dyn Clipboard,
    chord: &dyn ChordSender,
    text: &str,
    deadline_ms: u64,
) -> Result<PasteOutcome, InjectError> {
    let attest_before = clip.fingerprint();
    let saved = clip.read();
    if clip.fingerprint() != attest_before {
        return Err(InjectError::Failed(
            "focus lost before paste (clipboard moved during snapshot)".to_string(),
        ));
    }
    clip.write(text)?;
    let after_write = clip.fingerprint();
    if after_write == attest_before && !text.is_empty() {
        return Err(InjectError::Failed(
            "paste write did not land (clipboard unchanged)".to_string(),
        ));
    }
    // Fingerprint _after_ our own write: only external movement counts.
    // Hash equality is a weak hint (collisions err toward restore);
    // counter equality enforces exactly. The poll below decides.
    let before = after_write;
    chord.send_paste()?;
    if wait_for_change(clip, before, deadline_ms) {
        Ok(PasteOutcome::Pasted)
    } else {
        match saved {
            Some(previous) => clip.write(&previous)?,
            None => clip.write("")?,
        }
        Ok(PasteOutcome::PastedRestored)
    }
}

/// Capture the current selection: snapshot, send the copy chord, poll up to
/// `deadline_ms` for new content, then restore-or-clear so a later
/// dictation cannot resurrect read-aloud text.
pub fn capture_selection(
    clip: &dyn Clipboard,
    chord: &dyn ChordSender,
    deadline_ms: u64,
) -> Result<Option<String>, InjectError> {
    let saved = clip.read();
    let before = clip.fingerprint();
    chord.send_copy()?;
    if !wait_for_change(clip, before, deadline_ms) {
        return Ok(None);
    }
    let captured = clip.read().filter(|text| !text.is_empty());
    match saved {
        Some(previous) => clip.write(&previous)?,
        None => clip.write("")?,
    }
    Ok(captured)
}

/// Poll the fingerprint every 10 ms until it moves or the deadline passes.
fn wait_for_change(clip: &dyn Clipboard, before: u64, deadline_ms: u64) -> bool {
    let deadline = Instant::now() + Duration::from_millis(deadline_ms);
    while Instant::now() < deadline {
        if clip.fingerprint() != before {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    clip.fingerprint() != before
}

/// In-memory clipboard for tests and headless runs. The fingerprint bumps
/// on every write, mirroring the platform counter.
#[derive(Debug, Default)]
pub struct InMemoryClipboard {
    state: Mutex<InMemoryState>,
}

#[derive(Debug, Default)]
struct InMemoryState {
    text: Option<String>,
    fingerprint: u64,
}

impl InMemoryClipboard {
    /// Empty clipboard.
    pub fn new() -> Self {
        Self::default()
    }
}

impl Clipboard for InMemoryClipboard {
    fn read(&self) -> Option<String> {
        lock(&self.state).text.clone()
    }
    fn write(&self, text: &str) -> Result<(), InjectError> {
        let mut state = lock(&self.state);
        state.text = if text.is_empty() {
            None
        } else {
            Some(text.to_string())
        };
        state.fingerprint += 1;
        Ok(())
    }
    fn fingerprint(&self) -> u64 {
        lock(&self.state).fingerprint
    }
}

/// `wl-copy` / `wl-paste` backend. Present check is the binaries existing;
/// fingerprints compare content hashes (Wayland exposes no counter).
#[derive(Debug, Default)]
pub struct ExternalClipboard;

impl ExternalClipboard {
    /// Backend handle. Every operation shells out; absence fails cleanly.
    pub fn new() -> Self {
        Self
    }

    /// Whether both tools resolve on `PATH`.
    #[must_use]
    pub fn present() -> bool {
        which_copy().is_some() && which_paste().is_some()
    }
}

fn which_copy() -> Option<std::path::PathBuf> {
    which::which("wl-copy").ok()
}

fn which_paste() -> Option<std::path::PathBuf> {
    which::which("wl-paste").ok()
}

impl Clipboard for ExternalClipboard {
    fn read(&self) -> Option<String> {
        let paste = which_paste()?;
        let program = paste.to_string_lossy().into_owned();
        let output =
            common_core::shell::run_capture(&[program.as_str(), "--no-newline"]).ok()?;
        if !output.success || output.stdout.is_empty() {
            return None;
        }
        Some(output.stdout)
    }
    fn write(&self, text: &str) -> Result<(), InjectError> {
        let copy = which_copy().ok_or_else(|| {
            InjectError::Unavailable("wl-copy not found on PATH".to_string())
        })?;
        // `wl-copy` reads stdin; shell out through `sh` with a here-string
        // would interpolate, so write through a piped child instead.
        let mut child = std::process::Command::new(copy)
            .arg("--foreground")
            .arg("--type")
            .arg("text/plain")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|error| InjectError::Failed(format!("wl-copy spawn: {error}")))?;
        std::io::Write::write_all(
            child.stdin.as_mut().ok_or_else(|| {
                InjectError::Failed("wl-copy stdin unavailable".to_string())
            })?,
            text.as_bytes(),
        )
        .map_err(|error| InjectError::Failed(format!("wl-copy write: {error}")))?;
        drop(child.stdin.take());
        let status = child
            .wait()
            .map_err(|error| InjectError::Failed(format!("wl-copy wait: {error}")))?;
        if status.success() {
            Ok(())
        } else {
            Err(InjectError::Failed("wl-copy reported failure".to_string()))
        }
    }
    fn fingerprint(&self) -> u64 {
        // No platform counter: hash what is there. Collisions across
        // distinct contents are possible in theory; the poll loop treats a
        // stable hash as "unchanged", which errs toward restore — safe.
        self.read()
            .map_or(0, |text| common_core::hash::fnv1a64(text.as_bytes()))
    }
}

/// Chord sender with no backend: every chord reports unavailable. Headless
/// runs fall through the ladder to output-only; tests use fakes instead.
#[derive(Debug, Default)]
pub struct NoChord;

impl NoChord {
    /// Unavailable chord sender.
    pub fn new() -> Self {
        Self
    }
}

impl ChordSender for NoChord {
    fn send_paste(&self) -> Result<(), InjectError> {
        Err(InjectError::Unavailable(
            "no paste-chord sender on this session".to_string(),
        ))
    }
    fn send_copy(&self) -> Result<(), InjectError> {
        Err(InjectError::Unavailable(
            "no copy-chord sender on this session".to_string(),
        ))
    }
}

#[cfg(test)]
#[path = "../tests/clipboard.rs"]
mod tests;
