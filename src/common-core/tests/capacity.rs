use common_core::capacity::nonzero_capacity;

#[test]
fn zero_refuses() {
    assert!(nonzero_capacity(0, "pool").is_err());
    assert_eq!(nonzero_capacity(4, "pool").unwrap().get(), 4);
}
