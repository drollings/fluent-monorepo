use super::*;
use crate::lang::en;
use std::sync::Arc;
use std::time::Instant;

fn en_tokenizer() -> Tokenizer {
    let vocab = Arc::new(Vocab::new(en::lexicon_config()));
    en::tokenizer(vocab).expect("english tokenizer builds")
}

fn snapshot(doc: &Doc) -> Vec<(String, u32, bool)> {
    let strings = doc.vocab().strings();
    doc.tokens()
        .iter()
        .map(|t| (t.lexeme.orth_text(strings), t.idx, t.spacy))
        .collect()
}

/// Rule tables freeze when the builder finishes: constructor-provided
/// exceptions are live rules, but anything registered afterwards is refused
/// instead of mutating shared tables under concurrent readers.
#[test]
fn post_freeze_registration_errors() {
    let t = en_tokenizer();
    let err = t
        .add_special_case("gonna", &[("gon", None), ("na", None)])
        .expect_err("post-freeze registration must fail");
    let detail = err.to_string();
    assert!(
        detail.contains("frozen") || detail.contains("freeze"),
        "error names the freeze: {detail}"
    );
}

/// Pre-freeze registrations (the constructor's exception table) stay visible:
/// the decided rule output is unchanged by the freeze.
#[test]
fn constructor_rules_visible_after_freeze() {
    let t = en_tokenizer();
    assert_eq!(
        snapshot(&t.tokenize("I can't go").expect("tokenizes"))[1].0,
        "ca",
        "constructor exception rule still fires"
    );
}

/// Byte-range span path, multilingual adversarial fixture: exact orth,
/// char-idx, and spacy goldens recorded from the pre-change implementation.
/// The span path may slice bytes instead of collecting chars, but every
/// observable (including char-based `idx`) must be identical.
#[test]
fn byte_range_equivalence_adversarial_fixture() {
    let t = en_tokenizer();
    let cases: &[(&str, &[(&str, u32, bool)])] = &[
        ("Hello world", &[("Hello", 0, true), ("world", 6, false)]),
        (
            "caf\u{e9} na\u{ef}ve r\u{e9}sum\u{e9}",
            &[("caf\u{e9}", 0, true), ("na\u{ef}ve", 5, true), ("r\u{e9}sum\u{e9}", 11, false)],
        ),
        ("e\u{301}cole", &[("e\u{301}cole", 0, false)]),
        (
            "日本語 テスト",
            &[("日本語", 0, true), ("テスト", 4, false)],
        ),
        (
            "emoji \u{1f600}\u{1f389} test",
            &[
                ("emoji", 0, true),
                ("\u{1f600}", 6, false),
                ("\u{1f389}", 7, true),
                ("test", 9, false),
            ],
        ),
        ("a\u{200b}b", &[("a\u{200b}b", 0, false)]),
        (
            "tab\there",
            &[("tab", 0, false), ("\t", 3, false), ("here", 4, false)],
        ),
        (
            "مرحبا بالعالم",
            &[("مرحبا", 0, true), ("بالعالم", 6, false)],
        ),
        (
            "mixed-dash – en – and — em",
            &[
                ("mixed", 0, false),
                ("-", 5, false),
                ("dash", 6, true),
                ("–", 11, true),
                ("en", 13, true),
                ("–", 16, true),
                ("and", 18, true),
                ("—", 22, true),
                ("em", 24, false),
            ],
        ),
        (
            "don't \"quote\" (paren) 3.14",
            &[
                ("do", 0, false),
                ("n't", 2, true),
                ("\"", 6, false),
                ("quote", 7, false),
                ("\"", 12, true),
                ("(", 14, false),
                ("paren", 15, false),
                (")", 20, true),
                ("3.14", 22, false),
            ],
        ),
        (
            "a b",
            &[("a", 0, false), (" ", 1, false), ("b", 2, false)],
        ),
        (
            "line1\nline2\r\nline3",
            &[
                ("line1", 0, false),
                ("\n", 5, false),
                ("line2", 6, false),
                ("\r\n", 11, false),
                ("line3", 13, false),
            ],
        ),
    ];
    for (text, expected) in cases {
        let doc = t.tokenize(text).expect("tokenizes");
        let got = snapshot(&doc);
        let want: Vec<(String, u32, bool)> = expected
            .iter()
            .map(|(o, i, s)| ((*o).to_string(), *i, *s))
            .collect();
        assert_eq!(got, want, "byte-identical segmentation for {text:?}");
    }
}

/// No lock is held across spans: 32 threads sharing one tokenizer must finish
/// faster than one thread doing the same total work (strict speedup on a
/// multicore host), with byte-identical output per doc.
#[ignore = "heavy (~9s): 32-thread contention smoke; run via `make test-heavy`"]
#[test]
fn contention_smoke_32_threads() {
    let t = Arc::new(en_tokenizer());
    let texts = [
        "Hello world, this is a contention probe.",
        " caf\u{e9} na\u{ef}ve 日本語 \u{1f600} mixed-dash – test ",
        "don't \"quote\" (paren) 3.14 at 5a.m. sharp",
    ];
    let reference: Vec<Vec<(String, u32, bool)>> = texts
        .iter()
        .map(|text| snapshot(&t.tokenize(text).expect("tokenizes")))
        .collect();

    let reps = 30usize;
    let threads = 32usize;
    let single_start = Instant::now();
    for _ in 0..threads * reps {
        for text in &texts {
            let _ = t.tokenize(text).expect("tokenizes");
        }
    }
    let single_elapsed = single_start.elapsed();

    let par_start = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let (t, texts, reference) = (Arc::clone(&t), &texts, &reference);
            scope.spawn(move || {
                for _ in 0..reps {
                    for (text, want) in texts.iter().zip(reference.iter()) {
                        let got = snapshot(&t.tokenize(text).expect("tokenizes"));
                        assert_eq!(&got, want, "identical output under contention");
                    }
                }
            });
        }
    });
    let par_elapsed = par_start.elapsed();
    assert!(
        par_elapsed < single_elapsed,
        "shared tokenizer scales (no span-path lock): parallel {par_elapsed:?} vs single {single_elapsed:?}"
    );
}
