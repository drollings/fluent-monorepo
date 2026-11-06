use common_core::path::{
    collapse_separators, file_name_lexical, is_absolute_lexical, normalize_lexical,
    normalize_std_path, parent_lexical,
};
use std::path::{Path, PathBuf};

#[test]
fn collapse_separators_backslash_and_runs() {
    assert_eq!(collapse_separators("a\\b"), "a/b");
    assert_eq!(collapse_separators("a//b"), "a/b");
    assert_eq!(collapse_separators("a\\\\b"), "a/b");
    assert_eq!(collapse_separators("C:\\a\\b"), "C:/a/b");
    assert_eq!(collapse_separators("a/b"), "a/b");
    assert_eq!(collapse_separators(""), "");
    assert_eq!(collapse_separators("/"), "/");
    // Dot segments pass through untouched (pattern syntax, not paths).
    assert_eq!(collapse_separators("a/./b"), "a/./b");
    assert_eq!(collapse_separators("a/../b"), "a/../b");
}

#[test]
fn normalize_lexical_matrix() {
    assert_eq!(normalize_lexical("a//b/./c/../d"), "a/b/d");
    assert_eq!(normalize_lexical("C:\\a\\b"), "C:/a/b");
    assert_eq!(normalize_lexical("C:/x"), "C:/x");
    assert_eq!(normalize_lexical("/../a"), "/a");
    assert_eq!(normalize_lexical("/.."), "/");
    assert_eq!(normalize_lexical("/a/../../b"), "/b");
    assert_eq!(normalize_lexical(".."), "");
    assert_eq!(normalize_lexical("../a"), "a");
    assert_eq!(normalize_lexical("a/../.."), "");
    assert_eq!(normalize_lexical("/a/b/"), "/a/b");
    assert_eq!(normalize_lexical("a/b/"), "a/b");
    assert_eq!(normalize_lexical("/"), "/");
    assert_eq!(normalize_lexical(""), "");
    assert_eq!(normalize_lexical("."), "");
    assert_eq!(normalize_lexical("./a"), "a");
    assert_eq!(normalize_lexical("/x"), "/x");
    assert_eq!(normalize_lexical("/a//b"), "/a/b");
    assert_eq!(normalize_lexical("/a/./b"), "/a/b");
    assert_eq!(normalize_lexical("a/b"), "a/b");
}

#[test]
fn parent_lexical_matrix() {
    assert_eq!(parent_lexical("/"), None);
    assert_eq!(parent_lexical("/a"), Some("/".to_string()));
    assert_eq!(parent_lexical("/a/b"), Some("/a".to_string()));
    assert_eq!(parent_lexical("/a/b/"), Some("/a".to_string()));
    assert_eq!(parent_lexical("a"), None);
    assert_eq!(parent_lexical("a/b"), Some("a".to_string()));
}

#[test]
fn file_name_lexical_matrix() {
    assert_eq!(file_name_lexical("/a/b.ts"), Some("b.ts"));
    assert_eq!(file_name_lexical("/a/b/"), Some("b"));
    assert_eq!(file_name_lexical("a"), Some("a"));
    assert_eq!(file_name_lexical("/"), Some(""));
    assert_eq!(file_name_lexical(".gitignore"), Some(".gitignore"));
    assert_eq!(file_name_lexical("/x/.gitignore"), Some(".gitignore"));
    assert_eq!(file_name_lexical("C:/a/b"), Some("b"));
}

#[test]
fn is_absolute_lexical_matrix() {
    assert!(is_absolute_lexical("/x"));
    assert!(is_absolute_lexical("/"));
    assert!(is_absolute_lexical("C:/x"));
    assert!(is_absolute_lexical("C:\\x"));
    assert!(!is_absolute_lexical("relative/path.ts"));
    assert!(!is_absolute_lexical(""));
    assert!(!is_absolute_lexical("C:x"));
}

#[test]
fn symlink_hop_cap_is_shared_at_forty() {
    // M12-S2: the single hop cap composed by `orator_core::paths`
    // (fast check) and `orator_tools::sandbox` (authoritative jail).
    assert_eq!(common_core::path::MAX_SYMLINK_HOPS, 40);
}

#[test]
fn normalize_std_path_matrix() {
    let norm = |path: &str| normalize_std_path(Path::new(path));
    assert_eq!(norm(""), Some(PathBuf::new()));
    assert_eq!(norm("a/.."), Some(PathBuf::new()));
    assert_eq!(norm("."), Some(PathBuf::new()));
    assert_eq!(norm("./a/./b"), Some(PathBuf::from("a/b")));
    assert_eq!(norm("a//b"), Some(PathBuf::from("a/b")));
    assert_eq!(norm("a/b/"), Some(PathBuf::from("a/b")));
    assert_eq!(norm("a/../b"), Some(PathBuf::from("b")));
    assert_eq!(norm("/"), Some(PathBuf::from("/")));
    assert_eq!(norm("/a/../b"), Some(PathBuf::from("/b")));
}

#[test]
fn normalize_std_path_fails_closed_where_the_string_helper_does_not() {
    assert_eq!(normalize_std_path(Path::new("../a")), None);
    assert_eq!(normalize_std_path(Path::new("/..")), None);
    assert_eq!(normalize_std_path(Path::new("a/../../x")), None);
    // The existing fail-open string helper is unchanged.
    assert_eq!(normalize_lexical("../a"), "a");
    assert_eq!(normalize_lexical("/../a"), "/a");
}
