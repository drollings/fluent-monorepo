use super::*;
use crate::indicator::fin_or_none;

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

#[test]
fn floats_are_never_truthy_or_truncating() {
    // The reference runtime treats nonzero numbers as `true`; this surface
    // refuses instead — numbers never sneak into boolean positions.
    assert_eq!(PineValue::Float(0.0).as_bool(), None);
    assert_eq!(PineValue::Float(1.0).as_bool(), None);
    assert_eq!(PineValue::Float(f64::NAN).as_bool(), None);
    assert_eq!(PineValue::Float(1.9).as_int(), None);
}

#[test]
fn new_variants_never_read_as_scalar() {
    let values = [
        PineValue::Str("fast".to_owned()),
        PineValue::ColorHandle(ColorHandle::new(0x28_28_28_3C)),
        PineValue::EnumHandle(EnumHandle::new("Smoothings", "WMA")),
    ];
    for value in &values {
        assert_eq!(value.as_float(), None, "never numeric: {value}");
        assert_eq!(value.as_int(), None, "never an int: {value}");
        assert_eq!(value.as_bool(), None, "never truthy: {value}");
    }
}

#[test]
fn na_is_none_and_survives_the_float_boundary() {
    let absent: Option<PineValue> = None;
    assert!(absent.is_none(), "absence is None, never a sentinel");
    let nan = PineValue::Float(f64::NAN)
        .as_float()
        .expect("NaN still reads");
    assert!(nan.is_nan());
    assert_eq!(fin_or_none(nan), None, "NaN maps to None at the boundary");
    assert_eq!(fin_or_none(1.5), Some(1.5));
}

#[test]
fn only_collections_flow_back_to_caller_homes() {
    // Write-back keys off the value kind, never the parameter or the
    // return: arrays, int-maps, maps, and matrices ride by reference and
    // persist through the parameter, while scalars, handles, objects, and
    // drawings stay by value. All three call paths (tree-only calls, loop
    // user calls, and the cross-engine bridge harvest) decide on this
    // one predicate.
    for value in [
        PineValue::Array(vec![Some(PineValue::Float(1.0))]),
        PineValue::IntMap(std::collections::HashMap::from([(0, 1)])),
        PineValue::Map(std::collections::BTreeMap::from([(
            "k".to_owned(),
            (PineValue::Int(0), Some(PineValue::Int(1))),
        )])),
        PineValue::Matrix(MatrixValue {
            rows: 1,
            cols: 1,
            cells: vec![Some(PineValue::Float(1.0))],
        }),
    ] {
        assert!(value.is_collection_writeback(), "rides back: {value}");
    }
    for value in [
        PineValue::Float(1.0),
        PineValue::Int(1),
        PineValue::Bool(true),
        PineValue::Str("s".to_owned()),
        PineValue::ColorHandle(ColorHandle::new(0x28_28_28_3C)),
        PineValue::EnumHandle(EnumHandle::new("Smoothings", "WMA")),
        PineValue::Object(UserInstance::new("T", FieldTable::new())),
        PineValue::Drawing(DrawingHandle::new(DrawingKind::Label, 0)),
    ] {
        assert!(!value.is_collection_writeback(), "stays by value: {value}");
    }
}

#[test]
fn nz_fills_absence_with_default() {
    assert_eq!(nz(None, PineValue::Int(3)), PineValue::Int(3));
    assert_eq!(
        nz(Some(PineValue::Float(1.5)), PineValue::Int(3)),
        PineValue::Float(1.5)
    );
    assert_eq!(
        nz(None, PineValue::Str("d".to_owned())),
        PineValue::Str("d".to_owned())
    );
}

#[test]
fn fixnan_forward_fills_without_resurrecting_warmup() {
    let series = [
        None,
        None,
        Some(1.0),
        None,
        None,
        Some(3.0),
        None,
        Some(f64::NAN),
    ];
    let filled = fixnan(&series);
    assert_eq!(
        filled,
        vec![
            None,
            None,
            Some(1.0),
            Some(1.0),
            Some(1.0),
            Some(3.0),
            Some(3.0),
            Some(3.0),
        ],
        "warmup prefix stays absent; NaN fills like any other absence"
    );
}

#[test]
fn handles_carry_identity_beside_numerics() {
    let color = ColorHandle::new(0xFF_00_FF_00);
    assert_eq!(color.raw(), 0xFF_00_FF_00);
    let member = EnumHandle::new("Smoothings", "WMA");
    assert_eq!(member.enum_name, "Smoothings");
    assert_eq!(member.member, "WMA");
    assert_eq!(
        PineValue::EnumHandle(member.clone()),
        PineValue::EnumHandle(EnumHandle::new("Smoothings", "WMA"))
    );
    assert_eq!(
        PineValue::Str("a".to_owned()),
        PineValue::Str("a".to_owned())
    );
}

#[test]
fn object_handles_share_until_first_mutation() {
    // Slot protocol: cloning an object shares one resident table (one
    // `Arc` bump, never a deep copy); the first mutation through a
    // shared handle copies, so the writer separates while old readers
    // keep the original.
    let fields: HashMap<String, Option<PineValue>> = [("n".to_owned(), Some(PineValue::Int(0)))]
        .into_iter()
        .collect();
    let table = FieldTable::from(fields);
    let shared = table.clone();
    assert!(table.shares(&shared), "clone shares the resident table");
    let instance = UserInstance::new("Counter", table);
    let carried = PineValue::Object(instance.clone());
    let PineValue::Object(back) = carried.clone() else {
        panic!("object carries through");
    };
    assert!(
        instance.fields.shares(&back.fields),
        "slot carries share the handle"
    );
    let mut writer = back.fields.clone();
    assert!(writer.shares(&instance.fields), "pre-write still shares");
    writer.insert("n".to_owned(), Some(PineValue::Int(1)));
    assert!(
        !writer.shares(&instance.fields),
        "first mutation copies the table"
    );
    assert_eq!(
        instance.fields.get("n"),
        Some(&Some(PineValue::Int(0))),
        "old readers keep the original"
    );
    assert_eq!(
        writer.get("n"),
        Some(&Some(PineValue::Int(1))),
        "the writer sees its own put"
    );
}
