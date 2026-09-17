use super::*;
use std::fs::File;
use std::io::Write;

/// Write a minimal GGUF header (one string + one uint32 key) so
/// `weights_identity` can read a real arch/quant.
fn write_gguf(path: &Path, arch: &str, file_type: u32) {
    let mut f = File::create(path).unwrap();
    f.write_all(b"GGUF").unwrap();
    f.write_all(&1u32.to_le_bytes()).unwrap();
    f.write_all(&0u64.to_le_bytes()).unwrap(); // tensor_count
    f.write_all(&2u64.to_le_bytes()).unwrap(); // kv_count
    let kv_str = |f: &mut File, key: &str, value: &str| {
        f.write_all(&(key.len() as u64).to_le_bytes()).unwrap();
        f.write_all(key.as_bytes()).unwrap();
        f.write_all(&8u32.to_le_bytes()).unwrap(); // string type
        f.write_all(&(value.len() as u64).to_le_bytes()).unwrap();
        f.write_all(value.as_bytes()).unwrap();
    };
    kv_str(&mut f, "general.architecture", arch);
    f.write_all(&("general.file_type".len() as u64).to_le_bytes())
        .unwrap();
    f.write_all(b"general.file_type").unwrap();
    f.write_all(&4u32.to_le_bytes()).unwrap(); // uint32
    f.write_all(&file_type.to_le_bytes()).unwrap();
}

#[test]
fn weights_identity_reads_arch_quant_and_short_id() {
    let dir = tempfile::tempdir().unwrap();
    let gguf = dir.path().join("model.gguf");
    write_gguf(&gguf, "llama", 15); // 15 => q4_k_m
    let idn = weights_identity(&gguf);
    assert_eq!(idn.arch, "llama");
    assert_eq!(idn.quant, "q4_k_m");
    assert_eq!(idn.short_id.len(), 12, "12-char content short id");
    assert!(idn.short_id.chars().all(|c| c.is_ascii_hexdigit()));
    // Deterministic across calls.
    assert_eq!(weights_identity(&gguf).short_id, idn.short_id);
}

#[test]
fn weights_identity_defaults_for_missing_file() {
    let idn = weights_identity(Path::new("/nonexistent/nope.gguf"));
    assert_eq!(idn.arch, "");
    assert_eq!(idn.quant, "");
    assert_eq!(
        idn.short_id.len(),
        12,
        "placeholder short id for missing file"
    );
}

#[test]
fn in_flight_lease_counts_active_dispatches() {
    // The residency engine must never evict a model serving a request, so
    // each dispatch holds a lease across its server call: the count tracks
    // concurrent holders and returns to zero when they drop.
    let client = crate::instances::client::InstanceClient::new(
        reqwest::Client::new(),
        "http://127.0.0.1:1",
        None,
    );
    let manager = std::sync::Arc::new(InstanceManager::new(
        "base",
        client,
        vec![],
        crate::config::SidecarConfig::default(),
    ));
    assert_eq!(manager.in_flight(), 0, "idle manager holds nothing");
    let a = manager.hold_in_flight();
    let b = manager.hold_in_flight();
    assert_eq!(manager.in_flight(), 2, "two concurrent dispatches");
    drop(a);
    assert_eq!(manager.in_flight(), 1, "lease released on drop");
    drop(b);
    assert_eq!(
        manager.in_flight(),
        0,
        "last lease returns the count to zero"
    );
}

fn sibling_rr_profile(name: &str, group: &str) -> crate::config::InstanceProfile {
    crate::config::InstanceProfile {
        embedding: None,
        name: Some(name.into()),
        group: Some(group.into()),
        count: 1,
        num_ctx: 8192,
        parallel: None,
        pinned: true,
        no_sleep: false,
        sleep_idle_seconds: None,
        default: false,
        resume: false,
        params: None,
        max_ctx: None,
        session: false,
    }
}

fn sibling_rr_manager() -> InstanceManager {
    let client = crate::instances::client::InstanceClient::new(
        reqwest::Client::new(),
        "http://127.0.0.1:1",
        None,
    );
    InstanceManager::new(
        "lfm2.5-2.6b",
        client,
        vec![
            sibling_rr_profile("quick-0", "quick"),
            sibling_rr_profile("quick-1", "quick"),
        ],
        crate::config::SidecarConfig::default(),
    )
}

#[test]
fn sibling_rr_alternates_across_unsessioned_picks() {
    let manager = sibling_rr_manager();
    let all_ready = |_: &str| true;
    let picks: Vec<String> = (0..4)
        .map(|_| {
            manager
                .sibling_for_group_where("quick", &all_ready as &dyn Fn(&str) -> bool)
                .expect("sibling window")
        })
        .collect();
    assert_eq!(picks, vec!["quick-0", "quick-1", "quick-0", "quick-1"]);
}

#[test]
fn sibling_rr_skips_unready_sibling() {
    let manager = sibling_rr_manager();
    let only_one = |name: &str| name != "quick-0";
    for _ in 0..3 {
        assert_eq!(
            manager
                .sibling_for_group_where("quick", &only_one as &dyn Fn(&str) -> bool)
                .as_deref(),
            Some("quick-1"),
            "unready sibling never serves"
        );
    }
    // The cursor advanced past the served picks instead of wedging: with both
    // windows ready the rotation resumes at quick-0.
    let all_ready = |_: &str| true;
    assert_eq!(
        manager
            .sibling_for_group_where("quick", &all_ready as &dyn Fn(&str) -> bool)
            .as_deref(),
        Some("quick-0")
    );
}

#[test]
fn sibling_rr_single_sibling_is_noop() {
    let client = crate::instances::client::InstanceClient::new(
        reqwest::Client::new(),
        "http://127.0.0.1:1",
        None,
    );
    let manager = InstanceManager::new(
        "m",
        client,
        vec![sibling_rr_profile("quick", "quick")],
        crate::config::SidecarConfig::default(),
    );
    let all_ready = |_: &str| true;
    assert_eq!(
        manager.sibling_for_group_where("quick", &all_ready as &dyn Fn(&str) -> bool),
        None,
        "single window keeps group addressing (byte-identical wire)"
    );
}

#[test]
fn sibling_rr_unknown_group_is_noop() {
    let manager = sibling_rr_manager();
    let all_ready = |_: &str| true;
    assert_eq!(
        manager.sibling_for_group_where("nope", &all_ready as &dyn Fn(&str) -> bool),
        None,
        "unknown group keeps the original instance"
    );
}
