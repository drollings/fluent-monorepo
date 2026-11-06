//! Live reference agreement: re-runs the real HuggingFace
//! processor through the checked-in probe and compares fresh outputs
//! to the frozen fixtures and our layer. Skips cleanly without the
//! reference tree, python3, or the torch stack — never fails a
//! hermetic gate. Run via `make orator_test_live`-style live-ai
//! targets (`--features live-ai --test live -- --ignored`).

use std::path::PathBuf;

/// Reference tree, overridable for relocated stores.
fn reference_dir() -> PathBuf {
    std::env::var("LFM3_REFERENCE_DIR").map_or_else(
        |_| PathBuf::from("/ai/models/lfm3/LiquidAI/LFM3-Audio-2B"),
        PathBuf::from,
    )
}

/// The frozen vectors the hermetic suite pins.
fn frozen() -> serde_json::Value {
    serde_json::from_str(include_str!("fixtures/reference_expansion.json"))
        .expect("fixtures are valid JSON")
}

/// Run the checked-in probe against the reference tree. Returns
/// `None` (with a notice) when the env contract is unmet: missing
/// tree, missing python, or a missing torch/transformers stack.
/// A probe that RUNS but fails refuses loudly instead of skipping —
/// a present-but-broken reference is a signal, not an absence.
fn probe_output() -> Option<serde_json::Value> {
    let dir = reference_dir();
    if !dir.is_dir() {
        eprintln!("live: reference tree missing at {}, skipping", dir.display());
        return None;
    }
    let probe = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/probe_reference.py");
    let output = std::process::Command::new("python3")
        .arg(&probe)
        .arg(&dir)
        .output()
        .ok()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("No module named") || stderr.contains("ModuleNotFoundError") {
            eprintln!("live: reference stack incomplete, skipping ({})", stderr.lines().next().unwrap_or(""));
            return None;
        }
        panic!(
            "live: probe ran but failed (the reference is present but broken):\n{stderr}"
        );
    }
    serde_json::from_slice(&output.stdout).ok()
}

/// Fresh outputs minus the env-dependent tree path.
fn comparable(mut value: serde_json::Value) -> serde_json::Value {
    if let Some(map) = value.as_object_mut() {
        map.remove("reference_dir");
        map.remove("notes");
    }
    value
}

#[ignore = "needs the reference tree + torch stack; run via the live-ai target"]
#[test]
fn reference_probe_reproduces_the_frozen_fixtures() {
    let Some(fresh) = probe_output() else {
        return;
    };
    assert_eq!(
        comparable(fresh),
        comparable(frozen()),
        "fresh reference outputs match the frozen fixtures (re-record with the probe when the reference moves)"
    );
}

#[ignore = "needs the reference tree + torch stack; run via the live-ai target"]
#[test]
fn fresh_reference_outputs_match_our_assembly() {
    use orator_lfm::{assemble_prompt, AudioClip, AudioOutClip, TextTurn};
    use orator_lfm::{AUDIO_OUT_TOKEN_ID, AUDIO_TOKEN_ID};

    let Some(fresh) = probe_output() else {
        return;
    };
    // Audio expansion: framing and ids against fresh reference output.
    let expansion = &fresh["audio_expansion"];
    let mel = expansion["mel_frames"].as_u64().and_then(|v| usize::try_from(v).ok()).expect("mel frames");
    let tokens = expansion["encoder_tokens"].as_u64().and_then(|v| usize::try_from(v).ok()).expect("tokens");
    assert_eq!(orator_lfm::arch::encoder_tokens_for_frames(mel), tokens);
    let turn = TextTurn {
        role: "user".to_string(),
        text: expansion["text"].as_str().expect("text").to_string(),
        audio: Some(AudioClip { mel_frames: mel }),
        audio_out: None,
    };
    let (text, ids) = assemble_prompt(&turn).expect("assembles");
    assert_eq!(text, expansion["framed_turn"].as_str().expect("framed turn"));
    assert_eq!(ids, vec![AUDIO_TOKEN_ID; tokens]);
    // Audio-out: frames plus the EOS frame, fresh from the codec.
    let out = &fresh["audio_out"];
    let frames = out["codec_frames"].as_u64().and_then(|v| usize::try_from(v).ok()).expect("codec frames");
    let placeholders = out["placeholders"].as_u64().and_then(|v| usize::try_from(v).ok()).expect("placeholders");
    assert_eq!(placeholders, frames + 1, "every clip carries the EOS frame");
    let turn = TextTurn {
        role: "assistant".to_string(),
        text: out["text"].as_str().expect("text").to_string(),
        audio: None,
        audio_out: Some(AudioOutClip { frames }),
    };
    let (text, ids) = assemble_prompt(&turn).expect("assembles");
    assert_eq!(text, out["framed_turn"].as_str().expect("framed turn"));
    assert_eq!(ids, vec![AUDIO_OUT_TOKEN_ID; placeholders]);
}

#[ignore = "needs the reference tree; run via the live-ai target"]
#[test]
fn generation_defaults_match_the_reference_config() {
    use orator_lfm::params::GenerationDefaults;

    let dir = reference_dir();
    if !dir.is_dir() {
        eprintln!("live: reference tree missing at {}, skipping", dir.display());
        return;
    }
    let generation: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("generation_config.json")).expect("generation config reads"),
    )
    .expect("generation config parses");
    let model: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("config.json")).expect("model config reads"),
    )
    .expect("model config parses");
    let defaults = GenerationDefaults::default();
    // Floats cross a precision boundary (JSON f64 vs Rust f32), so
    // floats compare within one f32 ulp instead of bit-exactly; ints
    // and bools stay exact.
    fn close(actual: Option<f64>, expected: f32, key: &str) {
        let actual = actual.unwrap_or_else(|| panic!("{key} is a number"));
        assert!(
            (actual - f64::from(expected)).abs() < 1e-6,
            "{key}: {actual} vs {expected}"
        );
    }
    assert_eq!(generation["do_sample"], serde_json::json!(defaults.text.do_sample));
    close(generation["temperature"].as_f64(), defaults.text.temperature, "temperature");
    assert_eq!(generation["top_k"].as_u64(), Some(u64::from(defaults.text.top_k)));
    close(
        generation["audio_temperature"].as_f64(),
        defaults.audio.temperature,
        "audio_temperature",
    );
    assert_eq!(generation["audio_top_k"].as_u64(), Some(u64::from(defaults.audio.top_k)));
    close(
        generation["audio_repetition_penalty"].as_f64(),
        defaults.audio.repetition_penalty,
        "audio_repetition_penalty",
    );
    assert_eq!(
        generation["audio_repetition_window"].as_u64().and_then(|v| usize::try_from(v).ok()),
        Some(defaults.audio.repetition_window)
    );
    assert_eq!(
        model["interleaved_n_text"].as_u64().and_then(|v| usize::try_from(v).ok()),
        Some(defaults.interleave.text_slots)
    );
    assert_eq!(
        model["interleaved_n_audio"].as_u64().and_then(|v| usize::try_from(v).ok()),
        Some(defaults.interleave.audio_frames)
    );
    // `maskgit_schedule` is absent from the config: the reference
    // default (`[4,4,4]`, one third of the codebooks per pass)
    // applies, which the hermetic suite pins.
    let schedule = &model["maskgit_schedule"];
    assert!(
        schedule.is_null() || *schedule == serde_json::json!([4, 4, 4]),
        "maskgit schedule default holds: {schedule}"
    );
}
