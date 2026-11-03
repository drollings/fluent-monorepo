//! Full environment diagnosis: audio plus injection, models, endpoint,
//! and shortcuts. Every line is actionable; absence is reported, never
//! raised. Hermetic: no network, no hardware writes.

use crate::config::s2s_base_url;
use orator_models::ModelStatus;

/// One full diagnosis run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullReport {
    /// Audio section (devices, codec, model home).
    pub audio: orator_audio::DoctorReport,
    /// Injection rung availability, strongest first.
    pub injection: Vec<String>,
    /// Model presence per catalog entry.
    pub models: Vec<ModelStatus>,
    /// Configured LLM endpoint.
    pub endpoint: String,
    /// Audio endpoint and bundle component presence.
    pub speech: Vec<String>,
    /// Shortcut chords and their registration state.
    pub shortcuts: Vec<String>,
}

/// Collect the full report. Never fails. Endpoint and voice lines
/// follow the loaded configuration, so `doctor` shows the settings
/// the commands will use.
#[must_use]
pub fn full_report(config: &orator_core::OratorConfig) -> FullReport {
    FullReport {
        audio: orator_audio::doctor_report(),
        injection: injection_lines(),
        models: orator_models::list_models(),
        endpoint: config.llm.endpoint.clone(),
        speech: speech_lines(config),
        shortcuts: shortcut_lines(),
    }
}

/// Render the full report for humans.
#[must_use]
pub fn render_full(report: &FullReport) -> String {
    use std::fmt::Write as _;
    let mut out = crate::render_doctor(&report.audio);
    out.push_str("injection:\n");
    for line in &report.injection {
        let _ = writeln!(out, "  - {line}");
    }
    out.push_str("models:\n");
    for model in &report.models {
        let state = if model.present {
            match model.verified {
                Some(true) => "present verified",
                Some(false) => "present MISMATCH",
                None => "present",
            }
        } else {
            "missing"
        };
        let pin = if model.pinned { "pinned" } else { "unpinned" };
        let _ = writeln!(out, "  - {} ({}): {state} [{pin}]", model.name, model.kind);
    }
    let _ = writeln!(out, "endpoint: {}", report.endpoint);
    out.push_str("speech:\n");
    for line in &report.speech {
        let _ = writeln!(out, "  - {line}");
    }
    out.push_str("shortcuts:\n");
    for line in &report.shortcuts {
        let _ = writeln!(out, "  - {line}");
    }
    out
}

fn injection_lines() -> Vec<String> {
    vec![
        "RemoteDesktop (portal): unwired — falls through".to_string(),
        "ClipboardPaste: available — needs a focused window".to_string(),
        "VirtualKeyboard: needs a wlroots compositor".to_string(),
        "Uinput: needs device-group membership".to_string(),
        "OutputOnly: always available (stdout)".to_string(),
    ]
}

fn speech_lines(config: &orator_core::OratorConfig) -> Vec<String> {
    let voice = config.tts.voice.clone();
    let s2s = config.s2s.clone();
    let mut lines = vec![format!("endpoint: {}", s2s_base_url(&s2s))];
    lines.push(if s2s.auto_boot {
        "auto-boot: on (a supervised server boots when none answers)".to_string()
    } else {
        "auto-boot: off (the endpoint above is expected to be independently started)".to_string()
    });
    let bundle = orator_core::bundle_from_s2s(&s2s);
    for missing in bundle.missing() {
        lines.push(format!("missing: {}", missing.display()));
    }
    if bundle.present() {
        lines.push("bundle liquidai/lfm3-audio-2b: complete (backbone + mmproj; codec in projector)".to_string());
    } else {
        lines.push("bundle liquidai/lfm3-audio-2b: incomplete — text-only serving".to_string());
    }
    let kokoro_paths = orator_tts::default_paths();
    let voice_pack = kokoro_paths.as_ref().map(|(_, voices)| voices.join(format!("{voice}.bin")));
    match orator_s2s::resolve_audio_server() {
        Some(binary) => lines.push(format!("audio server: {}", binary.display())),
        None => match orator_s2s::managed_binary() {
            Some(home) => lines.push(format!(
                "audio server: missing (set LLAMA_AUDIO_SERVER or install to {})",
                home.display()
            )),
            None => lines.push(
                "audio server: missing (set LLAMA_AUDIO_SERVER; no home directory for the managed install)".to_string(),
            ),
        },
    }
    match orator_s2s::resolve_tts_binary() {
        Some(binary) => lines.push(format!("gpu worker: {}", binary.display())),
        None => lines.push(
            "gpu worker: no llama-tts binary (set LLAMA_TTS or add it to $PATH)".to_string(),
        ),
    }
    let kokoro_model = kokoro_paths.as_ref().map(|(model, _)| model);
    let model_exists = kokoro_model.is_some_and(|model| model.exists());
    let pack_exists = voice_pack.as_ref().is_some_and(|pack| pack.exists());
    if model_exists && pack_exists {
        lines.push(format!(
            "voice: kokoro {voice} ready (fallback when the server sends no audio)"
        ));
    } else if model_exists {
        lines.push(format!(
            "voice: kokoro {voice} pack missing at {} (server-side synthesis only)",
            voice_pack.as_ref().expect("pack checked").display()
        ));
    } else {
        lines.push(format!(
            "voice: server-side synthesis only (kokoro weights missing at {})",
            kokoro_model.map_or_else(
                || "the managed model directory".to_string(),
                |model| model.display().to_string()
            )
        ));
    }
    if model_exists {
        lines.push("kokoro weights: present (selectable via --backend kokoro)".to_string());
    }
    lines
}

fn shortcut_lines() -> Vec<String> {    let chord = "Ctrl+Alt+d";
    match orator_desktop::parse_chord(chord) {
        Some(parsed) if !parsed.is_reserved() => {
            vec![format!("dictate: {} — valid, registers at runtime", parsed.pretty())]
        }
        _ => vec![format!("dictate: {chord} — invalid chord")],
    }
}

#[cfg(test)]
#[path = "../tests/doctor.rs"]
mod tests;
