//! Lexer tests: egui differential corpus (transcribed), v6 token units,
//! and golden snapshots for the three target files.
//!
//! The egui-charts crate is reference-only and never a dependency, so its
//! unit-test corpus (`lexer.rs :: tests`) is transcribed here as literal
//! expectations: every case below names the exact egui input and the token
//! kinds egui produces for it. Any divergence fails here first.

use super::*;
use std::path::PathBuf;

fn kinds_of(source: &str) -> Vec<TokenKind> {
    lex(source).into_iter().map(|token| token.kind).collect()
}

/// Fixture path (server `tests/data/pine_closure`), never the volatile live root:
/// the golden snapshots pin fixture bytes, which are checked in.
fn data_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../protopyne-server/tests/data/pine_closure")
        .join(name)
}

// ---- Transcribed egui corpus: `test_tokenize_numbers` ----

#[test]
fn differential_numbers_match_egui() {
    // egui input "42 3.25 1e5 2.5e-3" yields Number(42/3.25/1e5/2.5e-3).
    assert_eq!(
        kinds_of("42 3.25 1e5 2.5e-3"),
        vec![
            TokenKind::Number(42.0),
            TokenKind::Number(3.25),
            TokenKind::Number(1e5),
            TokenKind::Number(2.5e-3),
            TokenKind::Eof,
        ]
    );
}

// ---- Transcribed egui corpus: `test_tokenize_strings` ----

#[test]
fn differential_strings_match_egui() {
    // egui input `"Hello" 'World'` yields two plain strings.
    assert_eq!(
        kinds_of("\"Hello\" 'World'"),
        vec![
            TokenKind::String("Hello".to_owned()),
            TokenKind::String("World".to_owned()),
            TokenKind::Eof,
        ]
    );
}

// ---- Transcribed egui corpus: `test_tokenize_identifiers` ----

#[test]
fn differential_identifiers_match_egui() {
    // egui input "length ta.sma close": dotted names stay one identifier,
    // bare `close` is the builtin keyword.
    assert_eq!(
        kinds_of("length ta.sma close"),
        vec![
            TokenKind::Identifier("length".to_owned()),
            TokenKind::Identifier("ta.sma".to_owned()),
            TokenKind::Close,
            TokenKind::Eof,
        ]
    );
}

// ---- Transcribed egui corpus: `test_tokenize_operators` ----

#[test]
fn differential_operators_match_egui() {
    // egui input "+ - * / == != <= >=".
    assert_eq!(
        kinds_of("+ - * / == != <= >="),
        vec![
            TokenKind::Plus,
            TokenKind::Minus,
            TokenKind::Star,
            TokenKind::Slash,
            TokenKind::EqualEqual,
            TokenKind::NotEqual,
            TokenKind::LessEqual,
            TokenKind::GreaterEqual,
            TokenKind::Eof,
        ]
    );
}

// ---- Transcribed egui corpus: `test_tokenize_comment` ----

#[test]
fn differential_comment_match_egui() {
    // egui input "// This is a comment\nclose": comment text trimmed,
    // then a real newline token, then the keyword.
    assert_eq!(
        kinds_of("// This is a comment\nclose"),
        vec![
            TokenKind::Comment("This is a comment".to_owned()),
            TokenKind::Newline,
            TokenKind::Close,
            TokenKind::Eof,
        ]
    );
}

// ---- Overlap probes beyond egui's unit tests (same documented rules) ----

#[test]
fn differential_keywords_match_egui() {
    assert_eq!(
        kinds_of("if else for while var varip and or not true false"),
        vec![
            TokenKind::If,
            TokenKind::Else,
            TokenKind::For,
            TokenKind::While,
            TokenKind::Var,
            TokenKind::Varip,
            TokenKind::And,
            TokenKind::Or,
            TokenKind::Not,
            TokenKind::True,
            TokenKind::False,
            TokenKind::Eof,
        ]
    );
    assert_eq!(
        kinds_of("open high low close volume time"),
        vec![
            TokenKind::Open,
            TokenKind::High,
            TokenKind::Low,
            TokenKind::Close,
            TokenKind::Volume,
            TokenKind::Time,
            TokenKind::Eof,
        ]
    );
}

#[test]
fn differential_keyword_prefixes_stay_identifiers() {
    // egui matches whole dotted idents only: `andrew`, `closed`, `timex`
    // are identifiers, while `close.foo` keeps its dotted tail.
    assert_eq!(
        kinds_of("andrew closed timex close.foo"),
        vec![
            TokenKind::Identifier("andrew".to_owned()),
            TokenKind::Identifier("closed".to_owned()),
            TokenKind::Identifier("timex".to_owned()),
            TokenKind::Identifier("close.foo".to_owned()),
            TokenKind::Eof,
        ]
    );
}

#[test]
fn differential_delimiters_match_egui() {
    assert_eq!(
        kinds_of("( ) [ ] , ? : < > = ! %"),
        vec![
            TokenKind::LeftParen,
            TokenKind::RightParen,
            TokenKind::LeftBracket,
            TokenKind::RightBracket,
            TokenKind::Comma,
            TokenKind::Question,
            TokenKind::Colon,
            TokenKind::Less,
            TokenKind::Greater,
            TokenKind::Equal,
            TokenKind::Not,
            TokenKind::Percent,
            TokenKind::Eof,
        ]
    );
    assert_eq!(
        kinds_of("a => b"),
        vec![
            TokenKind::Identifier("a".to_owned()),
            TokenKind::Arrow,
            TokenKind::Identifier("b".to_owned()),
            TokenKind::Eof,
        ]
    );
}

#[test]
fn differential_numbers_corners_match_egui() {
    // Leading-dot numbers, signed exponents, and egui's char-greedy
    // number/identifier split (`123abc` lexes as Number then Identifier).
    assert_eq!(
        kinds_of(".5 1.5e-3 2E+4"),
        vec![
            TokenKind::Number(0.5),
            TokenKind::Number(1.5e-3),
            TokenKind::Number(2e4),
            TokenKind::Eof,
        ]
    );
    assert_eq!(
        kinds_of("123abc"),
        vec![
            TokenKind::Number(123.0),
            TokenKind::Identifier("abc".to_owned()),
            TokenKind::Eof,
        ]
    );
}

#[test]
fn differential_string_escapes_match_egui() {
    assert_eq!(
        kinds_of("\"a\\n b\\t\\\\\\\"\""),
        vec![TokenKind::String("a\n b\t\\\"".to_owned()), TokenKind::Eof,]
    );
}

#[test]
fn differential_unknown_chars_skip_silently() {
    // egui skips unknown characters (`@`, emoji, stray backticks) while
    // still advancing columns past them.
    let tokens = lex("a @ b");
    assert_eq!(tokens.len(), 3); // Ident(a), Ident(b), Eof
    assert_eq!(tokens[0].kind, TokenKind::Identifier("a".to_owned()));
    assert_eq!((tokens[0].line, tokens[0].column), (1, 1));
    assert_eq!(tokens[1].kind, TokenKind::Identifier("b".to_owned()));
    assert_eq!((tokens[1].line, tokens[1].column), (1, 5));
}

#[test]
fn differential_positions_match_egui() {
    let tokens = lex("ab\n  cd");
    assert_eq!((tokens[0].line, tokens[0].column), (1, 1));
    assert_eq!(tokens[1].kind, TokenKind::Newline);
    assert_eq!((tokens[2].line, tokens[2].column), (2, 3));
    assert_eq!(tokens[3].kind, TokenKind::Eof);
    assert_eq!((tokens[3].line, tokens[3].column), (2, 5));
}

// ---- v6 surface (no egui counterpart; must not disturb the overlap) ----

#[test]
fn v6_reassignment_and_keywords() {
    assert_eq!(
        kinds_of("x := 1"),
        vec![
            TokenKind::Identifier("x".to_owned()),
            TokenKind::ColonEqual,
            TokenKind::Number(1.0),
            TokenKind::Eof,
        ]
    );
    assert_eq!(
        kinds_of("import export enum type method const simple switch"),
        vec![
            TokenKind::Import,
            TokenKind::Export,
            TokenKind::Enum,
            TokenKind::Type,
            TokenKind::Method,
            TokenKind::Const,
            TokenKind::Simple,
            TokenKind::Switch,
            TokenKind::Eof,
        ]
    );
}

#[test]
fn v6_as_stays_identifier() {
    // `as` is a contextual import marker, not a keyword: the later parser
    // matches it as `Identifier("as")`.
    assert_eq!(
        kinds_of("import a/b/1 as UTIL"),
        vec![
            TokenKind::Import,
            TokenKind::Identifier("a".to_owned()),
            TokenKind::Slash,
            TokenKind::Identifier("b".to_owned()),
            TokenKind::Slash,
            TokenKind::Number(1.0),
            TokenKind::Identifier("as".to_owned()),
            TokenKind::Identifier("UTIL".to_owned()),
            TokenKind::Eof,
        ]
    );
}

#[test]
fn v6_dotted_names_lex_whole() {
    assert_eq!(
        kinds_of("UTIL.Smoothings.WMA ta.sma color.rgb"),
        vec![
            TokenKind::Identifier("UTIL.Smoothings.WMA".to_owned()),
            TokenKind::Identifier("ta.sma".to_owned()),
            TokenKind::Identifier("color.rgb".to_owned()),
            TokenKind::Eof,
        ]
    );
}

#[test]
fn v6_annotations_lex_as_trivia() {
    assert_eq!(
        kinds_of("//@version=6"),
        vec![TokenKind::Comment("@version=6".to_owned()), TokenKind::Eof,]
    );
    assert_eq!(
        kinds_of("// @function foo"),
        vec![
            TokenKind::Comment("@function foo".to_owned()),
            TokenKind::Eof,
        ]
    );
}

#[test]
fn v6_emoji_strings_and_comments_survive() {
    assert_eq!(
        kinds_of("'👁️ First MA'"),
        vec![TokenKind::String("👁️ First MA".to_owned()), TokenKind::Eof,]
    );
    assert_eq!(
        kinds_of("a := 1 // 2π constant"),
        vec![
            TokenKind::Identifier("a".to_owned()),
            TokenKind::ColonEqual,
            TokenKind::Number(1.0),
            TokenKind::Comment("2π constant".to_owned()),
            TokenKind::Eof,
        ]
    );
}

#[test]
fn v6_bare_default_arm_lexes() {
    // `switch` default arms are a bare `=>` with no scrutinee expression.
    assert_eq!(
        kinds_of("switch x\n 1 => a\n => b"),
        vec![
            TokenKind::Switch,
            TokenKind::Identifier("x".to_owned()),
            TokenKind::Newline,
            TokenKind::Number(1.0),
            TokenKind::Arrow,
            TokenKind::Identifier("a".to_owned()),
            TokenKind::Newline,
            TokenKind::Arrow,
            TokenKind::Identifier("b".to_owned()),
            TokenKind::Eof,
        ]
    );
}

// ---- Golden snapshots for the three target files ----

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("golden")
        .join(name)
}

fn snapshot_of(tokens: &[PineToken]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for token in tokens {
        let _ = writeln!(out, "{}:{} {}", token.line, token.column, token.kind);
    }
    out
}

fn assert_golden(source_file: &str, golden_file: &str) {
    let text = std::fs::read_to_string(data_path(source_file)).expect("target readable");
    let tokens = lex(&text);
    assert!(!tokens.is_empty(), "no tokens for {source_file}");
    assert_eq!(
        tokens.last().expect("nonempty").kind,
        TokenKind::Eof,
        "stream always terminates for {source_file}"
    );
    let path = golden_path(golden_file);
    if std::env::var("UPDATE_PINE_LEXER_GOLDENS").is_ok() {
        std::fs::write(&path, snapshot_of(&tokens)).expect("golden writable");
    }
    let frozen = std::fs::read_to_string(&path)
        .expect("golden frozen; rerun with UPDATE_PINE_LEXER_GOLDENS=1");
    assert_eq!(snapshot_of(&tokens), frozen, "token drift in {source_file}");
}

#[test]
fn golden_lib_util_tokens() {
    assert_golden("libraries/_libUtil.pine", "lex_libUtil.txt");
}

#[test]
fn golden_lib_mas_tokens() {
    assert_golden("libraries/_libMAs.pine", "lex_libMAs.txt");
}

#[test]
fn golden_fast_mas_tokens() {
    assert_golden("indicators/_FastMAs.pine", "lex_FastMAs.txt");
}

#[test]
fn target_streams_cover_v6_surface() {
    // Coarse probes over the real files: every v6 token the roadmap
    // requires appears at least once across the triple.
    let mut all = Vec::new();
    for file in [
        "libraries/_libUtil.pine",
        "libraries/_libMAs.pine",
        "indicators/_FastMAs.pine",
    ] {
        let text = std::fs::read_to_string(data_path(file)).expect("target readable");
        all.extend(lex(&text));
    }
    let has = |kind: &TokenKind| all.iter().any(|token| &token.kind == kind);
    for kind in [
        TokenKind::Import,
        TokenKind::Export,
        TokenKind::Enum,
        TokenKind::Type,
        TokenKind::Method,
        TokenKind::Const,
        TokenKind::Simple,
        TokenKind::Switch,
        TokenKind::ColonEqual,
        TokenKind::Arrow,
        TokenKind::Var,
        TokenKind::Question,
    ] {
        assert!(has(&kind), "v6 surface token missing from triple: {kind}");
    }
    assert!(
        all.iter()
            .any(|token| token.kind == TokenKind::Identifier("UTIL.Smoothings.WMA".to_owned())),
        "dotted enum member missing from triple"
    );
}
