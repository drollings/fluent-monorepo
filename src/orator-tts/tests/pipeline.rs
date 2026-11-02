//! Pipeline tests: prebuffer lead, stale-generation cutover, mark order —
//! all against a scripted seam, no model.

use std::sync::Arc;

use orator_core::{Epoch, TtsStream};

use crate::pipeline::{SynthOut, TtsPipeline, PREBUFFER_SECS};

/// Scripted seam: 0.2 s of voiced audio per call, counting invocations.
struct Script {
    calls: std::sync::Mutex<u64>,
    texts: std::sync::Mutex<Vec<String>>,
}

impl Script {
    fn new() -> Self {
        Self {
            calls: std::sync::Mutex::new(0),
            texts: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl crate::pipeline::SynthSeam for Script {
    fn synth(&self, text: &str) -> Result<SynthOut, orator_core::TtsError> {
        *common_core::sync::lock(&self.calls) += 1;
        common_core::sync::lock(&self.texts).push(text.to_string());
        Ok(SynthOut {
            samples: vec![0.5; 4800],
            rate: crate::kokoro::KOKORO_SAMPLE_RATE,
        })
    }
}

fn pipeline() -> (TtsPipeline, Arc<Script>) {
    let seam = Arc::new(Script::new());
    let pipe = TtsPipeline::new(Arc::clone(&seam) as Arc<dyn crate::pipeline::SynthSeam>, Epoch(1));
    (pipe, seam)
}

#[test]
fn first_step_holds_the_prebuffer_lead() {
    let (mut pipe, seam) = pipeline();
    pipe.push_text(
        "First sentence here now. Second sentence follows along friend. Third one closes it out.",
        Epoch(1),
    );
    assert!(pipe.queued() >= 3);
    let calls_before = *common_core::sync::lock(&seam.calls);
    assert_eq!(calls_before, 0);
    let chunk = pipe.step().expect("step").expect("audio ready");
    // 0.2 s per chunk: three synths before the 0.6 s lead holds.
    assert_eq!(*common_core::sync::lock(&seam.calls), 3);
    assert_eq!(chunk.sample_rate, crate::kokoro::KOKORO_SAMPLE_RATE);
    assert!((PREBUFFER_SECS - 0.6).abs() < f32::EPSILON);
    assert!(!chunk.marks.is_empty());
}

#[test]
fn stale_epoch_never_plays() {
    let (mut pipe, seam) = pipeline();
    pipe.push_text("Old news from yesterday friend.", Epoch(1));
    let first = pipe.step().expect("step").expect("audio");
    assert!(first.marks.iter().any(|mark| mark.text == "Old"));
    // Barge-in: a newer epoch clears the queue and bumps generation.
    pipe.push_text("Brand new message here friend.", Epoch(2));
    assert_eq!(pipe.epoch(), Epoch(2));
    let second = pipe.step().expect("step").expect("audio");
    let spoken: Vec<&str> = second.marks.iter().map(|mark| mark.text.as_str()).collect();
    assert!(
        !spoken.iter().any(|word| ["Old", "news", "yesterday"].contains(word)),
        "stale words must never play: {spoken:?}"
    );
    assert!(spoken.contains(&"Brand"));
    // Old epoch text is dropped, never staged.
    let queued = pipe.queued();
    pipe.push_text("Ancient history here friend.", Epoch(1));
    assert_eq!(pipe.queued(), queued);
    let _ = seam;
}

#[test]
fn marks_follow_sample_order() {
    let (mut pipe, _) = pipeline();
    pipe.push_text("One two three four five six.", Epoch(1));
    let chunk = pipe.step().expect("step").expect("audio");
    let mut prev_end = 0u64;
    for mark in &chunk.marks {
        assert!(mark.start_samples < mark.end_samples);
        assert!(mark.end_samples <= chunk.samples.len() as u64);
        assert!(mark.start_samples >= prev_end);
        prev_end = mark.end_samples;
    }
}

#[test]
fn drained_pipeline_reports_none() {
    let (mut pipe, _) = pipeline();
    assert!(pipe.step().expect("empty").is_none());
    pipe.push_text("Short one here.", Epoch(1));
    while pipe.step().expect("drain").is_some() {}
    assert!(pipe.step().expect("drained").is_none());
}
