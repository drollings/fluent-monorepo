//! Router-local `String` specialization of `common_core::score::ScoreMatrix`.
//! Generic math lives in `common_core::score`; this file is the only place router
//! routes appear as score dimensions (`plan`/`rigor`/`local`). Do not promote
//! `router_default` to `common-core` — it is domain.
//!
//! Confidence (`coherence`) measures producer self-doubt (well-formedness);
//! `completeness`/`risk`/`complexity` are task-value (outcome fitness) — bands
//! measure task-value, not confidence.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

pub use common_core::score::{normalize_score, RouteBands, ScoredRoute};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScoreMatrix {
    inner: common_core::score::ScoreMatrix<String>,
}

impl ScoreMatrix {
    pub fn resolve(&self, scores: &HashMap<String, f64>) -> Vec<ScoredRoute<String>> {
        self.inner.resolve(scores)
    }

    pub fn dimensions(&self) -> &[String] {
        &self.inner.dimensions
    }
    pub fn weights(&self) -> &[f64] {
        &self.inner.weights
    }
    pub fn routes(&self) -> &HashMap<String, RouteBands<String>> {
        &self.inner.routes
    }
}

/// Closed set of well-known router routes that carry score bands.
///
/// The wire format stays the lowercase strings (`"plan"`, `"rigor"`,
/// `"local"`), so existing JSON configs carrying those names deserialize
/// byte-identically; new code matches on this enum instead of repeating the
/// raw literals. Dynamic route names from operator config stay strings —
/// only these three band-carrying categories are closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RouterRouteKind {
    Plan,
    Rigor,
    Local,
}

/// Typed rejection for unknown route names: never a silent default.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown router route: {0}")]
pub struct UnknownRouterRoute(pub String);

impl RouterRouteKind {
    /// Canonical lowercase wire string for this route.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            RouterRouteKind::Plan => "plan",
            RouterRouteKind::Rigor => "rigor",
            RouterRouteKind::Local => "local",
        }
    }
}

impl fmt::Display for RouterRouteKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RouterRouteKind {
    type Err = UnknownRouterRoute;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text.to_lowercase().as_str() {
            "plan" => Ok(RouterRouteKind::Plan),
            "rigor" => Ok(RouterRouteKind::Rigor),
            "local" => Ok(RouterRouteKind::Local),
            _ => Err(UnknownRouterRoute(text.to_string())),
        }
    }
}

impl ScoreMatrix {
    pub fn router_default() -> Self {
        use common_core::score::ScoreDimension;
        let mut routes = HashMap::new();
        routes.insert(
            RouterRouteKind::Plan.as_str().into(),
            RouteBands {
                bands: {
                    let mut b = HashMap::new();
                    b.insert(ScoreDimension::Completeness.as_str().into(), (0.0, 0.5));
                    b
                },
            },
        );
        routes.insert(
            RouterRouteKind::Rigor.as_str().into(),
            RouteBands {
                bands: {
                    let mut b = HashMap::new();
                    b.insert(ScoreDimension::Completeness.as_str().into(), (0.7, 1.0));
                    b.insert(ScoreDimension::Risk.as_str().into(), (0.4, 1.0));
                    b
                },
            },
        );
        routes.insert(
            RouterRouteKind::Local.as_str().into(),
            RouteBands {
                bands: {
                    let mut b = HashMap::new();
                    b.insert(ScoreDimension::Completeness.as_str().into(), (0.7, 1.0));
                    b.insert(ScoreDimension::Risk.as_str().into(), (0.0, 0.4));
                    b
                },
            },
        );
        Self {
            inner: common_core::score::ScoreMatrix {
                dimensions: vec![
                    ScoreDimension::Coherence.as_str().into(),
                    ScoreDimension::Complexity.as_str().into(),
                    ScoreDimension::Completeness.as_str().into(),
                    ScoreDimension::Risk.as_str().into(),
                ],
                weights: vec![0.3, 0.2, 0.3, 0.2],
                routes,
            },
        }
    }
}

impl Default for ScoreMatrix {
    fn default() -> Self {
        Self::router_default()
    }
}

#[cfg(test)]
#[path = "../tests/score_matrix_calibration.rs"]
mod score_matrix_calibration_tests;
#[cfg(test)]
#[path = "../tests/score_matrix_golden.rs"]
mod score_matrix_golden;
#[cfg(test)]
#[path = "../tests/score_matrix.rs"]
mod tests;
