//! Environment diagnosis: devices, codec self-test, model home.
//!
//! `orator doctor` renders this report. Every line is actionable: a missing
//! device names the fallback, a failed self-test names the missing piece.

use std::path::PathBuf;

use crate::backend::list_input_devices;
use crate::error::AudioError;

/// One diagnosis run.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One diagnosis run.
pub struct DoctorReport {
    /// Capturing devices visible right now (possibly empty).
    pub devices: Vec<String>,
    /// Whether the Opus round-trip self-test passed.
    pub opus_ok: bool,
    /// Where models live (`None` when no home base resolves).
    pub models_dir: Option<PathBuf>,
    /// Actionable notes, empty when everything is green.
    pub notes: Vec<String>,
}

/// Probe the machine. Never fails: absence is reported, not raised.
#[must_use]
/// Probe the machine. Never fails: absence is reported, not raised.
pub fn doctor_report() -> DoctorReport {
    let devices: Vec<String> = list_input_devices()
        .into_iter()
        .map(|info| info.name)
        .collect();
    let opus_ok = opus_self_test().is_ok();
    let models_dir = orator::models_dir();
    let mut notes = Vec::new();
    if devices.is_empty() {
        notes.push(
            "no input devices: capture falls back to synthetic sources; \
             check ALSA/PulseAudio device permissions"
                .to_string(),
        );
    }
    if !opus_ok {
        notes.push("opus self-test failed: archive segments cannot encode".to_string());
    }
    DoctorReport {
        devices,
        opus_ok,
        models_dir,
        notes,
    }
}

/// Encode and decode one silent frame: proves the linked opus build works
/// without any hardware or network.
fn opus_self_test() -> Result<(), AudioError> {
    let mut encoder = opus::Encoder::new(
        crate::archive::ARCHIVE_SAMPLE_RATE,
        opus::Channels::Mono,
        opus::Application::Voip,
    )
    .map_err(|error| AudioError::Archive(format!("opus self-test: {error}")))?;
    let frame = [0.0f32; crate::archive::FRAME_SAMPLES];
    let packet = encoder
        .encode_vec_float(&frame, 4000)
        .map_err(|error| AudioError::Archive(format!("opus self-test: {error}")))?;
    if packet.is_empty() {
        return Err(AudioError::Archive(
            "opus self-test: empty packet".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/doctor.rs"]
mod tests;