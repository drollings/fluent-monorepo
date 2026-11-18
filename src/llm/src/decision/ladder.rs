//! Ladder composition: deterministic rule → cache → model → static fallback.
//!
//! The full chain walks the canonical async `first_accept_in_order` (no new
//! walker): `Ok(Some)` wins, `Ok(None)` skips, `Err` continues unless the
//! stop predicate says terminal. `Unavailable` is a skip-to-fallback signal,
//! never a label; only `Rejected` is terminal.

use std::future::Future;

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
///     &DecisionCache::try_new(4).expect("cache"),
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
    // Owned rung payloads move into the walk (no per-rung clone): the rule
    // fires once on its rung, the fallback once on its rung, the factory
    // once on the model rung. The closure is `FnMut`, so each `take`
    // runs at most once.
    let mut rule = rule;
    let mut fallback = Some(fallback);
    let mut model = Some(model);
    let outcome = fluent_wvr::ladder::first_accept_in_order(
        [Rung::Rule, Rung::Cache, Rung::Model, Rung::Fallback],
        |rung| {
            let rule = if rung == Rung::Rule {
                rule.take()
            } else {
                None
            };
            let fallback = if rung == Rung::Fallback {
                fallback.take()
            } else {
                None
            };
            let factory = if rung == Rung::Model {
                model.take()
            } else {
                None
            };
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
                    Rung::Fallback => Ok(Some(fallback.expect("fallback rung runs once"))),
                }
            }
        },
        |error: &DecisionError| matches!(error, DecisionError::Rejected(_)),
    )
    .await;
    match outcome {
        Ok(Some(decision)) => Ok(decision),
        // The fallback rung always yields `Some`, so exhaustion here is a
        // walker bug, not a verdict.
        Ok(None) => unreachable!("fallback rung always yields a decision"),
        Err(error) => Err(error),
    }
}
