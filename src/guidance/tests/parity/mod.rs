//! P0 fixture corpora for legacy parity (`parity/*.rs` per roadmap
//! P0.1). Self-consistency goldens live here; behavior goldens live with
//! their owning suites.

pub mod corpus_cjk;

#[test]
fn cjk_queries_resolve_within_the_corpus() {
    use super::super::cjk_bigrams;
    for query in corpus_cjk::CJK_QUERIES {
        let doc = corpus_cjk::CJK_DOCS
            .iter()
            .find(|d| d.path == query.expect_path)
            .expect("golden query must name a corpus doc");
        // Every bigram of the query must occur in the expected doc text —
        // the exact substring property the FTS bigram column relies on.
        let wanted = cjk_bigrams(query.query);
        assert!(!wanted.is_empty(), "golden query must carry CJK: {}", query.query);
        for bigram in &wanted {
            assert!(
                doc.text.contains(bigram.as_str()),
                "query {:?} bigram {bigram:?} missing from {}",
                query.query,
                doc.path
            );
        }
    }
}
