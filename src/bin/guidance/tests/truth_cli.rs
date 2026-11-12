//! Truth-model wiring goldens: generation mismatch rebuilds from
//! sidecars, and renames adopt sidecars by hash instead of
//! reprocessing. Written with the wiring (red: no marker, no adopt).

#[path = "common.rs"]
#[allow(dead_code)]
mod common;

use common::{run, stdout, workspace};

fn run_args(root: &str, json_dir: &str, db: &str) -> Vec<String> {
    vec![
        "run".to_string(),
        "--workspace".to_string(),
        root.to_string(),
        "--json-dir".to_string(),
        json_dir.to_string(),
        "--db".to_string(),
        db.to_string(),
    ]
}

#[test]
fn generation_mismatch_rebuilds_from_sidecars() {
    let dir = workspace();
    let root = dir.path().to_str().unwrap().to_string();
    let json_dir_str = dir.path().join(".guidance").to_str().unwrap().to_string();
    let db_str = dir
        .path()
        .join(".guidance.db")
        .to_str()
        .unwrap()
        .to_string();
    let args = run_args(&root, &json_dir_str, &db_str);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run(dir.path(), &arg_refs);
    assert!(out.status.success(), "first run: {out:?}\n{}", stdout(&out));
    // Corrupt the marker the run just wrote: the next run must refuse
    // the mixed pair and rebuild instead of serving it.
    let marker = dir.path().join("index.generation");
    assert!(marker.is_file(), "run writes the generation marker");
    std::fs::write(&marker, "stale-generation").expect("corrupt");
    let out = run(dir.path(), &arg_refs);
    let text = stdout(&out);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "rebuild converges: {out:?}\n{text}\n{err}"
    );
    let marker_text = std::fs::read_to_string(&marker).expect("marker");
    assert_ne!(marker_text, "stale-generation", "marker rewritten");
    assert!(err.contains("rebuilding"), "rebuild named: {text}\n{err}");
}

#[test]
fn rename_adopts_sidecar_by_hash() {
    let dir = workspace();
    std::fs::write(
        dir.path().join("a.rs"),
        "pub fn moved() -> u64 {\n    7\n}\n",
    )
    .expect("write");
    let root = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance");
    let json_dir_str = json_dir.to_str().unwrap().to_string();
    let sync = |extra: &[&str]| {
        let mut args = vec![
            "sync".to_string(),
            "--workspace".to_string(),
            root.clone(),
            "--json-dir".to_string(),
            json_dir_str.clone(),
        ];
        args.extend(extra.iter().map(|s| s.to_string()));
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run(dir.path(), &arg_refs)
    };
    let out = sync(&[]);
    assert!(out.status.success(), "sync: {out:?}");
    assert!(json_dir.join("src").join("a.rs.json").is_file());
    std::fs::rename(dir.path().join("a.rs"), dir.path().join("c.rs")).expect("rename");
    let out = sync(&[]);
    let text = stdout(&out);
    assert!(out.status.success(), "rename sync: {out:?}\n{text}");
    assert!(
        json_dir.join("src").join("c.rs.json").is_file(),
        "renamed sidecar adopted, not reprocessed"
    );
    assert!(
        !json_dir.join("src").join("a.rs.json").exists(),
        "orphan sidecar moved, not duplicated"
    );
}

#[test]
fn denied_lint_holds_stuck_with_truthful_stale() {
    // A behavior-risky diagnostic (manual_* family) escalates instead
    // of applying; the sidecar is backdated below the source so the
    // stale verdict stays truthful while the report explains why.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"stuckfix\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("manifest");
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).expect("srcdir");
    std::fs::write(
        src.join("main.rs"),
        "fn convert(x: Option<u32>) -> Option<u32> {\n    match x {\n        Some(v) => Some(v + 1),\n        _ => None,\n    }\n}\n\nfn main() {\n    println!(\"{}\", convert(Some(1)).unwrap_or(0));\n}\n",
    )
    .expect("write");
    let root = dir.path().to_str().unwrap().to_string();
    let json_dir = dir.path().join(".guidance");
    let json_dir_str = json_dir.to_str().unwrap().to_string();
    let db_str = dir
        .path()
        .join(".guidance.db")
        .to_str()
        .unwrap()
        .to_string();
    let args = run_args(&root, &json_dir_str, &db_str);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run(dir.path(), &arg_refs);
    let text = stdout(&out);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(
        out.status.code(),
        Some(2),
        "stuck exits 2: {out:?}\n{text}\n{err}"
    );
    assert!(text.contains("denied behavior-risky"), "hold named: {text}");
    let sidecar = json_dir.join("src").join("src").join("main.rs.json");
    let sidecar = if sidecar.is_file() {
        sidecar
    } else {
        json_dir.join("src").join("main.rs.json")
    };
    assert!(sidecar.is_file(), "sidecar exists");
    let source = src.join("main.rs");
    let side_mtime = std::fs::metadata(&sidecar)
        .expect("sidecar meta")
        .modified()
        .expect("mtime");
    let src_mtime = std::fs::metadata(&source)
        .expect("source meta")
        .modified()
        .expect("mtime");
    assert!(side_mtime < src_mtime, "stuck sidecar reads stale");
}
