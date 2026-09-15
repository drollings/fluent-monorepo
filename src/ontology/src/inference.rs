use std::collections::{HashMap, HashSet};

use fluent_dag::type_inference::TypeInference;
use guidance_rdf::parser::Term;
use guidance_rdf::parser::Triple;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RuleType {
    SubclassTransitivity,
    SubpropertyTransitivity,
    DomainRange,
    InverseOf,
}

#[derive(Debug, Clone)]
pub struct InferenceRule {
    pub rule_type: RuleType,
    pub trigger_predicate: String,
}

#[derive(Error, Debug)]
pub enum InferenceError {
    #[error("no triples provided")]
    EmptyInput,
}

pub struct InferenceEngine {
    rules: Vec<InferenceRule>,
}

impl InferenceEngine {
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    pub fn add_rule(&mut self, rule: InferenceRule) {
        self.rules.push(rule);
    }

    pub fn infer(&self, triples: &[Triple]) -> Result<Vec<Triple>, InferenceError> {
        let mut derived: Vec<Triple> = Vec::new();

        for rule in &self.rules {
            if rule.rule_type == RuleType::SubclassTransitivity {
                infer_subclass_transitivity(triples, &mut derived, &rule.trigger_predicate);
            }
        }

        Ok(derived)
    }
}

impl Default for InferenceEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Materialize every implied subclass edge absent from the input.
///
/// IRIs are interned to dense ids and closed with the shared
/// [`TypeInference`] index (single topological pass over the acyclic
/// majority, scoped fixpoint over cyclic leftovers) instead of the old
/// O(E²) String-key round-robin fixpoint. The derived SET is identical
/// (pinned by equivalence tests); emission order is deterministic
/// (sorted by subject, then object IRI) rather than fixpoint-discovery
/// order — no caller observes order (inference tests pin counts/sets).
///
/// Reflexive pairs need care: the index counts every class as its own
/// ancestor, but the old fixpoint derived `(a, a)` only for classes on a
/// genuine directed cycle (a length-≥1 path back to themselves). The
/// materialization keeps that rule — `(a, a)` is emitted exactly when
/// some *other* class is mutually reachable with `a`.
fn infer_subclass_transitivity(base: &[Triple], derived: &mut Vec<Triple>, predicate_iri: &str) {
    let mut id_of: HashMap<String, i64> = HashMap::new();
    let mut iris: Vec<String> = Vec::new();
    let mut edges: Vec<[i64; 2]> = Vec::new();
    let mut base_pairs: HashSet<(i64, i64)> = HashSet::new();
    for t in base.iter().chain(derived.iter()) {
        if !is_subclass_triple(t, predicate_iri) {
            continue;
        }
        if let (Some(s), Some(o)) = (triple_subject_iri(t), triple_object_iri(t)) {
            let sub = *id_of.entry(s.clone()).or_insert_with(|| {
                iris.push(s.clone());
                iris.len() as i64 - 1
            });
            let obj = *id_of.entry(o.clone()).or_insert_with(|| {
                iris.push(o.clone());
                iris.len() as i64 - 1
            });
            edges.push([sub, obj]);
            base_pairs.insert((sub, obj));
        }
    }
    if iris.is_empty() {
        return;
    }
    let class_ids: Vec<i64> = (0..iris.len() as i64).collect();
    let closure = TypeInference::build(&class_ids, &edges);
    let mut implied: Vec<(String, String)> = Vec::new();
    for &sub in &class_ids {
        for &obj in &class_ids {
            if base_pairs.contains(&(sub, obj)) || !closure.is_subclass_of(sub, obj) {
                continue;
            }
            if sub == obj
                && !class_ids.iter().any(|&other| {
                    other != sub
                        && closure.is_subclass_of(sub, other)
                        && closure.is_subclass_of(other, sub)
                })
            {
                continue;
            }
            implied.push((iris[sub as usize].clone(), iris[obj as usize].clone()));
        }
    }
    implied.sort();
    for (sub, obj) in implied {
        derived.push(build_subclass_triple(&sub, predicate_iri, &obj));
    }
}

fn is_subclass_triple(t: &Triple, predicate_iri: &str) -> bool {
    matches!(&t.predicate, Term::Iri(s) if s == predicate_iri)
}

fn triple_subject_iri(t: &Triple) -> Option<String> {
    match &t.subject {
        Term::Iri(s) => Some(s.clone()),
        _ => None,
    }
}

fn triple_object_iri(t: &Triple) -> Option<String> {
    match &t.object {
        Term::Iri(s) => Some(s.clone()),
        _ => None,
    }
}

fn build_subclass_triple(subject_iri: &str, predicate_iri: &str, object_iri: &str) -> Triple {
    Triple {
        subject: Term::Iri(subject_iri.to_string()),
        predicate: Term::Iri(predicate_iri.to_string()),
        object: Term::Iri(object_iri.to_string()),
    }
}

pub struct CapabilityInference {
    hierarchy: HashMap<String, Vec<String>>,
    /// Reverse index (parent → children) maintained alongside `hierarchy`
    /// so invalidation can scope to the affected subtree instead of
    /// clearing the whole memo.
    children: HashMap<String, Vec<String>>,
    direct_capabilities: HashMap<String, HashSet<String>>,
    inferred_cache: HashMap<String, HashSet<String>>,
}

impl CapabilityInference {
    pub fn new() -> Self {
        Self {
            hierarchy: HashMap::new(),
            children: HashMap::new(),
            direct_capabilities: HashMap::new(),
            inferred_cache: HashMap::new(),
        }
    }

    pub fn load_hierarchy(&mut self, triples: &[Triple], predicate_iri: &str) {
        for t in triples {
            if !is_subclass_triple(t, predicate_iri) {
                continue;
            }
            let Some(child) = triple_subject_iri(t) else {
                continue;
            };
            let Some(parent) = triple_object_iri(t) else {
                continue;
            };
            self.hierarchy.entry(child.clone()).or_default().push(parent.clone());
            self.children.entry(parent).or_default().push(child);
        }
        // Bulk load touches an unbounded subtree; a full clear is the
        // sound (and rare — boot-time) choice. Incremental mutators below
        // scope precisely.
        self.inferred_cache.clear();
    }

    pub fn add_subclass_edge(&mut self, child_iri: &str, parent_iri: &str) {
        self.hierarchy
            .entry(child_iri.to_string())
            .or_default()
            .push(parent_iri.to_string());
        self.children
            .entry(parent_iri.to_string())
            .or_default()
            .push(child_iri.to_string());
        self.invalidate(child_iri);
    }

    pub fn register_capability(&mut self, class_iri: &str, capability_name: &str) {
        self.direct_capabilities
            .entry(class_iri.to_string())
            .or_default()
            .insert(capability_name.to_string());
        self.invalidate(class_iri);
    }

    /// Drop cached inferences for `class_iri` and its transitive
    /// descendants — exactly the classes whose inherited capability set can
    /// mention `class_iri` (capabilities flow child ← ancestors). Unrelated
    /// subtrees keep their memos.
    pub fn invalidate(&mut self, class_iri: &str) {
        let mut stack = vec![class_iri.to_string()];
        let mut visited = HashSet::new();
        while let Some(current) = stack.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            self.inferred_cache.remove(&current);
            if let Some(kids) = self.children.get(&current) {
                stack.extend(kids.iter().cloned());
            }
        }
    }

    pub fn infer_capabilities(&mut self, class_iri: &str) -> &HashSet<String> {
        if self.inferred_cache.contains_key(class_iri) {
            return &self.inferred_cache[class_iri];
        }

        let mut merged = HashSet::new();

        if let Some(direct) = self.direct_capabilities.get(class_iri) {
            merged.extend(direct.iter().cloned());
        }

        let mut visited = HashSet::new();
        self.collect_ancestor_caps(class_iri, &mut merged, &mut visited);

        self.inferred_cache.insert(class_iri.to_string(), merged);
        &self.inferred_cache[class_iri]
    }

    fn collect_ancestor_caps(
        &self,
        class_iri: &str,
        out: &mut HashSet<String>,
        visited: &mut HashSet<String>,
    ) {
        if !visited.insert(class_iri.to_string()) {
            return;
        }

        let parents = match self.hierarchy.get(class_iri) {
            Some(p) => p.clone(),
            None => return,
        };

        for parent in &parents {
            if let Some(direct) = self.direct_capabilities.get(parent) {
                out.extend(direct.iter().cloned());
            }
            self.collect_ancestor_caps(parent, out, visited);
        }
    }

    pub fn duck_type(&mut self, class_iri: &str, capability_name: &str) -> bool {
        let caps = self.infer_capabilities(class_iri);
        caps.contains(capability_name)
    }
}

impl Default for CapabilityInference {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "../tests/inference.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/inference_equiv.rs"]
mod inference_equiv_tests;
