use common_core::io::*;
use std::fs;
use std::path::Path;


use tempfile::TempDir;

#[test]
fn read_file_alloc_returns_none_for_nonexistent() {
        assert!(read_file_alloc("/nonexistent/path/file.txt").is_none());
}

#[test]
fn read_file_alloc_reads_content() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "hello").unwrap();
        assert_eq!(
            read_file_alloc(&path.to_string_lossy()),
            Some("hello".into())
        );
}

#[test]
fn make_path_absolute_is_pure_resolution() {
        // `make_path_absolute` resolves without creating anything; directory
        // creation is the caller's job via `ensure_dir`.
        let dir = TempDir::new().unwrap();
        let rel = "a/b/c";
        let target = format!("{}/{}", dir.path().to_string_lossy(), rel);
        let abs = make_path_absolute(&target).unwrap();
        assert_eq!(abs, target);
        assert!(!Path::new(&abs).exists());
        ensure_dir(&abs).unwrap();
        assert!(Path::new(&abs).exists());
}

#[test]
fn make_path_absolute_idempotent() {
        let dir = TempDir::new().unwrap();
        let abs = dir.path().join("test").to_string_lossy().to_string();
        let result = make_path_absolute(&abs).unwrap();
        assert_eq!(result, abs);
}

#[test]
fn resolve_path_absolute_unchanged() {
        let result = resolve_path("/base/dir", "/other/path");
        assert_eq!(result, "/other/path");
}

#[test]
fn resolve_path_dot_returns_base() {
        let result = resolve_path("/base/dir/file.txt", ".");
        assert_eq!(result, "/base/dir");
}

#[test]
fn resolve_path_joins_relative() {
        let result = resolve_path("/base/dir/file.txt", "sub/file.rs");
        assert_eq!(result, "/base/dir/sub/file.rs");
}

#[test]
fn resolve_path_relative_base() {
        let result = resolve_path("relative/base/file.txt", "sub/file.rs");
        assert!(result.ends_with("sub/file.rs"));
}

#[test]
fn strip_path_prefix_basic() {
        assert_eq!(strip_path_prefix("/a/b/c", "/a/b"), "c");
        assert_eq!(strip_path_prefix("/a/b/c", "/x"), "/a/b/c");
        assert_eq!(strip_path_prefix("/a/b/c", "/a/b/c"), "");
}

#[test]
fn read_file_alloc_err_missing() {
        let result = read_file_alloc_err("/nonexistent/path/file.txt");
        assert!(result.is_err());
}

#[test]
fn mtime_returns_some_for_existing_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "hello").unwrap();
        assert!(mtime(&path).is_some());
}

#[test]
fn mtime_returns_none_for_nonexistent() {
        assert!(mtime(Path::new("/nonexistent/file.txt")).is_none());
}

#[test]
fn write_atomic_creates_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.txt");
        write_atomic(&path, b"hello").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
}

#[test]
fn write_atomic_overwrites_existing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "old").unwrap();
        write_atomic(&path, b"new").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
}

#[test]
fn read_to_string_err_reads_content() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "hello").unwrap();
        assert_eq!(read_to_string_err(&path).unwrap(), "hello");
}

#[test]
fn read_to_string_err_missing_file() {
        assert!(read_to_string_err(Path::new("/nonexistent/file.txt")).is_err());
}

#[test]
fn ensure_dir_creates_nested() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("a/b/c");
        ensure_dir(&nested).unwrap();
        assert!(nested.is_dir());
}

#[test]
fn ensure_dir_idempotent() {
        let dir = TempDir::new().unwrap();
        ensure_dir(dir.path()).unwrap();
        ensure_dir(dir.path()).unwrap();
}

#[test]
fn write_atomic_same_stem_siblings_do_not_share_tmp() {
    // Regression: the old `with_extension("tmp")` mapped both `a.json`
    // and `a.toml` to one tmp file (`a.tmp`), corrupting concurrent
    // writes. Sibling tmp files must be unique per target.
    use std::sync::Barrier;
    let dir = TempDir::new().unwrap();
    let barrier = std::sync::Arc::new(Barrier::new(2));
    for round in 0..20 {
        let (ja, ta) = (dir.path().join("a.json"), dir.path().join("a.toml"));
        let (pa, pb) = (format!("{{\"round\":{round},\"f\":\"json\"}}"), format!("<x r=\"{round}\"/>"));
        let (ra, rb) = (pa.clone(), pb.clone());
        let (jaa, taa) = (ja.clone(), ta.clone());
        let b = std::sync::Arc::clone(&barrier);
        let ha = std::thread::spawn(move || {
            b.wait();
            write_atomic(&jaa, ra.as_bytes()).unwrap();
        });
        let b = std::sync::Arc::clone(&barrier);
        let hb = std::thread::spawn(move || {
            b.wait();
            write_atomic(&taa, rb.as_bytes()).unwrap();
        });
        ha.join().unwrap();
        hb.join().unwrap();
        assert_eq!(std::fs::read_to_string(&ja).unwrap(), pa);
        assert_eq!(std::fs::read_to_string(&ta).unwrap(), pb);
    }
}

#[test]
fn write_atomic_concurrent_writers_are_all_or_nothing() {
    use std::sync::Barrier;
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("shared.json");
    std::fs::write(&path, b"init").unwrap();
    let payloads: Vec<Vec<u8>> = (0..8).map(|i| format!("payload-{i:04}-{}", "x".repeat(2000)).into_bytes()).collect();
    let barrier = std::sync::Arc::new(Barrier::new(8));
    let handles: Vec<_> = payloads
        .iter()
        .map(|p| {
            let b = std::sync::Arc::clone(&barrier);
            let path = path.clone();
            let p = p.clone();
            std::thread::spawn(move || {
                b.wait();
                for _ in 0..25 {
                    write_atomic(&path, &p).unwrap();
                }
            })
        })
        .collect();
    // While writers run, every observed read is one full payload (or init).
    for _ in 0..50 {
        let seen = std::fs::read(&path).unwrap();
        assert!(
            seen == b"init" || payloads.contains(&seen),
            "torn read: {} bytes",
            seen.len()
        );
    }
    for h in handles {
        h.join().unwrap();
    }
    assert!(payloads.contains(&std::fs::read(&path).unwrap()));
    // No tmp debris visible under the real name or alongside it.
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        assert_eq!(name, "shared.json", "debris left behind: {name}");
    }
}

#[test]
fn write_atomic_stale_tmp_never_read_as_real() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("real.json");
    // Simulate crash debris from an earlier writer under our tmp pattern.
    std::fs::write(dir.path().join("real.json.12345.1.tmp"), b"stale").unwrap();
    write_atomic(&path, b"{\"ok\":true}").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"ok\":true}");
}

#[test]
fn make_path_absolute_does_not_create_dirs() {
    let dir = TempDir::new().unwrap();
    let missing = dir.path().join("no/such/dirs/here");
    assert!(!missing.exists());
    let abs = make_path_absolute(&missing.to_string_lossy()).unwrap();
    assert_eq!(abs, missing.to_string_lossy());
    assert!(!missing.exists(), "resolution must not create directories");
}
