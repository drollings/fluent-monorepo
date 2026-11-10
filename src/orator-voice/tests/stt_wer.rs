//! WER goldens: identical, substitution, deletion, insertion, empty.

use crate::stt::wer::wer;

#[test]
fn identical_scores_zero() {
    assert!((wer("hello world", "hello world") - 0.0).abs() < 1e-9);
}

#[test]
fn one_substitution_over_two_words_scores_half() {
    assert!((wer("hello world", "hello there") - 0.5).abs() < 1e-9);
}

#[test]
fn deletion_scores_by_reference_length() {
    assert!((wer("one two three", "one three") - (1.0 / 3.0)).abs() < 1e-9);
}

#[test]
fn insertion_counts_and_can_exceed_one() {
    assert!((wer("hi", "hi there friend") - 2.0).abs() < 1e-9);
}

#[test]
fn empty_pairs_follow_the_contract() {
    assert!((wer("", "") - 0.0).abs() < 1e-9);
    assert!((wer("", "words here") - 1.0).abs() < 1e-9);
    assert!((wer("words here", "") - 1.0).abs() < 1e-9);
}

#[test]
fn whitespace_normalizes_before_scoring() {
    assert!((wer("  hello   world\n", "hello world") - 0.0).abs() < 1e-9);
}
