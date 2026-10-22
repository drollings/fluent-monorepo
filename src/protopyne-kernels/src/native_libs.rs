//! Native library registry: publisher/name identity with versions ignored.
//!
//! Every shipped `.pine` file stays byte-identical on disk as the executable
//! contract: resolvers keep linking Pine, and nothing here changes that. This
//! module only records which `publisher/name` pairs a future pure-Rust
//! implementation may serve, so link-time wiring can supersede a whole
//! library while the Pine text remains loaded for audit and parity.
//!
//! Identity is publisher and name only, mirroring the graph identity used at
//! link time. A requested version is accepted at the lookup boundary and
//! deliberately ignored: it rides along as metadata on the loaded source and
//! on the audit row, never as a dispatch key. Disabled entries refuse with a
//! reason, exactly like unknown ones, so no caller can observe a behavior
//! change while serving stays off.
//!
//! Whole-library override wiring lives here too: [`apply_overrides`] runs on
//! the link path after `resolve_imports`, marks natively served modules on a
//! [`NativeOverlay`] that borrows the resolved graph (the graph itself is
//! never mutated — Pine stays linked for audit and parity), and routes
//! [`NativeOverlay::resolve_reference`] hits for provided symbols to the
//! native table. Serving defaults on behind [`NativeOptions::enabled`]
//! since the cutover (explicit `enabled: false` keeps Pine-oracle runs);
//! subset-override requests refuse (whole-library granularity only), and
//! version skew across importers warns once and still resolves.

use std::collections::{BTreeSet, HashMap};
use std::fmt;

use crate::error::KernelError;
use crate::market_context::{MarketContext, MarketContextKey};
use crate::mtf::{Lookahead, MtfStore};
use crate::pine::{ModuleGraph, ModuleId, VersionWarn};
use crate::value::{EnumHandle, FieldTable, NativeCall, PineValue, UserInstance};

/// Identity of one natively servable Pine library: publisher and name only.
///
/// Versions never participate in identity — the same pair resolves the same
/// way for every pinned version — matching how the import graph keys modules.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NativeLibraryId {
    /// Publisher namespace (`showmethegrail`).
    pub publisher: String,
    /// Library name (`libBars`).
    pub name: String,
}

impl NativeLibraryId {
    /// Build an identity from parts.
    #[must_use]
    pub fn new(publisher: &str, name: &str) -> Self {
        Self {
            publisher: publisher.to_owned(),
            name: name.to_owned(),
        }
    }
}

impl fmt::Display for NativeLibraryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.publisher, self.name)
    }
}

impl From<&crate::pine::LibraryId> for NativeLibraryId {
    fn from(id: &crate::pine::LibraryId) -> Self {
        Self::new(&id.publisher, &id.name)
    }
}

impl From<&crate::pine::ModuleId> for NativeLibraryId {
    fn from(id: &crate::pine::ModuleId) -> Self {
        Self::new(&id.publisher, &id.name)
    }
}

/// One row of the native table: which `publisher/name` pair may be served
/// natively, and whether serving is currently switched on.
///
/// Rows for planned libraries are present but disabled, so the table
/// documents intent while every lookup still refuses and Pine keeps serving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeLibraryEntry {
    /// Publisher namespace.
    pub publisher: &'static str,
    /// Library name.
    pub name: &'static str,
    /// Whether native serving is switched on for this pair.
    pub enabled: bool,
}

/// Native table: planned libraries present but disabled, so lookups refuse
/// and Pine keeps serving until wiring switches a row on.
pub const NATIVE_LIBRARIES: &[NativeLibraryEntry] = &[
    NativeLibraryEntry {
        publisher: "showmethegrail",
        name: "libBars",
        enabled: false,
    },
    NativeLibraryEntry {
        publisher: "showmethegrail",
        name: "libApex",
        enabled: false,
    },
    NativeLibraryEntry {
        publisher: "showmethegrail",
        name: "libBuffers",
        enabled: false,
    },
];

/// Look up the native identity for `publisher/name`, ignoring `version`.
///
/// Exact match on publisher and name only; the version argument is accepted
/// and never parsed or dispatched on. Unknown pairs — and table rows that
/// are still disabled — refuse with a reason instead of guessing.
pub fn find_native(
    publisher: &str,
    name: &str,
    version: Option<u64>,
) -> Result<NativeLibraryId, KernelError> {
    let _ = version;
    match NATIVE_LIBRARIES
        .iter()
        .find(|entry| entry.publisher == publisher && entry.name == name)
    {
        Some(entry) if entry.enabled => Ok(NativeLibraryId::new(entry.publisher, entry.name)),
        Some(entry) => Err(KernelError::UnknownIndicator(format!(
            "native library '{}/{}' is registered but disabled (served from Pine)",
            entry.publisher, entry.name
        ))),
        None => Err(KernelError::UnknownIndicator(format!(
            "unknown native library '{publisher}/{name}' (no native implementation registered)"
        ))),
    }
}

/// Prove that versions play no role for `publisher/name`: every pinned
/// version from 1 to 99 plus an unpinned request resolves identically, and
/// the pair names a row of the native table.
///
/// Returns false for pairs outside the table — those are not version-ignored
/// native candidates, they are unknown.
#[must_use]
pub fn version_is_ignored(publisher: &str, name: &str) -> bool {
    if !NATIVE_LIBRARIES
        .iter()
        .any(|entry| entry.publisher == publisher && entry.name == name)
    {
        return false;
    }
    let first = outcome_key(publisher, name, None);
    (1..=99).all(|version| outcome_key(publisher, name, Some(version)) == first)
}

/// Which implementation served a library link: the Pine oracle or Rust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeServed {
    /// Served from the shipped Pine source.
    Pine,
    /// Served from the native implementation.
    Rust,
}

impl fmt::Display for NativeServed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pine => f.write_str("pine"),
            Self::Rust => f.write_str("rust"),
        }
    }
}

/// One audit row for a library link: what was requested against what served.
///
/// The requested version and the Pine SHA travel as metadata only; serving
/// never branches on them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeAudit {
    /// Requested publisher namespace.
    pub requested_publisher: String,
    /// Requested library name.
    pub requested_name: String,
    /// Requested version, or `None` when unpinned.
    pub requested_version: Option<u64>,
    /// Which implementation served the link.
    pub served: NativeServed,
    /// Hex digest of the Pine source kept loaded for audit/parity.
    pub pine_sha: String,
}

impl NativeAudit {
    /// Record one library link.
    #[must_use]
    pub fn new(
        publisher: &str,
        name: &str,
        version: Option<u64>,
        served: NativeServed,
        pine_sha: &str,
    ) -> Self {
        Self {
            requested_publisher: publisher.to_owned(),
            requested_name: name.to_owned(),
            requested_version: version,
            served,
            pine_sha: pine_sha.to_owned(),
        }
    }
}

impl fmt::Display for NativeAudit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let version = self
            .requested_version
            .map_or_else(|| "unpinned".to_owned(), |version| version.to_string());
        write!(
            f,
            "native audit: requested {}/{} version {} served {} pine_sha {}",
            self.requested_publisher, self.requested_name, version, self.served, self.pine_sha
        )
    }
}

/// Comparable outcome of one lookup: the served identity on success, the
/// refusal text otherwise. Versions that resolve identically share a key.
fn outcome_key(publisher: &str, name: &str, version: Option<u64>) -> String {
    match find_native(publisher, name, version) {
        Ok(id) => format!("ok:{id}"),
        Err(err) => format!("err:{err}"),
    }
}

/// One native implementation behind a table row: the Pine-visible symbols it
/// supersedes. Whole-library granularity only — a provider names the full
/// export surface it serves, never a subset, so two runtimes can never
/// split one logical object (single-owner `varip` state stays whole).
pub trait NativeProvider: Send + Sync {
    /// Pine-visible symbol names served natively (`Bars`, free functions,
    /// methods by bare name).
    fn provides(&self) -> &[&str];

    /// Whether served calls need the tick frame (legs, stamps, bar flags).
    /// Tick-path providers (today `libBars`) keep this `true`: their frame
    /// is meaningless on closed-bar runs. Legs-independent providers (today
    /// `libBuffers`, whose methods are pure functions of fields plus args)
    /// return `false` so the engine serves them on closed-bar runs too.
    fn needs_tick_frame(&self) -> bool {
        true
    }

    /// Attempt one native instance-method call by mutating `fields` in
    /// place and yielding the call outcome. `Declined` means not handled:
    /// fields are untouched and the engine runs the Pine body, so every
    /// unlisted method keeps its proven behavior. `Served(value)`
    /// reports the table already updated. `Err` refuses fail-closed — a
    /// provider that starts a call finishes it or names why, never a
    /// silent mid-call fallback. Default handles nothing.
    ///
    /// In-place discipline: the taken table is mutated, never copied, and
    /// a `this`-returning serve hands the resident table back by handle
    /// (`FieldTable` is copy-on-write) — per-call full-array copies (clone
    /// plus drop) are what made the interpreted hot paths slow, and
    /// reintroducing them here would reproduce the bottleneck natively.
    fn call(
        &self,
        _type_name: &str,
        _method: &str,
        _fields: &mut FieldTable,
        _args: &[Option<PineValue>],
        _ctx: &MethodCtx,
    ) -> Result<NativeCall, KernelError> {
        Ok(NativeCall::Declined)
    }

    /// Whether this provider serves closed-bar context reads (the
    /// `MarketContext.fetch` shape) through a caller-owned store. Default
    /// false; context providers (today `libApex`) return true so the engine
    /// routes those calls with the run's bar tape instead of any tick frame.
    fn serves_bar_context(&self) -> bool {
        false
    }

    /// Serve one closed-bar context read over the engine's resident store
    /// (already fed with chart bars through the cursor). Default declines;
    /// context providers resolve the bundle, run the shared context fold,
    /// and write the Pine field table. Guards decline before touching
    /// fields, so a decline always runs the Pine body from the taken state.
    fn serve_bar_context(&self, _serve: BarContextServe<'_>) -> Result<NativeCall, KernelError> {
        Ok(NativeCall::Declined)
    }
}

/// Assemble one native `fetch` tick from a call-site frame: the bar-so-far
/// legs plus the confirm legs the Pine body would observe (built from the
/// same legs when flagged). The hydrate-every-tick bridge and the
/// resident-ring lease share this assembler, so the frame Pine observes
/// cannot fork between the two hydration modes.
pub fn fetch_tick_from_frame(ctx: &MethodCtx) -> crate::bars::NativeFetchTick {
    crate::bars::NativeFetchTick {
        legs: crate::indicator::BarTick {
            open: ctx.legs.open,
            high: ctx.legs.high,
            low: ctx.legs.low,
            close: ctx.legs.close,
            volume: ctx.legs.volume,
        },
        tick_ms: ctx.tick_ms,
        bar_open_ms: ctx.bar_open_ms,
        is_new: ctx.is_new,
        is_confirmed: ctx.is_confirmed,
        confirm: ctx.is_confirmed.then_some(crate::bars::ConfirmedBar {
            bar_open_ms: ctx.bar_open_ms,
            o: ctx.legs.open,
            h: ctx.legs.high,
            l: ctx.legs.low,
            c: ctx.legs.close,
            volume: ctx.legs.volume,
        }),
    }
}

/// Key of one leased object slot: the plain-variable homes a resident ring
/// may serve. Nested, map-entry, and lexical-receiver takes never lease —
/// they run the hydrate-every-tick bridge as today (correct, just
/// uncached). Frame and store homes with the same name stay distinct keys,
/// matching the take order that prefers the frame.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LeaseKey {
    /// Plain frame binding (`self.variables`).
    Variables(String),
    /// Persistent store binding (`var`/`varip` with the flag).
    VarStore(String, bool),
}

impl LeaseKey {
    /// Build the frame-store key for a plain variable home.
    #[must_use]
    pub fn variables(name: &str) -> Self {
        Self::Variables(name.to_owned())
    }

    /// Build the persistent-store key for a `var`/`varip` home.
    #[must_use]
    pub fn var_store(name: &str, is_varip: bool) -> Self {
        Self::VarStore(name.to_owned(), is_varip)
    }
}

/// One cached resident ring: the ring itself, its cursor, and the table
/// version it was synced at. A hit serves with zero hydrate; any version
/// skew re-hydrates instead of guessing.
#[derive(Debug)]
pub struct LeasedRing {
    /// Resident ring, current through the last served tick.
    pub ring: crate::bars::BarRing,
    /// Cursor the next serve advances from (non-new ticks).
    pub cursor: crate::bars::RingCursor,
    /// Table version at the last sync.
    pub version: u64,
}

/// NOT-A-LADDER: commit-vs-fallback telemetry for the leased `fetch` seam: `commits`
/// counts resident-ring serves with zero hydrate (the `Commit` signal
/// carried through to a served call value); `fallbacks` counts every fetch
/// through the seam that did not commit — lease misses (provider bridge
/// plus re-hydrate), lease-hit declines (Pine body takes over, entry
/// dropped), and errors (entry dropped, next tick re-hydrates).
/// Slot-based takes run the uncached bridge outside the lease and are not
/// counted. Values are observed (acceptance record, benches), never
/// asserted: pinning them would turn a telemetry change into a build
/// break.
#[derive(Debug, Clone, Copy, Default)]
pub struct FetchCounters {
    commits: u64,
    fallbacks: u64,
}

impl FetchCounters {
    /// Zero-hydrate resident-ring serves.
    #[must_use]
    pub fn commits(self) -> u64 {
        self.commits
    }

    /// Seam fetches that re-hydrated, declined, or errored instead.
    #[must_use]
    pub fn fallbacks(self) -> u64 {
        self.fallbacks
    }

    /// Record one zero-hydrate serve.
    pub(crate) fn record_commit(&mut self) {
        self.commits += 1;
    }

    /// Record one miss, decline, or error at the seam.
    pub(crate) fn record_fallback(&mut self) {
        self.fallbacks += 1;
    }
}

/// Whole-library native implementation of `showmethegrail/libBars`: the
/// concrete ring, tick accumulator, and resampler from the kernels surface,
/// served behind the Pine-visible names below.
pub struct LibBarsProvider;

impl NativeProvider for LibBarsProvider {
    fn provides(&self) -> &[&str] {
        &[
            "Bars",
            "calculateDriftPct",
            "estimateBuyVolume",
            "getLTFBoundary",
            "getCandleIndex",
            "isValidVolume",
        ]
    }

    /// Serve the per-tick `Bars.fetch` hot path natively (v1 scope: zero
    /// arguments only). Any other method, any argument shape, or any
    /// capacity-exhaustion/pull signal declines so the engine
    /// runs the Pine body — including its identical lower-tf refusal.
    /// State flows through the Pine field table both ways: the table stays
    /// the single source of truth, native execution a pure transition.
    fn call(
        &self,
        type_name: &str,
        method: &str,
        fields: &mut FieldTable,
        args: &[Option<PineValue>],
        ctx: &MethodCtx,
    ) -> Result<NativeCall, KernelError> {
        if type_name != "Bars" || method != "fetch" || !args.is_empty() {
            return Ok(NativeCall::Declined);
        }
        let (mut ring, cursor) = crate::bars::BarRing::from_pine_fields(fields)?;
        let tick = fetch_tick_from_frame(ctx);
        let (outcome, _) = ring.serve_fetch_tick(fields, cursor, type_name, &tick)?;
        Ok(outcome)
    }
}

/// The single registered `libBars` provider. Later libraries add their own
/// statics beside this one; the match in [`provider_for`] stays the one
/// registration site.
static LIB_BARS_PROVIDER: LibBarsProvider = LibBarsProvider;

/// Whole-library native implementation of `showmethegrail/libBuffers`:
/// the `BufferFloat` hot methods (`init/push/get/size/clear`) over the
/// Pine field table. Analytics, `getLinearized`, `compact`, iteration
/// helpers, and all of `BufferInt` decline to Pine — the DVP `reconcile`
/// path only ever calls the hot five (~30k calls/bar), so v1 serves the
/// measured bottleneck and nothing else.
pub struct LibBuffersProvider;

impl NativeProvider for LibBuffersProvider {
    fn provides(&self) -> &[&str] {
        &["BufferFloat"]
    }

    fn needs_tick_frame(&self) -> bool {
        false
    }

    /// Serve the hot methods natively (positional args only). Named args,
    /// bad arity, misshaped state, and unserved methods decline so the
    /// engine runs the Pine body — including its identical refusals.
    /// State flows through the Pine field table both ways, exactly like
    /// the `libBars` bridge: unmapped fields (notably `_linear`, which the
    /// oracle only invalidates on ring writes) pass through byte-identical.
    fn call(
        &self,
        type_name: &str,
        method: &str,
        fields: &mut FieldTable,
        args: &[Option<PineValue>],
        _ctx: &MethodCtx,
    ) -> Result<NativeCall, KernelError> {
        Ok(crate::buffers::serve_buffer_method(
            type_name, method, fields, args, false,
        ))
    }
}

/// The single registered `libBuffers` provider.
static LIB_BUFFERS_PROVIDER: LibBuffersProvider = LibBuffersProvider;

/// Chart-timeframe member of `UTIL.Timeframes`: the only `tf` handle a
/// natively served `fetch()` observes. Pine sets `tf := target_tf` with the
/// default chart member, a no-op on the default init — a foreign member
/// means an earlier explicit fetch moved it, so Pine re-normalizes instead
/// of native pretending.
const CHART_TF_MEMBER: &str = "C";

/// Whole-library native implementation of `showmethegrail/libApex`: the
/// `MarketContext.fetch` chart branch through the shared-store context fold
/// (v1 scope: bare `fetch()` only). Any explicit `target_tf`, any other
/// method, or any misshaped receiver declines so the engine runs the Pine
/// body — including its identical `request.security` branch and refusals.
pub struct LibApexProvider;

/// The single registered `libApex` provider.
static LIB_APEX_PROVIDER: LibApexProvider = LibApexProvider;

impl NativeProvider for LibApexProvider {
    fn provides(&self) -> &[&str] {
        &["MarketContext"]
    }

    fn needs_tick_frame(&self) -> bool {
        false
    }

    fn serves_bar_context(&self) -> bool {
        true
    }

    /// Serve the bare `fetch()` shape: resolve the span bundle, run the
    /// shared context fold over the resident store, and write the seventeen
    /// Pine fields (`tf` untouched — the chart member is already home).
    /// Every guard declines before touching fields; the served value is the
    /// updated object, mirroring Pine's `this` return.
    fn serve_bar_context(&self, serve: BarContextServe<'_>) -> Result<NativeCall, KernelError> {
        let BarContextServe {
            type_name,
            method,
            fields,
            args,
            store,
            key,
            now_ms,
        } = serve;
        if type_name != "MarketContext" || method != "fetch" || !args.is_empty() {
            return Ok(NativeCall::Declined);
        }
        match fields.get("tf") {
            Some(Some(PineValue::EnumHandle(handle))) if handle.member == CHART_TF_MEMBER => {}
            _ => return Ok(NativeCall::Declined),
        }
        let regime_enum = match fields.get("regime") {
            Some(Some(PineValue::EnumHandle(handle))) => handle.enum_name.clone(),
            _ => return Ok(NativeCall::Declined),
        };
        let Ok(registry) = crate::indicator::builtin_registry() else {
            return Ok(NativeCall::Declined);
        };
        // Carry reads the taken table (the previous bar's writes): `vol`
        // and `vol_ratio` persist past guard-failing bars exactly like the
        // Pine `nz` shape, defaulting fresh otherwise.
        let mut prev = MarketContext::fresh(key.tf);
        prev.vol = fields
            .get("vol")
            .and_then(|slot| slot.as_ref())
            .and_then(PineValue::as_float);
        prev.vol_ratio = fields
            .get("vol_ratio")
            .and_then(|slot| slot.as_ref())
            .and_then(PineValue::as_float);
        let prevs = std::collections::HashMap::from([(key.tf_ms(), prev)]);
        let Ok(contexts) = crate::market_context::fetch_all(
            store,
            &registry,
            std::slice::from_ref(key),
            &prevs,
            now_ms,
            Lookahead::Off,
        ) else {
            return Ok(NativeCall::Declined);
        };
        let Some(context) = contexts.into_iter().next() else {
            return Ok(NativeCall::Declined);
        };
        for (name, value) in [
            ("ma_primary", context.ma_primary),
            ("ma_signal", context.ma_signal),
            ("ma_mean", context.ma_mean),
            ("atr", context.atr),
            ("atr_long", context.atr_long),
            ("cci", context.cci),
            ("rsi", context.rsi),
            ("stdev", context.stdev),
            ("vol", context.vol),
            ("vol_sma", context.vol_sma),
            ("atr_distance", context.atr_distance),
            ("ma_health", context.ma_health),
            ("volatility_ratio", context.volatility_ratio),
            ("vol_ratio", context.vol_ratio),
            ("trend_strength", context.trend_strength),
            ("trend_direction", context.trend_direction),
        ] {
            fields.insert(name.to_owned(), value.map(PineValue::Float));
        }
        fields.insert(
            "regime".to_owned(),
            Some(PineValue::EnumHandle(EnumHandle::new(
                &regime_enum,
                &context.regime.to_string(),
            ))),
        );
        let value = PineValue::Object(UserInstance::new(type_name, fields.clone()));
        Ok(NativeCall::Served(Some(value)))
    }
}

/// Registration: the provider behind a table row, if a native
/// implementation exists yet. Membership comes from the one native table;
/// rows without an implementation stay Pine-served and
/// never refuse — refusal would break callers that never opted in.
pub(crate) fn provider_for(publisher: &str, name: &str) -> Option<&'static dyn NativeProvider> {
    if !NATIVE_LIBRARIES
        .iter()
        .any(|entry| entry.publisher == publisher && entry.name == name)
    {
        return None;
    }
    match (publisher, name) {
        ("showmethegrail", "libBars") => Some(&LIB_BARS_PROVIDER),
        ("showmethegrail", "libBuffers") => Some(&LIB_BUFFERS_PROVIDER),
        ("showmethegrail", "libApex") => Some(&LIB_APEX_PROVIDER),
        _ => None,
    }
}

/// Link-time switch for native serving. The single default rule: links
/// route native where a provider exists (`enabled: true` stays the link
/// default — Pine stays linked underneath for audit and parity, and
/// unserved symbols still fall through to Pine); execution stays Pine
/// unless the engine opts in via `set_native_options` (the engine default
/// `enabled: false` is preserved, so existing drivers keep proving exactly
/// what they proved before). Construct explicitly with `enabled: false` for
/// Pine-oracle runs. The row `enabled` flags are lookup intent for
/// `find_native` only (pinned all-disabled — serving follows provider
/// existence under this switch, never a per-library call-site flag).
#[derive(Debug, Clone)]
pub struct NativeOptions {
    /// Whether linked libraries with a native provider serve natively.
    pub enabled: bool,
    /// Symbol-scoped override request. Always refused: overrides apply to a
    /// whole library or not at all, so this field exists only to name the
    /// refusal at the type level (`None` is the only accepted value).
    pub only_symbols: Option<Vec<String>>,
}

impl Default for NativeOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            only_symbols: None,
        }
    }
}

/// Per-tick legs for a natively served method call, converted once at the
/// call cursor (same decimal-to-float reading as the kernel batch path).
#[derive(Debug, Clone, Copy)]
pub struct TickLegs {
    /// Bar-so-far open.
    pub open: f64,
    /// Bar-so-far high.
    pub high: f64,
    /// Bar-so-far low.
    pub low: f64,
    /// Tick price.
    pub close: f64,
    /// Cumulative volume leg.
    pub volume: f64,
}

/// Call-site frame for a natively served instance method: the bar flags and
/// legs the Pine body would observe through its builtins, extracted by the
/// engine before dispatch so providers stay engine-free. Today this is the
/// Bars tick frame (bar-open time where Pine reads `time`, side-channel
/// stamp where it reads `timenow`); future providers extend dispatch with
/// their own frames through new provider methods, never by redefining this
/// one.
#[derive(Debug, Clone, Copy)]
pub struct MethodCtx {
    /// `barstate.isnew` at the call cursor.
    pub is_new: bool,
    /// `barstate.isconfirmed` at the call cursor.
    pub is_confirmed: bool,
    /// Tick legs at the cursor.
    pub legs: TickLegs,
    /// Bar-open milliseconds (`time` leg).
    pub bar_open_ms: i64,
    /// Side-channel tick stamp (`timenow`).
    pub tick_ms: i64,
}

impl MethodCtx {
    /// Detached frame for legs-independent providers (today `libBuffers`):
    /// no tick legs exist on closed-bar runs, and the served methods are
    /// pure functions of fields plus args, so legs read zero and flags
    /// stay quiet. Tick-frame providers never observe this — the engine
    /// only builds it when `needs_tick_frame()` is false.
    #[must_use]
    pub fn detached() -> Self {
        Self {
            is_new: false,
            is_confirmed: false,
            legs: TickLegs {
                open: 0.0,
                high: 0.0,
                low: 0.0,
                close: 0.0,
                volume: 0.0,
            },
            bar_open_ms: 0,
            tick_ms: 0,
        }
    }
}

/// Call-site frame for a closed-bar context read: the engine's resident
/// store already fed with chart bars through the cursor, the span key, and
/// the confirming `now`. Providers read the previous bar's carry from the
/// field table themselves and run the shared context fold — the engine
/// never interprets library fields, and legs-independent routing stays what
/// `needs_tick_frame() == false` promises.
pub struct BarContextServe<'a> {
    /// Receiving Pine type (`MarketContext`).
    pub type_name: &'a str,
    /// Called method (`fetch`).
    pub method: &'a str,
    /// Taken receiver fields (previous bar's writes carry through here).
    pub fields: &'a mut FieldTable,
    /// Evaluated call-site values (empty for the served bare shape).
    pub args: &'a [Option<PineValue>],
    /// Resident per-engine store, fed through the cursor.
    pub store: &'a mut MtfStore,
    /// Span bundle for this chart.
    pub key: &'a MarketContextKey,
    /// Bar-close `now` confirming the last bucket.
    pub now_ms: i64,
}

/// Where one `ALIAS.item` reference resolves: the native table or Pine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeResolution {
    /// Served natively: owning library plus the Pine-visible symbol.
    Native {
        /// Native identity of the serving library.
        library: NativeLibraryId,
        /// Pine-visible symbol name.
        symbol: String,
    },
    /// Served from Pine: the exporting module, as the graph resolves it.
    Pine(ModuleId),
}

/// Link-time overlay over a resolved [`ModuleGraph`]: which table libraries
/// serve natively, one audit row per table library linked, and skew
/// findings. Borrows the graph — Pine order, exports, alias tables, and
/// refusal behavior stay exactly as resolved; the overlay only adds the
/// native routing view on top.
pub struct NativeOverlay<'graph> {
    graph: &'graph ModuleGraph,
    served: HashMap<NativeLibraryId, &'static dyn NativeProvider>,
    audits: Vec<NativeAudit>,
    warnings: Vec<VersionWarn>,
}

impl fmt::Debug for NativeOverlay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut served: Vec<String> = self.served.keys().map(ToString::to_string).collect();
        served.sort();
        f.debug_struct("NativeOverlay")
            .field("served", &served)
            .field("audits", &self.audits)
            .field("warnings", &self.warnings)
            .finish()
    }
}

impl<'graph> NativeOverlay<'graph> {
    /// The resolved Pine graph underneath (still fully linked for audit).
    #[must_use]
    pub fn graph(&self) -> &'graph ModuleGraph {
        self.graph
    }

    /// Whether `publisher/name` serves natively under this overlay.
    #[must_use]
    pub fn is_served(&self, publisher: &str, name: &str) -> bool {
        self.served
            .contains_key(&NativeLibraryId::new(publisher, name))
    }

    /// Native symbol table for a served library; `None` when Pine serves it.
    #[must_use]
    pub fn provides(&self, publisher: &str, name: &str) -> Option<&[&str]> {
        self.served
            .get(&NativeLibraryId::new(publisher, name))
            .map(|provider| provider.provides())
    }

    /// One audit row per native-table library linked (served natively or
    /// from Pine), in graph order. Libraries outside the table are never
    /// observed here.
    #[must_use]
    pub fn audits(&self) -> &[NativeAudit] {
        &self.audits
    }

    /// Version-skew findings: at most one per served library.
    #[must_use]
    pub fn warnings(&self) -> &[VersionWarn] {
        &self.warnings
    }

    /// Bind `ALIAS.item` from one importer. Served symbols of served
    /// libraries route to the native table; everything else — unserved
    /// libraries, unprovided symbols, unknown aliases — falls through to
    /// the graph, whose export-visibility and unknown-alias refusals are
    /// preserved unchanged.
    pub fn resolve_reference(
        &self,
        importer: &ModuleId,
        alias: &str,
        item: &str,
    ) -> Result<NativeResolution, KernelError> {
        if let Some(target) = self
            .graph
            .alias_table(importer)
            .unwrap_or_default()
            .get(alias)
        {
            let library = NativeLibraryId::from(target);
            if let Some(provider) = self.served.get(&library) {
                if provider.provides().contains(&item) {
                    return Ok(NativeResolution::Native {
                        library,
                        symbol: item.to_owned(),
                    });
                }
            }
        }
        Ok(NativeResolution::Pine(
            self.graph.resolve_reference(importer, alias, item)?.clone(),
        ))
    }
}

/// Hex digest of kept Pine source for audit rows: FNV-1a over the
/// byte-identical text via the shared [`common_core::hash::fnv1a64`] helper
/// (no second hash implementation lives here). Audit identity only (change
/// detection across links) — never tamper-proofing.
#[must_use]
pub fn pine_sha_of(text: &str) -> String {
    format!("{:016x}", common_core::hash::fnv1a64(text.as_bytes()))
}

/// Run the link path after `resolve_imports`: mark every linked module that
/// has a native provider as served when [`NativeOptions::enabled`], keep its
/// Pine source loaded (the graph is borrowed, never mutated), emit one
/// [`NativeAudit`] row per native-table library linked with its Pine SHA,
/// and warn once per served library whose importers pinned more than one
/// version (skew still resolves — versions stay ignored).
///
/// Symbol-scoped requests (`only_symbols`) refuse with a reason:
/// whole-library granularity only. Unknown imports and cycles already
/// refused inside `resolve_imports`, so enabling native can never bypass
/// them — this function only ever sees an acyclic, fully linked graph.
pub fn apply_overrides<'graph>(
    graph: &'graph ModuleGraph,
    options: &NativeOptions,
) -> Result<NativeOverlay<'graph>, KernelError> {
    let mut served: HashMap<NativeLibraryId, &'static dyn NativeProvider> = HashMap::new();
    if options.enabled {
        for id in graph.order() {
            if let Some(provider) = provider_for(&id.publisher, &id.name) {
                served.insert(NativeLibraryId::from(id), provider);
            }
        }
    }
    if let Some(symbols) = &options.only_symbols {
        let mut scope: Vec<String> = served.keys().map(ToString::to_string).collect();
        scope.sort();
        let scope = if scope.is_empty() {
            "no linked library is natively served".to_owned()
        } else {
            scope.join(", ")
        };
        return Err(KernelError::InvalidInput(format!(
            "partial native override refused (whole-library granularity only): {} requested symbol(s) for {scope}; overrides serve an entire library or nothing",
            symbols.len()
        )));
    }

    let mut audits = Vec::new();
    for id in graph.order() {
        if !NATIVE_LIBRARIES
            .iter()
            .any(|entry| entry.publisher == id.publisher && entry.name == id.name)
        {
            continue;
        }
        let Some(entry) = graph.get(id) else {
            continue;
        };
        let served_impl = if served.contains_key(&NativeLibraryId::from(id)) {
            NativeServed::Rust
        } else {
            NativeServed::Pine
        };
        audits.push(NativeAudit::new(
            &id.publisher,
            &id.name,
            Some(entry.id.version),
            served_impl,
            &pine_sha_of(&entry.source.text),
        ));
    }

    let mut warnings = Vec::new();
    for native_id in served.keys() {
        let module_id = ModuleId::new(&native_id.publisher, &native_id.name);
        let Some(entry) = graph.get(&module_id) else {
            continue;
        };
        let mut pins: BTreeSet<u64> = BTreeSet::new();
        pins.insert(entry.id.version);
        for importer_id in graph.order() {
            let Some(importer) = graph.get(importer_id) else {
                continue;
            };
            for import in &importer.source.imports {
                if import.library.publisher == native_id.publisher
                    && import.library.name == native_id.name
                {
                    pins.insert(import.library.version);
                }
            }
        }
        if pins.len() > 1 {
            let list = pins
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            warnings.push(VersionWarn {
                declared: None,
                message: format!(
                    "native {native_id} requested at versions {list}; serving one native implementation (versions ignored)"
                ),
            });
        }
    }

    Ok(NativeOverlay {
        graph,
        served,
        audits,
        warnings,
    })
}

#[cfg(test)]
#[path = "../tests/native_libs.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/native_override.rs"]
mod native_override_tests;

#[cfg(test)]
#[path = "../tests/native_parity_freeze.rs"]
mod native_freeze_tests;

#[cfg(test)]
#[path = "../tests/native_bridge.rs"]
mod native_bridge_tests;

#[cfg(test)]
#[path = "../tests/native_call_contract.rs"]
mod native_call_contract_tests;

#[cfg(test)]
#[path = "../tests/resident_ring.rs"]
mod resident_ring_tests;

#[cfg(test)]
#[path = "../tests/native_ring_lease.rs"]
mod native_ring_lease_tests;

#[cfg(test)]
#[path = "../tests/native_apex.rs"]
mod native_apex_tests;
