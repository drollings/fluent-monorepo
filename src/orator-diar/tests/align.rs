//! Alignment tests: midpoint attribution, naming, suggestions advisory.

use std::collections::HashMap;

use orator_core::Channel;

use crate::align::{align_words, speaker_name, NameSuggestions};
use crate::segment::Segment;

#[test]
fn words_take_the_covering_segment() {
    let segments = vec![
        Segment { index: 0, start_sample: 0, end_sample: 24_000, channel: Channel::Sys },
        Segment { index: 1, start_sample: 12_000, end_sample: 36_000, channel: Channel::Sys },
    ];
    // Word midpoints in ms: 500 sits in segment 0, 1500 in segment 1.
    let labels = align_words(&[500, 1500], &segments, &[0, 1]);
    assert_eq!(labels, vec![0, 1]);
}

#[test]
fn names_stay_stable_and_suggestions_advisory() {
    assert_eq!(speaker_name(0), "Speaker 1");
    assert_eq!(speaker_name(2), "Speaker 3");
    let empty = NameSuggestions::none();
    assert_eq!(empty.display(0), "Speaker 1");
    let mut names = HashMap::new();
    names.insert(0, "Ada".to_string());
    let suggested = NameSuggestions { names };
    assert_eq!(suggested.display(0), "Ada");
    assert_eq!(suggested.display(1), "Speaker 2");
}
