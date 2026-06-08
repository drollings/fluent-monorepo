use std::collections::{HashMap, HashSet};

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
        Self {
            rules: Vec::new(),
        }
    }

    pub fn add_rule(&mut self, rule: InferenceRule) {
        self.rules.push(rule);
    }

    pub fn infer(&self, triples: &[Triple]) -> Result<Vec<Triple>, InferenceError> {
        let mut derived: Vec<Triple> = Vec::new();

        for rule in &self.rules {
            if rule.rule_type == RuleType::SubclassTransitivity {
                infer_subclass_transitivity(triples, &mut derived, &rule.trigger_predicate)?;
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

fn infer_subclass_transitivity(
    base: &[Triple],
    derived: &mut Vec<Triple>,
    predicate_iri: &str,
) -> Result<(), InferenceError> {
    let mut known: HashSet<(String, String)> = HashSet::new();

    for t in base {
        if is_subclass_triple(t, predicate_iri) {
            if let (Some(s), Some(o)) = (triple_subject_iri(t), triple_object_iri(t)) {
                known.insert((s, o));
            }
        }
    }
    for t in derived.iter() {
        if is_subclass_triple(t, predicate_iri) {
            if let (Some(s), Some(o)) = (triple_subject_iri(t), triple_object_iri(t)) {
                known.insert((s, o));
            }
        }
    }

    let mut changed = true;
    while changed {
        changed = false;
        let edges: Vec<(String, String)> = known.iter().cloned().collect();

        for (sub_a, obj_a) in &edges {
            for (sub_b, obj_b) in &edges {
                if obj_a != sub_b {
                    continue;
                }
                let new_edge = (sub_a.clone(), obj_b.clone());
                if known.contains(&new_edge) {
                    continue;
                }
                let triple = build_subclass_triple(&new_edge.0, predicate_iri, &new_edge.1);
                derived.push(triple);
                known.insert(new_edge);
                changed = true;
            }
        }
    }

    Ok(())
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
    direct_capabilities: HashMap<String, HashSet<String>>,
    inferred_cache: HashMap<String, HashSet<String>>,
}

impl CapabilityInference {
    pub fn new() -> Self {
        Self {
            hierarchy: HashMap::new(),
            direct_capabilities: HashMap::new(),
            inferred_cache: HashMap::new(),
        }
    }

    pub fn load_hierarchy(&mut self, triples: &[Triple], predicate_iri: &str) {
        for t in triples {
            if !is_subclass_triple(t, predicate_iri) {
                continue;
            }
            let child = match triple_subject_iri(t) {
                Some(s) => s,
                None => continue,
            };
            let parent = match triple_object_iri(t) {
                Some(s) => s,
                None => continue,
            };
            self.hierarchy
                .entry(child)
                .or_default()
                .push(parent);
        }
        self.inferred_cache.clear();
    }

    pub fn add_subclass_edge(&mut self, child_iri: &str, parent_iri: &str) {
        self.hierarchy
            .entry(child_iri.to_string())
            .or_default()
            .push(parent_iri.to_string());
        self.inferred_cache.clear();
    }

    pub fn register_capability(&mut self, class_iri: &str, capability_name: &str) {
        self.direct_capabilities
            .entry(class_iri.to_string())
            .or_default()
            .insert(capability_name.to_string());
        self.inferred_cache.clear();
    }

    pub fn invalidate(&mut self, _class_iri: &str) {
        self.inferred_cache.clear();
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

        self.inferred_cache
            .insert(class_iri.to_string(), merged);
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
mod tests {
    use super::*;

    fn build_triple(s: &str, p: &str, o: &str) -> Triple {
        Triple {
            subject: Term::Iri(s.to_string()),
            predicate: Term::Iri(p.to_string()),
            object: Term::Iri(o.to_string()),
        }
    }

    const RDFS_SUBCLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

    #[test]
    fn test_inference_empty() {
        let engine = InferenceEngine::new();
        let result = engine.infer(&[]).unwrap();
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_rule_addition() {
        let mut engine = InferenceEngine::new();
        engine.add_rule(InferenceRule {
            rule_type: RuleType::SubclassTransitivity,
            trigger_predicate: RDFS_SUBCLASS_OF.to_string(),
        });
        assert_eq!(engine.rules.len(), 1);
    }

    #[test]
    fn test_subclass_transitivity_ab_bc_gives_ac() {
        let ab = build_triple("Scientist", RDFS_SUBCLASS_OF, "Person");
        let bc = build_triple("Person", RDFS_SUBCLASS_OF, "Agent");

        let mut engine = InferenceEngine::new();
        engine.add_rule(InferenceRule {
            rule_type: RuleType::SubclassTransitivity,
            trigger_predicate: RDFS_SUBCLASS_OF.to_string(),
        });

        let derived = engine.infer(&[ab, bc]).unwrap();
        assert_eq!(derived.len(), 1);
        assert_eq!(
            derived[0].subject,
            Term::Iri("Scientist".to_string())
        );
        assert_eq!(
            derived[0].object,
            Term::Iri("Agent".to_string())
        );
    }

    #[test]
    fn test_subclass_transitivity_longer_chain() {
        let ab = build_triple("Developer", RDFS_SUBCLASS_OF, "Programmer");
        let bc = build_triple("Programmer", RDFS_SUBCLASS_OF, "Person");
        let cd = build_triple("Person", RDFS_SUBCLASS_OF, "Agent");

        let mut engine = InferenceEngine::new();
        engine.add_rule(InferenceRule {
            rule_type: RuleType::SubclassTransitivity,
            trigger_predicate: RDFS_SUBCLASS_OF.to_string(),
        });

        let derived = engine.infer(&[ab, bc, cd]).unwrap();
        assert_eq!(derived.len(), 3);
    }

    #[test]
    fn test_subclass_transitivity_no_new_edges() {
        let ab = build_triple("Cat", RDFS_SUBCLASS_OF, "Animal");

        let mut engine = InferenceEngine::new();
        engine.add_rule(InferenceRule {
            rule_type: RuleType::SubclassTransitivity,
            trigger_predicate: RDFS_SUBCLASS_OF.to_string(),
        });

        let derived = engine.infer(&[ab]).unwrap();
        assert_eq!(derived.len(), 0);
    }

    #[test]
    fn test_capability_inference_direct() {
        let mut ci = CapabilityInference::new();
        ci.register_capability("Person", "has_birth_date");
        assert!(ci.duck_type("Person", "has_birth_date"));
        assert!(!ci.duck_type("Person", "has_altitude"));
    }

    #[test]
    fn test_capability_inference_inherited() {
        let mut ci = CapabilityInference::new();
        let triple = build_triple("Scientist", RDFS_SUBCLASS_OF, "Person");
        ci.load_hierarchy(&[triple], RDFS_SUBCLASS_OF);
        ci.register_capability("Person", "has_birth_date");
        assert!(ci.duck_type("Scientist", "has_birth_date"));
    }

    #[test]
    fn test_capability_inference_transitive() {
        let mut ci = CapabilityInference::new();
        let ab = build_triple("Developer", RDFS_SUBCLASS_OF, "Person");
        let bc = build_triple("Person", RDFS_SUBCLASS_OF, "Agent");
        ci.load_hierarchy(&[ab, bc], RDFS_SUBCLASS_OF);
        ci.register_capability("Agent", "has_id");
        assert!(ci.duck_type("Developer", "has_id"));
    }

    #[test]
    fn test_capability_inference_cache_invalidation() {
        let mut ci = CapabilityInference::new();
        let triple = build_triple("Cat", RDFS_SUBCLASS_OF, "Animal");
        ci.load_hierarchy(&[triple], RDFS_SUBCLASS_OF);
        assert!(!ci.duck_type("Cat", "can_purr"));
        ci.register_capability("Animal", "can_breathe");
        assert!(ci.duck_type("Cat", "can_breathe"));
    }

    #[test]
    fn test_capability_inference_cycle_safe() {
        let mut ci = CapabilityInference::new();
        let ab = build_triple("CycleA", RDFS_SUBCLASS_OF, "CycleB");
        let ba = build_triple("CycleB", RDFS_SUBCLASS_OF, "CycleA");
        ci.load_hierarchy(&[ab, ba], RDFS_SUBCLASS_OF);
        ci.register_capability("CycleA", "cycle_cap");
        assert!(ci.duck_type("CycleB", "cycle_cap"));
        let caps = ci.infer_capabilities("CycleA");
        assert!(caps.contains("cycle_cap"));
    }

    #[test]
    fn test_capability_inference_traverses_chain() {
        let mut ci = CapabilityInference::new();
        let ep = build_triple("Engineer", RDFS_SUBCLASS_OF, "Person");
        let pa = build_triple("Person", RDFS_SUBCLASS_OF, "Agent");
        ci.load_hierarchy(&[ep, pa], RDFS_SUBCLASS_OF);
        ci.register_capability("Agent", "has_id");
        ci.register_capability("Person", "has_name");
        let caps = ci.infer_capabilities("Engineer");
        assert!(caps.contains("has_id"));
        assert!(caps.contains("has_name"));
        assert!(!caps.contains("has_altitude"));
    }
}
