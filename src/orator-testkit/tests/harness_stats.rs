//! Stats tests: histogram rendering names every stage.

use crate::{ScriptScenario, ScriptStep};

use crate::harness::replay::run_scenario;
use crate::harness::stats::{histograms, render_stats};

#[test]
fn stats_name_every_stage() {
    let scenario = ScriptScenario::clean(vec![
        ScriptStep { stage: "stt".to_string(), latency_ms: 80, peak_bytes: 1 },
        ScriptStep { stage: "tts_first_audio".to_string(), latency_ms: 100, peak_bytes: 1 },
    ]);
    let (log, _) = run_scenario(&scenario);
    let text = render_stats(&histograms(&log));
    assert!(text.contains("stt:"));
    assert!(text.contains("tts_first_audio:"));
    assert!(text.contains("n=1"));
}
