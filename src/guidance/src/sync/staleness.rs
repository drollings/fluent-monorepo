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
}
