//! Full-doctor tests: every section renders, green or actionable.

use crate::doctor::{full_report, render_full};

#[test]
fn full_report_names_every_section() {
    let report = full_report(&orator_core::OratorConfig::default());
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
    let report = full_report(&orator_core::OratorConfig::default());
    assert!(report.models.iter().any(|row| row.kind == "whisper"));
    assert!(report.models.iter().any(|row| row.kind == "llm"));
    assert!(report.models.iter().any(|row| row.kind == "tts"));
}

#[test]
fn report_honors_the_loaded_configuration() {
    let mut config = orator_core::OratorConfig::default();
    config.llm.endpoint = "10.0.0.9:1234".to_string();
    config.tts.voice = "test-voice".to_string();
    let report = full_report(&config);
    assert_eq!(report.endpoint, "10.0.0.9:1234");
    assert!(
        render_full(&report).contains("test-voice"),
        "voice lines follow the config"
    );
}
