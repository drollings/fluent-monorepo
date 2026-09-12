use super::*;

// Ported from zvec `utils/glob.ts` + `utils/file-selection.ts` behavior,
// including the verbatim `search.test.mjs:302` ordered-glob case.

#[test]
fn normalize_collapses_separators_and_dots() {
    assert_eq!(normalize_path_pattern("src\\\\a.ts"), "src/a.ts");
    assert_eq!(normalize_path_pattern("src//a.ts"), "src/a.ts");
    assert_eq!(normalize_path_pattern("./src/a.ts"), "src/a.ts");
}

#[test]
fn basename_patterns_match_nested_paths() {
    assert!(ripgrep_glob_matches("*.ts", "src/a.ts"));
    assert!(ripgrep_glob_matches("a.ts", "src/a.ts"));
    assert!(!ripgrep_glob_matches("*.ts", "src/a.js"));
    assert!(!ripgrep_glob_matches("*.ts", "src/a.TS"));
    assert!(ripgrep_glob_matches_case_insensitive("*.ts", "src/a.TS"));
}

#[test]
fn doublestar_spans_directories() {
    assert!(ripgrep_glob_matches("src/**", "src/a.ts"));
    assert!(ripgrep_glob_matches("src/**", "src/nested/deep/a.ts"));
    assert!(ripgrep_glob_matches("src/**", "src"));
    assert!(!ripgrep_glob_matches("src/**", "docs/c.ts"));
    assert!(ripgrep_glob_matches("**/*.test.ts", "src/b.test.ts"));
    assert!(!ripgrep_glob_matches("**/*.test.ts", "src/a.ts"));
}

#[test]
fn ordered_globs_honor_later_overrides() {
    // Verbatim port of search.test.mjs:302.
    let globs = vec!["!*.ts".to_string(), "a.ts".to_string()];
    assert!(matches_ordered_globs("src/a.ts", &globs, &[]));
    assert!(!matches_ordered_globs("src/b.test.ts", &globs, &[]));
    assert!(!matches_ordered_globs("docs/c.ts", &globs, &[]));
}

#[test]
fn ordered_globs_include_src_exclude_tests() {
    let globs = vec!["src/**".to_string(), "!**/*.test.ts".to_string()];
    assert!(matches_ordered_globs("src/a.ts", &globs, &[]));
    assert!(!matches_ordered_globs("src/b.test.ts", &globs, &[]));
    assert!(!matches_ordered_globs("docs/c.ts", &globs, &[]));
}

#[test]
fn char_classes_and_alternations() {
    assert!(ripgrep_glob_matches("file.[ch]", "file.c"));
    assert!(ripgrep_glob_matches("file.[ch]", "file.h"));
    assert!(!ripgrep_glob_matches("file.[ch]", "file.x"));
    assert!(ripgrep_glob_matches("*.{ts,js}", "a.ts"));
    assert!(ripgrep_glob_matches("*.{ts,js}", "a.js"));
    assert!(!ripgrep_glob_matches("*.{ts,js}", "a.py"));
    assert!(ripgrep_glob_matches("file.?", "file.c"));
}

#[test]
fn path_patterns_match_exact_or_prefix() {
    assert!(path_pattern_matches("src", "src/a.ts"));
    assert!(path_pattern_matches("src/a.ts", "src/a.ts"));
    assert!(!path_pattern_matches("src", "src2/a.ts"));
    assert!(!path_pattern_matches("src", "docs/c.ts"));
    assert!(path_pattern_matches("src/**", "src/a.ts"));
}

#[test]
fn file_types_resolve_without_shelling_to_rg() {
    // Static table (hermetic deviation from `rg --type-list` subprocess).
    let types = resolve_file_type_patterns(&["ts".to_string()], &[]).expect("ts");
    assert!(matches_file_selection("src/a.ts", &[], &[], &types));
    assert!(matches_file_selection("src/a.tsx", &[], &[], &types));
    assert!(!matches_file_selection("src/a.py", &[], &[], &types));
    assert!(resolve_file_type_patterns(&["cobol-fortran-xyz".to_string()], &[]).is_err());
    let all = resolve_file_type_patterns(&["all".to_string()], &[]).expect("all");
    assert!(matches_file_selection(
        "anything/at/all.zig",
        &[],
        &[],
        &all
    ));
}

#[test]
fn empty_pattern_never_matches() {
    assert!(!ripgrep_glob_matches("", "src/a.ts"));
    assert!(!ripgrep_glob_matches("   ", "src/a.ts"));
}

// M1.1 characterization: glob normalizers pinned verbatim. These are
// *pattern* cleaners, not lexical path cleaners: they collapse separator
// runs and strip leading `./` but deliberately preserve `.`/`..` segments,
// trailing slashes, and glob metacharacters for the matcher. That
// divergence from `change_set::normalize_path` is pinned here so the M1.5
// migration cannot silently change match behavior.

#[test]
fn m1_glob_normalizer_matrix() {
    // Separator runs collapse; leading `./` strips (repeatedly).
    assert_eq!(normalize_path_pattern("src//a.ts"), "src/a.ts");
    assert_eq!(normalize_path_pattern("src\\\\a.ts"), "src/a.ts");
    assert_eq!(normalize_path_pattern("./src/a.ts"), "src/a.ts");
    assert_eq!(normalize_path_pattern("././src/a.ts"), "src/a.ts");
    // Dot segments in the middle are PRESERVED (not lexically resolved).
    assert_eq!(normalize_path_pattern("a/./b"), "a/./b");
    assert_eq!(normalize_path_pattern("a//b/./c/../d"), "a/b/./c/../d");
    // Trailing slashes are PRESERVED.
    assert_eq!(normalize_path_pattern("src/"), "src/");
    // Absolute patterns pass through untouched.
    assert_eq!(normalize_path_pattern("/abs//x"), "/abs/x");
    // Candidate normalization: separators only, no dot handling.
    assert_eq!(normalize_path_for_match("src\\\\a.ts"), "src/a.ts");
    assert_eq!(normalize_path_for_match("src//a.ts"), "src/a.ts");
    assert_eq!(normalize_path_for_match("./src/a.ts"), "./src/a.ts");
    assert_eq!(normalize_path_for_match("a/./b"), "a/./b");
}

#[test]
fn m1_glob_absolute_detection_matrix() {
    assert!(is_absolute_path_pattern("/x"));
    assert!(is_absolute_path_pattern("/"));
    assert!(is_absolute_path_pattern("C:/x"));
    assert!(is_absolute_path_pattern("c:/x"));
    // Backslash drives and non-alpha drives are NOT absolute here
    // (diverges from `change_set::is_absolute_path` — pinned).
    assert!(!is_absolute_path_pattern("C:\\x"));
    assert!(!is_absolute_path_pattern("1:/x"));
    assert!(!is_absolute_path_pattern("relative/x"));
    assert!(!is_absolute_path_pattern("C:x"));
    assert!(!is_absolute_path_pattern(""));
}
