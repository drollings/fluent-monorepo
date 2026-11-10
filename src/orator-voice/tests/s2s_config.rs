//! Speech-config tests: bundle presence shape, hot-editable fields.

use orator::default_bundle;

use crate::s2s::config::{S2sConfig, AUDIO_IN_RATE, AUDIO_OUT_RATE};

#[test]
fn reference_bundle_names_the_three_components() {
    let bundle = default_bundle();
    assert!(bundle.mmproj.ends_with("mmproj.gguf"));
    assert!(bundle.vocoder.ends_with("vocoder.gguf"));
    assert!(bundle.tokenizer.ends_with("tokenizer.gguf"));
    assert!(bundle.backbone.ends_with("latest.gguf"));
    // Presence reflects disk truth (missing entries listed, never panics).
    let _ = bundle.present();
    let _ = bundle.missing().len();
}

#[test]
fn rates_match_the_server_contract() {
    assert_eq!(AUDIO_IN_RATE, 16_000);
    assert_eq!(AUDIO_OUT_RATE, 24_000);
}

#[test]
fn fields_stay_hot_editable() {
    use fluent_wvr::FieldAccess;
    let mut config = S2sConfig::default();
    assert_eq!(config.mode, "interleaved");
    config.set_field("mode", "asr").expect("mode sets");
    assert_eq!(config.get_field("mode").expect("read back"), "asr");
    config
        .set_field("nope", "1")
        .expect_err("unknown field fails");
    assert!(config.field_names().contains(&"voice_prompt"));
}
