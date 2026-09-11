use super::*;
use crate::search_constants::RRF_K;

#[path = "parity/mod.rs"]
pub mod parity;

// Ported P0 contract tests: RRF tie-break + N-route fusion (zvec
// `fuseCandidates` semantics), `extractSymbolNames` + `symbolNameFromToken`,
// surrogate-safe boundaries (`code.test.mjs:327`), group discipline
// (`zvec-storage` validation codes), FTS/CJK goldens (new).

fn fragment(id: &str, group: Option<&str>) -> EntityFragment {
    EntityFragment {
        id: id.to_string(),
        group: group.map(str::to_string),
        file_id: "file-a".to_string(),
        range: FragmentSpan::File,
        content: FragmentContent::Text {
            text: "fn f() {}".to_string(),
        },
        metadata: None,
    }
}

// --- RRF fusion (fuseCandidates: score = Σ 1/(K+rank), id tie-break) ---

#[test]
fn fusion_sums_rrf_across_lists_and_orders_by_score() {
    // 1-based storage ranks: a: 1/61 · b: 1/62+1/61 · c: 1/62.
    let fused = rrf_fuse(&[vec!["a".to_string(), "b".to_string()], vec!["b".to_string(), "c".to_string()]], RRF_K);
    let ids: Vec<&str> = fused.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(ids, vec!["b", "a", "c"]);
    assert_eq!(fused[0].rank, 1);
    assert_eq!(fused[1].rank, 2);
    assert_eq!(fused[2].rank, 3);
    assert!((fused[0].score - (1.0 / 62.0 + 1.0 / 61.0)).abs() < 1e-12);
}

#[test]
fn fusion_breaks_score_ties_by_lexicographic_id() {
    let fused = rrf_fuse(&[vec!["b".to_string()], vec!["a".to_string()]], RRF_K);
    let ids: Vec<&str> = fused.iter().map(|h| h.id.as_str()).collect();
    assert_eq!(ids, vec!["a", "b"]);
    assert_eq!(fused[0].score.to_bits(), fused[1].score.to_bits());
}

#[test]
fn fusion_generalizes_to_n_routes() {
    // x appears in 3 routes at rank 0; y once at rank 0 of one route.
    let fused = rrf_fuse(
        &[
            vec!["x".to_string()],
            vec!["x".to_string(), "y".to_string()],
            vec!["x".to_string()],
        ],
        RRF_K,
    );
    assert_eq!(fused[0].id, "x");
    assert!((fused[0].score - 3.0 / 61.0).abs() < 1e-12);
    assert_eq!(fused[1].id, "y");
}

#[test]
fn fusion_ignores_empty_lists_and_dedupes_within_a_list() {
    let fused = rrf_fuse(
        &[vec![], vec!["a".to_string(), "a".to_string()]],
        RRF_K,
    );
    assert_eq!(fused.len(), 1);
    assert_eq!(fused[0].id, "a");
}

#[test]
fn matched_by_provenance_matches_default() {
    assert_eq!(derive_matched_by(&[RecallPath::Fts]), SearchMatchedBy::Fts);
    assert_eq!(
        derive_matched_by(&[RecallPath::Vector]),
        SearchMatchedBy::Vector
    );
    assert_eq!(
        derive_matched_by(&[RecallPath::Fts, RecallPath::Vector]),
        SearchMatchedBy::FtsAndVector
    );
    // zvec default: empty sources read as fts.
    assert_eq!(derive_matched_by(&[]), SearchMatchedBy::Fts);
}

// --- extractSymbolNames + symbolNameFromToken ---

#[test]
fn extract_symbol_names_skips_keyword_stoplist() {
    assert_eq!(
        extract_symbol_names("find Namespace::AlphaSymbol"),
        vec!["Namespace::AlphaSymbol".to_string()]
    );
    assert_eq!(
        extract_symbol_names("class Foo"),
        vec!["Foo".to_string()]
    );
    assert!(extract_symbol_names("find where explain").is_empty());
}

#[test]
fn symbol_name_from_token_keeps_uppercase_owner_only() {
    assert_eq!(
        symbol_name_from_token("AlphaSymbol"),
        Some("AlphaSymbol".to_string())
    );
    assert_eq!(
        symbol_name_from_token("Namespace::AlphaSymbol"),
        Some("Namespace::AlphaSymbol".to_string())
    );
    // Lowercase owner is dropped (zvec: `foo::bar` → `bar`).
    assert_eq!(symbol_name_from_token("foo::bar"), Some("bar".to_string()));
    assert_eq!(symbol_name_from_token("123"), None);
    assert_eq!(symbol_name_from_token("a-b"), None);
}

// --- char boundaries (surrogate-pair safety) ---

#[test]
fn floor_char_boundary_never_splits_a_char() {
    let text = "a😀b";
    assert_eq!(text.len(), 6);
    assert_eq!(floor_char_boundary(text, 0), 0);
    assert_eq!(floor_char_boundary(text, 1), 1);
    assert_eq!(floor_char_boundary(text, 2), 1);
    assert_eq!(floor_char_boundary(text, 4), 1);
    assert_eq!(floor_char_boundary(text, 5), 5);
    assert_eq!(floor_char_boundary(text, 6), 6);
    assert_eq!(floor_char_boundary(text, 100), 6);
}

// --- group discipline (zvec-storage validation codes) ---

#[test]
fn group_validation_accepts_one_major_plus_minors() {
    let fragments = vec![
        fragment("entity-a", Some("entity-a")),
        fragment("entity-a#1", Some("entity-a")),
    ];
    assert!(validate_fragment_groups("file-a", &fragments).is_ok());
    // Ungrouped fragments are entities in their own right.
    assert!(validate_fragment_groups("file-a", &[fragment("solo", None)]).is_ok());
}

#[test]
fn group_validation_rejects_wrong_file() {
    let stray = EntityFragment {
        file_id: "file-b".to_string(),
        ..fragment("x", None)
    };
    let err = validate_fragment_groups("file-a", &[stray]).unwrap_err();
    assert_eq!(err.code(), FragmentErrorCode::FragmentFileMismatch);
}

#[test]
fn group_validation_rejects_duplicate_ids() {
    let fragments = vec![fragment("dup", None), fragment("dup", None)];
    let err = validate_fragment_groups("file-a", &fragments).unwrap_err();
    assert_eq!(err.code(), FragmentErrorCode::DuplicateFragmentId);
}

#[test]
fn group_validation_requires_exactly_one_major() {
    // No major: group members without the id==group fragment.
    let orphan = vec![fragment("entity-a#1", Some("entity-a"))];
    let err = validate_fragment_groups("file-a", &orphan).unwrap_err();
    assert_eq!(err.code(), FragmentErrorCode::InvalidFragmentGroup);
    // Two groups each with their own major are fine.
    let two_groups = vec![
        fragment("entity-a", Some("entity-a")),
        fragment("entity-b", Some("entity-b")),
        fragment("entity-b#1", Some("entity-b")),
    ];
    assert!(validate_fragment_groups("file-a", &two_groups).is_ok());
    // A double-major is impossible under id uniqueness (two fragments cannot
    // both carry the group id as their own id), so the no-major case above
    // is the canonical INVALID_FRAGMENT_GROUP violation.
}

#[test]
fn public_entity_id_prefers_group() {
    assert_eq!(public_entity_id(&fragment("frag", Some("entity"))), "entity");
    assert_eq!(public_entity_id(&fragment("solo", None)), "solo");
    // Empty group behaves as no group (zvec `""` major marker).
    assert_eq!(public_entity_id(&fragment("m", Some(""))), "m");
}

#[test]
fn public_entity_ids_collect_majors_and_ungrouped() {
    let fragments = vec![
        fragment("entity-a", Some("entity-a")),
        fragment("entity-a#1", Some("entity-a")),
        fragment("solo", None),
    ];
    assert_eq!(
        public_entity_ids(&fragments),
        vec!["entity-a".to_string(), "solo".to_string()]
    );
}

// --- serde contracts (wire shapes mirror types.ts kinds) ---

#[test]
fn range_and_content_kinds_round_trip() {
    let range = FragmentSpan::Text {
        start_line: 1,
        end_line: 2,
        start_offset: 0,
        end_offset: 10,
    };
    let json = serde_json::to_string(&range).expect("serialize");
    assert!(json.contains("\"kind\":\"text\""));
    let back: FragmentSpan = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, range);

    let content = FragmentContent::Image {
        data: vec![0x89, 0x50],
        format: ImageFormat::Png,
    };
    let json = serde_json::to_string(&content).expect("serialize");
    assert!(json.contains("\"kind\":\"image\""));
    let back: FragmentContent = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, content);
}

#[test]
fn symbol_vocabularies_are_closed() {
    // 6 symbol types, 7 modifiers — the roadmap C.3 sets.
    assert_eq!(CodeSymbolType::all().len(), 6);
    assert_eq!(CodeEntityModifier::all().len(), 7);
    let json = serde_json::to_string(&CodeSymbolType::Function).expect("serialize");
    assert_eq!(json, "\"function\"");
}

// --- FTS/CJK goldens (new; P1 builds the table on this contract) ---

#[test]
fn cjk_detection_covers_han_hiragana_katakana_hangul() {
    assert!(is_cjk_char('日'));
    assert!(is_cjk_char('あ'));
    assert!(is_cjk_char('ア'));
    assert!(is_cjk_char('한'));
    assert!(!is_cjk_char('a'));
    assert!(!is_cjk_char('1'));
    assert!(!is_cjk_char(' '));
}

#[test]
fn cjk_bigrams_expand_runs_without_dictionary() {
    assert_eq!(cjk_bigrams("日本語"), vec!["日本", "本語"]);
    assert_eq!(
        cjk_bigrams("日本語テスト"),
        vec!["日本", "本語", "語テ", "テス", "スト"]
    );
    // Non-CJK text breaks runs; single trailing chars emit nothing.
    assert!(cjk_bigrams("hello").is_empty());
    assert_eq!(cjk_bigrams("a日本b"), vec!["日本"]);
    assert!(cjk_bigrams("日").is_empty());
    assert!(cjk_bigrams("").is_empty());
}

#[test]
fn search_plan_value_types_carry_wire_shape() {
    let plan = SearchPlan {
        routes: vec![SearchPlanRoute {
            mode: SearchPlanRouteMode::Fts,
            query: "AlphaSymbol".to_string(),
        }],
        limit: Some(7),
        trace: true,
        ..Default::default()
    };
    let json = serde_json::to_string(&plan).expect("serialize");
    let back: SearchPlan = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, plan);
}
