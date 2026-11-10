//! M0 hygiene pins: shared lock/IO/path helpers behave like the call sites they replace.

use std::path::PathBuf;
use std::sync::Mutex;

#[test]
fn lock_recovers_poisoned_vec() {
    let m = Mutex::new(vec![1u8, 2, 3]);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = m.lock().unwrap();
        panic!("poison");
    }));
    let guard = common_core::sync::lock(&m);
    assert_eq!(*guard, vec![1u8, 2, 3]);
}

#[test]
fn lock_recovers_poisoned_option_slot() {
    let m: Mutex<Option<u32>> = Mutex::new(Some(7));
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = m.lock().unwrap();
        panic!("poison");
    }));
    let mut guard = common_core::sync::lock(&m);
    assert_eq!(guard.take(), Some(7));
}

#[test]
fn lock_recovers_poisoned_unit() {
    let m = Mutex::new(());
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = m.lock().unwrap();
        panic!("poison");
    }));
    let _guard = common_core::sync::lock(&m);
}

#[test]
fn read_err_matches_std_for_plain_file() {
    let dir = fluent_wvr_testutil::tempdir();
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "hello").expect("write");
    let want = std::fs::read_to_string(&file).expect("std read");
    let got = common_core::io::read_to_string_err(&file).expect("shared read");
    assert_eq!(got, want);
    assert!(common_core::io::read_to_string_err(&dir.path().join("missing")).is_err());
}

#[test]
fn baseline_loader_parity_with_std() {
    let dir = fluent_wvr_testutil::tempdir();
    let file = dir.path().join("base.json");
    let body = r#"{"metrics": {"a": 1.0}, "tolerances": {}, "bounds": {}, "history": []}"#;
    std::fs::write(&file, body).expect("write");
    let via_std = std::fs::read_to_string(&file).ok();
    let via_shared = common_core::io::read_to_string_err(&file).ok();
    assert_eq!(via_shared, via_std);
}

#[test]
fn absolute_or_matches_std_join() {
    let abs = if cfg!(windows) {
        PathBuf::from("C:/x/y")
    } else {
        PathBuf::from("/x/y")
    };
    assert_eq!(crate::sync_engine::absolute_or(abs.clone()), abs);
    let rel = PathBuf::from("some/rel/path.txt");
    let want = std::path::absolute(&rel).unwrap_or(rel.clone());
    assert_eq!(crate::sync_engine::absolute_or(rel), want);
}
