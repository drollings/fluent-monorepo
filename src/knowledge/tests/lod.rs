use super::*;

#[test]
fn lod_values_and_order() {
    assert_eq!(Lod::LOD0.as_u8(), 0);
    assert_eq!(Lod::LOD5.as_u8(), 5);
    assert!(Lod::LOD0 < Lod::LOD5);
    assert_eq!(Lod::try_from(3).unwrap(), Lod::LOD3);
    assert_eq!(Lod::try_from(6).unwrap_err(), 6);
    assert_eq!(Lod::LOD2.to_string(), "LOD2");
    assert_eq!(LOD0_FULL_TEXT, 0);
    assert_eq!(LOD5_LABEL, 5);
    assert!(LAZY_LOD_RANGE.contains(&2));
    assert!(!LAZY_LOD_RANGE.contains(&5));
}
