//! Splitter goldens: buffer-end holds, abbreviation guard, sizing exact,
//! hard/soft flags, trailing gaps, and agreement with the sentence oracle.

use crate::splitter::{
    find_cut, split_all, tail_gap_ms, Chunk, SentenceTools, CAP_CHUNK, FIRST_MAX, MIN_CHUNK,
};

fn cut_text(buffer: &str, first: bool) -> Option<(String, bool)> {
    find_cut(buffer, first, None).map(|cut| (buffer[..cut.len].trim_end().to_string(), cut.hard))
}

#[test]
fn never_cuts_at_buffer_end() {
    assert_eq!(cut_text("Hello world, this is a test.", false), None);
    assert_eq!(cut_text("Hello world, this is a test. ", false), None);
    assert_eq!(
        cut_text("Hello world, this is a test. Next bit here.", false),
        Some(("Hello world, this is a test.".to_string(), true))
    );
}

#[test]
fn full_sentence_cuts_hard() {
    assert_eq!(
        cut_text("First sentence here. Second one.", false),
        Some(("First sentence here.".to_string(), true))
    );
    assert_eq!(
        cut_text("Is that really so? Yes indeed ok friend.", false),
        Some(("Is that really so?".to_string(), true))
    );
    assert_eq!(
        cut_text("Stop that right now! Hammer time now friend.", false),
        Some(("Stop that right now!".to_string(), true))
    );
}

#[test]
fn tiny_fragments_stay_merged() {
    assert_eq!(cut_text("Hi. There you are friend.", false), None);
    let chunks = split_all("Hi. There you are friend.");
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].text, "Hi. There you are friend.");
}

#[test]
fn first_chunk_takes_the_early_comma() {
    assert_eq!(
        cut_text("Well now friends, this is a longer opening clause.", true),
        Some(("Well now friends,".to_string(), false))
    );
    // Not first anymore: no comma fast-path.
    assert_eq!(
        cut_text("Well now friends, this is a longer opening clause.", false),
        None
    );
    // Comma past the fast-path window holds too.
    let long = format!("{} , tail here now.", "word ".repeat(30));
    assert_eq!(find_cut(&long, true, None), None);
}

#[test]
fn cap_breaks_long_runs_at_a_word_boundary() {
    let run = format!("{} end of the line here.", "word ".repeat(60));
    let cut = find_cut(&run, false, None).expect("cap must cut");
    assert!(!cut.hard);
    assert!(cut.len <= CAP_CHUNK + 8);
    assert!(run[..cut.len].ends_with([' ', ',']));
    assert!(!run[..cut.len].ends_with("wo"));
}

#[test]
fn abbreviation_guard_holds_titles_and_initials() {
    assert_eq!(cut_text("Dr. Smith is here now friend.", false), None);
    assert_eq!(cut_text("Meet Mrs. Jones at noon today.", false), None);
    assert_eq!(cut_text("It costs e.g. five bucks total.", false), None);
    // …but a real sentence end after a name still cuts.
    assert_eq!(
        cut_text("Smith is here today friend. Jones left early now.", false),
        Some(("Smith is here today friend.".to_string(), true))
    );
}

#[test]
fn lowercase_continuation_holds_the_buffer_end() {
    // A period followed by lowercase reads as an abbreviation tail,
    // not a sentence end: the buffer holds for more text.
    assert_eq!(
        cut_text("Met at noon today friend. jones left early now.", false),
        None
    );
}

#[test]
fn split_all_covers_and_flags() {
    // "listen." is below MIN so it merges into the next sentence.
    let chunks = split_all("Hello there good friend, listen. This is the second sentence here!");
    assert_eq!(chunks.len(), 2);
    assert_eq!(
        (chunks[0].text.as_str(), chunks[0].hard),
        ("Hello there good friend,", false)
    );
    assert_eq!(
        (chunks[1].text.as_str(), chunks[1].hard),
        ("listen. This is the second sentence here!", true)
    );
}

#[test]
fn gaps_breathe_after_sentences_and_flow_after_commas() {
    let hard = Chunk {
        text: "Done.".to_string(),
        hard: true,
    };
    let soft = Chunk {
        text: "Well,".to_string(),
        hard: false,
    };
    assert_eq!(tail_gap_ms(&hard, false), 140);
    assert_eq!(tail_gap_ms(&soft, false), 60);
    assert_eq!(tail_gap_ms(&hard, true), 0);
    assert_eq!(tail_gap_ms(&soft, true), 0);
    assert!(tail_gap_ms(&hard, false) > tail_gap_ms(&soft, false));
}

#[test]
fn sizing_constants_match_the_proven_numbers() {
    assert_eq!(MIN_CHUNK, 16);
    assert_eq!(FIRST_MAX, 100);
    assert_eq!(CAP_CHUNK, 220);
}

#[test]
fn streaming_cuts_agree_with_the_sentence_oracle() {
    let tools = SentenceTools::new().expect("sentence tools build");
    let text = "Hello world today friend. How are you doing? I am fine, thanks a lot.";
    // Every hard streaming cut must sit on an oracle boundary.
    let mut rest = text;
    let mut consumed = 0usize;
    let mut first = true;
    let mut cuts = 0usize;
    while let Some(cut) = find_cut(rest, first, Some(&tools)) {
        if cut.hard {
            assert!(
                tools.boundary_at(text, consumed + cut.len),
                "hard cut must confirm: {:?}",
                &rest[..cut.len]
            );
            cuts += 1;
        }
        consumed += cut.len;
        rest = &text[consumed..];
        rest = rest.trim_start();
        consumed = text.len() - rest.len();
        first = false;
    }
    assert!(cuts >= 2, "oracle text must yield hard cuts");
}

#[test]
fn token_budget_rechunks_without_losing_words() {
    // The token budget (`CAP_CHUNK / 4` ≈ four chars per token) keeps a
    // no-pause run near the same length: every word survives, and no
    // chunk runs far past the cap.
    let run = "word ".repeat(200);
    let chunks = split_all(&run);
    assert!(!chunks.is_empty(), "long run splits");
    let covered: usize = chunks
        .iter()
        .map(|chunk| chunk.text.split_whitespace().count())
        .sum();
    assert_eq!(covered, 200, "budget re-chunking never loses words");
    for chunk in &chunks {
        assert!(
            chunk.text.chars().count() <= CAP_CHUNK + 8,
            "chunk stays near the cap: {} chars",
            chunk.text.chars().count()
        );
    }
    // A spaceless run has no word boundary to cut on: the budget
    // chunker still preserves every char (one or two synth calls,
    // never a dropped tail).
    let solid = "x".repeat(300);
    let chunks = split_all(&solid);
    assert!(!chunks.is_empty(), "unbreakable tail still synthesizes");
    let kept: String = chunks.iter().map(|chunk| chunk.text.as_str()).collect();
    assert_eq!(kept.len(), 300, "budget re-chunking never loses chars");
}
