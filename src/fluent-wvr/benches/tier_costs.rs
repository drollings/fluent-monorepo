//! Soak-tier cost table for the reflection tiers (M8.t1).
//!
//! Measures direct field access vs `serde_json::from_value` vs `set_field`
//! vs `HashMap` access on 3/12/40-field fixtures, plus one `dyn Component`
//! call, `Instrumented` overhead with and without a subscriber, and one
//! `MiddlewareChain` layer. Each bench asserts one correctness property of
//! its fixture first, so the harness fails only on harness breakage
//! (missing fixture, non-hermetic clock use) — never on absolute nanos.
//! Ratios are recorded from the criterion report; see the skill guide.
//!
//! Run: `cargo bench -p fluent-wvr`. Never runs in regular gates.

#![recursion_limit = "256"]

use criterion::{criterion_group, criterion_main, Criterion};
use fluent_wvr::prelude::*;
use std::collections::HashMap;
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

// ── Fixtures: the same struct serves Tiers 1–3, so fixtures cannot drift ──

#[derive(
    Clone,
    Default,
    fluent_wvr::FieldAccess,
    fluent_wvr::Describable,
    serde::Serialize,
    serde::Deserialize,
)]
#[allow(dead_code)]
/// ── Fixtures: the same struct serves Tiers 1–3, so fixtures cannot drift ──
struct S3 {
    name: String,
    count: u64,
    enabled: bool,
}

#[derive(
    Clone,
    Default,
    fluent_wvr::FieldAccess,
    fluent_wvr::Describable,
    serde::Serialize,
    serde::Deserialize,
)]
#[allow(dead_code)]
#[allow(clippy::struct_excessive_bools)]
struct S12 {
    f00: String,
    f01: u64,
    f02: bool,
    f03: String,
    f04: u64,
    f05: bool,
    f06: String,
    f07: u64,
    f08: bool,
    f09: String,
    f10: u64,
    f11: bool,
}

#[derive(
    Clone,
    Default,
    fluent_wvr::FieldAccess,
    fluent_wvr::Describable,
    serde::Serialize,
    serde::Deserialize,
)]
#[allow(dead_code)]
#[allow(clippy::struct_excessive_bools)]
struct S40 {
    f00: String,
    f01: u64,
    f02: bool,
    f03: String,
    f04: u64,
    f05: bool,
    f06: String,
    f07: u64,
    f08: bool,
    f09: String,
    f10: u64,
    f11: bool,
    f12: String,
    f13: u64,
    f14: bool,
    f15: String,
    f16: u64,
    f17: bool,
    f18: String,
    f19: u64,
    f20: bool,
    f21: String,
    f22: u64,
    f23: bool,
    f24: String,
    f25: u64,
    f26: bool,
    f27: String,
    f28: u64,
    f29: bool,
    f30: String,
    f31: u64,
    f32: bool,
    f33: String,
    f34: u64,
    f35: bool,
    f36: String,
    f37: u64,
    f38: bool,
    f39: String,
}

macro_rules! impl_unit {
    ($ty:ty, $label:literal) => {
        impl WorkUnit for $ty {
            fn name(&self) -> &str {
                $label
            }
            fn depends(&self) -> &[ArcIntern<str>] {
                &[]
            }
            fn provides(&self) -> &[ArcIntern<str>] {
                &[]
            }
            fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
                Ok(WorkOutput::ok($label))
            }
        }
        fluent_wvr::impl_component!($ty);
    };
}

impl_unit!(S3, "s3");
impl_unit!(S12, "s12");
impl_unit!(S40, "s40");

/// Set fields round-robin by name with a parseable value per position, so
/// wide-struct dispatch pays the full chain length. Positions cycle
/// String → u64 → bool, matching every fixture's field order.
fn round_robin_value(position: usize) -> &'static str {
    match position % 3 {
        0 => "x",
        1 => "42",
        _ => "true",
    }
}

fn bench_tier1_direct(c: &mut Criterion) {
    let mut group = c.benchmark_group("tier1_direct_write");
    let mut s3 = S3 {
        count: 1,
        ..Default::default()
    };
    assert_eq!(s3.count, 1);
    group.bench_function("3_fields", |b| {
        b.iter(|| s3.count = black_box(7));
    });
    let mut s12 = S12 {
        f07: 1,
        ..Default::default()
    };
    assert_eq!(s12.f07, 1);
    group.bench_function("12_fields", |b| {
        b.iter(|| s12.f07 = black_box(7));
    });
    let mut s40 = S40 {
        f37: 1,
        ..Default::default()
    };
    assert_eq!(s40.f37, 1);
    group.bench_function("40_fields", |b| {
        b.iter(|| s40.f37 = black_box(7));
    });
    group.finish();
}

fn bench_tier2_serde(c: &mut Criterion) {
    let mut group = c.benchmark_group("tier2_serde_from_value");
    let value3 = serde_json::to_value(S3 {
        name: "n".into(),
        count: 1,
        enabled: true,
    })
    .expect("fixture serializes");
    let back: S3 = serde_json::from_value(value3.clone()).expect("fixture round-trips");
    assert_eq!(back.count, 1);
    group.bench_function("3_fields", |b| {
        b.iter(|| black_box(serde_json::from_value::<S3>(black_box(value3.clone()))));
    });
    let value12 = serde_json::to_value(S12::default()).expect("fixture serializes");
    let _: S12 = serde_json::from_value(value12.clone()).expect("fixture round-trips");
    group.bench_function("12_fields", |b| {
        b.iter(|| black_box(serde_json::from_value::<S12>(black_box(value12.clone()))));
    });
    let value40 = serde_json::to_value(S40::default()).expect("fixture serializes");
    let _: S40 = serde_json::from_value(value40.clone()).expect("fixture round-trips");
    group.bench_function("40_fields", |b| {
        b.iter(|| black_box(serde_json::from_value::<S40>(black_box(value40.clone()))));
    });
    group.finish();
}

fn bench_tier3_set_field(c: &mut Criterion) {
    let mut group = c.benchmark_group("tier3_set_field");
    let names3 = ["name", "count", "enabled"];
    let mut s3 = S3::default();
    s3.set_field("count", "1").expect("fixture parses");
    assert_eq!(s3.get_field("count").expect("reads"), "1");
    group.bench_function("3_fields", |b| {
        let mut i = 0usize;
        b.iter(|| {
            let position = i % names3.len();
            s3.set_field(
                black_box(names3[position]),
                black_box(round_robin_value(position)),
            )
            .expect("fixture value must parse");
            i = i.wrapping_add(1);
        });
    });
    let names12 = [
        "f00", "f01", "f02", "f03", "f04", "f05", "f06", "f07", "f08", "f09", "f10", "f11",
    ];
    let mut s12 = S12::default();
    s12.set_field("f07", "1").expect("fixture parses");
    group.bench_function("12_fields", |b| {
        let mut i = 0usize;
        b.iter(|| {
            let position = i % names12.len();
            s12.set_field(
                black_box(names12[position]),
                black_box(round_robin_value(position)),
            )
            .expect("fixture value must parse");
            i = i.wrapping_add(1);
        });
    });
    let names40 = [
        "f00", "f01", "f02", "f03", "f04", "f05", "f06", "f07", "f08", "f09", "f10", "f11", "f12",
        "f13", "f14", "f15", "f16", "f17", "f18", "f19", "f20", "f21", "f22", "f23", "f24", "f25",
        "f26", "f27", "f28", "f29", "f30", "f31", "f32", "f33", "f34", "f35", "f36", "f37", "f38",
        "f39",
    ];
    let mut s40 = S40::default();
    s40.set_field("f37", "1").expect("fixture parses");
    group.bench_function("40_fields", |b| {
        let mut i = 0usize;
        b.iter(|| {
            let position = i % names40.len();
            s40.set_field(
                black_box(names40[position]),
                black_box(round_robin_value(position)),
            )
            .expect("fixture value must parse");
            i = i.wrapping_add(1);
        });
    });
    group.finish();
}

fn bench_tier4_hashmap(c: &mut Criterion) {
    let mut group = c.benchmark_group("tier4_hashmap_get");
    let build = |names: &[&str]| {
        let mut map: HashMap<ArcIntern<str>, serde_json::Value> = HashMap::new();
        for name in names {
            map.insert((*name).into(), serde_json::json!(1));
        }
        assert!(map.contains_key(&ArcIntern::from(names[0])));
        map
    };
    let map3 = build(&["name", "count", "enabled"]);
    group.bench_function("3_fields", |b| {
        let keys = ["name", "count", "enabled"];
        let mut i = 0usize;
        b.iter(|| {
            let key: ArcIntern<str> = keys[i % keys.len()].into();
            black_box(map3.get(black_box(&key)));
            i = i.wrapping_add(1);
        });
    });
    let names12: Vec<String> = (0..12).map(|i| format!("f{i:02}")).collect();
    let refs12: Vec<&str> = names12.iter().map(String::as_str).collect();
    let map12 = build(&refs12);
    group.bench_function("12_fields", |b| {
        let mut i = 0usize;
        b.iter(|| {
            let key: ArcIntern<str> = refs12[i % refs12.len()].into();
            black_box(map12.get(black_box(&key)));
            i = i.wrapping_add(1);
        });
    });
    let names40: Vec<String> = (0..40).map(|i| format!("f{i:02}")).collect();
    let refs40: Vec<&str> = names40.iter().map(String::as_str).collect();
    let map40 = build(&refs40);
    group.bench_function("40_fields", |b| {
        let mut i = 0usize;
        b.iter(|| {
            let key: ArcIntern<str> = refs40[i % refs40.len()].into();
            black_box(map40.get(black_box(&key)));
            i = i.wrapping_add(1);
        });
    });
    group.finish();
}

fn bench_dyn_call(c: &mut Criterion) {
    let comp: Arc<dyn Component> = Arc::new(S12::default());
    let ctx = WorkContext::default();
    comp.execute(&ctx).expect("fixture executes");
    c.bench_function("dyn_component_execute", |b| {
        b.iter(|| black_box(comp.execute(black_box(&ctx))));
    });
}

/// Minimal counting subscriber: hermetic, measures real dispatch overhead
/// without depending on a formatting subscriber.
struct CountingSubscriber {
    events: AtomicU64,
}

impl tracing::Subscriber for CountingSubscriber {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, _: &tracing::Event<'_>) {
        self.events.fetch_add(1, Ordering::Relaxed);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn bench_instrumented(c: &mut Criterion) {
    let mut group = c.benchmark_group("instrumented_execute");
    let bare: Arc<dyn Component> = Arc::new(S12::default());
    let wrapped: Arc<dyn Component> = Arc::new(Instrumented::new(S12::default(), "bench"));
    let ctx = WorkContext::default();
    wrapped.execute(&ctx).expect("fixture executes");
    group.bench_function("bare_baseline", |b| {
        b.iter(|| black_box(bare.execute(black_box(&ctx))));
    });
    group.bench_function("no_subscriber", |b| {
        b.iter(|| black_box(wrapped.execute(black_box(&ctx))));
    });
    let subscriber = CountingSubscriber {
        events: AtomicU64::new(0),
    };
    let dispatch = tracing::dispatcher::Dispatch::new(subscriber);
    group.bench_function("with_subscriber", |b| {
        b.iter(|| {
            tracing::dispatcher::with_default(&dispatch, || {
                black_box(wrapped.execute(black_box(&ctx)))
            })
        });
    });
    group.finish();
}

struct BenchMiddleware;

impl Middleware for BenchMiddleware {
    fn wrap(&self, inner: Arc<dyn Component>) -> Arc<dyn Component> {
        Arc::new(Instrumented::new(inner, "bench-mw"))
    }
}

fn bench_middleware_chain(c: &mut Criterion) {
    let chain = MiddlewareChain::new().push(Box::new(BenchMiddleware));
    let wrapped = chain.apply(Arc::new(S12::default()) as Arc<dyn Component>);
    let ctx = WorkContext::default();
    wrapped.execute(&ctx).expect("fixture executes");
    c.bench_function("middleware_chain_one_layer_execute", |b| {
        b.iter(|| black_box(wrapped.execute(black_box(&ctx))));
    });
}

criterion_group!(
    tiers,
    bench_tier1_direct,
    bench_tier2_serde,
    bench_tier3_set_field,
    bench_tier4_hashmap,
    bench_dyn_call,
    bench_instrumented,
    bench_middleware_chain,
);
criterion_main!(tiers);