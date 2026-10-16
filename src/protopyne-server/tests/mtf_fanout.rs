//! Shared-`MtfStore` per-timeframe fan-out proof: one store over the real
//! `SPY_1M` tape serves three span jobs (1M/5M/15M) through the shared
//! [`ResultPool`](fluent_concurrency::pool::ResultPool), and the pooled
//! contexts equal serial reads exactly, rerun byte-identically, and agree
//! with the Pine oracle within the frozen eps.
//!
//! Placement: forwarded from `backtest.rs` (sibling of the shared
//! `common_mtf` harness it composes), not from `runner.rs` — the
//! multi-symbol determinism test lives beside the runner, while this file
//! proves the store-sharing fan-out pattern the milestone wires the
//! backtest paths through. Production backtest legs share no MTF state
//! (each leg owns its history and venue), so the shared store appears here
//! as the pattern proof, never as unread theater inside the decide path.
//!
//! Pine legs come from the store's own frames (confirmed buckets only), so
//! both legs run bit-identical series by construction: resample
//! equivalence itself is `bars`/`mtf` territory, already proven, and is
//! not this test's job. The `tf` field is excluded from comparison (the
//! engine reports chart-relative `C`, native names the span — locked).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::DateTime;
use protopyne_kernels::{
    builtin_registry, fetch_all, parse_pine_source, parse_program, resolve_imports, LibraryId,
    LinkedModule, Lookahead, MarketContext, MarketContextKey, ModuleInput, MtfStore, PineValue,
    RunOutcome, UserInstance,
};
use protopyne_types::{Bar, Timeframe};
use rust_decimal::Decimal;

use super::common_mtf::{data_root, engine_for, linked_from_graph, load_history};

/// Frozen agreement tolerance shared with the kernel-vs-Pine matrix.
const EPS: f64 = 1e-9;

const APEX_IMPORTS: &str = "import showmethegrail/libUtil/12 as UTIL\n\
     import showmethegrail/libBuffers/6 as libBuffers\n\
     import showmethegrail/libBars/12 as BARS\n\
     import showmethegrail/libDrawing/61 as libDrawing\n\
     import showmethegrail/libApex/88 as libApex\n";
const CTX_DRIVER: &str =
    "var ctx = libApex.MarketContext.new()\nif barstate.isfirst\n    ctx.init()\nctx.fetch()\n";

/// One shipped library as a graph input (pattern cited from the kernels
/// freeze suite).
fn shipped(relpath: &str, publisher: &str, name: &str, version: u64) -> ModuleInput {
    let text = std::fs::read_to_string(data_root().join(relpath)).expect("shipped library loads");
    ModuleInput::new(
        LibraryId {
            publisher: publisher.to_owned(),
            name: name.to_owned(),
            version,
        },
        parse_pine_source(&format!("{publisher}/{name}"), &text),
        parse_program(&text).expect("shipped library parses"),
    )
}

/// The five apex libraries plus one context driver, linked
/// dependency-first. Exactly the five — the wider closure carries
/// libraries whose top-level code observes `request.security` and only
/// runs with full injections, which is the headless proof's job, not
/// this test's.
fn linked_apex(driver: &str) -> Vec<LinkedModule> {
    let mut inputs = vec![
        shipped("libraries/_libUtil.pine", "showmethegrail", "libUtil", 12),
        shipped(
            "libraries/_libBuffers.pine",
            "showmethegrail",
            "libBuffers",
            6,
        ),
        shipped("libraries/_libBars.pine", "showmethegrail", "libBars", 12),
        shipped(
            "libraries/_libDrawing.pine",
            "showmethegrail",
            "libDrawing",
            61,
        ),
        shipped("libraries/_libApex.pine", "showmethegrail", "libApex", 88),
    ];
    inputs.push(ModuleInput::new(
        LibraryId {
            publisher: "local".to_owned(),
            name: "Driver".to_owned(),
            version: 1,
        },
        parse_pine_source("local/Driver", driver),
        parse_program(driver).expect("driver parses"),
    ));
    let graph = resolve_imports(inputs).expect("apex libraries resolve");
    linked_from_graph(&graph)
}

fn ctx_at(outcome: &RunOutcome, bar: usize) -> UserInstance {
    match outcome.value_at(bar, "ctx") {
        Some(Some(PineValue::Object(instance))) => instance,
        other => panic!("expected MarketContext object for ctx on bar {bar}, got {other:?}"),
    }
}

fn pine_field(instance: &UserInstance, name: &str) -> Option<f64> {
    match instance.fields.get(name) {
        Some(None) => None,
        Some(Some(PineValue::Float(value))) => Some(*value),
        Some(Some(PineValue::Int(value))) => Some(*value as f64),
        other => panic!("expected number-or-na field {name}, got {other:?}"),
    }
}

fn pine_enum(instance: &UserInstance, name: &str) -> String {
    match instance.fields.get(name) {
        Some(Some(PineValue::EnumHandle(handle))) => handle.member.clone(),
        other => panic!("expected enum field {name}, got {other:?}"),
    }
}

/// One store frame as engine legs: f64 legs round-trip through `Decimal`
/// exactly (`from_f64` emits the shortest repr, which parses back to the
/// same bits), stamps and integral volumes are exact.
fn frame_bars(store: &MtfStore, span_ms: i64) -> Vec<Bar> {
    let stamps = store.frame_stamps(span_ms).expect("span materialized");
    let frame = store.frame(span_ms).expect("span materialized");
    let decimal = |value: f64| Decimal::from_f64_retain(value).expect("f64 converts");
    stamps
        .iter()
        .enumerate()
        .map(|(i, stamp)| {
            Bar::new()
                .ts(DateTime::from_timestamp_millis(*stamp).expect("stamp in range"))
                .o(decimal(frame.open[i]))
                .h(decimal(frame.high[i]))
                .l(decimal(frame.low[i]))
                .c(decimal(frame.close[i]))
                .volume(frame.volume[i] as u64)
                .maybe_adj(None)
                .build()
        })
        .collect()
}

fn native_fields(ctx: &MarketContext) -> Vec<(&'static str, Option<f64>)> {
    vec![
        ("ma_primary", ctx.ma_primary),
        ("ma_signal", ctx.ma_signal),
        ("ma_mean", ctx.ma_mean),
        ("atr", ctx.atr),
        ("atr_long", ctx.atr_long),
        ("cci", ctx.cci),
        ("rsi", ctx.rsi),
        ("stdev", ctx.stdev),
        ("vol", ctx.vol),
        ("vol_sma", ctx.vol_sma),
        ("atr_distance", ctx.atr_distance),
        ("ma_health", ctx.ma_health),
        ("volatility_ratio", ctx.volatility_ratio),
        ("vol_ratio", ctx.vol_ratio),
        ("trend_strength", ctx.trend_strength),
        ("trend_direction", ctx.trend_direction),
    ]
}

/// Pine-vs-native agreement over one span: regime exact, every numeric
/// field na-matched then within [`EPS`].
fn assert_span_agrees(span_ms: i64, pine: &UserInstance, native: &MarketContext) {
    assert_eq!(
        pine_enum(pine, "regime"),
        native.regime.to_string(),
        "{span_ms}ms regime"
    );
    for (field, got) in native_fields(native) {
        match (pine_field(pine, field), got) {
            (None, None) => {}
            (Some(want), Some(got)) => {
                assert!(
                    (want - got).abs() < EPS,
                    "{span_ms}ms {field}: pine {want} vs native {got}"
                );
            }
            (want, got) => {
                panic!("{span_ms}ms {field}: na-ness diverges (pine {want:?}, native {got:?})")
            }
        }
    }
}

struct SpanJob {
    span_ms: i64,
    key: MarketContextKey,
    now_ms: i64,
}

/// Fan the span jobs out over the shared store through one bounded pool:
/// workers clamp to 1..=8, the queue fits exactly one slot per span, and
/// outcomes return sorted by span so completion order never leaks into
/// results. The lock guards only synchronous ensure/read work — no `await`
/// runs under it.
async fn pooled_contexts(
    store: Arc<Mutex<MtfStore>>,
    jobs: Vec<SpanJob>,
) -> Result<Vec<(i64, MarketContext)>, String> {
    let workers = jobs.len().clamp(1, 8);
    let pool = fluent_concurrency::pool::ResultPool::new(
        fluent_concurrency::tokio_runtime(),
        workers,
        jobs.len().max(1),
        move |job: SpanJob| {
            let store = Arc::clone(&store);
            async move {
                let registry = builtin_registry().map_err(|e| e.to_string())?;
                let mut guard = common_core::sync::lock(&store);
                let contexts = fetch_all(
                    &mut guard,
                    &registry,
                    std::slice::from_ref(&job.key),
                    &HashMap::new(),
                    job.now_ms,
                    Lookahead::Off,
                )
                .map_err(|e| e.to_string())?;
                let ctx = contexts.into_iter().next().expect("one context");
                Ok::<(i64, MarketContext), String>((job.span_ms, ctx))
            }
        },
    );
    let mut out = Vec::with_capacity(jobs.len());
    for job in jobs {
        let (span_ms, ctx) = pool
            .submit(job)
            .await
            .map_err(|e| format!("span submit failed: {e:?}"))?;
        out.push((span_ms, ctx));
    }
    pool.shutdown().await;
    out.sort_by_key(|(span_ms, _)| *span_ms);
    Ok(out)
}

fn span_jobs(last_ts_ms: i64) -> Vec<SpanJob> {
    [
        (Timeframe::M1, 60_000),
        (Timeframe::M5, 300_000),
        (Timeframe::M15, 900_000),
    ]
    .into_iter()
    .map(|(tf, span_ms)| {
        let key = MarketContextKey::new(tf);
        assert_eq!(key.tf_ms(), span_ms, "key names its span");
        SpanJob {
            span_ms,
            key,
            now_ms: last_ts_ms + span_ms,
        }
    })
    .collect()
}

#[tokio::test]
async fn shared_store_fans_out_per_span_with_pine_agreement() {
    let base = load_history("SPY_1M.csv");
    assert!(!base.is_empty(), "minute history loads");
    let last_ts_ms = base.last().expect("nonempty").ts.timestamp_millis();
    let store = Arc::new(Mutex::new(MtfStore::new()));
    common_core::sync::lock(&store)
        .set_base(&base)
        .expect("base loads");

    // Serial reads on the shared store first: the pool must reproduce these
    // exactly, so any perturbation the fan-out introduces fails here.
    let mut serial = Vec::new();
    for job in span_jobs(last_ts_ms) {
        let registry = builtin_registry().expect("builtins");
        let mut guard = common_core::sync::lock(&store);
        let contexts = fetch_all(
            &mut guard,
            &registry,
            std::slice::from_ref(&job.key),
            &HashMap::new(),
            job.now_ms,
            Lookahead::Off,
        )
        .expect("serial fetch fills");
        serial.push((
            job.span_ms,
            contexts.into_iter().next().expect("one context"),
        ));
    }
    serial.sort_by_key(|(span_ms, _)| *span_ms);

    let pooled = pooled_contexts(Arc::clone(&store), span_jobs(last_ts_ms))
        .await
        .expect("pooled fan-out runs");
    assert_eq!(pooled, serial, "pooled reads equal serial reads exactly");

    // Rerun equality under the pool: same bytes twice.
    let again = pooled_contexts(Arc::clone(&store), span_jobs(last_ts_ms))
        .await
        .expect("pooled rerun runs");
    assert_eq!(again, pooled, "pooled reruns are byte-identical");

    // One store served every span: frames materialized once, shared by all
    // readers — no per-span store, no recompute per reader.
    {
        let guard = common_core::sync::lock(&store);
        for span_ms in [60_000, 300_000, 900_000] {
            assert!(
                guard.frame_stamps(span_ms).is_some(),
                "{span_ms}ms frame lives in the shared store"
            );
        }
    }

    // Pine agreement per span over the store's own frames: identical series
    // on both legs, so this gates the pooled numbers, not resampling. Each
    // span charts its own timeframe, so the no-arg `fetch` takes the chart
    // branch over matching legs (a mismatched chart would take the
    // `request.security` branch and refuse headlessly — by design).
    let modules = linked_apex(&format!("{APEX_IMPORTS}{CTX_DRIVER}"));
    let mut max_diff = 0.0f64;
    for (span_ms, native) in &pooled {
        let legs = {
            let guard = common_core::sync::lock(&store);
            frame_bars(&guard, *span_ms)
        };
        assert!(!legs.is_empty(), "{span_ms}ms legs nonempty");
        let period = match span_ms {
            60_000 => "1",
            300_000 => "5",
            900_000 => "15",
            other => panic!("unexpected span {other}"),
        };
        let mut engine = engine_for(period);
        let outcome = engine
            .run_linked(&modules, &legs, protopyne_kernels::InputSet::new())
            .expect("apex run evaluates");
        let pine = ctx_at(&outcome, legs.len() - 1);
        assert_span_agrees(*span_ms, &pine, native);
        for (field, got) in native_fields(native) {
            if let (Some(want), Some(got)) = (pine_field(&pine, field), got) {
                max_diff = max_diff.max((want - got).abs());
            }
        }
    }
    assert!(
        max_diff < EPS,
        "pooled max-abs-diff {max_diff} under frozen eps"
    );
}
