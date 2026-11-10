//! Shadow-verify pins: individual checks, merged verification, and
//! bisection to the single culprit.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::shadow::{bisect_culprit, verify_merged, Bisect, ShadowPatch};

fn patch(file: &str, after: &str) -> ShadowPatch {
    ShadowPatch {
        file: file.to_string(),
        before: "old\n".to_string(),
        after: after.to_string(),
    }
}

#[test]
fn merged_pass_accepts_all_patches() {
    let dir = fluent_wvr_testutil::tempdir();
    let patches = vec![patch("a.rs", "new a\n"), patch("b.rs", "new b\n")];
    let calls = Arc::new(AtomicUsize::new(0));
    let check_calls = Arc::clone(&calls);
    let ok = verify_merged(dir.path(), &patches, &|_| {
        check_calls.fetch_add(1, Ordering::SeqCst);
        true
    });
    assert!(ok);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "merged result verifies once");
}

#[test]
fn bisect_finds_the_single_culprit() {
    let dir = fluent_wvr_testutil::tempdir();
    let mut patches = Vec::new();
    for i in 0..7 {
        patches.push(patch(&format!("f{i:02}.rs"), "good\n"));
    }
    patches.push(patch("bad.rs", "BAD\n"));
    let culprit = bisect_culprit(dir.path(), &patches, &|shadow| {
        !std::fs::read_to_string(shadow.join("bad.rs"))
            .unwrap_or_default()
            .contains("BAD")
    });
    assert_eq!(culprit, Bisect::Culprit("bad.rs".to_string()));
}

#[test]
fn bisect_property_single_culprit_always_found() {
    // Property over sizes and positions: exactly one bad patch among N
    // is always identified, and the rest requeue cleanly.
    for size in [1usize, 2, 3, 5, 8] {
        for bad_at in 0..size {
            let dir = fluent_wvr_testutil::tempdir();
            let patches: Vec<ShadowPatch> = (0..size)
                .map(|i| {
                    patch(
                        &format!("f{i}.rs"),
                        if i == bad_at { "BAD\n" } else { "good\n" },
                    )
                })
                .collect();
            let culprit = bisect_culprit(dir.path(), &patches, &|shadow| {
                !std::fs::read_dir(shadow).map_or(true, |entries| {
                    entries.filter_map(Result::ok).any(|entry| {
                        std::fs::read_to_string(entry.path()).unwrap_or_default().contains("BAD")
                    })
                })
            });
            assert_eq!(
                culprit,
                Bisect::Culprit(format!("f{bad_at}.rs")),
                "size {size} bad-at {bad_at}"
            );
        }
    }
}

#[test]
fn empty_patch_set_trivially_verifies() {
    let dir = fluent_wvr_testutil::tempdir();
    assert!(verify_merged(dir.path(), &[], &|_| false));
    assert_eq!(bisect_culprit(dir.path(), &[], &|_| false), Bisect::Clean);
}
