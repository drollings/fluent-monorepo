//! Ladder composition: deterministic rule → cache → model → static fallback.
//!
//! The full chain walks the canonical async `first_accept_in_order` (no new
//! walker): `Ok(Some)` wins, `Ok(None)` skips, `Err` continues unless the
//! stop predicate says terminal. `Unavailable` is a skip-to-fallback signal,
//! never a label; only `Rejected` is terminal. The pure prefix
//! (`rule → cache`) is additionally exposed as a `ComponentCascade` for sync
//! callers, composing the sync twin.

use std::future::Future;
use std::sync::Arc;

use fluent_wvr::wrapper::ComponentCascade;
use fluent_wvr::{Component, WorkContext, WorkError, WorkOutput, WorkUnit};

use crate::decision::{Decision, DecisionBreaker, DecisionCache, DecisionError};

/// Ordered ladder rungs (owned; no clones or counters needed by callers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rung {
    Rule,
    Cache,
    Model,
    Fallback,
}

/// Run the full chain.
///
/// `model` is a lazy factory so a rule/cache win never constructs the send
/// future. A model success populates `cache` under `key` when `cacheable`.
///
/// ```rust
/// use fluent_llm::client::block_on;
/// use fluent_llm::decision::{decide_with_ladder, Decision, DecisionBreaker, DecisionCache};
///
/// let rule = Decision::from_distribution(
///     "RULE",
///     vec![("RULE".to_string(), 1.0)],
///     "m".to_string(),
///     1,
///     0,
///     false,
/// )
/// .expect("rule");
/// let out = block_on(decide_with_ladder(
///     Some(rule),
///     &DecisionCache::new(4).expect("cache"),
///     "k".to_string(),
///     true,
///     &DecisionBreaker::new(3, 60_000).expect("breaker"),
///     || async { unreachable!("rule wins first") },
///     Decision::from_distribution(
///         "FALLBACK",
///         vec![("FALLBACK".to_string(), 1.0)],
///         "m".to_string(),
///         1,
///         0,
///         false,
///     )
///     .expect("fallback"),
/// ))
/// .expect("ladder");
/// assert_eq!(out.label, "RULE");
/// ```
pub async fn decide_with_ladder<MF, Fut>(
    rule: Option<Decision>,
    cache: &DecisionCache,
    key: String,
    cacheable: bool,
    breaker: &DecisionBreaker,
    model: MF,
    fallback: Decision,
) -> Result<Decision, DecisionError>
where
    MF: FnOnce() -> Fut + Send,
    Fut: Future<Output = Result<Decision, DecisionError>> + Send,
{
    let mut model = Some(model);
    let outcome = fluent_wvr::ladder::first_accept_in_order(
        [Rung::Rule, Rung::Cache, Rung::Model, Rung::Fallback],
        |rung| {
            let rule = rule.clone();
            let fallback = fallback.clone();
            // Take the factory synchronously on the Model rung (the async
            // block below owns it; the closure stays FnMut).
            let factory = if rung == Rung::Model {
                model.take()
            } else {
                None
            };
            // Rebind the key per call so the async block owns only a
            // `Copy` borrow, never the outer variable.
            let cache_key = key.as_str();
            async move {
                match rung {
                    Rung::Rule => Ok(rule.map(|d| {
                        let mut hit = d;
                        hit.cached = false;
                        hit
                    })),
                    Rung::Cache => {
                        if cacheable {
                            Ok(cache.get(cache_key))
                        } else {
                            Ok(None)
                        }
                    }
                    Rung::Model => {
                        // Tripped breaker short-circuits without sending; the
                        // walk continues to the static fallback.
                        breaker.check()?;
                        let factory = factory.expect("model rung runs once");
                        match factory().await {
                            Ok(decision) => {
                                breaker.record_success();
                                // Degraded successes answer once but never
                                // populate the cache (see `get_or_decide`).
                                if cacheable && !decision.degraded {
                                    cache.insert(cache_key.to_string(), decision.clone());
                                }
                                Ok(Some(decision))
                            }
                            Err(DecisionError::Unavailable(detail)) => {
                                breaker.record_transport_failure();
                                Err(DecisionError::Unavailable(detail))
                            }
                            Err(DecisionError::Rejected(detail)) => {
                                breaker.record_permanent();
                                Err(DecisionError::Rejected(detail))
                            }
                            Err(DecisionError::Decode(detail)) => {
                                breaker.record_permanent();
                                Err(DecisionError::Decode(detail))
                            }
                        }
                    }
                    Rung::Fallback => Ok(Some(fallback)),
                }
            }
        },
        |error: &DecisionError| matches!(error, DecisionError::Rejected(_)),
    )
    .await;
    match outcome {
        Ok(Some(decision)) => Ok(decision),
        Ok(None) => Ok(fallback),
        Err(error) => Err(error),
    }
}

/// Fixed-decision sync unit for the pure prefix cascade.
struct FixedDecisionUnit {
    name: &'static str,
    decision: Option<Decision>,
}

impl WorkUnit for FixedDecisionUnit {
    fn name(&self) -> &str {
        self.name
    }
    fn depends(&self) -> &[fluent_wvr::ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[fluent_wvr::ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        match &self.decision {
            Some(d) => WorkOutput::typed("prefix", d),
            None => Ok(WorkOutput::fail("prefix rung declined")),
        }
    }
}

fluent_wvr::impl_fieldless!(FixedDecisionUnit);

impl fluent_wvr::Describable for FixedDecisionUnit {
    fn describe(&self) -> serde_json::Value {
        fluent_wvr::describe_work_unit(self.name(), self.depends(), self.provides(), Some("pure"))
    }
}

fluent_wvr::impl_component!(FixedDecisionUnit);

/// Pure `rule → cache` prefix as a `ComponentCascade` for sync callers.
///
/// ```rust
/// use fluent_llm::decision::{prefix_cascade, Decision};
/// use fluent_wvr::{WorkContext, WorkOutput};
///
/// let rule = Decision::from_distribution(
///     "RULE",
///     vec![("RULE".to_string(), 1.0)],
///     "m".to_string(),
///     1,
///     0,
///     false,
/// )
/// .expect("rule");
/// let out = prefix_cascade(Some(rule), None)
///     .execute_first_ok(&WorkContext::default())
///     .expect("prefix");
/// let back: Decision = out.data_as().expect("typed");
/// assert_eq!(back.label, "RULE");
/// ```
#[must_use]
pub fn prefix_cascade(rule: Option<Decision>, cached: Option<Decision>) -> ComponentCascade {
    let rule_unit: Arc<dyn Component> = Arc::new(FixedDecisionUnit {
        name: "decision.prefix.rule",
        decision: rule,
    });
    let cache_unit: Arc<dyn Component> = Arc::new(FixedDecisionUnit {
        name: "decision.prefix.cache",
        decision: cached,
    });
    ComponentCascade::with_units(vec![rule_unit, cache_unit])
}
