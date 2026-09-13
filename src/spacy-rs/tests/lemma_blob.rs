use super::*;

fn blob() -> LemmaBlob {
    LemmaBlob::from_bytes(crate::lang::en::LEMMAS_BLOB)
        .expect("embedded English lemma blob parses")
}

#[test]
fn parses_all_pos_keys() {
    let b = blob();
    let keys: Vec<&str> = b.pos_keys().collect();
    assert_eq!(keys, vec!["adj", "adv", "noun", "punct", "verb"]);
}

#[test]
fn rules_are_present_per_pos() {
    let b = blob();
    assert_eq!(b.rules("noun").len(), 9);
    assert_eq!(b.rules("verb").len(), 8);
    assert_eq!(b.rules("punct").len(), 4);
    assert!(b.rules("noun").contains(&("ies", "y")));
    assert!(b.rules("missing").is_empty());
}

#[test]
fn index_binary_search_hits_edges_and_misses() {
    let b = blob();
    assert!(b.index_contains("noun", "'hood"));
    assert!(b.index_contains("noun", "zyrian"));
    assert!(b.index_contains("verb", "aah"));
    assert!(b.index_contains("adj", "zymotic"));
    assert!(!b.index_contains("noun", "zzzznope"));
    assert!(!b.index_contains("missing", "cat"));
}

#[test]
fn exc_for_returns_lemma_list() {
    let b = blob();
    let lemmas = b.exc_for("verb", "went").expect("went is an exception");
    let forms: Vec<&str> = lemmas
        .split(|&x| x == 0)
        .filter(|s| !s.is_empty())
        .filter_map(|s| std::str::from_utf8(s).ok())
        .collect();
    assert_eq!(forms, vec!["go"]);
    let children = b.exc_for("noun", "children").expect("children exception");
    let forms: Vec<&str> = children
        .split(|&x| x == 0)
        .filter(|s| !s.is_empty())
        .filter_map(|s| std::str::from_utf8(s).ok())
        .collect();
    assert_eq!(forms, vec!["child"]);
    assert!(b.exc_for("noun", "notanexception").is_none());
}

#[test]
fn rejects_bad_magic_and_version() {
    let data: &'static [u8] = &[0, 0, 0, 0];
    assert!(LemmaBlob::from_bytes(data).is_err());
    let data: &'static [u8] = &[
        0x31, 0x4D, 0x4C, 0x53, // "SLM1"
        2, 0, // version 2 (unsupported)
        0, 0, // zero pos
    ];
    assert!(LemmaBlob::from_bytes(data).is_err());
}

/// M2.1 characterization: a short header (fewer than 4 bytes) fails as
/// truncated magic, never panics.
#[test]
fn rejects_short_magic() {
    let data: &'static [u8] = &[0x32, 0x4D];
    assert!(LemmaBlob::from_bytes(data).is_err());
    let empty: &'static [u8] = &[];
    assert!(LemmaBlob::from_bytes(empty).is_err());
}

/// M2.1 characterization: truncation is monotone and trailing-tolerant —
/// prefixes that still cover every consumed section parse (the lemma loader,
/// unlike the ortho loader, ignores trailing bytes), while any shorter
/// prefix fails loudly, never panics. The threshold below is bisected at
/// test time so the test pins the *shape* (monotone + full parses), not a
/// hard-coded layout offset; the migration must preserve trailing tolerance.
#[test]
fn truncation_is_monotone_and_trailing_tolerant() {
    let full = crate::lang::en::LEMMAS_BLOB;
    assert!(LemmaBlob::from_bytes(full).is_ok(), "full blob parses");
    // Bisect the smallest prefix that still parses. Monotone: a longer
    // input can only turn bounded `get` reads from `None` into `Some`.
    let (mut lo, mut hi) = (0usize, full.len());
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if LemmaBlob::from_bytes(&full[..mid]).is_ok() {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    let threshold = lo;
    assert!(threshold > 0 && threshold <= full.len());
    // Spot-check below the threshold: every cut fails loudly, none panics.
    let mut lens: Vec<usize> = (0..64).collect();
    lens.extend((0..threshold).step_by(65_536));
    lens.push(threshold.saturating_sub(1));
    for len in lens {
        if len < threshold {
            assert!(
                LemmaBlob::from_bytes(&full[..len]).is_err(),
                "prefix len {len} must fail"
            );
        }
    }
}

/// M2.1 characterization: a section offset pointing past the end fails as
/// out-of-range, never panics (the `slice` bounds check, not the parser).
#[test]
fn rejects_out_of_range_section() {
    // SLM2 header: `section_off` is the u32 at bytes 20..24 (real value 44).
    // Maxing its low byte pushes the directory past the mapped sections.
    let mut bad = crate::lang::en::LEMMAS_BLOB.to_vec();
    bad[20] = 0xFF;
    let leaked: &'static [u8] = Box::leak(bad.into_boxed_slice());
    assert!(LemmaBlob::from_bytes(leaked).is_err());
}
