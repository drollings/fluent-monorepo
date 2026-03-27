/// schema.zig — Coral Context SQLite Schema (DDL + Queries)
///
/// Defines all CREATE TABLE / CREATE INDEX statements and canned SQL queries
/// for the Coral Context database, backed by SQLite.
///
/// §2.2 — context_nodes: LOD text pyramid + float embedding BLOB
/// §2.3 — targets: DAG target definitions with bitset dependency words
/// §2.4 — edge tables: depends_on, provides_capability, neighbor_of
/// §2.5 — wasm_tools: compiled WASM binaries with schema hash
/// §2.7 — provenance_registry: source + authority for imported nodes
/// §2.8 — approval_workflow: human-in-the-loop review state
/// §2.9 — contradictions: conflicting statements about a subject
/// §2.10 — entity_types: rdf:type assertions from YAGO ingestion
pub const LOD_COUNT: usize = 6;

// ---------------------------------------------------------------------------
// §2.2 ContextNode Table
// ---------------------------------------------------------------------------

/// Core semantic entity.  embedding is a raw BLOB of little-endian IEEE 754
/// float32 values (4 bytes each).  valid_to is nullable.
pub const DDL_CONTEXT_NODES: []const u8 =
    \\CREATE TABLE IF NOT EXISTS context_nodes (
    \\    id INTEGER PRIMARY KEY,
    \\    lod0 TEXT NOT NULL DEFAULT '',
    \\    lod1 TEXT NOT NULL DEFAULT '',
    \\    lod2 TEXT NOT NULL DEFAULT '',
    \\    lod3 TEXT NOT NULL DEFAULT '',
    \\    lod4 TEXT NOT NULL DEFAULT '',
    \\    lod5 TEXT NOT NULL DEFAULT '',
    \\    embedding BLOB NOT NULL DEFAULT x'',
    \\    valid_from REAL NOT NULL DEFAULT 0.0,
    \\    valid_to REAL,
    \\    confidence INTEGER NOT NULL DEFAULT 0,
    \\    provenance_id INTEGER NOT NULL DEFAULT 0
    \\)
;

// ---------------------------------------------------------------------------
// §2.3 Target Table
// ---------------------------------------------------------------------------

/// DAG execution target.  depends_words / provides_words are BLOBs encoding
/// the raw usize word array for a DynamicBitSetUnmanaged (native byte order).
pub const DDL_TARGETS: []const u8 =
    \\CREATE TABLE IF NOT EXISTS targets (
    \\    id INTEGER PRIMARY KEY,
    \\    name TEXT NOT NULL,
    \\    depends_words BLOB NOT NULL DEFAULT x'',
    \\    provides_words BLOB NOT NULL DEFAULT x'',
    \\    total_bits INTEGER NOT NULL DEFAULT 0,
    \\    is_essential INTEGER NOT NULL DEFAULT 0
    \\)
;

/// Index for O(1) lookup by target name.
pub const DDL_TARGETS_NAME_INDEX: []const u8 =
    \\CREATE INDEX IF NOT EXISTS targets_by_name ON targets(name)
;

// ---------------------------------------------------------------------------
// §2.4 Edge Tables
// ---------------------------------------------------------------------------

pub const DDL_DEPENDS_ON: []const u8 =
    \\CREATE TABLE IF NOT EXISTS depends_on (
    \\    from_id INTEGER NOT NULL,
    \\    to_id INTEGER NOT NULL,
    \\    PRIMARY KEY (from_id, to_id)
    \\)
;

pub const DDL_PROVIDES_CAPABILITY: []const u8 =
    \\CREATE TABLE IF NOT EXISTS provides_capability (
    \\    from_id INTEGER NOT NULL,
    \\    to_id INTEGER NOT NULL,
    \\    PRIMARY KEY (from_id, to_id)
    \\)
;

/// NEIGHBOR_OF: semantic similarity / KNN-derived edge.
/// distance: cosine distance [0.0, 2.0].
/// edge_type: "neighbor_of" | "semantic_similarity" | "temporal_sequence" | "rdf_property"
pub const DDL_NEIGHBOR_OF: []const u8 =
    \\CREATE TABLE IF NOT EXISTS neighbor_of (
    \\    from_id INTEGER NOT NULL,
    \\    to_id INTEGER NOT NULL,
    \\    distance REAL NOT NULL DEFAULT 0.0,
    \\    edge_type TEXT NOT NULL DEFAULT 'neighbor_of',
    \\    PRIMARY KEY (from_id, to_id)
    \\)
;

// ---------------------------------------------------------------------------
// §2.5 WASM Tool Cache
// ---------------------------------------------------------------------------

pub const DDL_WASM_TOOLS: []const u8 =
    \\CREATE TABLE IF NOT EXISTS wasm_tools (
    \\    id INTEGER PRIMARY KEY,
    \\    target_id INTEGER NOT NULL DEFAULT 0,
    \\    wasm_b64 TEXT NOT NULL DEFAULT '',
    \\    schema_hash TEXT NOT NULL DEFAULT '',
    \\    test_passed INTEGER NOT NULL DEFAULT 0,
    \\    created_at REAL NOT NULL DEFAULT 0.0,
    \\    expires_at REAL,
    \\    access_count INTEGER NOT NULL DEFAULT 0
    \\)
;

// ---------------------------------------------------------------------------
// §2.7 Provenance Registry
// ---------------------------------------------------------------------------

pub const DDL_PROVENANCE_REGISTRY: []const u8 =
    \\CREATE TABLE IF NOT EXISTS provenance_registry (
    \\    provenance_id INTEGER PRIMARY KEY,
    \\    source TEXT NOT NULL DEFAULT '',
    \\    imported_at REAL NOT NULL DEFAULT 0.0,
    \\    authority TEXT NOT NULL DEFAULT ''
    \\)
;

// ---------------------------------------------------------------------------
// §2.8 Approval Workflow
// ---------------------------------------------------------------------------

pub const DDL_APPROVAL_WORKFLOW: []const u8 =
    \\CREATE TABLE IF NOT EXISTS approval_workflow (
    \\    node_id INTEGER PRIMARY KEY,
    \\    status TEXT NOT NULL DEFAULT 'pending',
    \\    reviewed_by TEXT,
    \\    reviewed_at REAL,
    \\    confidence_before INTEGER NOT NULL DEFAULT 0,
    \\    confidence_after INTEGER NOT NULL DEFAULT 0
    \\)
;

// ---------------------------------------------------------------------------
// §2.9 Contradictions
// ---------------------------------------------------------------------------

pub const DDL_CONTRADICTIONS: []const u8 =
    \\CREATE TABLE IF NOT EXISTS contradictions (
    \\    node_a INTEGER NOT NULL,
    \\    node_b INTEGER NOT NULL,
    \\    predicate TEXT NOT NULL DEFAULT '',
    \\    value_a TEXT NOT NULL DEFAULT '',
    \\    value_b TEXT NOT NULL DEFAULT '',
    \\    detected_at REAL NOT NULL DEFAULT 0.0,
    \\    PRIMARY KEY (node_a, node_b)
    \\)
;

// ---------------------------------------------------------------------------
// §2.10 Entity Types
// ---------------------------------------------------------------------------

pub const DDL_ENTITY_TYPES: []const u8 =
    \\CREATE TABLE IF NOT EXISTS entity_types (
    \\    entity_id INTEGER NOT NULL,
    \\    type_id INTEGER NOT NULL,
    \\    PRIMARY KEY (entity_id, type_id)
    \\)
;

// ---------------------------------------------------------------------------
// §2.11 Property Uses (YAGO ontology)
// ---------------------------------------------------------------------------

/// Maps predicates to their domain type, enabling property inheritance.
pub const DDL_PROPERTY_USES: []const u8 =
    \\CREATE TABLE IF NOT EXISTS property_uses (
    \\    predicate TEXT NOT NULL,
    \\    domain_type_id INTEGER NOT NULL,
    \\    FOREIGN KEY (domain_type_id) REFERENCES context_nodes(id),
    \\    PRIMARY KEY (predicate, domain_type_id)
    \\)
;

// ---------------------------------------------------------------------------
// Canned SQL queries
// ---------------------------------------------------------------------------

/// BFS hop-distance from a semantic center node, capped at depth 10.
/// Bind parameter 1: center node id (i64).
/// Returns (id, dist, lod4), ordered by dist ascending.
pub const QUERY_NEIGHBOR_BFS: []const u8 =
    \\WITH RECURSIVE bfs(id, dist) AS (
    \\    SELECT ?1, 0
    \\    UNION
    \\    SELECT n.to_id, b.dist + 1
    \\    FROM bfs b JOIN neighbor_of n ON n.from_id = b.id
    \\    WHERE b.dist < 10
    \\)
    \\SELECT b.id, b.dist, cn.lod4
    \\FROM bfs b JOIN context_nodes cn ON cn.id = b.id
    \\ORDER BY b.dist
;

/// Transitive DEPENDS_ON traversal from a root target.
/// Bind parameter 1: root target id (i64).
/// Returns (id, name, depends_words, provides_words, total_bits).
pub const QUERY_TRANSITIVE_DEPS: []const u8 =
    \\WITH RECURSIVE tr(id) AS (
    \\    SELECT to_id FROM depends_on WHERE from_id = ?1
    \\    UNION
    \\    SELECT d.to_id FROM depends_on d JOIN tr ON tr.id = d.from_id
    \\)
    \\SELECT t.id, t.name, t.depends_words, t.provides_words, t.total_bits
    \\FROM tr JOIN targets t ON t.id = tr.id
;

// ---------------------------------------------------------------------------
// Full schema initialization sequence
// ---------------------------------------------------------------------------

/// All DDL statements to initialize a fresh Coral Context database.
/// Each is idempotent via IF NOT EXISTS / IF NOT EXISTS guards.
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
    DDL_PROPERTY_USES,
};

// ---------------------------------------------------------------------------
// Schema versioning & migrations
// ---------------------------------------------------------------------------

/// Current schema version. Increment when adding migrations.
pub const SCHEMA_VERSION: u32 = 3;

/// DDL for the schema version tracking table.
pub const DDL_SCHEMA_VERSION: []const u8 =
    \\CREATE TABLE IF NOT EXISTS schema_version (
    \\    version INTEGER NOT NULL DEFAULT 0
    \\)
;

/// Migration 2 → 3: add expires_at and access_count to wasm_tools.
pub const MIGRATION_2_3: []const u8 =
    \\ALTER TABLE wasm_tools ADD COLUMN expires_at REAL;
    \\ALTER TABLE wasm_tools ADD COLUMN access_count INTEGER NOT NULL DEFAULT 0
;

/// Migrations applied sequentially from version 0 to SCHEMA_VERSION.
/// Index i applies migration from version i to i+1.
/// Empty string = no-op (initial schema already created by SCHEMA_DDL).
pub const MIGRATIONS = [_][]const u8{
    // 0 → 1: Initial schema (all tables created by SCHEMA_DDL)
    "",
    // 1 → 2: Add property_uses table
    DDL_PROPERTY_USES,
    // 2 → 3: Add expires_at and access_count to wasm_tools
    MIGRATION_2_3,
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

test "schema DDL contains expected column names" {
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

    try testing.expect(std.mem.indexOf(u8, DDL_DEPENDS_ON, "from_id") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_NEIGHBOR_OF, "distance") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_NEIGHBOR_OF, "edge_type") != null);

    try testing.expect(std.mem.indexOf(u8, DDL_WASM_TOOLS, "wasm_b64") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_WASM_TOOLS, "test_passed") != null);

    try testing.expect(std.mem.indexOf(u8, DDL_PROVENANCE_REGISTRY, "provenance_id") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_APPROVAL_WORKFLOW, "status") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_CONTRADICTIONS, "predicate") != null);
}

test "schema uses SQLite syntax" {
    try testing.expect(std.mem.indexOf(u8, DDL_CONTEXT_NODES, "CREATE TABLE") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_TARGETS, "CREATE TABLE") != null);
    // No Datalog or CozoScript syntax
    try testing.expect(std.mem.indexOf(u8, DDL_CONTEXT_NODES, ":create") == null);
    // No PostgreSQL-specific types
    try testing.expect(std.mem.indexOf(u8, DDL_CONTEXT_NODES, "VECTOR(1536)") == null);
}

test "SCHEMA_DDL has 12 statements" {
    try testing.expectEqual(@as(usize, 12), SCHEMA_DDL.len);
}

test "entity_types DDL is non-empty" {
    try testing.expect(DDL_ENTITY_TYPES.len > 0);
    try testing.expect(std.mem.indexOf(u8, DDL_ENTITY_TYPES, "entity_types") != null);
}

test "property_uses DDL is non-empty" {
    try testing.expect(DDL_PROPERTY_USES.len > 0);
    try testing.expect(std.mem.indexOf(u8, DDL_PROPERTY_USES, "property_uses") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_PROPERTY_USES, "predicate") != null);
    try testing.expect(std.mem.indexOf(u8, DDL_PROPERTY_USES, "domain_type_id") != null);
}

test "query templates are SQL" {
    try testing.expect(std.mem.indexOf(u8, QUERY_TRANSITIVE_DEPS, "SELECT") != null);
    try testing.expect(std.mem.indexOf(u8, QUERY_NEIGHBOR_BFS, "SELECT") != null);
    try testing.expect(std.mem.indexOf(u8, QUERY_NEIGHBOR_BFS, "neighbor_of") != null);
    try testing.expect(std.mem.indexOf(u8, QUERY_TRANSITIVE_DEPS, "depends_on") != null);
    // Recursive CTEs
    try testing.expect(std.mem.indexOf(u8, QUERY_NEIGHBOR_BFS, "WITH RECURSIVE") != null);
    try testing.expect(std.mem.indexOf(u8, QUERY_TRANSITIVE_DEPS, "WITH RECURSIVE") != null);
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
