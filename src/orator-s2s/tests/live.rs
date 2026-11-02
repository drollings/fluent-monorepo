//! Live speech: real audio endpoint, real inference. Skips cleanly
//! without the endpoint contract or an explicit opt-in — never fails
//! a hermetic gate.

use orator_s2s::{AudioSession, S2sConfig};

/// One ASR turn through the audio endpoint. Requires `LLM_BASE_URL`
/// (e.g. `http://127.0.0.1:8080` serving the audio model); otherwise
/// skips.
#[ignore = "needs a real audio endpoint; run via the live-ai target"]
#[test]
fn live_audio_asr_returns_text() {
    let base = std::env::var("LLM_BASE_URL").unwrap_or_default();
    if base.is_empty() {
        eprintln!("live: LLM_BASE_URL unset, skipping");
        return;
    }
    common_core::runtime::block_on_shared(async {
        let mut session = AudioSession::new(S2sConfig {
            endpoint: base,
            ..S2sConfig::default()
        });
        let text = session
            .transcribe_blocking(&[0.0; 16_000])
            .expect("live asr completes");
        eprintln!("live: asr heard {text:?}");
    });
}
