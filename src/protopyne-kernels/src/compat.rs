//! Kernel compatibility matrix: per-kernel epsilon, standing, and evidence.
//!
//! `eps` is a **correctness** threshold: the maximum absolute error allowed
//! between a kernel's output and its reference on the calibration sets.
//!
//! Two facts per row, never one: the [`CompatStatus`] is what the row
//! *claims*, and [`ParityEvidence`] is the artifact that claim rests on. A
//! numeric standing (any claim that eps was met) must never cite
//! [`ParityEvidence::ProbeRuns`] — "the probe ran" is not a comparison, and a
//! row that only has that is a red board, not a green one. The vocabulary is
//! the divergence log's legend in code terms; `doc/protopyne-rs/DIVERGENCE_LOG.md`
//! carries the one table that maps the two.
//!
//! Numeric authority for the `Expr` standing is the staged `polars.Expr`
//! reference the global calibration gate recomputes every run (the frozen
//! fact is the eps, not the numbers); rows with checked-in vectors cite
//! those instead, and the direct TradingView-export cross-check remains
//! unavailable to the prototype — an [`CompatStatus::ExternalParityVerified`]
//! row can only come from [`external_evidence`], which validates the export's
//! provenance. A kernel that cannot meet its eps stays
//! [`CompatStatus::FormulaVerified`], never gets a silently widened eps, and
//! never guards money until it verifies. Frozen eps values never widen
//! afterwards: the global gate snapshots the whole table.

use crate::indicator::IndicatorRegistry;

/// Verification standing of one kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatStatus {
    /// Meets frozen eps against the staged `Expr` reference on the calibration
    /// sets with the global must-NOT-fire controls quiet.
    ExprParityVerified,
    /// Meets eps on the hand-computed formula sets; the `Expr` cross-check
    /// is pending for the stated reason.
    FormulaVerified(&'static str),
    /// Meets frozen eps against an external export whose provenance
    /// [`external_evidence`] validated. Carries the minted artifact id; the
    /// variant has no public constructor.
    ExternalParityVerified(ArtifactId),
}

/// The artifact kind behind a row's claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParityEvidence {
    /// The suite executes the kernel and asserts structural facts (it runs,
    /// it aligns, warmup stays `na`); no reference numbers are compared.
    /// Never sufficient for a claim that eps was met.
    ProbeRuns,
    /// Reference numbers frozen in the tree, named by path relative to this
    /// crate's root.
    GoldenVector(&'static str),
    /// Reference numbers recomputed by the staged `Expr` gate on every run,
    /// named by the gate that does it; the frozen fact is the eps.
    StagedReference(&'static str),
    /// Numbers from an external export, named by the artifact id that
    /// [`external_evidence`] validates.
    ExternalExport(&'static str),
}

/// One row of the compatibility matrix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KernelCompat {
    /// Registry id.
    pub id: &'static str,
    /// Maximum absolute error vs. the row's reference.
    pub eps: f64,
    /// Verification standing: what the row claims.
    pub status: CompatStatus,
    /// The artifact that claim rests on.
    pub evidence: ParityEvidence,
}

/// Provenance of one external export: the fields that make its numbers
/// traceable to a real run of a named script over a named symbol and
/// timeframe.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExternalArtifact {
    /// Stable artifact id, minted into the standing.
    pub id: &'static str,
    /// Where the numbers came from (platform name).
    pub source: &'static str,
    /// Source version the export was taken with.
    pub source_version: &'static str,
    /// Export instant, ISO-8601 UTC.
    pub exported_at: &'static str,
    /// Exported symbol, exchange-qualified.
    pub symbol: &'static str,
    /// Chart timeframe the export was taken on.
    pub timeframe: &'static str,
    /// SHA-256 of the script source the export ran, 64 lowercase hex digits.
    pub script_sha256: &'static str,
    /// Exported observations, at least one.
    pub rows: &'static [ExternalRow],
}

/// One exported observation: the external value beside ours, at one bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExternalRow {
    /// Bar index in the export.
    pub bar: usize,
    /// The external reference's value.
    pub reference: f64,
    /// Our kernel's value at the same bar.
    pub ours: f64,
}

mod evidence {
    use super::ExternalArtifact;

    /// Artifact id, mintable only by [`mint`]: the private field is what
    /// makes [`super::CompatStatus::ExternalParityVerified`] unconstructible
    /// outside this module.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ArtifactId(&'static str);

    impl ArtifactId {
        /// The validated artifact id.
        #[must_use]
        pub const fn as_str(self) -> &'static str {
            self.0
        }
    }

    /// Validate one artifact's provenance and mint its id.
    ///
    /// A `const fn`, so a row can carry an external standing in a `const`
    /// table: any missing or malformed field is a refusal at the table's
    /// definition (compile time) rather than a row that ages silently.
    ///
    /// # Panics
    ///
    /// Names the first invalid field: `id`, `source`, `source_version`,
    /// `exported_at`, `symbol`, `timeframe`, `script_sha256` (absent, or not
    /// 64 lowercase hex digits), or the empty `rows`.
    // `assert!`/`assert_eq!` are not const-callable, so each refusal is a
    // `panic!` under its own `if`: the lint's suggested forms do not compile
    // in a `const fn`, and the const-ness is what makes the refusal a
    // table-definition error rather than a runtime surprise.
    #[allow(clippy::manual_assert)]
    pub const fn mint(artifact: &ExternalArtifact) -> ArtifactId {
        if artifact.id.is_empty() {
            panic!("external artifact: id is required");
        }
        if artifact.source.is_empty() {
            panic!("external artifact: source is required");
        }
        if artifact.source_version.is_empty() {
            panic!("external artifact: source_version is required");
        }
        if artifact.exported_at.is_empty() {
            panic!("external artifact: exported_at is required");
        }
        if artifact.symbol.is_empty() {
            panic!("external artifact: symbol is required");
        }
        if artifact.timeframe.is_empty() {
            panic!("external artifact: timeframe is required");
        }
        if artifact.script_sha256.len() != 64 {
            panic!("external artifact: script_sha256 must be 64 hex digits");
        }
        let bytes = artifact.script_sha256.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if !is_lower_hex(bytes[index]) {
                panic!("external artifact: script_sha256 must be 64 hex digits");
            }
            index += 1;
        }
        if artifact.rows.is_empty() {
            panic!("external artifact: rows are required");
        }
        ArtifactId(artifact.id)
    }

    const fn is_lower_hex(byte: u8) -> bool {
        matches!(byte, b'0'..=b'9' | b'a'..=b'f')
    }
}

pub use evidence::ArtifactId;

/// The only path to [`CompatStatus::ExternalParityVerified`]: validate the
/// export's provenance, then mint its artifact id.
///
/// Hand-writing the variant in [`COMPATIBILITY`] does not compile — the
/// payload can only come from here — and an invalid artifact refuses when the
/// table is evaluated, so a row cannot claim external numbers it cannot name.
///
/// # Panics
///
/// See [`evidence::mint`]: any missing or malformed provenance field.
#[must_use]
pub const fn external_evidence(artifact: &ExternalArtifact) -> CompatStatus {
    CompatStatus::ExternalParityVerified(evidence::mint(artifact))
}

/// Whether the file an evidence path names exists under this crate's root.
/// A vector path names the file outright; a staged-reference path is
/// `file :: symbol or gate`, and the file half is what must exist. Citation
/// rot is the failure mode this catches.
#[cfg(test)]
#[must_use]
pub fn evidence_path_exists(path: &str) -> bool {
    let file = path.split(" :: ").next().unwrap_or(path);
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(file)
        .exists()
}

/// Frozen per-kernel epsilons. These values are set once the must-NOT-fire
/// control set passes untouched and never widened afterwards.
pub const COMPATIBILITY: &[KernelCompat] = &[
    KernelCompat {
        id: "sma",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::GoldenVector("tests/data/sma14.json"),
    },
    KernelCompat {
        id: "ema",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::GoldenVector("tests/data/ema14.json"),
    },
    KernelCompat {
        id: "rma",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::GoldenVector("tests/data/golden/rma14.json"),
    },
    KernelCompat {
        id: "wma",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::GoldenVector("tests/data/golden/wma14.json"),
    },
    KernelCompat {
        id: "hma",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::GoldenVector("tests/data/golden/hma14.json"),
    },
    KernelCompat {
        id: "vwma",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::GoldenVector("tests/data/golden/vwma14.json"),
    },
    KernelCompat {
        id: "alma",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::GoldenVector("tests/data/golden/alma14.json"),
    },
    KernelCompat {
        id: "dema",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "tema",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "rsi",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::GoldenVector("tests/data/golden/rsi14.json"),
    },
    KernelCompat {
        id: "macd",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::GoldenVector("tests/data/golden/macd12269.json"),
    },
    KernelCompat {
        id: "stoch",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::GoldenVector("tests/data/golden/stoch14.json"),
    },
    KernelCompat {
        id: "tr",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::GoldenVector("tests/data/golden/tr.json"),
    },
    KernelCompat {
        id: "atr",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::GoldenVector("tests/data/golden/atr14.json"),
    },
    KernelCompat {
        id: "bb",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::GoldenVector("tests/data/golden/bb14.json"),
    },
    KernelCompat {
        id: "kc",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::GoldenVector("tests/data/golden/kc14.json"),
    },
    KernelCompat {
        id: "bbands",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "midpoint",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "midprice",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "trima",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "avgprice",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "medprice",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "typprice",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "wclprice",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "natr",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "apo",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "mom",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "ppo",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "roc",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "rocp",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "rocr",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "rocr100",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "rsv",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "stochf",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "trix",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "willr",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "aroon",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "stddev",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "var",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "correl",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "ad",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "adosc",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "obv",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "cci",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "cmo",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "mfi",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "vwap",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::StagedReference("src/calibration.rs :: ParityDashboard"),
    },
    KernelCompat {
        id: "eit",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("transcribed Pine recurrence, hand-stepped sets green; staged-expression cross-check pending"),
        evidence: ParityEvidence::StagedReference("tests/cal.rs :: must_fire_*"),
    },
    KernelCompat {
        id: "kalman",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("transcribed Pine recurrence, hand-stepped sets green; staged-expression cross-check pending"),
        evidence: ParityEvidence::StagedReference("tests/cal.rs :: must_fire_*"),
    },
    KernelCompat {
        id: "voss",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("transcribed Pine recurrence, hand-stepped sets green; staged-expression cross-check pending"),
        evidence: ParityEvidence::StagedReference("tests/cal.rs :: must_fire_*"),
    },
    // Second pending block: egui-charts ports verified against transcribed
    // formulas and hand-stepped sets (plus batch/update identity folds);
    // staged-expression cross-checks pending like the recurrence rows above.
    KernelCompat {
        id: "adx",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("RMA-composed DMI transcription, hand-stepped sets green; staged-expression cross-check pending"),
        evidence: ParityEvidence::StagedReference("tests/cal.rs :: must_fire_*"),
    },
    KernelCompat {
        id: "cmf",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("trailing money-flow ratio, hand-stepped sets green; staged-expression cross-check pending"),
        evidence: ParityEvidence::StagedReference("tests/cal.rs :: must_fire_*"),
    },
    KernelCompat {
        id: "donchian",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("trailing extremes transcription, hand-stepped sets green; staged-expression cross-check pending"),
        evidence: ParityEvidence::StagedReference("tests/cal.rs :: must_fire_*"),
    },
    KernelCompat {
        id: "ichimoku",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("Donchian-midline legs transcription, hand-stepped sets green; staged-expression cross-check pending"),
        evidence: ParityEvidence::StagedReference("tests/cal.rs :: must_fire_*"),
    },
    KernelCompat {
        id: "sar",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("Wilder stop-and-reverse transcription, hand-stepped sets green; staged-expression cross-check pending"),
        evidence: ParityEvidence::StagedReference("tests/cal.rs :: must_fire_*"),
    },
    KernelCompat {
        id: "supertrend",
        eps: 1e-9,
        status: CompatStatus::FormulaVerified("ATR ratchet transcription, hand-stepped sets green; staged-expression cross-check pending"),
        evidence: ParityEvidence::StagedReference("tests/cal.rs :: must_fire_*"),
    },
    KernelCompat {
        id: "native-libBars",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::GoldenVector("tests/data/golden/native_freeze_spy_1m.json"),
    },
    KernelCompat {
        id: "native-market-context",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::GoldenVector("tests/data/golden/native_freeze_spy_1m.json"),
    },
    KernelCompat {
        id: "native-libBuffers",
        eps: 1e-9,
        status: CompatStatus::ExprParityVerified,
        evidence: ParityEvidence::GoldenVector("tests/data/golden/native_freeze_spy_1m.json"),
    },
];

/// Native-library agreement rows: `(matrix id, publisher, name)` naming the
/// native-table library whose freeze agreement each `native-*` row claims.
/// These rows are NOT Expr-checked: their authority is native-vs-engine
/// agreement at the same frozen eps (see the freeze suite), while kernel
/// rows carry Expr-parity. Row ids are
/// frozen literals (the milestone's `native-libBars` for the ring/tick/
/// resample surface, `native-market-context` for the `MarketContext` view
/// over `libApex`, `native-libBuffers` for the `BufferFloat` hot methods);
/// the publisher/name pair is what the native table keys on, so coverage
/// checks join through this one mapping, never string surgery on the ids.
pub const NATIVE_MATRIX_ROWS: &[(&str, &str, &str)] = &[
    ("native-libBars", "showmethegrail", "libBars"),
    ("native-market-context", "showmethegrail", "libApex"),
    ("native-libBuffers", "showmethegrail", "libBuffers"),
];

/// Matrix id of the row claiming `publisher/name`, if any.
#[must_use]
pub fn native_row_id(publisher: &str, name: &str) -> Option<&'static str> {
    NATIVE_MATRIX_ROWS
        .iter()
        .find(|(_, pub_, name_)| *pub_ == publisher && *name_ == name)
        .map(|(id, _, _)| *id)
}

/// Whether a matrix id is a native-library row (served-view agreement, not a
/// kernel in the builtin registry).
#[must_use]
pub fn is_native_row(id: &str) -> bool {
    NATIVE_MATRIX_ROWS.iter().any(|(row, _, _)| *row == id)
}

/// Coverage for the native domain: every `native-*` row names a library in
/// the native table, and every native-table library has its row — no
/// orphans either way. Provider existence is orthogonal (the overlay owns
/// serving): rows also cover served views like the `MarketContext`, which
/// holds freeze agreement without a link-time provider. Eps rules match the
/// kernel side: finite and positive, never widened.
///
/// Needs the `pine` feature: it reads the native link table, which ships
/// with the language runtime.
#[cfg(feature = "pine")]
pub fn assert_native_matrix_covers() -> Result<(), String> {
    for (id, publisher, name) in NATIVE_MATRIX_ROWS {
        if !crate::native_libs::NATIVE_LIBRARIES
            .iter()
            .any(|entry| entry.publisher == *publisher && entry.name == *name)
        {
            return Err(format!("native row {id:?} names no native-table library"));
        }
        let row = compat_for(id).ok_or_else(|| format!("no compatibility row for {id:?}"))?;
        if !(row.eps.is_finite() && row.eps > 0.0) {
            return Err(format!("non-positive eps for {id:?}"));
        }
    }
    for entry in crate::native_libs::NATIVE_LIBRARIES {
        let id = native_row_id(entry.publisher, entry.name)
            .ok_or_else(|| format!("no parity row for {}/{}", entry.publisher, entry.name))?;
        if compat_for(id).is_none() {
            return Err(format!("no compatibility row for {id:?}"));
        }
    }
    Ok(())
}

/// Look up a kernel's compatibility row.
#[must_use]
pub fn compat_for(id: &str) -> Option<&'static KernelCompat> {
    COMPATIBILITY.iter().find(|row| row.id == id)
}

/// Ids the matrix must cover: every builtin kernel, no more, no less —
/// plus the native-library rows, which are checked against the native
/// table (not the kernel registry) by [`assert_native_matrix_covers`].
/// Without the `pine` feature the link table is absent, so only the kernel
/// rows are checked here; the native half stays covered by
/// `assert_native_matrix_covers` wherever the runtime ships.
pub fn assert_matrix_covers(registry: &IndicatorRegistry) -> Result<(), String> {
    for id in registry.ids() {
        let row = compat_for(&id).ok_or_else(|| format!("no compatibility row for {id:?}"))?;
        if !(row.eps.is_finite() && row.eps > 0.0) {
            return Err(format!("non-positive eps for {id:?}"));
        }
    }
    for row in COMPATIBILITY {
        if is_native_row(row.id) {
            continue;
        }
        if registry.get(row.id).is_none() {
            return Err(format!("stale compatibility row {:?}", row.id));
        }
    }
    #[cfg(feature = "pine")]
    assert_native_matrix_covers()?;
    Ok(())
}

#[cfg(test)]
#[path = "../tests/compat.rs"]
mod tests;

#[cfg(all(test, feature = "research"))]
#[path = "../tests/cal.rs"]
mod cal_tests;

#[cfg(all(test, feature = "research"))]
#[path = "../tests/cal_global.rs"]
mod cal_global_tests;
