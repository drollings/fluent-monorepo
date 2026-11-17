use super::first_writable_dir;

#[test]
fn first_writable_dir_picks_first_creatable_in_order() {
    let base = std::env::temp_dir().join(format!("cc-first-writable-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    // A regular file where a directory is expected: `create_dir_all` fails.
    let blocker_dir = base.join("blocked");
    std::fs::create_dir_all(&blocker_dir).unwrap();
    let blocker_file = blocker_dir.join("x");
    std::fs::write(&blocker_file, b"x").unwrap();
    let good_a = base.join("a");
    let good_b = base.join("b");
    let candidates = vec![blocker_file, good_a.clone(), good_b.clone()];
    let picked = first_writable_dir(&candidates);
    assert_eq!(picked, Some(&good_a));
    assert!(good_a.is_dir());
    assert!(
        !good_b.exists(),
        "selection must stop at the first writable dir"
    );
    let _ = std::fs::remove_dir_all(&base);
}
