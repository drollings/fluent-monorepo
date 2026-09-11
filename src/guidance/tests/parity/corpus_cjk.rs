//! CJK-heavy fixture corpus (Gate 0 §3, item iii).
//!
//! Frozen A/B corpora: (i) this monorepo (Rust-heavy, dogfood);
//! (ii) zvec-grep itself (TS-heavy, structural-extraction stress);
//! (iii) **this in-tree fixture corpus** (FTS-tokenizer risk).
//!
//! Item (iii) is deliberately in-tree rather than a third-party clone: P0
//! goldens pin tokenizer behavior (bigram expansion over Han/Hiragana/
//! Katakana/Hangul runs), which needs curated query→doc pairs, not volume.
//! P1 A/B runs the same query set against all three corpora; promoting (iii)
//! to a larger third-party corpus requires A/B evidence, not speculation.

/// One fixture document: workspace-relative path + source text.
pub struct CjkDoc {
    /// Workspace-relative path.
    pub path: &'static str,
    /// Source text (code with CJK comments/identifiers).
    pub text: &'static str,
}

/// One golden query: text + the doc it must surface.
pub struct CjkQuery {
    /// Query text.
    pub query: &'static str,
    /// Expected top document path.
    pub expect_path: &'static str,
}

/// Chinese / Japanese / Korean fixture documents.
pub const CJK_DOCS: &[CjkDoc] = &[
    CjkDoc {
        path: "src/segment.rs",
        text: "// 中文分词器：将查询切分为双字单元\n// 标点符号会打断字串\npub fn segment_zh(text: &str) -> Vec<String> {\n    todo!()\n}\n",
    },
    CjkDoc {
        path: "src/search_ja.ts",
        text: "// 日本語検索：クエリをバイグラムに展開する\n// ひらがなとカタカナは同一ランに含まれる\nexport function searchJa(query: string): string[] {\n  return [];\n}\n",
    },
    CjkDoc {
        path: "src/index_ko.py",
        text: "# 한국어 색인: 한글 음절을 자모가 아닌 음절 단위로 처리\n# 공백이 查询 런을 구분한다\ndef index_ko(text):\n    return []\n",
    },
    CjkDoc {
        path: "src/mixed.md",
        text: "# Hybrid Heading\n\nLatin text with 日本語 mixed in, plus 한국어 and 中文 in one doc.\n",
    },
];

/// Golden queries, each naming its expected document.
pub const CJK_QUERIES: &[CjkQuery] = &[
    CjkQuery {
        query: "分词器",
        expect_path: "src/segment.rs",
    },
    CjkQuery {
        query: "バイグラム",
        expect_path: "src/search_ja.ts",
    },
    CjkQuery {
        query: "색인",
        expect_path: "src/index_ko.py",
    },
    CjkQuery {
        query: "日本語",
        expect_path: "src/mixed.md",
    },
];
