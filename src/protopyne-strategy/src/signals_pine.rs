//! Pine-backed signal: one declared plot drives the shared rule vocabulary.
//!
//! A [`PineSignal`] links a root script (optionally with its imported
//! libraries) once, then evaluates the run's bars and reads one declared
//! `plot()` title. The value series feeds the same [`DirectionRule`] engine
//! the kernel signal uses, so both signal families share one rule home.
//!
//! Two trust boundaries are enforced at construction or evaluation, never
//! assumed:
//!
//! - the root must declare the supported Pine version;
//! - every construct named in a [`ParityClaim`] must exist in the parity
//!   matrix and must not be a `Refuse` row. A `Correct` row is a declared
//!   divergence, so naming it is the declaration.
//!
//! Evaluation injects security/lower-timeframe datasets built **only** from
//! the view's own bars, under the chart symbol and the view's spacing. Any
//! other pair keeps the engine's fail-closed refusal: a signal never fetches
//! ambient data.

use std::sync::Arc;

use protopyne_kernels::{
    fin_or_none, parity_for, parse_program, resolve_imports, InputSet, LibraryId, LinkedModule,
    LtfDataset, ModuleInput, ParityVerdict, PineEngine, PineRuntimeConfig, PineSource, PineValue,
    SUPPORTED_PINE_VERSION,
};

use crate::elements::{Signal, SignalConfig, SignalInstance, SignalOutput};
use crate::error::StrategyError;
use crate::signals_kernel::{apply_rule, validate_rule, DirectionRule};
use crate::view::MarketView;

/// Constructs a Pine signal declares it relies on.
///
/// Construction refuses an unknown construct (the matrix cannot vouch for
/// it) and a `Refuse` row (the construct is knowingly unsupported); naming a
/// `Correct` row records the accepted divergence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParityClaim {
    constructs: Vec<String>,
}

impl ParityClaim {
    /// Claim reliance on `constructs`, matched against the parity matrix.
    #[must_use]
    pub fn new<I, S>(constructs: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            constructs: constructs.into_iter().map(Into::into).collect(),
        }
    }

    /// Claim nothing: only the version and engine checks apply.
    #[must_use]
    pub fn none() -> Self {
        Self {
            constructs: Vec::new(),
        }
    }

    /// The claimed construct names.
    #[must_use]
    pub fn constructs(&self) -> &[String] {
        &self.constructs
    }

    fn validate(self) -> Result<(), StrategyError> {
        for construct in &self.constructs {
            let Some(row) = parity_for(construct) else {
                return Err(StrategyError::InvalidElement(format!(
                    "pine parity construct {construct:?} is not in the matrix"
                )));
            };
            if row.verdict == ParityVerdict::Refuse {
                return Err(StrategyError::InvalidElement(format!(
                    "pine parity construct {construct:?} refuses: {}",
                    row.reason
                )));
            }
        }
        Ok(())
    }
}

/// Execution selection for one Pine signal.
///
/// The compiled loop is the default: a signal that does not ask for the
/// tree-walk evaluates on the one shipping path. The tree-walk opt-out
/// exists only where the oracle does (test and `oracle` builds), so
/// production code cannot name another engine — and where it exists both
/// selections report byte-identical outcomes on lowered programs.
#[cfg(feature = "bytecode")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PineExecution {
    /// The compiled slot VM (default).
    #[default]
    Compiled,
    /// The tree-walk evaluator: the oracle opt-out, unnameable in
    /// shipping builds.
    #[cfg(any(test, feature = "oracle"))]
    TreeWalk,
}

/// One linked Pine program evaluated against the decision view.
#[derive(Debug, Clone)]
struct PineCore {
    modules: Vec<LinkedModule>,
    plot_title: String,
    rule: DirectionRule,
    activity_score: f64,
    chart_symbol: String,
    /// Evaluator selection. Fixed at construction: an instance never flips
    /// engines mid-run, so a decision closure cannot observe two evaluators.
    #[cfg(feature = "bytecode")]
    execution: PineExecution,
}

impl PineCore {
    fn evaluate(&self, view: &MarketView<'_>) -> Result<SignalOutput, StrategyError> {
        if view.data.bars.is_empty() {
            return Err(StrategyError::InvalidElement(
                "pine signal needs at least one bar".to_owned(),
            ));
        }
        let period = view.data.timeframe.to_pine_abbrev();
        let mut engine = PineEngine::new(
            PineRuntimeConfig::new()
                .chart_symbol(self.chart_symbol.clone())
                .chart_period(period.to_owned())
                .build(),
        );
        #[cfg(feature = "bytecode")]
        #[cfg(any(test, feature = "oracle"))]
        if self.execution == PineExecution::TreeWalk {
            engine.set_bytecode_mode(protopyne_kernels::pine::BytecodeMode::TreeWalk);
        }
        // The view is the only data source: the chart pair is served from
        // its bars, and every other pair keeps the engine's refusal.
        let dataset = LtfDataset::of_bars(view.data.bars, view.data.timeframe);
        engine
            .inject_security(&self.chart_symbol, period, dataset.clone())
            .map_err(|e| engine_error(&e))?;
        engine
            .inject_lower_tf(&self.chart_symbol, period, dataset)
            .map_err(|e| engine_error(&e))?;
        let outcome = engine
            .run_linked(&self.modules, view.data.bars, InputSet::new())
            .map_err(|e| engine_error(&e))?;
        let plot = outcome
            .plots
            .iter()
            .find(|plot| plot.title == self.plot_title)
            .ok_or_else(|| {
                let titles: Vec<&str> = outcome
                    .plots
                    .iter()
                    .map(|plot| plot.title.as_str())
                    .collect();
                StrategyError::InvalidElement(format!(
                    "pine signal has no plot titled {:?}; plots: {titles:?}",
                    self.plot_title
                ))
            })?;
        let series: Vec<Option<f64>> = plot
            .values
            .iter()
            .map(|value| {
                value
                    .as_ref()
                    .and_then(PineValue::as_float)
                    .and_then(fin_or_none)
            })
            .collect();
        apply_rule(self.rule, &[series], self.activity_score)
    }
}

/// A Pine script's declared plot as a signal, behind the `pine` feature.
#[derive(Debug)]
pub struct PineSignal {
    name: String,
    core: Arc<PineCore>,
}

impl PineSignal {
    /// Link one self-contained script. Sources that import libraries must use
    /// [`from_sources`](Self::from_sources), which receives the closure.
    pub fn new(
        source: PineSource,
        plot_title: impl Into<String>,
        rule: DirectionRule,
        parity: ParityClaim,
    ) -> Result<Self, StrategyError> {
        Self::from_sources(
            vec![(SELF_CONTAINED_KEY.to_owned(), source)],
            SELF_CONTAINED_KEY,
            plot_title,
            rule,
            parity,
        )
    }

    /// Link `sources` (keyed `publisher/name`) around the `root` key.
    ///
    /// Every source is parsed and resolved up front; unknown imports, cycles,
    /// and parse errors refuse before the first evaluation.
    pub fn from_sources(
        sources: Vec<(String, PineSource)>,
        root: &str,
        plot_title: impl Into<String>,
        rule: DirectionRule,
        parity: ParityClaim,
    ) -> Result<Self, StrategyError> {
        let root_source = sources
            .iter()
            .find(|(key, _)| key == root)
            .map(|(_, source)| source)
            .ok_or_else(|| {
                StrategyError::InvalidElement(format!(
                    "pine signal root {root:?} is not among the supplied sources"
                ))
            })?;
        require_version(root_source, root)?;
        parity.validate()?;
        validate_rule(&rule, 1)?;
        let modules = link_modules(sources)?;
        let chart_symbol = PineRuntimeConfig::default().chart_symbol;
        Ok(Self {
            name: "pine".to_owned(),
            core: Arc::new(PineCore {
                modules,
                plot_title: plot_title.into(),
                rule,
                activity_score: 1.0,
                chart_symbol,
                #[cfg(feature = "bytecode")]
                execution: PineExecution::Compiled,
            }),
        })
    }

    /// Override the attribution name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Set the options-activity score carried on every output (no chain feed
    /// exists here, so the default is `1.0`: unfiltered).
    #[must_use]
    pub fn with_activity(mut self, activity_score: f64) -> Self {
        Arc::make_mut(&mut self.core).activity_score = activity_score;
        self
    }

    /// The rule this signal evaluates.
    #[must_use]
    pub fn rule(&self) -> &DirectionRule {
        &self.core.rule
    }

    /// Opt this signal out to the tree-walk oracle. The default stays the
    /// compiled loop, and the selection is fixed for the signal's lifetime.
    /// The opt-out exists only in oracle builds; shipping builds cannot
    /// name it.
    #[cfg(feature = "bytecode")]
    #[must_use]
    pub fn with_execution(mut self, execution: PineExecution) -> Self {
        Arc::make_mut(&mut self.core).execution = execution;
        self
    }

    /// This signal's evaluator selection.
    #[cfg(feature = "bytecode")]
    #[must_use]
    pub fn execution(&self) -> PineExecution {
        self.core.execution
    }
}

impl Signal for PineSignal {
    fn name(&self) -> &str {
        &self.name
    }

    fn bind(&self, _cfg: &SignalConfig) -> Result<Box<dyn SignalInstance>, StrategyError> {
        Ok(Box::new(PineSignalInstance {
            core: Arc::clone(&self.core),
        }))
    }

    fn evaluate(&self, view: &MarketView<'_>) -> Result<SignalOutput, StrategyError> {
        self.core.evaluate(view)
    }
}

/// One run's Pine signal: the linked modules are immutable, and the engine
/// resets per run, so the instance keeps no state that could leak.
struct PineSignalInstance {
    core: Arc<PineCore>,
}

impl SignalInstance for PineSignalInstance {
    fn evaluate(&mut self, view: &MarketView<'_>) -> Result<SignalOutput, StrategyError> {
        self.core.evaluate(view)
    }

    fn warmup(&self) -> usize {
        0
    }
}

/// Graph key for a script handed to [`PineSignal::new`].
const SELF_CONTAINED_KEY: &str = "local/Signal";

fn require_version(source: &PineSource, root: &str) -> Result<(), StrategyError> {
    if source.version != Some(SUPPORTED_PINE_VERSION) {
        let declared = source.version.map_or_else(
            || "no version header".to_owned(),
            |version| format!("version {version}"),
        );
        return Err(StrategyError::InvalidElement(format!(
            "pine signal {root:?} declares {declared}, expected version {SUPPORTED_PINE_VERSION}"
        )));
    }
    Ok(())
}

fn link_modules(sources: Vec<(String, PineSource)>) -> Result<Vec<LinkedModule>, StrategyError> {
    let inputs = sources
        .into_iter()
        .map(|(key, source)| {
            let (publisher, name) = key.split_once('/').ok_or_else(|| {
                StrategyError::InvalidElement(format!(
                    "pine source key {key:?} must be publisher/name"
                ))
            })?;
            let program = parse_program(&source.text).map_err(|e| {
                StrategyError::InvalidElement(format!("pine source {key:?} does not parse: {e}"))
            })?;
            Ok(ModuleInput::new(
                LibraryId {
                    publisher: publisher.to_owned(),
                    name: name.to_owned(),
                    version: source.version.unwrap_or(SUPPORTED_PINE_VERSION),
                },
                source,
                program,
            ))
        })
        .collect::<Result<Vec<_>, StrategyError>>()?;
    let graph = resolve_imports(inputs)
        .map_err(|e| StrategyError::InvalidElement(format!("pine imports do not link: {e}")))?;
    Ok(graph
        .order()
        .iter()
        .map(|id| {
            let entry = graph.get(id).expect("an ordered module is always present");
            let aliases = graph
                .alias_table(id)
                .unwrap_or_default()
                .into_iter()
                .map(|(alias, target)| (alias, target.to_string()))
                .collect();
            LinkedModule::new(&id.to_string(), entry.program.clone(), aliases)
        })
        .collect())
}

fn engine_error(error: &protopyne_kernels::KernelError) -> StrategyError {
    StrategyError::InvalidElement(format!("pine run refused: {error}"))
}

#[cfg(test)]
#[path = "../tests/signals_pine.rs"]
mod tests;
