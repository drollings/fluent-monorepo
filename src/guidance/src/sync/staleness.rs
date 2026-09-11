use std::path::Path;

pub fn is_stale(json_path: &Path, source_path: &Path) -> bool {
    let Some(json_mtime) = common_core::io::mtime(json_path) else {
        return true;
    };
    let Some(source_mtime) = common_core::io::mtime(source_path) else {
        return false;
    };

    match source_mtime.duration_since(json_mtime) {
        Ok(dur) => dur.as_secs() > 1 || (dur.as_secs() == 1 && dur.subsec_nanos() > 0),
        Err(_) => false,
    }
}

pub fn should_generate(json_path: &Path, source_path: &Path) -> bool {
    if !json_path.exists() {
        return true;
    }
    is_stale(json_path, source_path)
}

/// Resolution-window probe: true when the two mtimes alone cannot decide
/// freshness — the source is at-or-newer than the sidecar but within the
/// 1-second timestamp-resolution window.
///
/// Axis statement: the window measures *producer self-doubt* (filesystem
/// timestamp granularity), never task value. Inside it the clock is
/// evidence-free, so the content hash decides instead. The threshold
/// gates which evidence to consult, never the conclusion.
#[must_use]
pub fn in_ambiguity_window(json_path: &Path, source_path: &Path) -> bool {
    let (Some(json_mtime), Some(source_mtime)) = (
        common_core::io::mtime(json_path),
        common_core::io::mtime(source_path),
    ) else {
        return false;
    };
    match source_mtime.duration_since(json_mtime) {
        Ok(dur) => dur.as_secs() == 0 || (dur.as_secs() == 1 && dur.subsec_nanos() == 0),
        Err(_) => false,
    }
}

/// Content tiebreak for the ambiguity window: the fresh source sha256
/// against the stored one (the `compute_diff` precedent — same hasher,
/// `common_core::hash::sha256_hex` — never a second hasher). `None`
/// stored means no evidence: fail open (not fresh — regenerate), never
/// serve ambiguous content as fresh.
///
/// Axis statement: the hash measures *task correctness* (bytes changed),
/// the complement of the window's self-doubt. It gates a regen decision
/// only — never cached, never persisted, never served as data.
#[must_use]
pub fn hash_decides_fresh(current_hash: &str, stored_hash: Option<&str>) -> bool {
    stored_hash.is_some_and(|want| want == current_hash)
}

/// Staleness with a content-aware clock gate: outside the ambiguity
/// window the mtimes rule exactly as [`should_generate`] (`hash_now`
/// never runs — the slow path pays zero hash cost); inside the window
/// the content hash decides. A hash-read failure fails open toward
/// regeneration.
pub fn should_generate_with_stored(
    json_path: &Path,
    source_path: &Path,
    stored_hash: Option<&str>,
    hash_now: impl FnOnce() -> Option<String>,
) -> bool {
    if is_stale(json_path, source_path) {
        return true;
    }
    if !in_ambiguity_window(json_path, source_path) {
        return false;
    }
    let Some(current) = hash_now() else {
        return true;
    };
    !hash_decides_fresh(&current, stored_hash)
}

pub fn match_hash_from_signature(signature: &str) -> String {
    common_core::hash::blake3_hex(signature.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluent_wvr_testutil::tempdir;
    use std::fs;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_json_absent_is_stale() {
        let dir = tempdir();
        let json = dir.path().join("nonexistent.json");
        let source = dir.path().join("source.zig");
        fs::write(&source, "content").expect("write source");
        assert!(is_stale(&json, &source));
    }

    #[test]
    fn test_stale_when_json_older_by_2s() {
        let dir = tempdir();
        let json = dir.path().join("test.json");
        let source = dir.path().join("test.zig");
        fs::write(&json, "old").expect("write json");
        thread::sleep(Duration::from_millis(1500));
        fs::write(&source, "newer").expect("write source");
        assert!(is_stale(&json, &source));
    }

    #[test]
    fn test_not_stale_when_json_newer() {
        let dir = tempdir();
        let json = dir.path().join("test.json");
        let source = dir.path().join("test.zig");
        fs::write(&source, "old").expect("write source");
        thread::sleep(Duration::from_millis(100));
        fs::write(&json, "newer").expect("write json");
        assert!(!is_stale(&json, &source));
    }

    #[test]
    fn test_not_stale_when_same_mtime() {
        let dir = tempdir();
        let json = dir.path().join("test.json");
        let source = dir.path().join("test.zig");
        let content = b"same time";
        fs::write(&json, content).expect("write json");
        fs::write(&source, content).expect("write source");
        assert!(!is_stale(&json, &source));
    }

    #[test]
    fn test_should_generate_no_json() {
        let dir = tempdir();
        let json = dir.path().join("missing.json");
        let source = dir.path().join("source.zig");
        fs::write(&source, "x").expect("write source");
        assert!(should_generate(&json, &source));
    }

    #[test]
    fn test_match_hash_is_consistent() {
        let h1 = match_hash_from_signature("fn hello(name: []const u8)");
        let h2 = match_hash_from_signature("fn hello(name: []const u8)");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_match_hash_differs_for_different_sigs() {
        let h1 = match_hash_from_signature("fn hello() void");
        let h2 = match_hash_from_signature("fn world() void");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_hash_tiebreak_equal_bytes_are_fresh() {
        assert!(hash_decides_fresh("abc123", Some("abc123")));
    }

    #[test]
    fn test_hash_tiebreak_changed_bytes_are_stale() {
        assert!(!hash_decides_fresh("def456", Some("abc123")));
    }

    #[test]
    fn test_hash_tiebreak_missing_evidence_fails_open() {
        assert!(!hash_decides_fresh("abc123", None));
    }

    #[test]
    fn test_window_closed_when_source_older() {
        let dir = tempdir();
        let json = dir.path().join("test.json");
        let source = dir.path().join("test.zig");
        fs::write(&source, "old").expect("write source");
        thread::sleep(Duration::from_millis(100));
        fs::write(&json, "newer").expect("write json");
        assert!(!in_ambiguity_window(&json, &source));
    }

    #[test]
    fn test_window_closed_when_newer_by_more_than_1s() {
        let dir = tempdir();
        let json = dir.path().join("test.json");
        let source = dir.path().join("test.zig");
        fs::write(&json, "old").expect("write json");
        thread::sleep(Duration::from_millis(1100));
        fs::write(&source, "newer").expect("write source");
        assert!(!in_ambiguity_window(&json, &source));
        assert!(is_stale(&json, &source));
    }

    #[test]
    fn test_window_open_for_back_to_back_writes() {
        let dir = tempdir();
        let json = dir.path().join("test.json");
        let source = dir.path().join("test.zig");
        fs::write(&json, "old").expect("write json");
        fs::write(&source, "newer").expect("write source");
        assert!(!is_stale(&json, &source));
        assert!(in_ambiguity_window(&json, &source));
    }

    #[test]
    fn test_gated_decision_matches_mtime_outside_window() {
        use std::cell::Cell;
        let calls = Cell::new(0usize);
        let hash_now = || {
            calls.set(calls.get() + 1);
            Some("h".to_string())
        };
        // Stale by the clock: fires without consulting the hash.
        let dir = tempdir();
        let json = dir.path().join("test.json");
        let source = dir.path().join("test.zig");
        fs::write(&json, "old").expect("write json");
        thread::sleep(Duration::from_millis(1100));
        fs::write(&source, "newer").expect("write source");
        assert!(should_generate_with_stored(&json, &source, Some("h"), hash_now));
        // Fresh by the clock (source older): skips without hashing.
        let dir = tempdir();
        let json = dir.path().join("old.json");
        let source = dir.path().join("old.zig");
        fs::write(&source, "old").expect("write source");
        thread::sleep(Duration::from_millis(100));
        fs::write(&json, "newer").expect("write json");
        assert!(!should_generate_with_stored(&json, &source, Some("h"), hash_now));
        assert_eq!(calls.get(), 0, "hash must not fire outside the window");
    }

    #[test]
    fn test_gated_decision_defers_to_hash_inside_window() {
        use std::cell::Cell;
        let calls = Cell::new(0usize);
        let dir = tempdir();
        let json = dir.path().join("test.json");
        let source = dir.path().join("test.zig");
        fs::write(&json, "old").expect("write json");
        let body = b"changed bytes";
        fs::write(&source, body).expect("write source");
        let current = common_core::hash::sha256_hex(body);
        // Identical bytes inside the window: skip (precision).
        assert!(!should_generate_with_stored(&json, &source, Some(&current), || {
            calls.set(calls.get() + 1);
            Some(current.clone())
        }));
        // Changed bytes inside the window: fire (recall).
        assert!(should_generate_with_stored(&json, &source, Some("other"), || {
            calls.set(calls.get() + 1);
            Some(current.clone())
        }));
        // Unreadable source inside the window: fail open toward work.
        assert!(should_generate_with_stored(&json, &source, Some(&current), || {
            calls.set(calls.get() + 1);
            None
        }));
        assert_eq!(calls.get(), 3, "hash runs exactly once per inside-window decision");
    }
}
