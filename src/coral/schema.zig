/// schema.zig — CozoDB Schema (CozoScript DDL)
///
/// Defines the unified schema for Coral Context using CozoDB as the single
/// backend, replacing the previous dual-engine (pgvector + LadybugDB/Cypher).
///
/// Architecture change:
///   BEFORE: PostgreSQL (payloads + embeddings) + LadybugDB Cypher (edges)
///   AFTER:  CozoDB (payloads + embeddings + edges + time-travel)
///
/// CozoDB data model:
///   - Stored relations (:create) hold persistent data
///   - Relations can have compound keys and arbitrary value columns
///   - Graph traversal is expressed in Datalog (recursive rules)
///   - Time travel is built-in via the `@` operator (no timescaledb needed)
///   - Embeddings stored as List<Float> columns; KNN computed in Zig
///
/// §2.2 — ContextNode Relation
///   Stores the LOD text pyramid and embedding for each semantic entity.
///   i64 as primary key — simple and efficient.
///
/// §2.3 — Target Relation
///   DAG target definitions with bitmask-encoded trait sets.
///   depends_mask / provides_mask are [Int] lists of bit positions.
///
/// §2.4 — Edge Relations
///   DEPENDS_ON, PROVIDES_CAPABILITY, NEIGHBOR_OF stored as relations.
///   Graph traversal via Datalog recursive rules (replaces Cypher MATCH).
///
/// §2.5 — WASM Tool Cache
///   Stores compiled .wasm binaries for verified LLM-generated tools.
///
/// §2.6 — Time Travel
///   CozoDB's built-in versioning is used for temporal queries.
///   Query state at time T: ?[...] := *relation @ T [...]
pub const LOD_COUNT: usize = 6;

// ---------------------------------------------------------------------------
// §2.2 ContextNode Relation
// ---------------------------------------------------------------------------

/// Core semantic entity. i64 id column.
/// LOD levels: 0=max detail, 1=summary, 2=brief, 3=tiny, 4=name, 5=minimal/alias
pub const DDL_CONTEXT_NODES: []const u8 =
    \\:create context_nodes {
    \\    id: Int
    \\    =>
    \\    lod0: String default "",
    \\    lod1: String default "",
    \\    lod2: String default "",
    \\    lod3: String default "",
    \\    lod4: String default "",
    \\    lod5: String default "",
    \\    embedding: [Float] default [],
    \\    valid_from: Float default 0.0,
    \\    valid_to: Float? default null,
    \\    confidence: Int default 0,
    \\    provenance_id: Int default 0
    \\}
;

// ---------------------------------------------------------------------------
// §2.3 Target Relation
// ---------------------------------------------------------------------------

/// DAG execution target.
/// depends_words / provides_words store the raw bitset word arrays as [Int],
/// allowing capability sets larger than 63 bits.  total_bits records the
/// logical bit-length so round-trips preserve trailing zero words.
pub const DDL_TARGETS: []const u8 =
    \\:create targets {
    \\    id: Int
    \\    =>
    \\    name: String,
    \\    depends_words: [Int] default [],
    \\    provides_words: [Int] default [],
    \\    total_bits: Int default 0,
    \\    is_essential: Bool default false
    \\}
;

/// Name index for O(1) lookup by target name.
pub const DDL_TARGETS_NAME_INDEX: []const u8 =
    \\::index create targets:by_name { name }
;

// ---------------------------------------------------------------------------
// §2.4 Edge Relations
// ---------------------------------------------------------------------------

/// DEPENDS_ON edge: from → to.
pub const DDL_DEPENDS_ON: []const u8 =
    \\:create depends_on {
    \\    from: Int,
    \\    to: Int
    \\}
;

/// PROVIDES_CAPABILITY edge.
pub const DDL_PROVIDES_CAPABILITY: []const u8 =
    \\:create provides_capability {
    \\    from: Int,
    \\    to: Int
    \\}
;

/// NEIGHBOR_OF edge (semantic similarity / KNN-derived).
/// distance: cosine distance from vector search (lower = more similar).
/// edge_type: "neighbor_of" | "semantic_similarity" | "temporal_sequence"
pub const DDL_NEIGHBOR_OF: []const u8 =
    \\:create neighbor_of {
    \\    from: Int,
    \\    to: Int
    \\    =>
    \\    distance: Float default 0.0,
    \\    edge_type: String default "neighbor_of"
    \\}
;

// ---------------------------------------------------------------------------
// §2.5 WASM Tool Cache
// ---------------------------------------------------------------------------

/// Stores compiled .wasm binaries for LLM-generated tools that passed testing.
/// wasm_bytes stored as String (base64-encoded) since CozoDB has no BYTEA.
pub const DDL_WASM_TOOLS: []const u8 =
    \\:create wasm_tools {
    \\    id: Int
    \\    =>
    \\    target_id: Int default 0,
    \\    wasm_b64: String default "",
    \\    schema_hash: String default "",
    \\    test_passed: Bool default false,
    \\    created_at: Float default 0.0
    \\}
;

// ---------------------------------------------------------------------------
// §2.7 Provenance Registry
// ---------------------------------------------------------------------------

/// Tracks the source of each node (YAGO, LLM, User, etc.)
pub const DDL_PROVENANCE_REGISTRY: []const u8 =
    \\:create provenance_registry {
    \\    provenance_id: Int
    \\    =>
    \\    source: String default "",
    \\    imported_at: Float default 0.0,
    \\    authority: String default ""
    \\}
;

// ---------------------------------------------------------------------------
// §2.8 Approval Workflow
// ---------------------------------------------------------------------------

/// Human-in-the-loop approval state for LLM-generated or low-confidence nodes.
pub const DDL_APPROVAL_WORKFLOW: []const u8 =
    \\:create approval_workflow {
    \\    node: Int
    \\    =>
    \\    status: String default "pending",
    \\    reviewed_by: String? default null,
    \\    reviewed_at: Float? default null,
    \\    confidence_before: Int default 0,
    \\    confidence_after: Int default 0
    \\}
;

// ---------------------------------------------------------------------------
// §2.10 Entity Types
// ---------------------------------------------------------------------------

/// Stores rdf:type assertions: entity → type class.
/// Used for YAGO ingestion to record class membership.
pub const DDL_ENTITY_TYPES: []const u8 =
    \\:create entity_types {
    \\    entity: Int,
    \\    type: Int
    \\}
;

// ---------------------------------------------------------------------------
// §2.9 Contradictions
// ---------------------------------------------------------------------------

/// Tracks conflicting statements about the same subject+predicate.
pub const DDL_CONTRADICTIONS: []const u8 =
    \\:create contradictions {
    \\    node_a: Int,
    \\    node_b: Int
    \\    =>
    \\    predicate: String default "",
    \\    value_a: String default "",
    \\    value_b: String default "",
    \\    detected_at: Float default 0.0
    \\}
;

// ---------------------------------------------------------------------------
// §2.6 Datalog Graph Traversal Queries (replaces Cypher MATCH)
// ---------------------------------------------------------------------------

/// Transitive DEPENDS_ON traversal from a root target.
/// Usage: substitute $root, then run as a read query.
pub const QUERY_TRANSITIVE_DEPS: []const u8 =
    \\# Find all transitive dependencies of a target
    \\transitive[to] :=
    \\    *depends_on{ from: $root, to }
    \\transitive[to] :=
    \\    transitive[from],
    \\    *depends_on{ from, to }
    \\?[to, name, depends_words, provides_words, total_bits] :=
    \\    transitive[to],
    \\    *targets{ id: to, name, depends_words, provides_words, total_bits }
;

/// BFS neighbor traversal for LOD routing.
/// Returns (node_id, hop_count) from semantic center.
pub const QUERY_NEIGHBOR_BFS: []const u8 =
    \\# Compute hop distance from semantic center for LOD selection
    \\bfs[to, dist] :=
    \\    to = $center, dist = 0
    \\bfs[to, dist + 1] :=
    \\    bfs[from, dist],
    \\    *neighbor_of{ from, to }
    \\?[id, dist, lod4] :=
    \\    bfs[id, dist],
    \\    *context_nodes{ id, lod4 }
    \\    :order dist
;

// ---------------------------------------------------------------------------
// Full schema initialization sequence
// ---------------------------------------------------------------------------

/// All DDL statements to initialize a fresh Coral Context database.
/// Execute in order — indexes depend on the base relations.
pub const SCHEMA_DDL = [_][]const u8{
    DDL_CONTEXT_NODES,
    DDL_TARGETS,
    DDL_TARGETS_NAME_INDEX,
    DDL_DEPENDS_ON,
    DDL_PROVIDES_CAPABILITY,
    DDL_NEIGHBOR_OF,
    DDL_WASM_TOOLS,
    DDL_PROVENANCE_REGISTRY,
    DDL_APPROVAL_WORKFLOW,
    DDL_CONTRADICTIONS,
    DDL_ENTITY_TYPES,
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const std = @import("std");
const testing = std.testing;

test "schema DDL constants are non-empty" {
    try testing.expect(DDL_CONTEXT_NODES.len > 0);
    try testing.expect(DDL_TARGETS.len > 0);
    try testing.expect(DDL_DEPENDS_ON.len > 0);
    try testing.expect(DDL_PROVIDES_CAPABILITY.len > 0);
    try testing.expect(DDL_NEIGHBOR_OF.len > 0);
    try testing.expect(DDL_WASM_TOOLS.len > 0);
}

test "LOD_COUNT is 6" {
    try testing.expectEqual(@as(usize, 6), LOD_COUNT);
}

test "schema DDL contains expected keywords" {
    try testing.expect(std.mem.indexOf(u8, DDL_CONTEXT_NODES, "lod0") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_CONTEXT_NODES, "lod4") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_CONTEXT_NODES, "lod5") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_CONTEXT_NODES, "embedding") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_CONTEXT_NODES, "valid_from") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_CONTEXT_NODES, "valid_to") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_CONTEXT_NODES, "confidence") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_CONTEXT_NODES, "provenance_id") != null);

    try testing.expect(std.mem.indexOf(u8, DDL_TARGETS, "depends_words") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_TARGETS, "provides_words") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_TARGETS, "total_bits") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_TARGETS, "is_essential") != null);

    try testing.expect(std.mem.indexOf(u8, DDL_DEPENDS_ON, "from") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_NEIGHBOR_OF, "distance") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_NEIGHBOR_OF, "edge_type") != null);

    try testing.expect(std.mem.indexOf(u8, DDL_WASM_TOOLS, "wasm_b64") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_WASM_TOOLS, "test_passed") != null);

    try testing.expect(std.mem.indexOf(u8, DDL_PROVENANCE_REGISTRY, "provenance_id") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_APPROVAL_WORKFLOW, "status") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_CONTRADICTIONS, "predicate") != null);
}

test "schema uses CozoDB syntax not PostgreSQL" {
    try testing.expect(std.mem.indexOf(u8, DDL_CONTEXT_NODES, ":create") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_TARGETS, ":create") != null);

    try testing.expect(std.mem.indexOf(u8, DDL_CONTEXT_NODES, "CREATE TABLE") == null);
    try testing.expect(std.mem.indexOf(u8, DDL_CONTEXT_NODES, "VECTOR(1536)") == null);
}

test "schema uses CozoDB syntax not Cypher" {
    try testing.expect(std.mem.indexOf(u8, DDL_CONTEXT_NODES, "CREATE NODE TABLE") == null);
    try testing.expect(std.mem.indexOf(u8, DDL_DEPENDS_ON, "CREATE REL TABLE") == null);
}

test "SCHEMA_DDL has 11 statements" {
    try testing.expectEqual(@as(usize, 11), SCHEMA_DDL.len);
}

test "entity_types DDL is non-empty" {
    try testing.expect(DDL_ENTITY_TYPES.len > 0);
    try testing.expect(std.mem.indexOf(u8, DDL_ENTITY_TYPES, "entity_types") != null);
}

test "query templates contain Datalog syntax" {
    try testing.expect(std.mem.indexOf(u8, QUERY_TRANSITIVE_DEPS, ":=") != null);
    try testing.expect(std.mem.indexOf(u8, QUERY_NEIGHBOR_BFS, ":=") != null);

    try testing.expect(std.mem.indexOf(u8, QUERY_NEIGHBOR_BFS, "neighbor_of") != null);

    try testing.expect(std.mem.indexOf(u8, QUERY_TRANSITIVE_DEPS, "depends_on") != null);
}

test "new DDL constants are non-empty" {
    try testing.expect(DDL_PROVENANCE_REGISTRY.len > 0);
    try testing.expect(DDL_APPROVAL_WORKFLOW.len > 0);
    try testing.expect(DDL_CONTRADICTIONS.len > 0);
}

test "LOD columns are generically named" {
    try testing.expect(std.mem.indexOf(u8, DDL_CONTEXT_NODES, "lod0_full") == null);
    try testing.expect(std.mem.indexOf(u8, DDL_CONTEXT_NODES, "lod1_summary") == null);
    try testing.expect(std.mem.indexOf(u8, DDL_CONTEXT_NODES, "lod2_brief") == null);
    try testing.expect(std.mem.indexOf(u8, DDL_CONTEXT_NODES, "lod3_tiny") == null);
    try testing.expect(std.mem.indexOf(u8, DDL_CONTEXT_NODES, "lod4_name") == null);
}
