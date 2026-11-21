use common_core::config::*;
use std::path::Path;

use std::fs;
use tempfile::TempDir;

#[test]
fn load_json_or_default_returns_default_on_missing() {
    let result = load_json_or_default::<TestConfig>(Path::new("/nonexistent/config.json"));
    assert_eq!(result.name, "default");
    assert_eq!(result.count, 0);
}

#[test]
fn load_json_or_default_loads_valid_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.json");
    fs::write(&path, r#"{"name":"loaded","count":42}"#).unwrap();

    let result = load_json_or_default::<TestConfig>(&path);
    assert_eq!(result.name, "loaded");
    assert_eq!(result.count, 42);
}

#[test]
fn load_json_or_default_returns_default_on_invalid_json() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.json");
    fs::write(&path, "not json").unwrap();

    let result = load_json_or_default::<TestConfig>(&path);
    assert_eq!(result.name, "default");
}

#[test]
fn load_json_or_default_warns_on_malformed_existing_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.json");
    fs::write(&path, r#"{"name": 42}"#).unwrap(); // wrong type for "name"

    let result = load_json_or_default::<TestConfig>(&path);
    assert_eq!(result.name, "default"); // falls back
                                        // Warning should have been printed to stderr; structural test only.
}

#[test]
fn load_json_strict_errors_on_missing() {
    let result = load_json::<TestConfig>(Path::new("/nonexistent/config.json"));
    assert!(result.is_err());
}

#[test]
fn load_json_strict_loads_valid_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.json");
    fs::write(&path, r#"{"name":"strict","count":7}"#).unwrap();

    let result = load_json::<TestConfig>(&path).unwrap();
    assert_eq!(result.name, "strict");
    assert_eq!(result.count, 7);
}

#[test]
fn load_json_strict_errors_on_invalid_json() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.json");
    fs::write(&path, "not json").unwrap();

    let result = load_json::<TestConfig>(&path);
    assert!(result.is_err());
}

#[derive(Debug, serde::Deserialize, serde::Serialize, PartialEq)]
struct TestConfig {
    #[serde(default = "default_name")]
    name: String,
    #[serde(default)]
    count: u32,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            name: default_name(),
            count: 0,
        }
    }
}

fn default_name() -> String {
    "default".to_string()
}

#[test]
fn find_hierarchical_prefers_first_existing() {
    let project = TempDir::new().unwrap();
    let user = TempDir::new().unwrap();
    let project_file = project.path().join("guidance-config.json");
    let user_file = user.path().join("guidance-config.json");
    fs::write(&project_file, "{}").unwrap();
    fs::write(&user_file, "{}").unwrap();
    assert_eq!(
        find_hierarchical(&[project_file.clone(), user_file.clone()]),
        Some(project_file)
    );
}

#[test]
fn find_hierarchical_falls_through_to_user() {
    let project = TempDir::new().unwrap();
    let user = TempDir::new().unwrap();
    let user_file = user.path().join("guidance-config.json");
    fs::write(&user_file, "{}").unwrap();
    assert_eq!(
        find_hierarchical(&[
            project.path().join("guidance-config.json"),
            user_file.clone()
        ]),
        Some(user_file)
    );
}

#[test]
fn find_hierarchical_none_when_absent() {
    let dir = TempDir::new().unwrap();
    assert_eq!(
        find_hierarchical(&[dir.path().join("a.json"), dir.path().join("b.json")]),
        None
    );
    let empty: Vec<std::path::PathBuf> = Vec::new();
    assert_eq!(find_hierarchical(&empty), None);
}

#[test]
fn find_hierarchical_skips_directories() {
    let dir = TempDir::new().unwrap();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    let file = dir.path().join("f.json");
    fs::write(&file, "{}").unwrap();
    assert_eq!(find_hierarchical(&[sub, file.clone()]), Some(file));
}

#[test]
fn home_or_dot_never_fails() {
    let home = home_or_dot();
    assert!(!home.as_os_str().is_empty());
}

#[test]
fn find_ancestor_file_at_depth_and_cap_edge() {
    let dir = TempDir::new().unwrap();
    let probe = "split-m1-probe.toml";
    fs::write(dir.path().join(probe), "x").unwrap();
    // Direct child: found with any nonzero cap.
    let child = dir.path().join("input.rs");
    fs::write(&child, "x").unwrap();
    assert_eq!(
        find_ancestor_file(&child, probe, 8),
        Some(dir.path().join(probe))
    );
    // Seven levels below the probe: the eighth check still lands on it.
    let mut deep = dir.path().to_path_buf();
    for part in ["d1", "d2", "d3", "d4", "d5", "d6", "d7"] {
        deep = deep.join(part);
    }
    fs::create_dir_all(&deep).unwrap();
    let edge = deep.join("input.rs");
    fs::write(&edge, "x").unwrap();
    assert_eq!(
        find_ancestor_file(&edge, probe, 8),
        Some(dir.path().join(probe))
    );
    // Eight levels below: the probe falls off the 8-check cap.
    let past = deep.join("d8").join("input.rs");
    fs::create_dir_all(past.parent().unwrap()).unwrap();
    fs::write(&past, "x").unwrap();
    assert_eq!(find_ancestor_file(&past, probe, 8), None);
}

#[test]
fn find_ancestor_file_termination() {
    let dir = TempDir::new().unwrap();
    // Nonexistent starts still resolve upward: only probes matter.
    let missing = dir.path().join("nope").join("input.rs");
    assert_eq!(
        find_ancestor_file(&missing, "split-m1-absent.toml", 8),
        None
    );
    // Empty tree: terminates past the root with None, never hangs.
    assert_eq!(
        find_ancestor_file(&missing, "split-m1-absent.toml", 100),
        None
    );
    // Zero cap never searches.
    let probe = "split-m1-probe.toml";
    fs::write(dir.path().join(probe), "x").unwrap();
    let child = dir.path().join("input.rs");
    fs::write(&child, "x").unwrap();
    assert_eq!(find_ancestor_file(&child, probe, 0), None);
}
