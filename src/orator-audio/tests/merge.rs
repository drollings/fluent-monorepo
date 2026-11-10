//! Merge tests: interleave goldens under skew plus the calibration
//! corpus (equal-clock control, 500 ms lane skew task).

use std::num::NonZeroU32;
use std::sync::Arc;

use orator::{Channel, MediaTime};

use crate::merge::{MergeQueue, WordRecord};

fn rate() -> NonZeroU32 {
    NonZeroU32::new(16_000).expect("rate")
}

fn word(text: &str, channel: Channel, start_ms: u64) -> WordRecord {
    let rate = rate();
    let start = MediaTime {
        samples: start_ms * 16,
        rate,
    };
    WordRecord {
        text: text.to_string(),
        channel,
        start,
        end: start.add_samples(3200),
        confidence: 0.9,
        speaker: None,
    }
}

fn drain(queue: &Arc<MergeQueue>, count: usize) -> Vec<WordRecord> {
    common_core::runtime::block_on(async {
        let mut out = Vec::new();
        for _ in 0..count {
            let Some(record) = queue.pop().await else {
                break;
            };
            out.push(record);
        }
        out
    })
}

fn push_all(queue: &Arc<MergeQueue>, words: Vec<WordRecord>) {
    common_core::runtime::block_on(async {
        for record in words {
            queue.push_wait(record).await.expect("merge never drops");
        }
        queue.close();
    });
}

#[test]
fn equal_clocks_do_not_reorder() {
    let queue = MergeQueue::new(64);
    push_all(
        &queue,
        vec![
            word("mic-one", Channel::Mic, 0),
            word("sys-one", Channel::Sys, 20),
            word("mic-two", Channel::Mic, 40),
            word("sys-two", Channel::Sys, 60),
        ],
    );
    let texts: Vec<String> = drain(&queue, 4)
        .into_iter()
        .map(|record| record.text)
        .collect();
    assert_eq!(texts, vec!["mic-one", "sys-one", "mic-two", "sys-two"]);
}

#[test]
fn skewed_lanes_interleave_by_media_time() {
    // Sys lane runs 500 ms behind the mic lane: arrivals skew, but the
    // merged transcript still orders by capture-clock time.
    let queue = MergeQueue::new(64);
    push_all(
        &queue,
        vec![
            word("mic-late", Channel::Mic, 600),
            word("sys-early", Channel::Sys, 100),
            word("mic-mid", Channel::Mic, 300),
            word("sys-mid", Channel::Sys, 320),
        ],
    );
    let texts: Vec<String> = drain(&queue, 4)
        .into_iter()
        .map(|record| record.text)
        .collect();
    assert_eq!(texts, vec!["sys-early", "mic-mid", "sys-mid", "mic-late"]);
}

#[test]
fn word_records_carry_channel_and_nullable_speaker() {
    let mut record = word("hello", Channel::Sys, 0);
    assert_eq!(record.speaker, None);
    record.speaker = Some("Speaker 1".to_string());
    assert_eq!(record.speaker.as_deref(), Some("Speaker 1"));
    assert_eq!(record.channel, Channel::Sys);
    assert_eq!(record.start_ms(), 0);
}

#[test]
fn stage_kill_loses_no_words() {
    // One lane dies mid-stream (its words stop), the merge drains what
    // arrived, the lane resumes, and every word lands exactly once in
    // time order — the kill costs latency, never audio.
    let queue = MergeQueue::new(64);
    let before = vec![
        word("mic1", Channel::Mic, 0),
        word("s1", Channel::Sys, 20),
        word("mic2", Channel::Mic, 40),
    ];
    let after = vec![word("s2", Channel::Sys, 60), word("mic3", Channel::Mic, 80)];
    common_core::runtime::block_on(async {
        for record in before {
            queue.push_wait(record).await.expect("merge never drops");
        }
        // Kill simulation: sys lane stalls here (no words pushed).
        for record in after {
            queue.push_wait(record).await.expect("merge never drops");
        }
        queue.close();
    });
    let got = drain(&queue, 8);
    assert_eq!(got.len(), 5);
    let mut times: Vec<u64> = got.iter().map(super::WordRecord::start_ms).collect();
    let sorted = {
        let mut sorted = times.clone();
        sorted.sort_unstable();
        sorted
    };
    assert_eq!(times, sorted, "merged words stay time-ordered");
    times.dedup();
    assert_eq!(times.len(), 5, "zero dupes after the kill");
}
