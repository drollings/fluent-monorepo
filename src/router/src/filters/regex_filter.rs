use std::collections::HashMap;

use regex::Regex;

use crate::config::{ConfidenceGate, FilterAction, FilterOutcome, FilterScope, PatternEntry};

use super::{Filter, FilterContext, FilterDecision, FilterKind, RegexMatch};

pub struct RegexFilter {
    pub name: String,
    pub regexes: Vec<Regex>,
    pub outcome: FilterOutcome,
    pub action: Option<FilterAction>,
    pub confidence_gate: ConfidenceGate,
    pub scopes: Vec<FilterScope>,
    pub error_message: String,
    /// The soft-redirect target route (`soft_redirect` outcomes only).
    pub route: Option<String>,
}

impl RegexFilter {
    pub fn from_entry(entry: &PatternEntry) -> Option<Self> {
        let regexes: Vec<Regex> = entry
            .regexes
            .iter()
            .filter_map(|r| Regex::new(r).ok())
            .collect();
        if regexes.is_empty() {
            return None;
        }
        Some(Self {
            name: entry.name.clone(),
            regexes,
            outcome: entry.outcome.clone(),
            action: entry.filter_action.clone(),
            confidence_gate: entry.confidence_gate.clone(),
            scopes: entry.scope.clone(),
            error_message: entry
                .error_message
                .clone()
                .unwrap_or_else(|| format!("blocked by '{}'", entry.name)),
            route: entry.route.clone(),
        })
    }
}

impl Default for RegexFilter {
    fn default() -> Self {
        Self {
            name: String::new(),
            regexes: Vec::new(),
            outcome: FilterOutcome::HardReject,
            action: None,
            confidence_gate: ConfidenceGate::None,
            scopes: Vec::new(),
            error_message: String::new(),
            route: None,
        }
    }
}

impl Filter for RegexFilter {
    fn kind(&self) -> FilterKind {
        FilterKind::Regex
    }

    fn evaluate(&self, ctx: &FilterContext) -> Option<FilterDecision> {
        // Scope check: a filter applies only when its declared scopes
        // intersect the context's active scope set.
        let scope_ok = self.scopes.iter().any(|s| ctx.active_scopes.contains(s));
        if !scope_ok {
            return None;
        }

        match self.outcome {
            FilterOutcome::HardReject => {
                // First matching regex → hard reject
                let matched = self
                    .regexes
                    .iter()
                    .find(|re| re.is_match(&ctx.user_message))?;
                let m = matched.find(&ctx.user_message)?;

                if let ConfidenceGate::LuhnValid = self.confidence_gate {
                    if !super::luhn::luhn_valid(m.as_str()) {
                        return None;
                    }
                }

                Some(FilterDecision::HardReject {
                    pattern: self.name.clone(),
                    message: self.error_message.clone(),
                })
            }
            FilterOutcome::OutputFilter => {
                // Collect ALL regex matches for codeword substitution
                let mut matches = Vec::new();
                let action = self.action.clone().unwrap_or(FilterAction::Redact);

                for re in &self.regexes {
                    for m in re.find_iter(&ctx.user_message) {
                        if let ConfidenceGate::LuhnValid = self.confidence_gate {
                            if !super::luhn::luhn_valid(m.as_str()) {
                                continue;
                            }
                        }
                        matches.push(RegexMatch {
                            pattern_name: self.name.clone(),
                            matched_text: m.as_str().to_string(),
                            start: m.start(),
                            end: m.end(),
                            action: action.clone(),
                        });
                    }
                }

                if matches.is_empty() {
                    return None;
                }

                Some(FilterDecision::OutputFilter {
                    action,
                    matched_pattern: self.name.clone(),
                    codewords: HashMap::new(),
                    matches,
                })
            }
            FilterOutcome::SoftRedirect => {
                // A soft_redirect fires only when the pattern names a target
                // route; a route-less entry never fires (existing configs
                // without `route` behave exactly as before). The gate is the
                // same task-value check the hard-reject arm applies.
                let route = self.route.clone()?;
                let matched = self
                    .regexes
                    .iter()
                    .find(|re| re.is_match(&ctx.user_message))?;
                let m = matched.find(&ctx.user_message)?;

                if let ConfidenceGate::LuhnValid = self.confidence_gate {
                    if !super::luhn::luhn_valid(m.as_str()) {
                        return None;
                    }
                }

                Some(FilterDecision::SoftRedirect {
                    route,
                    reason: format!("pattern match '{}'", self.name),
                })
            }
        }
    }
}
#[cfg(test)]
#[path = "../../tests/filters_regex_filter.rs"]
mod tests;
