use super::*;

/// Golden values produced by compiling the authoritative
/// `MurmurHash2.cpp` `MurmurHash64A` with seed 1 (and cross-checked
/// against a pure-Python port) on 2026-08-25.
#[test]
fn hash_parity_with_spacy_murmur64a() {
    let cases: &[(&str, u64)] = &[
        ("", 14313749767032693980),
        ("hello", 5983625672228268878),
        ("hello world", 2758594965276909933),
        ("Apple", 6418411030699964375),
        ("nsubj", 1638336668109737677),
        ("ROOT", 8206900633647566924),
        ("X", 4918752717281726814),
        ("éclair", 4594911457355527430),
        ("dyn-o-mite-dave", 6759597906574577614),
        ("do n't", 16989667434526043584),
        ("’s", 614914527630368944),
        ("the", 7425985699627899538),
        ("I", 4690420944186131903),
        ("don't", 8627107437989221290),
        ("n't", 2043519015752540944),
        ("www.example.com", 11376590935768553373),
        ("3.5%", 3579893109067563421),
        ("A", 14862748245026736845),
        ("AB", 3916325639175504915),
        ("ABC", 125840725284221120),
        ("ABCD", 7946017431112816765),
        ("ABCDE", 12382937101347572856),
        ("ABCDEF", 7176860159450443390),
        ("ABCDEFG", 6037090127816012894),
        ("ABCDEFGH", 16318043397631462693),
        ("ABCDEFGHI", 8133142713256632090),
    ];

    for (s, expected) in cases {
        assert_eq!(hash_utf8(s), *expected, "hash mismatch for {s:?}");
    }
}

#[test]
fn distinct_inputs_distinct_hashes() {
    assert_ne!(hash_utf8("the"), hash_utf8("teh"));
    assert_ne!(hash_utf8("apple"), hash_utf8("Apple"));
}

#[test]
fn block_boundary_handling() {
    // 8 bytes (one full block) vs 9 bytes (block + 1 tail byte) must differ.
    assert_ne!(hash_utf8("12345678"), hash_utf8("123456789"));
    // 7 vs 8 tail transitions.
    assert_ne!(hash_utf8("1234567"), hash_utf8("12345678"));
}

// ── Murmur drift guard (intentional-fork agreement) ──
//
// `fluent-types` carries a private copy of this module's `murmur64a` /
// `hash_utf8` (its header documents why: no `types → spacy-rs` edge). This
// test routes the SAME inputs through both copies — the local `hash_utf8`
// and the types copy inside `lemma_id_for_str` — and requires agreement
// through the shared public truncation seam (`id_for_namespace`). A vector
// mismatch fails the test; the expectation is never "updated" —
// investigate which copy drifted (the C-reference `hash_parity` test above
// independently pins this copy, so a failure here with that green means the
// types copy moved).
//
// Control group: empty, ASCII, multibyte, every tail length 0..7 (lengths
// 8..16), and multi-block inputs.

#[test]
fn murmur_copies_agree_across_crates() {
    use fluent_types::{id_for_namespace, lemma_id_for_str, InterlinguaNamespace};

    let vectors = [
        "",
        "hello",
        "hello world",
        "dog",
        "report",
        "café",
        "日本語",
        "🎉",
        "12345678",
        "123456789",
        "1234567890",
        "12345678901",
        "123456789012",
        "1234567890123",
        "12345678901234",
        "123456789012345",
        "1234567890123456",
        "the quick brown fox jumps over the lazy dog",
        "n't",
        "don't",
        "www.example.com",
    ];
    for s in vectors {
        let via_spacy_copy =
            id_for_namespace(hash_utf8(s) as i64, InterlinguaNamespace::SpacyLemma);
        assert_eq!(
            lemma_id_for_str(s),
            via_spacy_copy,
            "murmur fork drift on {s:?}: investigate before touching either copy"
        );
    }
}
