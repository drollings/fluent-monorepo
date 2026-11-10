//! Full-doctor tests: every section renders, green or actionable.

use crate::doctor::{full_report, render_full};

#[test]
fn full_report_names_every_section() {
    let report = full_report(&orator::OratorConfig::default());
    let text = render_full(&report);
    for section in [
        "devices:",
        "opus:",
        "models:",
        "injection:",
        "endpoint:",
        "voice:",
        "shortcuts:",
    ] {
        assert!(text.contains(section), "missing {section} in:\n{text}");
    }
}

#[test]
fn models_cover_all_families() {
    let report = full_report(&orator::OratorConfig::default());
    assert!(report.models.iter().any(|row| row.kind == "whisper"));
    assert!(report.models.iter().any(|row| row.kind == "llm"));
    assert!(report.models.iter().any(|row| row.kind == "tts"));
}

#[test]
fn report_honors_the_loaded_configuration() {
    let mut config = orator::OratorConfig::default();
    config.llm.endpoint = "10.0.0.9:1234".to_string();
    config.tts.voice = "test-voice".to_string();
    let report = full_report(&config);
    assert_eq!(report.endpoint, "10.0.0.9:1234");
    assert!(
        render_full(&report).contains("test-voice"),
        "voice lines follow the config"
    );
}

#[test]
fn desktop_section_reports_both_worlds() {
    use crate::doctor::{desktop_lines, full_report};
    use orator::OratorConfig;
    // Live report always carries the section.
    let text = crate::doctor::render_full(&full_report(&OratorConfig::default()));
    assert!(
        text.contains("desktop:"),
        "desktop section renders:\n{text}"
    );
    assert!(text.contains("roles:"), "roles section renders:\n{text}");
    assert!(text.contains("asr:"), "asr role renders:\n{text}");
    assert!(text.contains("tts:"), "tts role renders:\n{text}");
    // Headless world: every line names its working fallback.
    let headless = desktop_lines(&orator_desktop::DesktopEnv::default());
    assert_eq!(headless.len(), 5, "one line per surface");
    for line in &headless {
        assert!(
            line.contains("fallback")
                || line.contains("headless")
                || line.contains("absent")
                || line.contains("parser only"),
            "headless names the working path: {line}"
        );
    }
    assert!(
        headless.iter().any(|line| line.contains("tray: headless")),
        "{headless:?}"
    );
    assert!(
        headless.iter().any(|line| line.contains("D-Bus: absent")),
        "{headless:?}"
    );
    // Live world: every line names its candidate bind.
    let live = desktop_lines(&orator_desktop::DesktopEnv {
        current_desktop: Some("KDE".to_string()),
        session_bus: true,
        wayland: true,
        daemon_socket: true,
    });
    assert_eq!(live.len(), 5, "one line per surface");
    for line in &live {
        assert!(
            line.contains("candidate"),
            "live names the pending bind: {line}"
        );
    }
    assert!(
        live.iter().any(|line| line.contains("portal candidate")),
        "{live:?}"
    );
    assert!(
        live.iter().any(|line| line.contains("plugin candidate")),
        "{live:?}"
    );
}

/// Roles section: defaults resolve moonshine for ASR and kokoro for
/// TTS, each naming its backend; unknown roles refuse loudly.
#[test]
fn roles_section_reports_resolved_models() {
    use crate::doctor::roles_lines;
    use orator::OratorConfig;
    let lines = roles_lines(&OratorConfig::default());
    assert_eq!(lines.len(), 3, "asr + tts + chat: {lines:?}");
    let asr = lines
        .iter()
        .find(|line| line.starts_with("asr:"))
        .expect("asr line");
    assert!(asr.contains(orator::DEFAULT_ASR_MODEL), "{asr}");
    assert!(asr.contains("moonshine"), "{asr}");
    let tts = lines
        .iter()
        .find(|line| line.starts_with("tts:"))
        .expect("tts line");
    assert!(tts.contains(orator::DEFAULT_TTS_MODEL), "{tts}");
    assert!(tts.contains("kokoro"), "{tts}");
    let chat = lines
        .iter()
        .find(|line| line.starts_with("chat:"))
        .expect("chat line");
    assert!(
        chat.contains("bartowski/lfm3-1.7b"),
        "chat head is the llm default: {chat}"
    );
    assert!(chat.contains("llm"), "chat names its backend: {chat}");
    // A custom roles table renders its heads.
    let mut config = OratorConfig::default();
    config.roles.asr.models = vec!["liquidai/lfm3-audio-2b".to_string()];
    let lines = roles_lines(&config);
    let asr = lines
        .iter()
        .find(|line| line.starts_with("asr:"))
        .expect("asr line");
    assert!(asr.contains("liquidai/lfm3-audio-2b"), "{asr}");
    assert!(asr.contains("audio-server"), "{asr}");
}

/// Leaf-vs-role disagreement fixture (characterization pin): the
/// roles lines follow the role heads while the speech/endpoint lines
/// follow the root leaves — except the llm label, which follows the
/// chat head since the chat role landed. Both sides still render for
/// asr/s2s/tts, so a disagreeing file means two truths there.
#[test]
fn doctor_pins_leaf_vs_role_disagreement() {
    use crate::doctor::{full_report, render_full};
    use orator::OratorConfig;
    let mut config = OratorConfig::default();
    config.stt.model = "custom-leaf-whisper".to_string();
    config.roles.asr.models = vec!["liquidai/lfm3-audio-2b".to_string()];
    config.s2s.model = "custom-leaf-s2s".to_string();
    config.tts.voice = "custom-leaf-voice".to_string();
    config.llm.model = "custom-leaf-llm".to_string();
    config.llm.endpoint = "10.9.9.9:8181".to_string();
    let report = full_report(&config);
    let text = render_full(&report);
    // Roles side: the head wins the line, the ignored leaf is named.
    let asr = report
        .roles
        .iter()
        .find(|line| line.starts_with("asr:"))
        .expect("asr line");
    assert!(
        asr.contains("liquidai/lfm3-audio-2b"),
        "role head renders: {asr}"
    );
    assert!(
        asr.contains("stt.model=custom-leaf-whisper ignored"),
        "leaf named as ignored: {asr}"
    );
    // Leaves side: speech follows `tts.voice` + the `s2s` leaves,
    // endpoint follows the `llm` leaves.
    assert!(
        text.contains("custom-leaf-voice"),
        "tts leaf renders:\n{text}"
    );
    assert_eq!(report.endpoint, "10.9.9.9:8181");
    // The llm label follows the chat head (the shipped default head
    // here), not the disagreeing leaf — the M4 authority rule for
    // the new role.
    let (llm_model, _) = crate::chat::llm_serving(&config);
    assert_eq!(
        llm_model, "bartowski/lfm3-1.7b",
        "llm label follows the chat head"
    );
}

/// Composition provenance gap: every layer can set the same key,
/// the winner renders, and no layer is named. Model base says
/// threads=4, the role run block says 8, the model-side override
/// says 16 — the line shows 16 with no word for which layer won,
/// so a surprising tune is undebuggable from `doctor` alone.
#[test]
fn composed_params_name_no_winning_layer() {
    use crate::doctor::roles_lines;
    use orator::OratorConfig;
    let mut config = OratorConfig::default();
    config.roles.asr.models = vec!["large-v3-turbo-q5_0".to_string()];
    config.roles.asr.params = Some(serde_json::json!({"threads": 8}));
    let entry = config
        .models
        .models
        .get_mut("large-v3-turbo-q5_0")
        .expect("whisper listed");
    entry.params = Some(serde_json::json!({"threads": 4}));
    entry.role_params = Some(std::collections::HashMap::from([(
        "asr".to_string(),
        orator::ModelRoleOverride {
            params: Some(serde_json::json!({"threads": 16})),
        },
    )]));
    let composed = orator::compose_role_params(
        &config.models,
        &config.roles,
        "asr",
        "large-v3-turbo-q5_0",
    );
    assert_eq!(
        composed.get("threads"),
        Some(&serde_json::json!(16)),
        "override wins"
    );
    let lines = roles_lines(&config);
    let asr = lines
        .iter()
        .find(|line| line.starts_with("asr:"))
        .expect("asr line");
    assert!(asr.contains("threads=16"), "winner renders: {asr}");
    for layer in ["run-block", "runblock", "override", "model base", "layer"] {
        assert!(!asr.contains(layer), "no layer named ({layer}): {asr}");
    }
}

/// The chat line follows the same rendering: the head with its
/// backends and composed params, or the `llm.model` leaf note when
/// the list is empty.
#[test]
fn chat_line_renders_head_or_leaf_fallback() {
    use crate::doctor::roles_lines;
    use orator::OratorConfig;
    let mut config = OratorConfig::default();
    config.roles.chat.models = vec!["liquidai/lfm3-audio-2b".to_string()];
    config.roles.chat.params = Some(serde_json::json!({"ctx": 4096}));
    let lines = roles_lines(&config);
    let chat = lines
        .iter()
        .find(|line| line.starts_with("chat:"))
        .expect("chat line");
    assert!(chat.contains("liquidai/lfm3-audio-2b"), "{chat}");
    assert!(chat.contains("ctx=4096"), "composed params render: {chat}");
    config.roles.chat.models.clear();
    config.llm.model = "leaf-llm".to_string();
    let lines = roles_lines(&config);
    let chat = lines
        .iter()
        .find(|line| line.starts_with("chat:"))
        .expect("chat line");
    assert!(chat.contains("leaf-llm"), "leaf fallback named: {chat}");
    assert!(!chat.contains("no spec"), "{chat}");
}

/// Disagreement notes (exact strings): a role line names the
/// ignored leaf whenever the leaf differs from the effective head.
/// Agreement — including the default config — renders no note.
#[test]
fn role_lines_note_a_disagreeing_leaf() {
    use crate::doctor::roles_lines;
    use orator::OratorConfig;
    let mut config = OratorConfig::default();
    config.stt.model = "custom-leaf-whisper".to_string();
    config.roles.asr.models = vec!["liquidai/lfm3-audio-2b".to_string()];
    config.tts.voice = "custom-leaf-voice".to_string();
    config.roles.tts.models = vec!["liquidai/lfm3-audio-2b".to_string()];
    // The tts leaf is a voice id: the note fires only when the tables
    // override the voiced value, never against the head tag.
    config.roles.tts.params = Some(serde_json::json!({"voice": "table-voice"}));
    config.llm.model = "custom-leaf-llm".to_string();
    config.roles.chat.models = vec!["custom-chat-tag".to_string()];
    let lines = roles_lines(&config);
    let asr = lines
        .iter()
        .find(|line| line.starts_with("asr:"))
        .expect("asr line");
    assert!(
        asr.contains("role head; stt.model=custom-leaf-whisper ignored"),
        "exact asr note: {asr}"
    );
    let tts = lines
        .iter()
        .find(|line| line.starts_with("tts:"))
        .expect("tts line");
    assert!(
        tts.contains("role head; tts.voice=custom-leaf-voice ignored"),
        "exact tts note: {tts}"
    );
    let chat = lines
        .iter()
        .find(|line| line.starts_with("chat:"))
        .expect("chat line");
    assert!(
        chat.contains("role head; llm.model=custom-leaf-llm ignored"),
        "exact chat note: {chat}"
    );
    // Agreeing leaves render no note; neither does the default config.
    let mut agreed = OratorConfig::default();
    agreed.stt.model = orator::DEFAULT_ASR_MODEL.to_string();
    for line in roles_lines(&agreed) {
        assert!(!line.contains("ignored"), "agreement is quiet: {line}");
    }
    for line in roles_lines(&OratorConfig::default()) {
        assert!(!line.contains("ignored"), "defaults are quiet: {line}");
    }
    // Empty role list with a default fallback: the note names the
    // role default, not a head.
    let mut empty = OratorConfig::default();
    empty.roles.asr.models.clear();
    empty.stt.model = "custom-leaf-whisper".to_string();
    let lines = roles_lines(&empty);
    let asr = lines
        .iter()
        .find(|line| line.starts_with("asr:"))
        .expect("asr line");
    assert!(
        asr.contains("role default; stt.model=custom-leaf-whisper ignored"),
        "exact default note: {asr}"
    );
}

/// Session-model line: agreeing heads name the model with the
/// ignored leaf; split heads (the default shape) render the leaf
/// plainly with no note.
#[test]
fn session_model_line_names_agreement_or_leaf() {
    use crate::doctor::{full_report, render_full};
    let mut config = orator::OratorConfig::default();
    config.roles.asr.models = vec!["liquidai/lfm3-audio-2b".to_string()];
    config.roles.tts.models = vec!["liquidai/lfm3-audio-2b".to_string()];
    config.s2s.model = "custom-leaf-s2s".to_string();
    let text = render_full(&full_report(&config));
    assert!(
        text.contains("session model: liquidai/lfm3-audio-2b (asr+tts agree; s2s.model=custom-leaf-s2s ignored)"),
        "exact agreement line:\n{text}"
    );
    let plain = render_full(&full_report(&orator::OratorConfig::default()));
    assert!(
        plain.contains(&format!(
            "session model: {}",
            orator::OratorConfig::default().s2s.model
        )),
        "split heads render the leaf plainly:\n{plain}"
    );
    assert!(
        !plain.contains("s2s.model="),
        "no note without agreement:\n{plain}"
    );
}

fn render_full_text(report: &crate::doctor::FullReport) -> String {
    crate::doctor::render_full(report)
}

/// Chat starts surface in the speech section: defaults render, and a
/// customized file renders its own modes so the operator sees what
/// the next session will use.
#[test]
fn speech_section_reports_chat_session_starts() {
    use crate::doctor::full_report;
    let plain = render_full_text(&full_report(&orator::OratorConfig::default()));
    assert!(
        plain.contains("chat: narration=none ptt=off voice=short text=full mic=auto barge=on"),
        "defaults render:\n{plain}"
    );
    let mut config = orator::OratorConfig::default();
    config.chat.voice = "off".to_string();
    config.chat.barge = false;
    config.chat.narration = "summary".to_string();
    config.chat.ptt = "on".to_string();
    let tuned = render_full_text(&full_report(&config));
    assert!(
        tuned.contains("chat: narration=summary ptt=on voice=off text=full mic=auto barge=off"),
        "custom leaves render:\n{tuned}"
    );
}

#[test]
fn speech_section_reports_lfm_readiness_honestly() {
    use crate::doctor::{full_report, lfm_readiness_lines};
    let config = orator::OratorConfig::default();
    let lines = lfm_readiness_lines(&config);
    assert!(
        lines.iter().any(|line| line.starts_with("lfm:")),
        "lfm rows present:\n{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("no lfm3 backend")),
        "never claims serving:\n{lines:?}"
    );
    let text = crate::doctor::render_full(&full_report(&config));
    assert!(
        text.contains("lfm:"),
        "readiness surfaces in doctor:\n{text}"
    );
}

/// Bundle + LFM-home lines name the configured model, never a
/// compiled-in tag: a custom `s2s.model` with its own weights home
/// renders its own names (the default config renders exactly as
/// before).
#[test]
fn speech_section_names_the_configured_bundle_model() {
    use crate::doctor::{lfm_readiness_lines, speech_lines};
    let mut config = orator::OratorConfig::default();
    config.s2s.model = "custom-voice-model".to_string();
    config.s2s.mmproj = "custom-weights/mmproj.gguf".to_string();
    let lines = speech_lines(&config);
    let bundle = lines
        .iter()
        .find(|line| line.starts_with("bundle "))
        .expect("bundle line");
    assert!(
        bundle.contains("custom-voice-model"),
        "bundle names the configured model: {bundle}"
    );
    assert!(
        !bundle.contains("liquidai/lfm3-audio-2b"),
        "no compiled-in tag: {bundle}"
    );
    let lfm = lfm_readiness_lines(&config);
    assert!(
        lfm.iter().any(|line| line.contains("custom-weights")),
        "readiness checks the configured home:\n{lfm:?}"
    );
    assert!(
        !lfm.iter()
            .any(|line| line.contains("liquidai/lfm3-audio-2b-gguf")),
        "no compiled-in home:\n{lfm:?}"
    );
}

#[test]
fn roles_section_renders_composed_params() {
    use crate::doctor::roles_lines;
    use orator::OratorConfig;
    // Defaults carry no params: lines stay exactly as before.
    let lines = roles_lines(&OratorConfig::default());
    let asr = lines
        .iter()
        .find(|line| line.starts_with("asr:"))
        .expect("asr line");
    assert!(!asr.contains("params"), "no params, no suffix: {asr}");
    // A role run-block param plus a model override compose visibly.
    let mut config = OratorConfig::default();
    config.roles.asr.params = Some(serde_json::json!({"language": "en"}));
    config
        .models
        .models
        .get_mut("large-v3-turbo-q5_0")
        .expect("whisper listed")
        .role_params = Some(std::collections::HashMap::from([(
        "asr".to_string(),
        orator::ModelRoleOverride {
            params: Some(serde_json::json!({"threads": 8})),
        },
    )]));
    config.roles.asr.models = vec!["large-v3-turbo-q5_0".to_string()];
    let lines = roles_lines(&config);
    let asr = lines
        .iter()
        .find(|line| line.starts_with("asr:"))
        .expect("asr line");
    assert!(asr.contains("params"), "{asr}");
    assert!(asr.contains("language"), "{asr}");
    assert!(asr.contains("threads"), "{asr}");
}
