//! Dictation bounds: one place for the `--once` capture ceiling,
//! deadline math, and capture-device choice.

/// Default `--once` capture in seconds.
pub const DEFAULT_DICTATE_SECS: u64 = 30;
/// Hard ceiling for `--once` capture in seconds.
pub const MAX_DICTATE_SECS: u64 = 120;
/// Hard sample cap at the widest supported rate (120 s at 48 kHz).
pub const MAX_DICTATE_SAMPLES: usize = 120 * 48_000;

/// Which capture device `--once` opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureChoice {
    /// Default input device.
    Default,
    /// Named input device from `audio.mic`.
    Named(String),
}

/// Validate `--seconds`: zero and over-ceiling values refuse with the
/// bound named. Returns the capture duration on success.
pub fn validate_seconds(seconds: u64) -> Result<std::time::Duration, String> {
    if seconds == 0 {
        return Err("dictate --seconds 0 is invalid: pass 1..=120".to_string());
    }
    if seconds > MAX_DICTATE_SECS {
        return Err(format!(
            "dictate --seconds {seconds} exceeds the {MAX_DICTATE_SECS} s ceiling"
        ));
    }
    Ok(std::time::Duration::from_secs(seconds))
}

/// Choose the capture device from config. Unknown audio backends refuse;
/// the default mic opens the default device, any other name opens that
/// device. System-monitor and echo-cancel leaves stay duplex-reserved
/// and are not consulted by `--once` (mic-only by design).
pub fn capture_choice(backend: &str, mic: &str) -> Result<CaptureChoice, String> {
    if backend != "cpal" {
        return Err(format!(
            "unknown audio backend: {backend} (only `cpal` is supported)"
        ));
    }
    if mic == "default" || mic.trim().is_empty() {
        Ok(CaptureChoice::Default)
    } else {
        Ok(CaptureChoice::Named(mic.to_string()))
    }
}

/// Finish one dictated turn: refine, record clean text, inject with
/// continuation. Returns the ladder outcome so the caller reports the
/// actual sink (single print: injections echo, `OutputOnly` already
/// printed).
///
/// The refine chain runs with a declining LLM rung: one-shot CLI
/// dictation stays raw because refining would probe-or-boot a server
/// per utterance (unbounded, offline-hostile). The rung fills where a
/// queue is owned and measured (calibration law); the save step still
/// records every final text into `history`.
pub fn finish_turn(
    text: &str,
    memory: &mut orator_desktop::LadderMemory,
    history: &mut Vec<String>,
    clip: &dyn orator_desktop::Clipboard,
    chord: &dyn orator_desktop::ChordSender,
) -> Result<orator_desktop::InjectOutcome, String> {
    let refined = orator_desktop::refine_transcript(text, &|_| None, &mut |clean| {
        history.push(clean.to_string());
    });
    orator_desktop::inject(clip, chord, &refined.text, memory).map_err(|error| error.to_string())
}

/// Checked deadline from now plus `duration`. Fails closed instead of
/// panicking on overflow.
pub fn checked_deadline(
    now: std::time::Instant,
    duration: std::time::Duration,
) -> Result<std::time::Instant, String> {
    now.checked_add(duration)
        .ok_or_else(|| "dictate deadline overflows the clock".to_string())
}

/// Reserve bound for `seconds` at `rate` Hz, capped at
/// [`MAX_DICTATE_SAMPLES`].
#[must_use]
pub fn reserve_for(seconds: u64, rate: u32) -> usize {
    let want = seconds.saturating_mul(u64::from(rate)) as usize;
    want.min(MAX_DICTATE_SAMPLES)
}

#[cfg(test)]
#[path = "../tests/dictate.rs"]
mod tests;
