# fluent-egui — Domain-Blind Native Chart Scaffolding

**Context**: `fluent-egui` (`src/fluent-egui/`) is the generic paint-edge
crate for native chart UIs. It owns every piece of chart machinery that does
*not* know trading semantics — pane view-models, viewport geometry,
DAG-ordered compute plans, series caches, layout/theme configs, headless
paint geometry plus thin `egui` emitters, feed/wave plumbing, and replay —
so domain crates (e.g. `protopyne-egui`, the trading terminal) pour their
shape into it instead of reimplementing UI infrastructure.

**Prime directive**: display `f64` never flows back into decisions. Bars
arrive already converted (`protopyne-chart-model :: rows_of`); every
function here is `f64`-in/`f64`-out with holes (`None`) preserved, and every
refusal carries a reason (`EguiError`) instead of substituting a value.

## Relation to the sibling skills

| Crate | What fluent-egui takes from it | Where |
|---|---|---|
| `fluent-wvr` | `Component`/`WorkUnit` + `FieldAccess`/`Describable` derives, `bon::Builder`, `impl_component!`, `Instrumented`, `ComponentAdapter`, `check_for_execute` bridge | `config.rs` (`PaneConfig`, `ThemeConfig`, `ReplayState`), `chart.rs`-side timing |
| `fluent-concurrency` | `Scope` (feed ownership), `SupervisedBatch` (recompute waves), `ResultPool` (heavy rebuilds), `Limiter` (refresh gate), `StreamAbort` (pane-close signal), `flow` credit (bounded intake), `first_accept_in_order` (history fallback), `retry_async` (transport retry) | `feed.rs` |
| `fluent-dag` | `DependencyGraph` (pane ordering/readiness/cancellation), `CheckpointedStepGraph` (replay checkpoints/rewind) | `plan.rs`, `replay.rs` |
| `common-core` | `hash :: fnv1a64` (cache keys, content hashes), `retry`, `metrics :: LatencyHistogram` | `plan.rs`, `feed.rs`, `config.rs` |

Rules inherited from those skills apply unchanged: `dyn` at the pane
boundary, concrete types in per-bar loops; `execute` stays
sync/non-blocking/side-effect-free; containment-only supervision (failed
pane cancels its transitive dependents, never auto-restarts); configure
before `Arc`-sharing, override at runtime via `ComponentAdapter`.

## File index

| File | Owns | Key symbols |
|---|---|---|
| `src/view.rs` | Headless pane view-models over `ChartRow` slices | `PaneKind`, `PaneSeries`, `ribbon_view`, `delta_candle_view`, `pivot_marks` |
| `src/viewport.rs` | Pixel geometry on the canonical scale (headless) | `Viewport`, `visible_rows`, `thin_ticks`, `axis_ticks`, `BreakLine` |
| `src/plan.rs` | DAG-ordered pane plan + fingerprint-keyed cache | `PanePlan`, `PaneCache`, `CachedSeries`, `cache_key` |
| `src/config.rs` | Layout/theme control plane | `PaneConfig`, `ThemeConfig` |
| `src/paint.rs` | Headless geometry builders + thin `egui` emitters | `candle_geoms`, `ribbon_bands`, `delta_bar_geoms`, `oscillator_points`, `pivot_dots`, `paint_*` |
| `src/feed.rs` | Feeds, waves, caps, aborts, backpressure | `ChartFeed`, `PaneWaveUnit`, `run_wave`, `RefreshGate`, `PaneHandle`, `CreditIntake`, `load_history`, `TransportRetry` |
| `src/replay.rs` | Scrub-back controller + toolbar cursor | `ReplayController`, `ReplayState` |
| `src/error.rs` | Fail-closed refusals | `EguiError :: InvalidParam/Dependency` |

Conventions: `#![forbid(unsafe_code)]`; `autotests = false` with
`tests/<module>.rs` + `#[path]` forwarders, never inline `mod tests`;
`egui` (required) / `eframe` (optional `demo` feature) live only here and
in the domain terminal crate — no headless or trading crate may depend on
them in either direction (pinned by `tests/budget.rs` + `tests/lib.rs`).

## Key operations

### 1. Build view-models, then geometry, then paint

```rust
use fluent_egui::{ribbon_view, candle_geoms, ribbon_bands, paint_candles, paint_ribbon};

// Pure headless prep over converted rows (no egui types):
let ribbon = ribbon_view(&fast, &slow, 0.5)?;          // upper/lower/bullish, holes preserved
let geoms = candle_geoms(&rows, &ribbon.bullish, &viewport)?;
let bands = ribbon_bands(&ribbon, &viewport);
// Thin emitters inside the frame (blits cached geometry only):
paint_candles(&painter, &geoms, body_width, &theme);
paint_ribbon(&painter, &bands, half_width, &theme, &ribbon.bullish, 0.5);
```

Heads-up: geometry is **pane-local** (origin at the pane's top-left).
Emitters draw exactly the coordinates given, so shift each layer by its
pane rect's origin before painting. Zero-based panes (delta, oscillators)
need a viewport whose range spans zero; `delta_bar_geoms` refuses
otherwise. Colors resolve from `ThemeConfig` style keys at paint time —
series carry keys (`"bull"`, `"fast"`), never literals.

### 2. Order pane compute through the plan, read revisions in the frame

```rust
use fluent_egui::{PanePlan, cache_key};

let mut plan = PanePlan::new();
plan.register_node("price", &[], 0)?;
plan.register_node("ribbon", &["smoothing"], 0)?;
for pane in plan.order()? { /* schedule wave in topo order */ }
let dirty = plan.dirty_set("smoothing");   // failed node + transitive dependents, in plan order
let key = cache_key("ribbon", fingerprint); // length-framed name + ParamSet fingerprint via fnv1a64
let revision = cache.publish(key, values);  // bit-identical republish keeps the revision
```

`PaneCache :: publish` backdates on content hash (`None` slots hash as
`u64::MAX`, so identical `NaN` payloads keep the revision). Revisions are
confidence bookkeeping for blitting — never evidence a signal is
tradeable. Caller owns topology: register domain pane names and edges,
never fork `PanePlan`/`PaneCache`.

### 3. Run recompute as supervised waves, gate the frame path

```rust
use fluent_egui::{PaneWaveUnit, run_wave, RefreshGate, PaneHandle, CreditIntake};

let units: Vec<PaneWaveUnit> =
    panes.iter().map(|p| PaneWaveUnit::for_plan(&plan, p, values)).collect();
let summary = run_wave(units).await?;      // failed unit cancels dependents only
let gate = RefreshGate::new(permits)?;     // preview/refresh concurrency cap
gate.run(|| async { refresh_preview().await }).await;
let handle = PaneHandle::new();
// ... select task reads against handle.abort(); closing the pane:
handle.close();                            // sticky StreamAbort: body-lifetime rule
let intake = CreditIntake::new(limit, more_after)?;  // burst intake stays bounded
```

No raw `tokio :: spawn`, no sleeps, no blocking I/O in `execute` bodies or
paint code. History sources ladder through `load_history` (first-Ok wins,
terminal misses abort, exhaustion is clean `Ok(None)`); transport retries
compose `TransportRetry` over `retry_async`.

### 4. Replay by rewinding the plan, export in server shapes

```rust
use fluent_egui::{ReplayController, ReplayState};

let mut replay = ReplayController::with_plan(plan);
replay.mark("ribbon")?;                    // name a checkpoint
let suffix = replay.rewind_to("ribbon")?;  // un-completes suffix for re-run (mark consumed)
let wave: Vec<String> = replay.ready();    // uncompleted-ready panes in plan order
replay.publish(key, rerun_values);         // unchanged content keeps every revision
```

`ReplayState` (`cursor`/`total_bars`/`mark`) is a `Component` with
`Describable`, so the toolbar renders from its schema. Domain export
(CSV/JSON in server shapes) composes cache revisions — see
`protopyne-egui :: export` for the pattern, not a second serializer.
