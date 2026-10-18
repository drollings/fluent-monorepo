//! Native `BufferFloat` execution: the interpreted DVP `reconcile` path
//! issues ~30k `BufferFloat.push/get` calls per bar (1440 synthesized
//! candles × ring + history traffic), which dominates the release-gated
//! DVP suites. This suite pins the native fast path for the hot methods
//! (`init/push/get/size/clear`) against the shipped Pine oracle, plus the
//! engine agreement that lets DVP drivers opt in without re-blessing.
//!
//! Scope v1: `BufferFloat` hot methods only. Analytics (`stdev/max/sum/…`),
//! `getLinearized`, `compact`, iteration helpers, and all of `BufferInt`
//! decline to Pine (identical behavior, never a silent approximation).
//! Anything untranslatable (missing fields, misshaped state, named args,
//! bad arity) declines too — the Pine body runs exactly as today.
//!
//! Oracle discipline: every expectation below is hand-derived from the
//! shipped `_libBuffers.pine` method bodies (ring vs array branches,
//! newest-first `get`, `_linear` invalidation on ring writes only), never
//! from the implementation under test.

use std::collections::HashMap;

#[cfg(feature = "pine")]
use crate::native_libs::NativeOptions;
use crate::value::FieldTable;
use crate::PineValue;
#[cfg(feature = "pine")]
use crate::{InputSet, LinkedModule, PineEngine, PineRuntimeConfig};

#[cfg(feature = "pine")]
fn native_on() -> NativeOptions {
    NativeOptions {
        enabled: true,
        only_symbols: None,
    }
}

// Optional-arg constructors: every call site needs `Option<PineValue>`
// (absence vs value is the domain type for method args), so the `Some`
// wrap lives here rather than at ~50 call sites.
#[allow(clippy::unnecessary_wraps)]
fn float(value: f64) -> Option<PineValue> {
    Some(PineValue::Float(value))
}

#[allow(clippy::unnecessary_wraps)]
fn int(value: i64) -> Option<PineValue> {
    Some(PineValue::Int(value))
}

#[allow(clippy::unnecessary_wraps)]
fn bool_value(value: bool) -> Option<PineValue> {
    Some(PineValue::Bool(value))
}

/// One `BufferFloat` field table as the engine builds it: `data` array of
/// float-or-absence slots plus the scalar bookkeeping (`_linear` rides
/// along untouched unless the oracle invalidates it).
fn table(
    data: Vec<Option<f64>>,
    max_size: i64,
    head: Option<i64>,
    count: i64,
    needs_compaction: bool,
    linear: Option<Vec<Option<f64>>>,
) -> FieldTable {
    let mut fields = HashMap::new();
    fields.insert(
        "data".to_owned(),
        Some(PineValue::Array(
            data.into_iter()
                .map(|slot| slot.map(PineValue::Float))
                .collect(),
        )),
    );
    fields.insert("max_size".to_owned(), int(max_size));
    fields.insert("head".to_owned(), head.map(PineValue::Int));
    fields.insert("count".to_owned(), int(count));
    fields.insert("needs_compaction".to_owned(), bool_value(needs_compaction));
    fields.insert(
        "_linear".to_owned(),
        linear.map(|slots| {
            PineValue::Array(
                slots
                    .into_iter()
                    .map(|slot| slot.map(PineValue::Float))
                    .collect(),
            )
        }),
    );
    fields.into()
}

fn num(table: &FieldTable, name: &str) -> Option<f64> {
    match table.get(name) {
        Some(Some(PineValue::Float(value))) => Some(*value),
        Some(Some(PineValue::Int(value))) => Some(*value as f64),
        Some(None) => None,
        other => panic!("expected number-or-absence for {name}, got {other:?}"),
    }
}

fn data_of(table: &FieldTable) -> Vec<Option<f64>> {
    match table.get("data") {
        Some(Some(PineValue::Array(slots))) => slots
            .iter()
            .map(|slot| match slot {
                None => None,
                Some(PineValue::Float(value)) => Some(*value),
                Some(PineValue::Int(value)) => Some(*value as f64),
                Some(other) => panic!("float slot, got {other:?}"),
            })
            .collect(),
        other => panic!("array field data, got {other:?}"),
    }
}

fn flag(table: &FieldTable, name: &str) -> bool {
    match table.get(name) {
        Some(Some(PineValue::Bool(value))) => *value,
        other => panic!("expected bool for {name}, got {other:?}"),
    }
}

/// Serve one hot-method call through the native provider under test.
/// `Declined` is the decline-to-Pine path (behavior today, unchanged).
/// The table clone lives here in test code only — the engine passes the
/// taken table by mutable borrow, so the hot path copies nothing.
fn serve(
    method: &str,
    fields: &FieldTable,
    args: &[Option<PineValue>],
) -> Option<(FieldTable, Option<PineValue>)> {
    use crate::value::NativeCall;
    let mut next = fields.clone();
    match crate::buffers::serve_buffer_method("BufferFloat", method, &mut next, args, false) {
        NativeCall::Declined => None,
        NativeCall::Served(value) => Some((next, value)),
    }
}

/// Decline probe: the call must refuse service and leave the table
/// byte-identical (the engine would run the Pine body on it next).
fn declines(method: &str, fields: &FieldTable, args: &[Option<PineValue>], named: bool) -> bool {
    let mut next = fields.clone();
    let outcome =
        crate::buffers::serve_buffer_method("BufferFloat", method, &mut next, args, named);
    assert_eq!(next, *fields, "{method} decline leaves the table untouched");
    outcome.is_declined()
}

// ---------------------------------------------------------------------------
// `init`
// ---------------------------------------------------------------------------

#[test]
fn init_ring_builds_sized_absent_data() {
    let before = table(vec![], 20, None, 0, false, None);
    let (after, value) =
        serve("init", &before, &[int(5), bool_value(true)]).expect("init serves natively");
    assert_eq!(data_of(&after), vec![None, None, None, None, None]);
    assert_eq!(num(&after, "max_size"), Some(5.0));
    assert_eq!(num(&after, "head"), Some(0.0));
    assert_eq!(num(&after, "count"), Some(0.0));
    assert!(!flag(&after, "needs_compaction"));
    assert!(
        matches!(value, Some(PineValue::Object(_))),
        "init returns this"
    );
}

#[test]
fn init_array_builds_empty_data_with_absent_head() {
    let before = table(vec![], 20, None, 0, false, None);
    let (after, _) =
        serve("init", &before, &[int(5), bool_value(false)]).expect("init serves natively");
    assert!(data_of(&after).is_empty());
    assert_eq!(num(&after, "head"), None, "array mode keeps head absent");
}

#[test]
fn init_defaults_ring_mode_off_on_single_arg() {
    let before = table(vec![], 20, None, 0, false, None);
    let (after, _) = serve("init", &before, &[int(4)]).expect("one-arg init serves");
    assert!(data_of(&after).is_empty(), "default ring_mode is false");
    assert_eq!(num(&after, "max_size"), Some(4.0));
}

#[test]
fn init_preserves_linear_cache_like_pine() {
    // The oracle never touches `_linear` in `init` (even a stale cache
    // survives); the bridge must pass it through byte-identical.
    let before = table(vec![], 20, None, 0, false, Some(vec![Some(1.0)]));
    let (after, _) =
        serve("init", &before, &[int(3), bool_value(true)]).expect("init serves natively");
    assert_eq!(
        after.get("_linear"),
        before.get("_linear"),
        "_linear passes through init"
    );
}

#[test]
fn init_declines_bad_arity_and_named_and_negative() {
    let before = table(vec![], 20, None, 0, false, None);
    assert!(
        declines("init", &before, &[], true),
        "zero args decline (Pine binds its own refusal)"
    );
    assert!(
        declines("init", &before, &[int(3), bool_value(true), int(1)], false),
        "three args decline"
    );
    assert!(
        declines("init", &before, &[int(-2), bool_value(true)], false),
        "negative size declines (Pine refuses identically)"
    );
}

// ---------------------------------------------------------------------------
// `push` / `get` / `size` (ring mode)
// ---------------------------------------------------------------------------

fn push(fields: &FieldTable, value: Option<PineValue>) -> FieldTable {
    serve("push", fields, &[value])
        .expect("push serves natively")
        .0
}

#[test]
fn push_returns_the_resident_table_by_handle() {
    // The bridge hands back `this` as a handle to the resident table, not a
    // copy: the served object shares the receiver's table, array included.
    let fresh = table(vec![], 20, None, 0, false, None);
    let (sized, _) = serve("init", &fresh, &[int(3), bool_value(true)]).expect("init serves");
    let (after, value) = serve("push", &sized, &[float(1.0)]).expect("push serves");
    let Some(PineValue::Object(instance)) = value else {
        panic!("push returns this");
    };
    assert!(
        instance.fields.shares(&after),
        "push returns the resident table by handle, not a copy"
    );
}

#[test]
fn a_shared_table_copies_on_write() {
    // Sharing is copy-on-write: the handle and the resident table stay
    // independent, so a bridge decline or a stale reader never sees a write
    // through the other handle.
    let mut resident = table(vec![], 20, None, 0, false, None);
    let handle = resident.clone();
    assert!(resident.shares(&handle), "a clone shares until written");
    resident.insert("count".to_owned(), int(9));
    assert!(!resident.shares(&handle), "the first write separates them");
    assert_eq!(
        num(&handle, "count"),
        Some(0.0),
        "the handle keeps the old table"
    );
    assert_eq!(
        num(&resident, "count"),
        Some(9.0),
        "the write landed on the resident"
    );
}

#[test]
fn ring_push_wraps_and_get_reads_newest_first() {
    let fresh = table(vec![], 20, None, 0, false, None);
    let (sized, _) = serve("init", &fresh, &[int(3), bool_value(true)]).expect("init serves");
    let s1 = push(&sized, float(1.0));
    let s2 = push(&s1, float(2.0));
    let s3 = push(&s2, float(3.0));
    assert_eq!(num(&s3, "count"), Some(3.0));
    for (index, want) in [(0, 3.0), (1, 2.0), (2, 1.0)] {
        let (_, value) = serve("get", &s3, &[int(index)]).expect("get serves");
        assert_eq!(value, float(want), "newest-first read at {index}");
    }
    // Fourth push evicts the oldest (1.0): the oracle overwrites at head.
    let s4 = push(&s3, float(4.0));
    let (_, newest) = serve("get", &s4, &[int(0)]).expect("get serves");
    assert_eq!(newest, float(4.0));
    let (_, oldest) = serve("get", &s4, &[int(2)]).expect("get serves");
    assert_eq!(oldest, float(2.0), "1.0 evicted by the wrap");
    let (_, size) = serve("size", &s4, &[]).expect("size serves");
    assert_eq!(
        size,
        float(3.0),
        "ring size unifies to float (math.min over floats)"
    );
}

#[test]
fn ring_push_absent_value_stores_absence() {
    let fresh = table(vec![], 20, None, 0, false, None);
    let (sized, _) = serve("init", &fresh, &[int(2), bool_value(true)]).expect("init serves");
    let after = push(&sized, None);
    let (_, value) = serve("get", &after, &[int(0)]).expect("get serves");
    assert_eq!(value, None, "pushed na reads back na");
}

#[test]
fn ring_push_invalidates_linear_cache() {
    let fresh = table(vec![], 20, None, 0, false, None);
    let (sized, _) = serve("init", &fresh, &[int(2), bool_value(true)]).expect("init serves");
    let mut cached = sized;
    cached.insert(
        "_linear".to_owned(),
        Some(PineValue::Array(vec![float(9.0)])),
    );
    let after = push(&cached, float(1.0));
    assert_eq!(
        after.get("_linear"),
        Some(&None),
        "ring write clears the cache"
    );
}

// ---------------------------------------------------------------------------
// `push` / `get` / `size` (array mode)
// ---------------------------------------------------------------------------

#[test]
fn array_push_appends_and_flags_over_max() {
    let fresh = table(vec![], 20, None, 0, false, None);
    let (sized, _) = serve("init", &fresh, &[int(2), bool_value(false)]).expect("init serves");
    let s1 = push(&sized, float(1.0));
    let s2 = push(&s1, float(2.0));
    assert!(!flag(&s2, "needs_compaction"), "at max is not over max");
    let s3 = push(&s2, float(3.0));
    assert!(flag(&s3, "needs_compaction"), "over max flags compaction");
    // Newest-first read over the grown array.
    for (index, want) in [(0, 3.0), (1, 2.0), (2, 1.0)] {
        let (_, value) = serve("get", &s3, &[int(index)]).expect("get serves");
        assert_eq!(value, float(want), "array read at {index}");
    }
    let (_, size) = serve("size", &s3, &[]).expect("size serves");
    assert_eq!(size, int(3), "array size is the grown length");
}

#[test]
fn array_push_leaves_linear_cache_alone() {
    let fresh = table(vec![], 20, None, 0, false, None);
    let (sized, _) = serve("init", &fresh, &[int(4), bool_value(false)]).expect("init serves");
    let mut cached = sized;
    cached.insert(
        "_linear".to_owned(),
        Some(PineValue::Array(vec![float(9.0)])),
    );
    let after = push(&cached, float(1.0));
    assert_eq!(
        after.get("_linear"),
        cached.get("_linear"),
        "array writes keep the cache (oracle only clears on ring writes)"
    );
}

// ---------------------------------------------------------------------------
// `get` edges + `clear`
// ---------------------------------------------------------------------------

#[test]
fn get_out_of_bounds_reads_absent() {
    let fresh = table(vec![], 20, None, 0, false, None);
    let (sized, _) = serve("init", &fresh, &[int(2), bool_value(true)]).expect("init serves");
    let pushed = push(&sized, float(7.0));
    for index in [-1, 2, 99] {
        let (_, value) = serve("get", &pushed, &[int(index)]).expect("get serves");
        assert_eq!(value, None, "index {index} reads na");
    }
}

#[test]
fn clear_ring_keeps_length_and_clears_cache() {
    let fresh = table(vec![], 20, None, 0, false, None);
    let (sized, _) = serve("init", &fresh, &[int(3), bool_value(true)]).expect("init serves");
    let pushed = push(&push(&sized, float(1.0)), float(2.0));
    let (cleared, _) = serve("clear", &pushed, &[]).expect("clear serves");
    assert_eq!(
        data_of(&cleared),
        vec![None, None, None],
        "fill keeps length"
    );
    assert_eq!(num(&cleared, "head"), Some(0.0));
    assert_eq!(num(&cleared, "count"), Some(0.0));
    assert!(!flag(&cleared, "needs_compaction"));
    assert_eq!(
        cleared.get("_linear"),
        Some(&None),
        "ring clear clears the cache"
    );
}

#[test]
fn clear_array_empties_data_and_keeps_absent_head() {
    let fresh = table(vec![], 20, None, 0, false, None);
    let (sized, _) = serve("init", &fresh, &[int(3), bool_value(false)]).expect("init serves");
    let pushed = push(&sized, float(1.0));
    let (cleared, _) = serve("clear", &pushed, &[]).expect("clear serves");
    assert!(data_of(&cleared).is_empty());
    assert_eq!(num(&cleared, "head"), None);
}

// ---------------------------------------------------------------------------
// Decline paths (Pine runs exactly as today)
// ---------------------------------------------------------------------------

#[test]
fn unserved_methods_and_types_decline() {
    let fresh = table(vec![], 20, None, 0, false, None);
    let (sized, _) = serve("init", &fresh, &[int(2), bool_value(true)]).expect("init serves");
    for method in [
        "stdev",
        "max",
        "sum",
        "getLinearized",
        "compact",
        "valid_count",
    ] {
        assert!(
            declines(method, &sized, &[], false),
            "{method} stays interpreted"
        );
    }
    {
        // BufferInt stays interpreted in v1: same table, foreign type.
        let mut next = sized.clone();
        let outcome =
            crate::buffers::serve_buffer_method("BufferInt", "push", &mut next, &[int(1)], false);
        assert!(outcome.is_declined(), "BufferInt stays interpreted in v1");
        assert_eq!(next, sized, "decline leaves the table untouched");
    }
    // Push before init (absent data): Pine owns the refusal.
    let no_data = {
        let mut fields = table(vec![], 20, None, 0, false, None);
        fields.insert("data".to_owned(), None);
        fields
    };
    assert!(
        declines("push", &no_data, &[float(1.0)], false),
        "uninitialized buffer declines (Pine refuses identically)"
    );
}

// ---------------------------------------------------------------------------
// Engine agreement: Pine default vs native opt-in over one linked driver
// ---------------------------------------------------------------------------

#[cfg(feature = "pine")]
fn linked_buffers(driver: &str) -> Vec<LinkedModule> {
    use crate::pine::test_fixture as fixture;
    fixture::linked_modules(vec![
        fixture::shipped_module("libraries/_libUtil.pine", "showmethegrail", "libUtil", 12),
        fixture::shipped_module(
            "libraries/_libBuffers.pine",
            "showmethegrail",
            "libBuffers",
            6,
        ),
        fixture::driver_module(driver),
    ])
}

#[cfg(feature = "pine")]
fn script_engine() -> PineEngine {
    PineEngine::new(
        PineRuntimeConfig::new()
            .chart_symbol("SPY".to_owned())
            .chart_period("1".to_owned())
            .build(),
    )
}

#[cfg(feature = "pine")]
fn script_bars() -> Vec<protopyne_types::Bar> {
    use crate::pine::test_fixture as fixture;
    fixture::history_bars("historical/SPY_1M.csv")[..8].to_vec()
}

#[cfg(feature = "pine")]
#[test]
#[ignore = "requires trimmed CSV history fixtures no longer shipped in the tree"]
fn engine_agrees_on_buffer_sequences() {
    const DRIVER: &str = "import showmethegrail/libBuffers/6 as BUF\n\
        var BUF.BufferFloat ring = BUF.BufferFloat.new().init(4, true)\n\
        var BUF.BufferFloat arr = BUF.BufferFloat.new().init(3)\n\
        ring.push(close)\n\
        arr.push(close)\n\
        newest = ring.get(0)\n\
        oldest = ring.get(3)\n\
        oob = ring.get(9)\n\
        plot(newest, title='newest')\n";
    let modules = linked_buffers(DRIVER);
    let bars = script_bars();
    let pine = {
        let mut engine = script_engine();
        engine
            .run_linked(&modules, &bars, InputSet::new())
            .expect("pine run evaluates")
    };
    let native = {
        let mut engine = script_engine();
        engine
            .set_native_options(native_on())
            .expect("opt-in applies");
        engine
            .run_linked(&modules, &bars, InputSet::new())
            .expect("native run evaluates")
    };
    assert_eq!(
        pine.evaluated, native.evaluated,
        "same bar count under both positions"
    );
    assert_eq!(
        pine.snapshots.len(),
        native.snapshots.len(),
        "same snapshot count"
    );
    for (pine_snap, native_snap) in pine.snapshots.iter().zip(native.snapshots.iter()) {
        assert_eq!(pine_snap.bar, native_snap.bar, "snapshot bar index");
        assert_eq!(
            pine_snap.values.len(),
            native_snap.values.len(),
            "snapshot width on bar {}",
            pine_snap.bar
        );
        for (name, pine_value) in &pine_snap.values {
            let native_value = native_snap
                .values
                .get(name)
                .unwrap_or_else(|| panic!("native drops snapshot {name}"));
            assert_eq!(
                pine_value, native_value,
                "snapshot {name} on bar {}",
                pine_snap.bar
            );
        }
    }
    assert_eq!(pine.plots.len(), native.plots.len(), "same plot count");
    for (pine_plot, native_plot) in pine.plots.iter().zip(native.plots.iter()) {
        assert_eq!(pine_plot.title, native_plot.title, "plot title");
        assert_eq!(
            pine_plot.values, native_plot.values,
            "plot values for {}",
            pine_plot.title
        );
    }
}
