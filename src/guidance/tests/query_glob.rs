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
