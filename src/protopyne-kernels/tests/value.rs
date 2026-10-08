use super::*;

#[test]
fn int_auto_casts_to_float_bool_does_not() {
    assert_eq!(PineValue::Int(7).as_float(), Some(7.0));
    assert_eq!(PineValue::Float(1.5).as_float(), Some(1.5));
    assert_eq!(PineValue::Bool(true).as_float(), None);
}

#[test]
fn typed_accessors_reject_mismatched_kinds() {
    assert_eq!(PineValue::Int(7).as_int(), Some(7));
    assert_eq!(PineValue::Float(1.0).as_int(), None);
    assert_eq!(PineValue::Bool(false).as_bool(), Some(false));
    assert_eq!(PineValue::Int(0).as_bool(), None);
}
