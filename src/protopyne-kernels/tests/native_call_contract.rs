//! Triple-state contract for native method dispatch: the bridge reports
//! exactly one of two outcomes — decline to Pine (fields untouched, the
//! engine runs the Pine body) or serve with the call value (`None` is Pine
//! `na`). This suite pins the mapping so future providers cannot re-confuse
//! handled-vs-declined with value-vs-absence.

use std::collections::HashMap;

use crate::native_libs::{LibBuffersProvider, MethodCtx, NativeProvider};
use crate::value::{FieldTable, NativeCall};
use crate::PineValue;

fn int(value: i64) -> PineValue {
    PineValue::Int(value)
}

fn float(value: f64) -> PineValue {
    PineValue::Float(value)
}

fn bool_value(value: bool) -> PineValue {
    PineValue::Bool(value)
}

fn table() -> FieldTable {
    let mut fields = HashMap::new();
    fields.insert(
        "data".to_owned(),
        Some(PineValue::Array(vec![None, None, None])),
    );
    fields.insert("max_size".to_owned(), Some(int(3)));
    fields.insert("head".to_owned(), Some(int(0)));
    fields.insert("count".to_owned(), Some(int(0)));
    fields.insert("needs_compaction".to_owned(), Some(bool_value(false)));
    fields.into()
}

#[test]
fn declined_is_distinct_from_served_absence() {
    assert!(NativeCall::Declined.is_declined());
    assert!(!NativeCall::Served(None).is_declined());
    assert!(!NativeCall::Served(Some(float(1.0))).is_declined());
}

#[test]
fn served_carries_call_value_including_absence() {
    assert_eq!(NativeCall::Served(None).into_value(), None);
    assert_eq!(
        NativeCall::Served(Some(float(2.0))).into_value(),
        Some(float(2.0))
    );
}

#[test]
fn decline_leaves_field_table_bit_identical() {
    let before = table();
    for (method, args) in [
        ("stdev", vec![]),
        ("push", vec![Some(float(1.0)), Some(float(2.0))]),
        ("get", vec![]),
        ("size", vec![Some(int(0))]),
    ] {
        let mut next = before.clone();
        let outcome = crate::buffers::serve_buffer_method(
            "BufferFloat",
            method,
            &mut next,
            &args,
            false,
        );
        assert!(outcome.is_declined(), "{method} declines");
        assert_eq!(next, before, "{method} decline touches no field");
    }
}

#[test]
fn decline_paths_cover_type_method_named_and_shape() {
    let before = table();
    // Foreign type declines.
    let mut next = before.clone();
    assert!(
        crate::buffers::serve_buffer_method("BufferInt", "push", &mut next, &[Some(int(1))], false)
            .is_declined()
    );
    assert_eq!(next, before);
    // Named args decline.
    let mut next = before.clone();
    assert!(
        crate::buffers::serve_buffer_method(
            "BufferFloat",
            "push",
            &mut next,
            &[Some(float(1.0))],
            true
        )
        .is_declined()
    );
    assert_eq!(next, before);
    // Misshaped state declines (absent data array).
    let mut ragged = before.clone();
    ragged.insert("data".to_owned(), None);
    let mut next = ragged.clone();
    assert!(
        crate::buffers::serve_buffer_method(
            "BufferFloat",
            "push",
            &mut next,
            &[Some(float(1.0))],
            false
        )
        .is_declined()
    );
    assert_eq!(next, ragged);
}

#[test]
fn served_paths_report_values() {
    let before = table();
    // Push serves with the receiver object.
    let mut next = before.clone();
    match crate::buffers::serve_buffer_method(
        "BufferFloat",
        "push",
        &mut next,
        &[Some(float(1.0))],
        false,
    ) {
        NativeCall::Served(Some(PineValue::Object(_))) => {}
        other => panic!("push serves the receiver, got {other:?}"),
    }
    // Out-of-bounds get serves absence (Pine `na`), never a decline.
    let mut reading = next.clone();
    match crate::buffers::serve_buffer_method(
        "BufferFloat",
        "get",
        &mut reading,
        &[Some(int(9))],
        false,
    ) {
        NativeCall::Served(None) => {}
        other => panic!("oob get serves na, got {other:?}"),
    }
    assert_eq!(reading, next, "get writes no field");
    // Size serves a scalar.
    match crate::buffers::serve_buffer_method("BufferFloat", "size", &mut next, &[], false) {
        NativeCall::Served(Some(_)) => {}
        other => panic!("size serves a scalar, got {other:?}"),
    }
}

#[test]
fn provider_delegates_to_the_same_contract() {
    let provider = LibBuffersProvider;
    let before = table();
    let mut next = before.clone();
    match provider.call(
        "BufferFloat",
        "push",
        &mut next,
        &[Some(float(1.0))],
        &MethodCtx::detached(),
    ) {
        Ok(NativeCall::Served(Some(PineValue::Object(_)))) => {}
        other => panic!("provider serves push, got {other:?}"),
    }
    let mut next = before.clone();
    match provider.call(
        "BufferFloat",
        "stdev",
        &mut next,
        &[],
        &MethodCtx::detached(),
    ) {
        Ok(NativeCall::Declined) => {}
        other => panic!("provider declines analytics, got {other:?}"),
    }
    assert_eq!(next, before);
}
