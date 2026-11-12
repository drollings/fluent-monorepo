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
/// Outcome of a paste transaction.
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
    if let Err(error) = clip.write(text) {
        // Best-effort restore: our attempt may have cleared the seat
        // before dying (a killed `wl-copy` drops what it held), so put
        // the snapshot back instead of leaving the loss behind. The
        // restore itself is best-effort — the original error reports.
        match saved {
            Some(previous) => {
                let _ = clip.write(&previous);
            }
            None => {
                let _ = clip.write("");
            }
        }
        return Err(error);
    }
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

/// Poll quantum in ms shared by every clipboard child wait: matches
/// the fingerprint poll below so one rhythm owns all clipboard timing.
pub const CLIPBOARD_POLL_MS: u64 = 10;

/// Bound in ms for one clipboard child serve (`wl-copy --foreground`
/// holding the selection, `wl-paste` dumping it). A live compositor
/// answers in tens of ms; the bound sits two orders above that, so
/// only a wedged or seatless server trips it. Firing kills the child,
/// reaps it, restores the snapshot, and reports — never a hang, never
/// a zombie, never lost content. See the calibration register.
pub const CLIPBOARD_CHILD_DEADLINE_MS: u64 = 2000;

/// Why a bounded clipboard-child wait ended without a status.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Why a bounded clipboard-child wait ended without a status.
pub(crate) enum ChildWaitError {
    /// The deadline passed: the child was killed and reaped.
    Timeout,
    /// The wait itself failed (e.g. `try_wait` refused).
    Failed(String),
}

/// Wait for `child` up to `deadline_ms`, polling every
/// [`CLIPBOARD_POLL_MS`]. An exited child reports its status (late or
/// not — status is status); a still-running child at the deadline is
/// killed and reaped, then reported as [`ChildWaitError::Timeout`].
/// Never blocks past the deadline; never leaves a zombie.
pub(crate) fn wait_child_bounded(
    child: &mut std::process::Child,
    deadline_ms: u64,
) -> Result<std::process::ExitStatus, ChildWaitError> {
    let deadline = Instant::now() + Duration::from_millis(deadline_ms);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    match child.wait() {
                        Ok(_) => return Err(ChildWaitError::Timeout),
                        Err(error) => {
                            return Err(ChildWaitError::Failed(format!(
                                "reap after kill failed: {error}"
                            )));
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(CLIPBOARD_POLL_MS));
            }
            Err(error) => {
                return Err(ChildWaitError::Failed(format!("wait failed: {error}")));
            }
        }
    }
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
/// In-memory clipboard for tests and headless runs. The fingerprint bumps
/// on every write, mirroring the platform counter.
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
/// `wl-copy` / `wl-paste` backend. Present check is the binaries existing;
/// fingerprints compare content hashes (Wayland exposes no counter).
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
        let mut child = std::process::Command::new(paste)
            .arg("--no-newline")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        // Drain on a side thread so a large dump never wedges the
        // pipe while the bounded wait below runs: the kill on expiry
        // closes the write end, so the drain always terminates.
        let stdout = child.stdout.take();
        let drain = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut out) = stdout {
                use std::io::Read as _;
                let _ = out.read_to_end(&mut buf);
            }
            buf
        });
        let status = wait_child_bounded(&mut child, CLIPBOARD_CHILD_DEADLINE_MS).ok()?;
        if !status.success() {
            return None;
        }
        let raw = drain.join().ok()?;
        let text = String::from_utf8_lossy(&raw).into_owned();
        if text.is_empty() { None } else { Some(text) }
    }
    fn write(&self, text: &str) -> Result<(), InjectError> {
        let copy = which_copy().ok_or_else(|| {
            InjectError::Unavailable("wl-copy not found on PATH".to_string())
        })?;
        // `wl-copy` reads stdin; shell out through a piped child instead
        // of a here-string (no interpolation). `--foreground` keeps the
        // selection held by our child — ordered and observable — while
        // the bounded wait below keeps a seatless server from hanging
        // the caller: expiry kills, reaps, and reports with the remedy.
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
        match wait_child_bounded(&mut child, CLIPBOARD_CHILD_DEADLINE_MS) {
            Ok(status) if status.success() => Ok(()),
            Ok(_) => Err(InjectError::Failed("wl-copy reported failure".to_string())),
            Err(ChildWaitError::Timeout) => Err(InjectError::Failed(format!(
                "wl-copy --foreground exceeded {CLIPBOARD_CHILD_DEADLINE_MS} ms without serving the selection (compositor busy or absent)"
            ))),
            Err(ChildWaitError::Failed(detail)) => {
                Err(InjectError::Failed(format!("wl-copy wait: {detail}")))
            }
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
/// Chord sender with no backend: every chord reports unavailable. Headless
/// runs fall through the ladder to output-only; tests use fakes instead.
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