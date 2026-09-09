use super::*;

// The legacy literals these tests pin are the exact spellings previously
// hard-coded across arc_eager.rs (suffix args to `has_suffix_ci`, the three
// punctuation sets, the clause comma). The English blob must carry them
// byte-for-byte: that equality IS the zero-behavior-change proof for the
// blob migration at every call site.
fn legacy_trailing() -> Vec<&'static str> {
    vec![".", "!", "?", ";", ":", ",", "—", "--"]
}

fn legacy_boundary() -> Vec<&'static str> {
    vec![".", "!", "?", ";", ":", "-", "—", "--"]
}

fn legacy_bare_follow() -> Vec<&'static str> {
    vec![".", "!", "?", ";", ":", "—", "--", ",", "(", ")", "..."]
}

fn english() -> TaggerOrtho<'static> {
    TaggerOrtho::from_bytes(crate::lang::en::ORTHO_BLOB).expect("English ortho blob parses")
}

#[test]
fn english_blob_carries_exact_legacy_sets() {
    let o = english();
    assert_eq!(o.trailing_punct(), legacy_trailing().as_slice());
    assert_eq!(o.sentence_boundary(), legacy_boundary().as_slice());
    assert_eq!(o.bare_follow(), legacy_bare_follow().as_slice());
    assert_eq!(o.plural_s(), "s");
    assert_eq!(o.manner_ly(), "ly");
    assert_eq!(o.past_ed(), "ed");
    assert_eq!(o.part_ing(), "ing");
    assert_eq!(o.comma(), ",");
}

#[test]
fn predicates_match_legacy_spellings() {
    let o = english();
    // Suffix predicates vs the legacy case-insensitive idiom (total:
    // short words are false, where the raw idiom would underflow).
    let lic = |word: &str, suffix: &str| {
        word.len() >= suffix.len()
            && word
                .get(word.len() - suffix.len()..)
                .is_some_and(|sfx| sfx.eq_ignore_ascii_case(suffix))
    };
    let words = [
        "calls", "CALLS", "as", "quarterly", "QUARTERLY", "July", "opened", "OPENED", "red",
        "smiling", "SMILING", "king", "rain", "raining", "go", "a", "today", "statuses",
    ];
    for w in words {
        assert_eq!(o.is_plural_s(w), lic(w, "s"), "plural {w:?}");
        assert_eq!(o.is_manner_ly(w), lic(w, "ly"), "manner {w:?}");
        assert_eq!(o.is_past_ed(w), lic(w, "ed"), "past {w:?}");
        assert_eq!(o.is_part_ing(w), lic(w, "ing"), "part {w:?}");
    }
    // Token predicates vs the legacy matches! sets.
    for t in [".", "!", "?", ";", ":", ",", "-", "—", "--", "(", ")", "...", "calls", "x", ""] {
        assert_eq!(
            o.is_trailing_punct(t),
            legacy_trailing().contains(&t),
            "trailing {t:?}"
        );
        assert_eq!(
            o.is_boundary(t),
            legacy_boundary().contains(&t),
            "boundary {t:?}"
        );
        assert_eq!(
            o.is_bare_follow(t),
            legacy_bare_follow().contains(&t),
            "bare {t:?}"
        );
        assert_eq!(o.is_comma(t), t == ",", "comma {t:?}");
    }
    // Span predicates vs the legacy slice-alls.
    let streams: Vec<Vec<String>> = [
        vec!["calls", "."],
        vec!["yet", "."],
        vec!["work", "?"],
        vec![".", "calls"],
        vec!["?", "x"],
        vec!["Call", "the", "office", "."],
        vec!["Sit", ".", "Call", "."],
    ]
    .iter()
    .map(|s| s.iter().map(|t| t.to_string()).collect())
    .collect();
    for texts in &streams {
        for from in 0..=texts.len() {
            let legacy = texts[from..].iter().all(|t| legacy_trailing().contains(&t.as_str()));
            assert_eq!(o.trailing_only(texts, from), legacy, "texts={texts:?}");
        }
        for i in 0..texts.len() {
            let legacy = i == 0 || legacy_boundary().contains(&texts[i - 1].as_str());
            assert_eq!(o.starts_sentence(texts, i), legacy, "texts={texts:?} i={i}");
        }
    }
}

#[test]
fn loader_rejects_bad_blobs() {
    // Wrong magic.
    let mut bad = crate::lang::en::ORTHO_BLOB.to_vec();
    bad[0] ^= 0xFF;
    assert!(TaggerOrtho::from_bytes(Box::leak(bad.into_boxed_slice())).is_err());
    // Wrong version.
    let mut bad = crate::lang::en::ORTHO_BLOB.to_vec();
    bad[4] ^= 0xFF;
    assert!(TaggerOrtho::from_bytes(Box::leak(bad.into_boxed_slice())).is_err());
    // Truncation (every prefix must fail, never panic).
    for len in 0..crate::lang::en::ORTHO_BLOB.len() {
        assert!(
            TaggerOrtho::from_bytes(&crate::lang::en::ORTHO_BLOB[..len]).is_err(),
            "prefix len {len} must fail"
        );
    }
    // Empty scalar / NUL entry / non-UTF8 are build-time rejects; the
    // loader defends the same invariants.
    assert!(TaggerOrtho::from_bytes(&[]).is_err());
}

fn pinned_words() -> Vec<&'static str> {
    vec![
        "calls", "CALLS", "as", "quarterly", "QUARTERLY", "July", "opened", "OPENED", "red",
        "smiling", "SMILING", "king", "rain", "raining", "go", "a", "today", "statuses",
        ".", "!", "?", ";", ":", ",", "-", "—", "--", "(", ")", "...", "calls", "x", "",
    ]
}

#[test]
fn english_constructor_matches_fresh_parse() {
    // The shared English handle must answer identically to a freshly parsed
    // blob on a pinned token list — caching the parse never changes it.
    let cached = TaggerOrtho::english();
    let fresh = TaggerOrtho::from_bytes(crate::lang::en::ORTHO_BLOB).expect("fresh parse");
    assert_eq!(cached.trailing_punct(), fresh.trailing_punct());
    assert_eq!(cached.sentence_boundary(), fresh.sentence_boundary());
    assert_eq!(cached.bare_follow(), fresh.bare_follow());
    assert_eq!(cached.plural_s(), fresh.plural_s());
    assert_eq!(cached.manner_ly(), fresh.manner_ly());
    assert_eq!(cached.past_ed(), fresh.past_ed());
    assert_eq!(cached.part_ing(), fresh.part_ing());
    assert_eq!(cached.comma(), fresh.comma());
    for w in pinned_words() {
        assert_eq!(cached.is_plural_s(w), fresh.is_plural_s(w), "plural {w:?}");
        assert_eq!(cached.is_manner_ly(w), fresh.is_manner_ly(w), "manner {w:?}");
        assert_eq!(cached.is_past_ed(w), fresh.is_past_ed(w), "past {w:?}");
        assert_eq!(cached.is_part_ing(w), fresh.is_part_ing(w), "part {w:?}");
        assert_eq!(
            cached.is_trailing_punct(w),
            fresh.is_trailing_punct(w),
            "trailing {w:?}"
        );
        assert_eq!(cached.is_boundary(w), fresh.is_boundary(w), "boundary {w:?}");
        assert_eq!(cached.is_bare_follow(w), fresh.is_bare_follow(w), "bare {w:?}");
        assert_eq!(cached.is_comma(w), fresh.is_comma(w), "comma {w:?}");
    }
}

#[test]
fn english_handles_concurrent_construction() {
    // N threads building the English handle at once must neither deadlock
    // nor diverge in what they observe.
    let handles: Vec<_> = (0..8)
        .map(|_| {
            std::thread::spawn(|| {
                let o = TaggerOrtho::english();
                pinned_words()
                    .iter()
                    .map(|w| (o.is_plural_s(w), o.is_past_ed(w), o.is_boundary(w)))
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let mut first: Option<Vec<(bool, bool, bool)>> = None;
    for h in handles {
        let got = h.join().expect("worker panicked — deadlock or crash");
        match &first {
            None => first = Some(got),
            Some(want) => assert_eq!(&got, want, "concurrent handles diverged"),
        }
    }
}
