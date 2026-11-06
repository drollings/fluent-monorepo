//! Overlap goldens: chunked playout streams while synthesis runs.

use std::sync::{Arc, Mutex};

use crate::stream::stream_chunks;

fn events() -> Arc<Mutex<Vec<(String, usize)>>> {
    Arc::new(Mutex::new(Vec::new()))
}

#[test]
fn playback_overlaps_synthesis() {
    let log = events();
    let synth_log = Arc::clone(&log);
    let play_log = Arc::clone(&log);
    // Chunk 0 synthesizes fast, chunk 1 slowly: overlap means chunk 0
    // plays before chunk 1 finishes synthesizing. Ordering only — no
    // wall-clock assertions.
    let synth = move |text: &str| -> Result<Vec<f32>, String> {
        let index: usize = text.parse().expect("chunk index");
        if index == 1 {
            std::thread::sleep(std::time::Duration::from_millis(300));
        } else {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        common_core::sync::lock(&synth_log).push(("synth-done".to_string(), index));
        Ok(vec![index as f32; 8])
    };
    let play = move |samples: &[f32]| -> Result<(), String> {
        assert!(!samples.is_empty());
        common_core::sync::lock(&play_log).push(("play".to_string(), samples[0] as usize));
        Ok(())
    };
    stream_chunks(
        vec!["0".to_string(), "1".to_string(), "2".to_string()],
        synth,
        play,
    )
    .expect("streams");
    let log = common_core::sync::lock(&log);
    let position = |kind: &str, index: usize| {
        log.iter()
            .position(|entry| entry == &(kind.to_string(), index))
            .expect("event recorded")
    };
    // Chunk 0 plays before chunk 1 finishes: the pipeline overlaps.
    assert!(
        position("play", 0) < position("synth-done", 1),
        "first audio precedes slow-chunk synth: {log:?}"
    );
    // Order holds end to end.
    assert!(
        position("play", 0) < position("play", 1),
        "chunk order holds: {log:?}"
    );
    assert!(
        position("play", 1) < position("play", 2),
        "chunk order holds: {log:?}"
    );
}

#[test]
fn synth_errors_stop_the_stream_with_that_error() {
    let play = |_: &[f32]| -> Result<(), String> { Ok(()) };
    let error = stream_chunks(
        vec!["ok".to_string(), "bad".to_string()],
        |text| {
            if text == "bad" {
                Err("synth failed".to_string())
            } else {
                Ok(vec![1.0])
            }
        },
        play,
    )
    .expect_err("synth error stops");
    assert_eq!(error, "synth failed");
}

#[test]
fn empty_chunks_are_a_no_op() {
    let play = |_: &[f32]| -> Result<(), String> {
        panic!("no chunks, no play");
    };
    stream_chunks(Vec::new(), |_| Ok(vec![]), play).expect("empty streams clean");
}

#[test]
fn play_error_cancels_synth_promptly() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let synth_calls = Arc::new(AtomicUsize::new(0));
    let synth_count = Arc::clone(&synth_calls);
    let chunks: Vec<String> = (0..10).map(|index| index.to_string()).collect();
    let error = stream_chunks(
        chunks,
        move |text: &str| -> Result<Vec<f32>, String> {
            synth_count.fetch_add(1, Ordering::SeqCst);
            if text == "5" {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Ok(vec![1.0])
        },
        |_: &[f32]| -> Result<(), String> { Err("play failed".to_string()) },
    )
    .expect_err("play error stops");
    assert_eq!(error, "play failed");
    let calls = synth_calls.load(Ordering::SeqCst);
    assert!(
        calls <= 4,
        "bounded channel cancels synthesis promptly: {calls} calls for 10 chunks"
    );
}

#[test]
fn chunked_first_audio_beats_whole_turn() {
    // Synthesis cost proportional to text length (2 ms per char,
    // scripted): a 3-chunk turn voices its first chunk long before
    // the whole turn would finish. Banded, never exact.
    let fixture = "Hello world, this is the opening line for the test. \
         How are you doing on this fine day? Quite well, thank you.";
    let chunks: Vec<String> = orator_tts::split_all(fixture)
        .into_iter()
        .map(|chunk| chunk.text)
        .collect();
    assert!(chunks.len() >= 2, "fixture must chunk");
    let cost_ms = |text: &str| text.chars().count() as u64 * 2;
    let whole_ms = cost_ms(fixture);
    let synth = move |text: &str| -> Result<Vec<f32>, String> {
        std::thread::sleep(std::time::Duration::from_millis(cost_ms(text)));
        Ok(vec![0.5; 8])
    };
    let first_at = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
    let first_mark = Arc::clone(&first_at);
    let started = std::time::Instant::now();
    let play = move |_: &[f32]| -> Result<(), String> {
        let elapsed = started.elapsed().as_millis() as u64;
        first_mark.fetch_min(elapsed, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    };
    stream_chunks(chunks, synth, play).expect("streams");
    let first = first_at.load(std::sync::atomic::Ordering::SeqCst);
    eprintln!("first-audio chunked={first}ms whole-turn~{whole_ms}ms");
    assert!(
        first < whole_ms,
        "first chunk voices before the whole turn would: chunked={first}ms whole~{whole_ms}ms"
    );
}

#[test]
fn single_sentence_is_one_identical_synth_call() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    // Transparency golden: a single sentence chunks to exactly one
    // call, and the streamed samples equal the whole-turn synth of
    // the same text — chunking never rewrites a one-chunk turn.
    let fixture = "Hello there.";
    let chunks: Vec<String> = orator_tts::split_all(fixture)
        .into_iter()
        .map(|chunk| chunk.text)
        .collect();
    assert_eq!(chunks.len(), 1, "single sentence stays one chunk");
    let calls = Arc::new(AtomicUsize::new(0));
    let synth_count = Arc::clone(&calls);
    let synth = move |text: &str| -> Result<Vec<f32>, String> {
        synth_count.fetch_add(1, Ordering::SeqCst);
        Ok(text.bytes().map(f32::from).collect())
    };
    let played = Arc::new(Mutex::new(Vec::new()));
    let played_out = Arc::clone(&played);
    stream_chunks(
        chunks,
        synth,
        move |samples: &[f32]| -> Result<(), String> {
            played_out
                .lock()
                .expect("play log")
                .extend_from_slice(samples);
            Ok(())
        },
    )
    .expect("streams");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one chunk, one synth call");
    let reference: Vec<f32> = fixture.bytes().map(f32::from).collect();
    assert_eq!(*played.lock().expect("play log"), reference);
}
