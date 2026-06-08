use std::fs;
use std::path::{Path, PathBuf};

pub fn make_path_absolute(path: &str) -> std::io::Result<String> {
    let pb = Path::new(path);
    let abs = if pb.is_absolute() {
        pb.to_path_buf()
    } else {
        std::env::current_dir()?.join(pb)
    };
    fs::create_dir_all(&abs)?;
    Ok(abs.to_string_lossy().to_string())
}

pub fn read_file_alloc(path: &str) -> Option<String> {
    fs::read_to_string(path).ok()
}

pub fn read_file_alloc_err(path: &str) -> Result<String, std::io::Error> {
    fs::read_to_string(path)
}

pub fn resolve_path(base: &str, relative: &str) -> String {
    if relative == "." {
        let base_path = Path::new(base);
        return base_path.parent().unwrap_or(base_path).to_string_lossy().to_string();
    }
    let base_path = Path::new(base);
    if base_path.is_absolute() {
        let joined = base_path.parent().unwrap_or(base_path).join(relative);
        return joined.to_string_lossy().to_string();
    }
    let rel_path = Path::new(relative);
    if rel_path.is_absolute() {
        return relative.to_string();
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let joined = cwd.join(base).parent().unwrap_or(&cwd).join(relative);
    joined.to_string_lossy().to_string()
}

pub fn strip_path_prefix<'a>(path: &'a str, prefix: &str) -> &'a str {
    if let Some(stripped) = path.strip_prefix(prefix) {
        stripped.trim_start_matches('/')
    } else {
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(read_file_alloc(&path.to_string_lossy()), Some("hello".into()));
    }

    #[test]
    fn make_path_absolute_creates_nested_dirs() {
        let dir = TempDir::new().unwrap();
        let rel = "a/b/c";
        let abs = make_path_absolute(&format!("{}/{}", dir.path().to_string_lossy(), rel)).unwrap();
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
}
