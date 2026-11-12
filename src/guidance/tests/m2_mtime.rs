//! Mtime-shortcut pins: granularity probe, completion touch, backdate,
//! rename adoption, verify audit, and the regen-decision matrix.

use std::time::Duration;

use fluent_wvr_testutil::tempdir;

use crate::components::{sweep_sidecars, ComponentVector, SidecarReceipt};
use crate::freshness::{mtime_skew_with_granularity, regen_decision};
use crate::sync::json_store;

fn receipt(pairs: &[(&str, &str)], source_hash: Option<&str>) -> SidecarReceipt {
    SidecarReceipt {
        components: ComponentVector {
            components: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        },
        source_hash: source_hash.map(str::to_string),
    }
}

fn test_doc() -> fluent_types::GuidanceDoc {
    fluent_types::GuidanceDoc {
        meta: fluent_types::Meta {
            module: "m2".into(),
            source: "src/m2.zig".into(),
            language: "zig".into(),
        },
        ..fluent_types::GuidanceDoc::default()
    }
}

#[test]
fn probe_reports_usable_granularity() {
    let dir = tempdir();
    let gran = crate::freshness::probe_timestamp_granularity(dir.path());
    assert!(gran <= Duration::from_secs(2));
}

#[test]
fn skew_boundaries_follow_granularity() {
    use std::time::{SystemTime, UNIX_EPOCH};
    let base = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let gran = Duration::from_secs(1);
    // Equal mtimes are evidence-free at any granularity.
    assert_eq!(
        mtime_skew_with_granularity(base, base, gran),
        crate::freshness::MtimeSkew::Ambiguous
    );
    // Exactly one granularity past the sidecar is still ambiguous.
    assert_eq!(
        mtime_skew_with_granularity(base, base + gran, gran),
        crate::freshness::MtimeSkew::Ambiguous
    );
    // One nanosecond past the window is stale.
    assert_eq!(
        mtime_skew_with_granularity(base, base + gran + Duration::from_nanos(1), gran),
        crate::freshness::MtimeSkew::SourceNewer
    );
    // Older source is fresh regardless of window.
    let earlier = base
        .checked_sub(Duration::from_secs(10))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    assert_eq!(
        mtime_skew_with_granularity(base, earlier, gran),
        crate::freshness::MtimeSkew::SourceOlder
    );
}

#[test]
fn regen_matrix_never_false_regens_identical() {
    use std::time::{SystemTime, UNIX_EPOCH};
    let base = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    // Inside the window the hash decides, so touched-but-identical
    // files never regen. Outside it the clock rules with zero hash
    // cost: an identical-bytes regen there is one idempotent pass
    // that converges on the next touch, never a loop.
    for gran in [
        Duration::from_nanos(1),
        Duration::from_secs(1),
        Duration::from_secs(2),
    ] {
        // Inside the window the hash decides.
        let inside = base + gran;
        assert!(!regen_decision(base, inside, gran, true));
        assert!(regen_decision(base, inside, gran, false));
        // Outside it the clock decides with zero hash cost.
        let outside = base + gran + Duration::from_nanos(1);
        assert!(regen_decision(base, outside, gran, true));
        assert!(regen_decision(base, outside, gran, false));
        // An older source never regens.
        let earlier = base
            .checked_sub(Duration::from_secs(10))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        assert!(!regen_decision(base, earlier, gran, false));
    }
}

#[test]
fn completion_touch_makes_sidecar_strictly_newer() {
    let dir = tempdir();
    let source = dir.path().join("a.zig");
    std::fs::write(&source, "pub fn a() void {}\n").expect("write");
    let sidecars = dir.path().join("sidecars");
    std::fs::create_dir_all(&sidecars).expect("mkdir");
    let sidecar = sidecars.join("a.zig.json");
    let body = common_core::hash::sha256_hex(b"pub fn a() void {}\n");
    json_store::save_guidance(&sidecar, &test_doc(), &receipt(&[], Some(&body))).expect("save");
    crate::freshness::completion_touch(&sidecar, &source, Duration::from_secs(1)).expect("touch");
    let sidecar_mtime = std::fs::metadata(&sidecar)
        .expect("stat")
        .modified()
        .expect("mtime");
    let source_mtime = std::fs::metadata(&source)
        .expect("stat")
        .modified()
        .expect("mtime");
    assert!(sidecar_mtime > source_mtime);
}

#[test]
fn verify_flags_forged_newer_mtime() {
    let dir = tempdir();
    let source = dir.path().join("a.zig");
    std::fs::write(&source, "pub fn a() void {}\n").expect("write");
    let sidecars = dir.path().join("sidecars");
    std::fs::create_dir_all(&sidecars).expect("mkdir");
    let sidecar = sidecars.join("a.zig.json");
    let body = common_core::hash::sha256_hex(b"pub fn a() void {}\n");
    json_store::save_guidance(&sidecar, &test_doc(), &receipt(&[], Some(&body))).expect("save");
    crate::freshness::completion_touch(&sidecar, &source, Duration::from_secs(1)).expect("touch");
    assert_eq!(
        crate::freshness::verify_sidecar(&sidecar, &source, Duration::from_secs(1)),
        crate::freshness::VerifyVerdict::Fresh
    );
    // Same bytes, forged newer source mtime plus changed bytes underneath:
    // the clock claims work, the hashes refuse it.
    std::fs::write(&source, "pub fn a() void { return; }\n").expect("rewrite");
    let forged = std::fs::metadata(&sidecar)
        .expect("stat")
        .modified()
        .expect("mtime")
        + Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(&source)
        .expect("open")
        .set_modified(forged)
        .expect("forge");
    assert_eq!(
        crate::freshness::verify_sidecar(&sidecar, &source, Duration::from_secs(1)),
        crate::freshness::VerifyVerdict::SkewSuspect
    );
}

#[test]
fn verify_detects_restored_source_bytes() {
    let dir = tempdir();
    let source = dir.path().join("a.zig");
    std::fs::write(&source, "pub fn a() void {}\n").expect("write");
    let sidecars = dir.path().join("sidecars");
    std::fs::create_dir_all(&sidecars).expect("mkdir");
    let sidecar = sidecars.join("a.zig.json");
    let body = common_core::hash::sha256_hex(b"pub fn a() void {}\n");
    json_store::save_guidance(&sidecar, &test_doc(), &receipt(&[], Some(&body))).expect("save");
    crate::freshness::completion_touch(&sidecar, &source, Duration::from_secs(1)).expect("touch");
    // A restore swaps different bytes under a preserved mtime: the mtime
    // gate reads fresh, but the audit must not.
    std::fs::write(&source, "pub fn changed() void {}\n").expect("restore");
    let kept = std::fs::metadata(&sidecar)
        .expect("stat")
        .modified()
        .expect("mtime")
        .checked_sub(Duration::from_secs(5))
        .unwrap_or(std::time::UNIX_EPOCH);
    std::fs::File::options()
        .write(true)
        .open(&source)
        .expect("open")
        .set_modified(kept)
        .expect("backdate");
    assert!(!crate::sync::staleness::is_stale(&sidecar, &source));
    assert_eq!(
        crate::freshness::verify_sidecar(&sidecar, &source, Duration::from_secs(1)),
        crate::freshness::VerifyVerdict::SourceChanged
    );
}

#[test]
fn rename_moves_sidecar_by_source_hash() {
    let dir = tempdir();
    let sources = dir.path().join("src");
    std::fs::create_dir_all(&sources).expect("mkdir");
    let sidecars = dir.path().join("sidecars");
    std::fs::create_dir_all(&sidecars).expect("mkdir");
    let body = b"pub fn moved() void {}\n";
    let old = sources.join("old.zig");
    std::fs::write(&old, body).expect("write");
    let hash = common_core::hash::sha256_hex(body);
    json_store::save_guidance(
        &sidecars.join("old.zig.json"),
        &test_doc(),
        &receipt(&[], Some(&hash)),
    )
    .expect("save");
    let new = sources.join("new.zig");
    std::fs::rename(&old, &new).expect("rename source");
    let moved =
        crate::freshness::adopt_renamed_sidecar(&sidecars, &sources, &new, &hash).expect("adopt");
    assert_eq!(moved.file_name().unwrap(), "new.zig.json");
    assert!(!sidecars.join("old.zig.json").exists());
    assert!(sidecars.join("new.zig.json").exists());
}

#[test]
fn sweep_leaves_source_untouched() {
    let dir = tempdir();
    let sources = dir.path().join("src");
    std::fs::create_dir_all(&sources).expect("mkdir");
    let sidecars = dir.path().join("sidecars");
    std::fs::create_dir_all(&sidecars).expect("mkdir");
    let source = sources.join("a.zig");
    std::fs::write(&source, "pub fn a() void {}\n").expect("write");
    let before = std::fs::read(&source).expect("read");
    let saved = ComponentVector {
        components: [("parser".to_string(), "p1".to_string())]
            .into_iter()
            .collect(),
    };
    json_store::save_guidance(
        &sidecars.join("a.zig.json"),
        &test_doc(),
        &receipt(&[("parser", "p1")], None),
    )
    .expect("save");
    let evolved = ComponentVector {
        components: [("parser".to_string(), "p2".to_string())]
            .into_iter()
            .collect(),
    };
    let _ = saved;
    let outcomes = sweep_sidecars(&sidecars, &sources, &evolved);
    assert_eq!(
        outcomes[0].decision,
        crate::components::SweepDecision::Backdated
    );
    assert_eq!(std::fs::read(&source).expect("read"), before);
}
