//! Precompiled classification tree.
//!
//! [`CompiledTree`] mirrors [`ClassificationNode`] with the per-request work
//! done once: `Filter` patterns compile into a single [`regex::RegexSet`]
//! and `Classifier` prompts pre-render into templates whose only per-request
//! step is threshold substitution. The serde/config type stays
//! [`ClassificationTree`] (a `RegexSet` is not serializable); build the
//! compiled form once per accepted tree version via
//! [`ClassificationTree::compile`] and evaluate against that.
//!
//! Invalid-pattern semantics are preserved exactly: today an invalid pattern
//! logs a warning per evaluation and is treated as a non-match, short-circuit
//! order included. Compilation collects the invalid patterns aside (with
//! their error text for the warning) and matching replays the original
//! ordered `any()` over valid-set hits and invalid warnings.

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use regex::{Regex, RegexSet};

use crate::config::classification::{ClassificationNode, ClassificationTree, InterlinguaMatch};
use crate::config::filters::FilterOutcome;
use crate::stages::classifier::prompt::substitute_thresholds;

/// Counts [`ClassificationTree::compile`] calls. Test builds only: the
/// compile-once proof asserts this moves on `compile()` and never on match
/// or render.
#[cfg(test)]
static COMPILE_COUNT: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
pub(crate) fn compile_count() -> usize {
    COMPILE_COUNT.load(Ordering::SeqCst)
}

/// Serializes the compile-counter tests: the counter is process-global, so
/// the tests that assert exact counts hold this while they run.
#[cfg(test)]
pub(crate) static COMPILE_TEST_LOCK: std::sync::Mutex<()> =
    std::sync::Mutex::new(());

/// Compilation failure. Unreachable today — invalid patterns are collected
/// as always-non-matching entries, never errors — but the signature stays
/// fallible so future structural validation has a channel.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CompileError {
    #[error("filter pattern set failed to build: {0}")]
    RegexSet(String),
}

/// The precompiled tree: same shape as the config tree, evaluation-ready.
#[derive(Debug, Clone)]
pub struct CompiledTree {
    pub(crate) root: CompiledNode,
}

impl CompiledTree {
    /// Resolve the filter at `path` (child keys from the root; empty targets
    /// the root itself) and match `text` with the original ordered-`any()`
    /// semantics. `None` when the path resolves to no filter node.
    pub fn filter_matches(&self, path: &[&str], text: &str) -> Option<bool> {
        Self::resolve(&self.root, path).and_then(|node| match node {
            CompiledNode::Filter(filter) => Some(filter.matches(text)),
            _ => None,
        })
    }

    /// Render the classifier prompt at `path` with per-request thresholds.
    /// `None` for non-classifier targets or classifiers with no routeable
    /// children — the same conditions as the live builder.
    pub fn render_prompt(
        &self,
        path: &[&str],
        coherence_threshold: f64,
        safety_threshold: f64,
    ) -> Option<String> {
        match Self::resolve(&self.root, path)? {
            CompiledNode::Classifier {
                prompt_template, ..
            } => Some(substitute_thresholds(
                prompt_template.as_ref()?,
                coherence_threshold,
                safety_threshold,
            )),
            _ => None,
        }
    }

    fn resolve<'a>(node: &'a CompiledNode, path: &[&str]) -> Option<&'a CompiledNode> {
        let mut current = node;
        for key in path {
            let CompiledNode::Classifier { children, .. } = current else {
                return None;
            };
            current = children
                .iter()
                .find(|c| &c.key == key)
                .map(|c| &c.node)?;
        }
        Some(current)
    }
}

/// One compiled node. `Terminal` carries its dispatch fields through
/// unchanged; `Filter` holds the [`RegexSet`]; `Classifier` holds the
/// pre-rendered prompt template.
#[derive(Debug, Clone)]
pub(crate) enum CompiledNode {
    Classifier {
        description: String,
        model: Option<String>,
        model_group: Option<String>,
        coherence_threshold: Option<f64>,
        safety_threshold: Option<f64>,
        children: Vec<CompiledChild>,
        prompt_template: Option<String>,
    },
    Terminal {
        route: String,
        group: Option<String>,
        #[allow(dead_code)]
        role: Option<String>,
        description: String,
        #[allow(dead_code)]
        always_route: bool,
    },
    Filter(CompiledFilter),
    Fallback {
        #[allow(dead_code)]
        description: Option<String>,
        node: Box<CompiledNode>,
    },
}

impl CompiledNode {
    /// Render this classifier's pre-rendered template with per-request
    /// thresholds. `None` for non-classifier nodes and for classifiers with
    /// no routeable children — the same conditions as the live builder.
    pub(crate) fn render_classifier_prompt(
        &self,
        coherence_threshold: f64,
        safety_threshold: f64,
    ) -> Option<String> {
        match self {
            CompiledNode::Classifier {
                prompt_template, ..
            } => Some(substitute_thresholds(
                prompt_template.as_ref()?,
                coherence_threshold,
                safety_threshold,
            )),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CompiledChild {
    pub key: String,
    #[allow(dead_code)]
    pub description: String,
    pub node: CompiledNode,
}

/// A filter with its valid patterns in one [`RegexSet`] and its invalid
/// patterns kept aside as warn-and-non-match entries, in original order.
#[derive(Debug, Clone)]
pub(crate) struct CompiledFilter {
    pub description: String,
    pub set: RegexSet,
    pub order: Vec<CompiledPattern>,
    pub outcome: FilterOutcome,
    pub redirect_to: Option<String>,
    pub match_interlingua: Option<InterlinguaMatch>,
}

/// One pattern in original position: a valid pattern's index into the
/// [`RegexSet`], or an invalid pattern's source plus its compile error for
/// the preserved warning.
#[derive(Debug, Clone)]
pub(crate) enum CompiledPattern {
    Valid { set_index: usize },
    Invalid { source: String, error: String },
}

impl CompiledFilter {
    /// The original ordered `any()`: valid patterns read their set hit,
    /// invalid patterns warn (with the stored error) and count as non-match,
    /// short-circuiting on the first hit exactly as before.
    pub fn matches(&self, text: &str) -> bool {
        if self.order.is_empty() {
            return false;
        }
        let set_matches = self.set.matches(text);
        self.order.iter().any(|pattern| match pattern {
            CompiledPattern::Valid { set_index } => set_matches.matched(*set_index),
            CompiledPattern::Invalid { source, error } => {
                tracing::warn!(
                    target: "router.pipeline.stage2.tree",
                    pattern = %source,
                    error = %error,
                    "invalid filter regex — treated as non-match",
                );
                false
            }
        })
    }
}

impl ClassificationTree {
    /// Precompile the tree: one [`RegexSet`] per filter node, one prompt
    /// template per classifier node. Invalid patterns never fail compilation;
    /// they are kept as warn-and-non-match entries.
    pub fn compile(&self) -> Result<CompiledTree, CompileError> {
        #[cfg(test)]
        COMPILE_COUNT.fetch_add(1, Ordering::SeqCst);
        Ok(CompiledTree {
            root: CompiledNode::compile(&self.root)?,
        })
    }
}

impl CompiledNode {
    fn compile(node: &ClassificationNode) -> Result<Self, CompileError> {
        match node {
            ClassificationNode::Classifier {
                description,
                model,
                model_group,
                coherence_threshold,
                safety_threshold,
                children,
            } => {
                let mut compiled = Vec::with_capacity(children.len());
                for child in children {
                    compiled.push(CompiledChild {
                        key: child.key.clone(),
                        description: child.description.clone(),
                        node: CompiledNode::compile(&child.node)?,
                    });
                }
                Ok(CompiledNode::Classifier {
                    description: description.clone(),
                    model: model.clone(),
                    model_group: model_group.clone(),
                    coherence_threshold: *coherence_threshold,
                    safety_threshold: *safety_threshold,
                    children: compiled,
                    prompt_template: node.build_prompt_template(),
                })
            }
            ClassificationNode::Terminal {
                route,
                group,
                role,
                description,
                always_route,
            } => Ok(CompiledNode::Terminal {
                route: route.clone(),
                group: group.clone(),
                role: role.clone(),
                description: description.clone(),
                always_route: *always_route,
            }),
            ClassificationNode::Filter {
                description,
                patterns,
                outcome,
                redirect_to,
                match_interlingua,
            } => {
                let mut valid_sources = Vec::new();
                let mut order = Vec::with_capacity(patterns.len());
                for source in patterns {
                    match Regex::new(source) {
                        Ok(_) => {
                            order.push(CompiledPattern::Valid {
                                set_index: valid_sources.len(),
                            });
                            valid_sources.push(source.as_str());
                        }
                        Err(e) => order.push(CompiledPattern::Invalid {
                            source: source.clone(),
                            error: e.to_string(),
                        }),
                    }
                }
                let set = RegexSet::new(&valid_sources)
                    .map_err(|e| CompileError::RegexSet(e.to_string()))?;
                Ok(CompiledNode::Filter(CompiledFilter {
                    description: description.clone(),
                    set,
                    order,
                    outcome: outcome.clone(),
                    redirect_to: redirect_to.clone(),
                    match_interlingua: match_interlingua.clone(),
                }))
            }
            ClassificationNode::Fallback { description, node } => {
                Ok(CompiledNode::Fallback {
                    description: description.clone(),
                    node: Box::new(CompiledNode::compile(node)?),
                })
            }
        }
    }
}

#[cfg(test)]
#[path = "../../tests/tree_compiled.rs"]
mod tests;
