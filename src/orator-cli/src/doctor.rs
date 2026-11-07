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
    /// Desktop binding routes per surface (won rung + remedy).
    pub desktop: Vec<String>,
    /// Capability roles and the model serving each.
    pub roles: Vec<String>,
}

/// Collect the full report. Never fails. Endpoint and voice lines
/// follow the loaded configuration, so `doctor` shows the settings
/// the commands will use. Desktop lines follow the live session.
#[must_use]
pub fn full_report(config: &orator_core::OratorConfig) -> FullReport {
    FullReport {
        audio: orator_audio::doctor_report(),
        injection: injection_lines(),
        models: orator_models::list_models(),
        endpoint: config.llm.endpoint.clone(),
        speech: speech_lines(config),
        shortcuts: shortcut_lines(),
        desktop: desktop_lines(&orator_desktop::read_desktop_env()),
        roles: roles_lines(config),
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
    out.push_str("desktop:\n");
    for line in &report.desktop {
        let _ = writeln!(out, "  - {line}");
    }
    out.push_str("roles:\n");
    for line in &report.roles {
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
    lines.push(if s2s.gpu {
        "gpu: on (supervised boots request full layer offload)".to_string()
    } else {
        "gpu: off (supervised boots run CPU)".to_string()
    });
    // Native session identity: the agreed heads model, or the leaf
    // when the heads cannot agree. A disagreeing leaf is named, so
    // the assured model never surprises.
    let session_model = orator_core::agreed_s2s_model(config);
    lines.push(if session_model == s2s.model {
        format!("session model: {session_model}")
    } else {
        format!(
            "session model: {session_model} (asr+tts agree; s2s.model={} ignored)",
            s2s.model
        )
    });
    let bundle = orator_core::bundle_from_s2s(&s2s, &config.models, &config.gguf_dir);
    for missing in bundle.missing() {
        lines.push(format!("missing: {}", missing.display()));
    }
    // The bundle follows the configured `s2s` model through the
    // models table, so the status line names that tag — never a
    // compiled-in model.
    if bundle.present() {
        lines.push(format!(
            "bundle {}: complete (backbone + mmproj; codec in projector)",
            s2s.model
        ));
    } else {
        lines.push(format!(
            "bundle {}: incomplete — text-only serving",
            s2s.model
        ));
    }
    let kokoro_paths = orator_tts::default_paths();
    let voice_pack = kokoro_paths
        .as_ref()
        .map(|(_, voices)| voices.join(format!("{voice}.bin")));
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
        None => lines
            .push("gpu worker: no llama-tts binary (set LLAMA_TTS or add it to $PATH)".to_string()),
    }
    let kokoro_model = kokoro_paths.as_ref().map(|(model, _)| model);
    let model_exists = kokoro_model.is_some_and(|model| model.exists());
    let pack_exists = voice_pack.as_ref().is_some_and(|pack| pack.exists());
    // Chat session starts: the output types and barge-in the loop
    // uses before any slash command overrides them per session.
    lines.push(format!(
        "chat: narration={} ptt={} voice={} text={} mic={} barge={} (override per session with /narration, /ptt, /set)",
        config.chat.narration,
        config.chat.ptt,
        config.chat.voice,
        config.chat.text,
        config.chat.mic,
        if config.chat.barge { "on" } else { "off" },
    ));
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
    lines.extend(lfm_readiness_lines(config));
    lines
}

/// LFM3-Audio readiness: which weight homes are servable-shaped and
/// what is still missing. Reports layout only — no runtime exists
/// in-tree yet, so nothing here claims serving. The home derives
/// from the configured voice projector's directory (same home the
/// serving bundle resolves under), never a compiled-in model path.
#[must_use]
pub fn lfm_readiness_lines(config: &orator_core::OratorConfig) -> Vec<String> {
    use std::fmt::Write as _;
    use std::path::{Path, PathBuf};
    let layout = orator_lfm::manifest::expected_layout();
    let mmproj = Path::new(&config.s2s.mmproj);
    let gguf_home: PathBuf = if mmproj.is_absolute() {
        mmproj
            .parent()
            .map_or_else(|| PathBuf::from(&config.gguf_dir), Path::to_path_buf)
    } else {
        Path::new(&config.gguf_dir).join(mmproj.parent().unwrap_or_else(|| Path::new("")))
    };
    let mut lines = Vec::new();
    match orator_lfm::manifest::verify_weights(
        &gguf_home,
        &[layout.gguf_text.clone(), layout.gguf_projector.clone()],
    ) {
        Ok(count) => lines.push(format!(
            "lfm: {count}/2 gguf files present (text backbone + projector entry)"
        )),
        Err(missing) => {
            let mut detail = missing.missing.join(", ");
            if !missing.empty.is_empty() {
                if !detail.is_empty() {
                    detail.push_str("; ");
                }
                let _ = write!(detail, "empty: {}", missing.empty.join(", "));
            }
            lines.push(format!(
                "lfm: gguf incomplete at {} ({detail})",
                gguf_home.display()
            ));
        }
    }
    lines.push(
        "lfm: native inference needs an LFM-capable server (no lfm3 backend in-tree; tensor core out of scope for this crate)"
            .to_string(),
    );
    lines
}

fn shortcut_lines() -> Vec<String> {
    let chord = "Ctrl+Alt+d";
    match orator_desktop::parse_chord(chord) {
        Some(parsed) if !parsed.is_reserved() => {
            vec![format!(
                "dictate: {} — valid, registers at runtime",
                parsed.pretty()
            )]
        }
        _ => vec![format!("dictate: {chord} — invalid chord")],
    }
}

/// Capability-role lines: role → serving model (first listed wins),
/// its backend, and where its weights resolve (or why they don't).
/// Pure over the loaded config so both worlds pin without hardware.
#[must_use]
pub fn roles_lines(config: &orator_core::OratorConfig) -> Vec<String> {
    use orator_core::{resolve_model_weights, resolve_role_model};
    let mut lines = Vec::with_capacity(3);
    for role in ["asr", "tts", "chat"] {
        // Root selection leaf each role falls back to when its side
        // says nothing: named in the disagreement note.
        let (leaf_key, leaf) = match role {
            "asr" => ("stt.model", config.stt.model.as_str()),
            "tts" => ("tts.voice", config.tts.voice.as_str()),
            _ => ("llm.model", config.llm.model.as_str()),
        };
        let head = resolve_role_model(&config.roles, role);
        // Effective model plus which layer named it: a listed head, a
        // role default, or (chat only) the root leaf itself.
        let (model, authority) = match (head, role) {
            (Some(model), _) => (model, "role head"),
            (None, "chat") => {
                lines.push(format!(
                    "chat: no model listed — llm.model {leaf} serves as fallback"
                ));
                continue;
            }
            (None, _) => {
                if let Some(fallback) =
                    orator_core::resolve_role_model_or_default(&config.roles, role, &config.default)
                {
                    if leaf == fallback {
                        lines.push(format!(
                            "{role}: no model listed — default {fallback} serves as fallback"
                        ));
                    } else {
                        lines.push(format!(
                            "{role}: no model listed — default {fallback} serves as fallback (role default; {leaf_key}={leaf} ignored)"
                        ));
                    }
                } else {
                    lines.push(format!("{role}: no model listed"));
                }
                continue;
            }
        };
        // The note fires only when a listed layer overrules a
        // disagreeing leaf — agreement renders exactly as before.
        // For TTS the leaf is a voice id, not a model tag, so the
        // comparison is against the composed voice (the value `say`
        // actually voices), never the head tag.
        let note = if role == "tts" {
            let tag = model.clone();
            let voiced = crate::fallback::resolve_tts_voice(config, &tag);
            if voiced == leaf {
                String::new()
            } else {
                format!("; {authority}; {leaf_key}={leaf} ignored")
            }
        } else if leaf == model {
            String::new()
        } else {
            format!("; {authority}; {leaf_key}={leaf} ignored")
        };
        let Some(spec) = config.models.models.get(&model) else {
            lines.push(format!("{role}: {model} (no spec in models table{note})"));
            continue;
        };
        let weights = match resolve_model_weights(&config.models, &config.gguf_dir, &model) {
            Some(path) if path.exists() => format!("weights {}", path.display()),
            Some(path) => format!("weights {} — missing", path.display()),
            None => "weights owned by backend".to_string(),
        };
        let backends = spec.backends.join("/");
        let backends = if backends.is_empty() {
            "no backend".to_string()
        } else {
            backends
        };
        let composed =
            orator_core::compose_role_params(&config.models, &config.roles, role, &model);
        let params = composed
            .as_object()
            .filter(|map| !map.is_empty())
            .map_or_else(String::new, |map| {
                let mut pairs: Vec<String> = map
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect();
                pairs.sort();
                format!("; params {}", pairs.join(" "))
            });
        lines.push(format!(
            "{role}: {model} ({backends}; {weights}{params}{note})"
        ));
    }
    lines
}

/// Desktop binding lines for `env`: one per surface, each naming the
/// won rung and either the working path or the exact pending bind.
/// Pure over the session facts, so both worlds pin without a live session.
#[must_use]
pub fn desktop_lines(env: &orator_desktop::DesktopEnv) -> Vec<String> {
    use orator_desktop::{
        probe_dbus, probe_hotkeys, probe_krunner, probe_notify, probe_tray, DbusRoute, HotkeyRoute,
        KrunnerRoute, NotifyRoute, TrayRoute,
    };
    let mut lines = Vec::with_capacity(5);
    match probe_hotkeys(env) {
        HotkeyRoute::PortalCandidate => lines.push(format!(
            "hotkeys: portal candidate (bus + {} — binds at runtime); fallback ready ({})",
            env.current_desktop.as_deref().unwrap_or("desktop"),
            orator_desktop::fallback_bindings()
                .iter()
                .map(|(command, _)| *command)
                .collect::<Vec<_>>()
                .join(", ")
        )),
        HotkeyRoute::Compositor(instructions) => lines.push(format!("hotkeys: {instructions}")),
        HotkeyRoute::Fallback => lines.push(
            "hotkeys: fallback ready (bind `orator chat --ptt` in any compositor)".to_string(),
        ),
    }
    match probe_tray(env) {
        TrayRoute::HostCandidate => lines.push(format!(
            "tray: host candidate ({} session — binds at runtime; headless render ready)",
            env.current_desktop.as_deref().unwrap_or("desktop")
        )),
        TrayRoute::Headless => lines.push(
            "tray: headless (state renders without a host — Recording never hides)".to_string(),
        ),
    }
    match probe_notify(env) {
        NotifyRoute::DaemonEmit => lines.push(
            "notifications: daemon-emit candidate (socket present — emits when the path lands)"
                .to_string(),
        ),
        NotifyRoute::CliFallback => lines
            .push("notifications: CLI fallback (`orator approve <id>` answers rounds)".to_string()),
    }
    match probe_dbus(env) {
        DbusRoute::ServiceCandidate => lines.push(
            "D-Bus: service candidate (bus present — ambient callers can never approve)"
                .to_string(),
        ),
        DbusRoute::Absent => {
            lines.push("D-Bus: absent (socket + CLI serve control)".to_string());
        }
    }
    match probe_krunner(env) {
        KrunnerRoute::PluginCandidate => lines.push(
            "krunner: plugin candidate (KDE — registers when the plugin lands; parser answers today)"
                .to_string(),
        ),
        KrunnerRoute::ParserOnly => lines.push(
            "krunner: parser only (prefix `orator` — ask, meeting, dictate offers)".to_string(),
        ),
    }
    lines
}

#[cfg(test)]
#[path = "../tests/doctor.rs"]
mod tests;
