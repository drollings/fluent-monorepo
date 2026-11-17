use super::*;
use guidance_rdf::parser::Parser;

fn parse_one(src: &str) -> Triple {
    let mut p = Parser::new(src);
    p.next().unwrap().unwrap()
}

#[test]
fn type_triple_registers_entity_node() {
    let mut mapper = TripleMapper::new(MappingConfig::default());
    let triple = parse_one(
        "<http://example.org/alice> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://schema.org/Person> .",
    );
    mapper.process_triple(&triple).unwrap();
    assert!(mapper.pending_node_count() >= 1);
}

#[test]
fn label_routes_to_lod4_and_drains() {
    let mut mapper = TripleMapper::new(MappingConfig::default());
    let triple = parse_one(
        "<http://example.org/alice> <http://www.w3.org/2000/01/rdf-schema#label> \"Alice\" .",
    );
    mapper.process_triple(&triple).unwrap();
    let nodes = mapper.drain_nodes();
    let node = nodes
        .iter()
        .find(|n| !n.lod[4].is_empty())
        .expect("lod4 node");
    assert_eq!(String::from_utf8_lossy(&node.lod[4]), "Alice");
    assert_eq!(mapper.pending_node_count(), 0);
}

#[test]
fn object_property_creates_edge_and_pending_counts() {
    let mut mapper = TripleMapper::new(MappingConfig::default());
    let triple = parse_one(
        "<http://example.org/alice> <http://example.org/knows> <http://example.org/bob> .",
    );
    mapper.process_triple(&triple).unwrap();
    assert_eq!(mapper.pending_edge_count(), 1);
    assert_eq!(mapper.drain_edges().len(), 1);
}
