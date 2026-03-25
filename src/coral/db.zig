/// db.zig — Coral Context Database Layer (SQLite backend)
///
/// SQLite backend storing the LOD text pyramid, float embeddings, and graph
/// edges using standard SQL and a recursive CTE for BFS graph traversal.
/// Semantic search uses in-process cosine similarity over stored embeddings.
///
/// Architecture:
///   §3.1 ContextNode — the universal semantic entity with LOD text pyramid
///   §3.2 Library     — SQLite database handle + schema initialization
///   §3.3 HydrationPipeline — embed → KNN (Zig cosine) → persist neighbor edges
///   §3.4 ContextPacker — LOD selection based on BFS graph distance
///   §3.5 NodeId — i64, matching SQLite INTEGER PRIMARY KEY
///
/// Embedding storage:
///   Float32 vectors are stored as raw BLOB (native byte order, 4 bytes/float).
///   KNN search: fetch all candidate BLOBs → decode to []f32 → cosine similarity
///   in Zig → sort top-K.  Correct for edge scale (≤100K nodes on-device).
///
/// Bitset storage (targets table):
///   DynamicBitSetUnmanaged word arrays stored as BLOB (raw usize bytes,
///   native byte order).  total_bits records the logical width for round-trips.
const std = @import("std");
pub const schema = @import("schema.zig");
const reflection = @import("common").reflection;
pub const SharedString = @import("common").SharedString;

const c = @cImport({
    @cInclude("sqlite3.h");
});

const SQLITE_STATIC: c.sqlite3_destructor_type = null;
const BUSY_TIMEOUT_MS: c_int = 5000;

// ---------------------------------------------------------------------------
// §3.5 NodeId — i64 matching SQLite INTEGER PRIMARY KEY
// ---------------------------------------------------------------------------

pub const NodeId = i64;

// ---------------------------------------------------------------------------
// §3.1 ContextNode — universal semantic entity
// ---------------------------------------------------------------------------

/// Manages context nodes for Zig's database layer, owns state, ensures consistent initialization/deinit, not thread-safe.
pub const ContextNode = struct {
    id: i64,
    /// Ref-counted backing store for lod[0].  Non-null for nodes created via
    /// init() or fetched from the DB.  lod_owned bit 0 must always be clear.
    source: ?SharedString.Ref = null,
    lod: [schema.LOD_COUNT][]const u8,
    /// Bitmask: bit i set → lod[i] is allocator-owned.  Bit 0 always clear.
    lod_owned: u8 = 0,
    embedding: []const f32,
    valid_from: f64,
    valid_to: ?f64,
    confidence: i32,
    provenance_id: i32,

    /// Create a new ContextNode with lod[0] = full_text (SharedString) and
    /// lod[4] = name (allocator-owned copy).  All other LOD slots are "".
    pub fn init(id: i64, name: []const u8, full_text: []const u8, allocator: std.mem.Allocator) !ContextNode {
        const src = try SharedString.Ref.init(allocator, full_text);
        errdefer src.deinit(allocator);
        const name_copy = try allocator.dupe(u8, name);
        errdefer allocator.free(name_copy);

        return ContextNode{
            .id = id,
            .source = src,
            .lod = [_][]const u8{ src.slice(), "", "", "", name_copy, "" },
            .lod_owned = 1 << 4, // only lod[4] (name) is allocator-owned
            .embedding = &[_]f32{},
            .valid_from = @floatFromInt(std.time.timestamp()),
            .valid_to = null,
            .confidence = 0,
            .provenance_id = 0,
        };
    }

    pub fn getLod(self: *const ContextNode, level: u3) []const u8 {
        if (level >= schema.LOD_COUNT) return "";
        return self.lod[level];
    }

    /// Set LOD level 1–5.  Level 0 is read-only here; use setSource() instead.
    pub fn setLod(self: *ContextNode, level: u3, value: []const u8) void {
        if (level == 0 or level >= schema.LOD_COUNT) return;
        self.lod[level] = value;
    }

    /// Replace the shared source text (lod[0]).  Releases the old SharedString.
    pub fn setSource(self: *ContextNode, allocator: std.mem.Allocator, text: []const u8) !void {
        const new_src = try SharedString.Ref.init(allocator, text);
        if (self.source) |old| old.deinit(allocator);
        self.source = new_src;
        self.lod[0] = new_src.slice();
    }

    /// Deep-copy this node.  lod[0] is shared via clone() (no byte copy);
    /// lod[1..5] slots marked in lod_owned are duped into `allocator`.
    pub fn clone(self: *const ContextNode, allocator: std.mem.Allocator) !ContextNode {
        var copy = self.*;
        copy.lod_owned = 0;

        // lod[0]: clone the SharedString ref or dupe the raw slice.
        if (self.source) |src| {
            copy.source = src.clone();
            copy.lod[0] = copy.source.?.slice();
        } else if (self.lod_owned & 1 != 0) {
            copy.lod[0] = try allocator.dupe(u8, self.lod[0]);
            copy.lod_owned |= 1;
        }

        // lod[1..5]: dupe allocator-owned slots.
        for (1..schema.LOD_COUNT) |i| {
            if (self.lod_owned & (@as(u8, 1) << @intCast(i)) != 0) {
                copy.lod[i] = try allocator.dupe(u8, self.lod[i]);
                copy.lod_owned |= @as(u8, 1) << @intCast(i);
            }
        }

        return copy;
    }

    /// Release all owned resources.  Safe to call multiple times.
    pub fn free(self: *ContextNode, allocator: std.mem.Allocator) void {
        // lod[0]: release via SharedString ref.
        if (self.source) |src| {
            src.deinit(allocator);
            self.source = null;
            self.lod[0] = "";
        }
        // lod[1..5]: free allocator-owned slots (bit 0 must always be clear).
        for (&self.lod, 0..) |*slot, i| {
            if (self.lod_owned & (@as(u8, 1) << @intCast(i)) != 0) {
                allocator.free(slot.*);
                slot.* = "";
                self.lod_owned &= ~(@as(u8, 1) << @intCast(i));
            }
        }
    }
};

/// KnnHit — result from in-Zig cosine similarity search.
pub const KnnHit = struct {
    id: i64,
    name: []const u8,
    distance: f32, // cosine distance in [0, 2]; 0 = identical
};

/// EdgeType — logical classification for neighbor_of edges.
pub const EdgeType = enum(i16) {
    depends_on = 0,
    provides_capability = 1,
    neighbor_of = 2,
    semantic_similarity = 3,
    temporal_sequence = 4,

    /// SQL table name (depends_on uses its own table; others share neighbor_of).
    pub fn relation(self: EdgeType) []const u8 {
        return switch (self) {
            .depends_on => "depends_on",
            .provides_capability => "provides_capability",
            .neighbor_of, .semantic_similarity, .temporal_sequence => "neighbor_of",
        };
    }

    /// String stored in the `edge_type` column of neighbor_of.
    pub fn label(self: EdgeType) []const u8 {
        return switch (self) {
            .depends_on => "depends_on",
            .provides_capability => "provides_capability",
            .neighbor_of => "neighbor_of",
            .semantic_similarity => "semantic_similarity",
            .temporal_sequence => "temporal_sequence",
        };
    }
};

/// GraphNode — node + BFS hop distance from semantic center.
pub const GraphNode = struct {
    id: i64,
    graph_distance: u32,
};

/// WasmTool — mirror of the `wasm_tools` SQLite table.
///
/// `editable` gives zero-overhead reflection support for TUI/REPL field
/// editing, config-file hydration, and DynamicEditable round-trips.
pub const WasmTool = struct {
    id: i64 = 0,
    target_id: i64 = 0,
    wasm_b64: []const u8 = "",
    schema_hash: []const u8 = "",
    test_passed: bool = false,
    created_at: f64 = 0.0,
    /// Zero-size reflection mixin.
    editable: reflection.Editable(@This()) = .{},
};

// ---------------------------------------------------------------------------
// §3.2 Library — SQLite database handle
// ---------------------------------------------------------------------------

/// StorageEngine — how to open the SQLite database.
pub const StorageEngine = enum {
    mem, // in-memory (:memory:) — for testing/ephemeral use
    sqlite, // persistent file
};

/// Library — unified database interface backed by SQLite.
pub const Library = struct {
    const Self = @This();

    allocator: std.mem.Allocator,
    db: ?*c.sqlite3,
    initialized: bool = false,
    mu: std.Thread.Mutex = .{},

    // ------------------------------------------------------------------
    // Lifecycle
    // ------------------------------------------------------------------

    /// Open the SQLite database.
    /// engine: .sqlite (persistent) or .mem (testing/ephemeral)
    /// path: file path for sqlite; ignored for mem
    pub fn init(
        allocator: std.mem.Allocator,
        engine: StorageEngine,
        path: []const u8,
    ) !*Self {
        const db_path: [:0]const u8 = switch (engine) {
            .mem => ":memory:",
            .sqlite => blk: {
                const p = try allocator.dupeZ(u8, path);
                break :blk p;
            },
        };
        defer if (engine == .sqlite) allocator.free(db_path);

        var db: ?*c.sqlite3 = null;
        const rc = c.sqlite3_open(db_path.ptr, &db);
        if (rc != c.SQLITE_OK) {
            if (db) |d| _ = c.sqlite3_close(d);
            return error.SqliteOpenFailed;
        }
        _ = c.sqlite3_busy_timeout(db, BUSY_TIMEOUT_MS);

        // Enable WAL mode for concurrent reads, and foreign keys.
        _ = c.sqlite3_exec(db, "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;", null, null, null);

        const self = try allocator.create(Self);
        self.* = .{ .allocator = allocator, .db = db };
        return self;
    }

    pub fn deinit(self: *Self) void {
        if (self.db) |db| {
            _ = c.sqlite3_close(db);
            self.db = null;
        }
        self.allocator.destroy(self);
    }

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    /// Run a single SQL statement (no params, no result rows).
    fn exec(self: *Self, sql: []const u8) !void {
        const sql_z = try self.allocator.dupeZ(u8, sql);
        defer self.allocator.free(sql_z);
        const rc = c.sqlite3_exec(self.db, sql_z.ptr, null, null, null);
        if (rc != c.SQLITE_OK) return error.SqliteExecFailed;
    }

    /// Prepare a statement; caller must finalize.
    fn prepare(self: *Self, sql: []const u8) !*c.sqlite3_stmt {
        var stmt: ?*c.sqlite3_stmt = null;
        const rc = c.sqlite3_prepare_v2(self.db, sql.ptr, @intCast(sql.len), &stmt, null);
        if (rc != c.SQLITE_OK or stmt == null) return error.SqlitePrepareFailed;
        return stmt.?;
    }

    fn step(stmt: *c.sqlite3_stmt) !bool {
        const rc = c.sqlite3_step(stmt);
        return switch (rc) {
            c.SQLITE_DONE => false,
            c.SQLITE_ROW => true,
            else => error.SqliteStepFailed,
        };
    }

    // ------------------------------------------------------------------
    // Schema initialization
    // ------------------------------------------------------------------

    /// Create all tables and indexes (idempotent — IF NOT EXISTS guards).
    pub fn initSchema(self: *Self) !void {
        for (schema.SCHEMA_DDL) |ddl| {
            try self.exec(ddl);
        }
        self.initialized = true;
    }

    // ------------------------------------------------------------------
    // ContextNode persistence
    // ------------------------------------------------------------------

    /// Insert or replace a ContextNode (upsert by id).
    pub fn insertNode(self: *Self, node: ContextNode) !void {
        self.mu.lock();
        defer self.mu.unlock();
        const sql =
            \\INSERT OR REPLACE INTO context_nodes
            \\    (id, lod0, lod1, lod2, lod3, lod4, lod5,
            \\     embedding, valid_from, valid_to, confidence, provenance_id)
            \\VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
        ;
        const stmt = try self.prepare(sql);
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_int64(stmt, 1, node.id);
        inline for (0..schema.LOD_COUNT) |i| {
            _ = c.sqlite3_bind_text(stmt, @intCast(i + 2), node.lod[i].ptr, @intCast(node.lod[i].len), SQLITE_STATIC);
        }
        const emb_bytes = std.mem.sliceAsBytes(node.embedding);
        _ = c.sqlite3_bind_blob(stmt, 8, emb_bytes.ptr, @intCast(emb_bytes.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_double(stmt, 9, node.valid_from);
        if (node.valid_to) |vt| {
            _ = c.sqlite3_bind_double(stmt, 10, vt);
        } else {
            _ = c.sqlite3_bind_null(stmt, 10);
        }
        _ = c.sqlite3_bind_int64(stmt, 11, node.confidence);
        _ = c.sqlite3_bind_int64(stmt, 12, node.provenance_id);

        _ = try step(stmt);
    }

    /// Fetch a ContextNode by id.  Strings in the returned node are
    /// allocator-owned; free with node.free(allocator).
    pub fn fetchNode(self: *Self, id: i64) !?ContextNode {
        const sql =
            \\SELECT lod0, lod1, lod2, lod3, lod4, lod5,
            \\       embedding, valid_from, valid_to, confidence, provenance_id
            \\FROM context_nodes WHERE id = ?1
        ;
        const stmt = try self.prepare(sql);
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_int64(stmt, 1, id);
        if (!try step(stmt)) return null;

        var node = ContextNode{
            .id = id,
            .lod = [_][]const u8{ "", "", "", "", "", "" },
            .embedding = &[_]f32{},
            .valid_from = c.sqlite3_column_double(stmt, 7),
            .valid_to = blk: {
                if (c.sqlite3_column_type(stmt, 8) == c.SQLITE_NULL) break :blk null;
                break :blk c.sqlite3_column_double(stmt, 8);
            },
            .confidence = @intCast(c.sqlite3_column_int(stmt, 9)),
            .provenance_id = @intCast(c.sqlite3_column_int(stmt, 10)),
        };

        // lod[0]: wrap in a SharedString so callers can share without copying.
        {
            const ptr: [*c]const u8 = c.sqlite3_column_text(stmt, 0);
            const col_len: usize = @intCast(c.sqlite3_column_bytes(stmt, 0));
            const s = if (ptr != null) ptr[0..col_len] else "";
            const src = try SharedString.Ref.init(self.allocator, s);
            node.source = src;
            node.lod[0] = src.slice();
            // bit 0 stays clear — lod[0] lifetime managed by source.
        }
        // lod[1..5]: allocator-owned copies.
        inline for (1..schema.LOD_COUNT) |i| {
            const ptr: [*c]const u8 = c.sqlite3_column_text(stmt, @intCast(i));
            const col_len: usize = @intCast(c.sqlite3_column_bytes(stmt, @intCast(i)));
            const s = if (ptr != null) ptr[0..col_len] else "";
            node.lod[i] = try self.allocator.dupe(u8, s);
            node.lod_owned |= @as(u8, 1) << @intCast(i);
        }

        return node;
    }

    // ------------------------------------------------------------------
    // Target persistence
    // ------------------------------------------------------------------

    /// Insert or replace a target definition.
    pub fn insertTarget(
        self: *Self,
        id: i64,
        name: []const u8,
        depends: *const std.bit_set.DynamicBitSetUnmanaged,
        provides: *const std.bit_set.DynamicBitSetUnmanaged,
        total_bits: usize,
        is_essential: bool,
    ) !void {
        const sql =
            \\INSERT OR REPLACE INTO targets
            \\    (id, name, depends_words, provides_words, total_bits, is_essential)
            \\VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        ;
        const stmt = try self.prepare(sql);
        defer _ = c.sqlite3_finalize(stmt);

        const bits_per_word = @bitSizeOf(usize);
        const dep_words = (depends.bit_length + bits_per_word - 1) / bits_per_word;
        const prov_words = (provides.bit_length + bits_per_word - 1) / bits_per_word;

        const dep_bytes = std.mem.sliceAsBytes(depends.masks[0..dep_words]);
        const prov_bytes = std.mem.sliceAsBytes(provides.masks[0..prov_words]);

        _ = c.sqlite3_bind_int64(stmt, 1, id);
        _ = c.sqlite3_bind_text(stmt, 2, name.ptr, @intCast(name.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_blob(stmt, 3, dep_bytes.ptr, @intCast(dep_bytes.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_blob(stmt, 4, prov_bytes.ptr, @intCast(prov_bytes.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_int64(stmt, 5, @intCast(total_bits));
        _ = c.sqlite3_bind_int64(stmt, 6, if (is_essential) 1 else 0);

        _ = try step(stmt);
    }

    // ------------------------------------------------------------------
    // WasmTool persistence
    // ------------------------------------------------------------------

    /// Insert or replace a WasmTool record.
    pub fn insertWasmTool(self: *Self, tool: WasmTool) !void {
        self.mu.lock();
        defer self.mu.unlock();
        const sql =
            \\INSERT OR REPLACE INTO wasm_tools
            \\    (id, target_id, wasm_b64, schema_hash, test_passed, created_at)
            \\VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        ;
        const stmt = try self.prepare(sql);
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_int64(stmt, 1, tool.id);
        _ = c.sqlite3_bind_int64(stmt, 2, tool.target_id);
        _ = c.sqlite3_bind_text(stmt, 3, tool.wasm_b64.ptr, @intCast(tool.wasm_b64.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 4, tool.schema_hash.ptr, @intCast(tool.schema_hash.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_int64(stmt, 5, if (tool.test_passed) 1 else 0);
        _ = c.sqlite3_bind_double(stmt, 6, tool.created_at);

        _ = try step(stmt);
    }

    /// Fetch a WasmTool by id.  Strings are allocator-owned; free with freeWasmTool().
    pub fn fetchWasmTool(self: *Self, id: i64) !?WasmTool {
        const sql =
            \\SELECT target_id, wasm_b64, schema_hash, test_passed, created_at
            \\FROM wasm_tools WHERE id = ?1
        ;
        const stmt = try self.prepare(sql);
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_int64(stmt, 1, id);
        if (!try step(stmt)) return null;

        const wasm_ptr: [*c]const u8 = c.sqlite3_column_text(stmt, 1);
        const wasm_len: usize = @intCast(c.sqlite3_column_bytes(stmt, 1));
        const hash_ptr: [*c]const u8 = c.sqlite3_column_text(stmt, 2);
        const hash_len: usize = @intCast(c.sqlite3_column_bytes(stmt, 2));

        return WasmTool{
            .id = id,
            .target_id = c.sqlite3_column_int64(stmt, 0),
            .wasm_b64 = try self.allocator.dupe(u8, if (wasm_ptr != null) wasm_ptr[0..wasm_len] else ""),
            .schema_hash = try self.allocator.dupe(u8, if (hash_ptr != null) hash_ptr[0..hash_len] else ""),
            .test_passed = c.sqlite3_column_int(stmt, 3) != 0,
            .created_at = c.sqlite3_column_double(stmt, 4),
        };
    }

    /// Free allocator-owned strings in a WasmTool returned by fetchWasmTool().
    pub fn freeWasmTool(self: *Self, tool: WasmTool) void {
        self.allocator.free(tool.wasm_b64);
        self.allocator.free(tool.schema_hash);
    }

    // ------------------------------------------------------------------
    // Edge insertion
    // ------------------------------------------------------------------

    /// Insert a DEPENDS_ON edge (upsert by primary key).
    pub fn insertDependsOn(self: *Self, from_id: i64, to_id: i64) !void {
        const sql = "INSERT OR IGNORE INTO depends_on (from_id, to_id) VALUES (?1, ?2)";
        const stmt = try self.prepare(sql);
        defer _ = c.sqlite3_finalize(stmt);
        _ = c.sqlite3_bind_int64(stmt, 1, from_id);
        _ = c.sqlite3_bind_int64(stmt, 2, to_id);
        _ = try step(stmt);
    }

    /// Insert a NEIGHBOR_OF edge with cosine distance and edge type.
    pub fn insertNeighborOf(
        self: *Self,
        from_id: i64,
        to_id: i64,
        distance: f32,
        edge_type: EdgeType,
    ) !void {
        self.mu.lock();
        defer self.mu.unlock();
        const sql =
            \\INSERT OR REPLACE INTO neighbor_of (from_id, to_id, distance, edge_type)
            \\VALUES (?1, ?2, ?3, ?4)
        ;
        const stmt = try self.prepare(sql);
        defer _ = c.sqlite3_finalize(stmt);
        const et = edge_type.label();
        _ = c.sqlite3_bind_int64(stmt, 1, from_id);
        _ = c.sqlite3_bind_int64(stmt, 2, to_id);
        _ = c.sqlite3_bind_double(stmt, 3, distance);
        _ = c.sqlite3_bind_text(stmt, 4, et.ptr, @intCast(et.len), SQLITE_STATIC);
        _ = try step(stmt);
    }

    // ------------------------------------------------------------------
    // Pipeline factories
    // ------------------------------------------------------------------

    pub fn createHydrationPipeline(self: *Self) HydrationPipeline {
        return HydrationPipeline.init(self.allocator, self);
    }

    pub fn createContextPacker(self: *Self, max_tokens: usize) ContextPacker {
        return ContextPacker.init(self.allocator, self, max_tokens);
    }

    /// Persist NEIGHBOR_OF edges from KNN hits.
    /// The center is knn_hits[0]; edges are written from center to all others.
    pub fn persistNeighborEdges(self: *Self, knn_hits: []const KnnHit) !void {
        if (knn_hits.len == 0) return;
        const center = knn_hits[0];
        for (knn_hits[1..]) |hit| {
            try self.insertNeighborOf(center.id, hit.id, hit.distance, .neighbor_of);
        }
    }

    // ------------------------------------------------------------------
    // Graph traversal helpers
    // ------------------------------------------------------------------

    /// Fetch outgoing neighbor IDs from neighbor_of for `node_id`.
    /// Returns a caller-owned slice; free with `allocator.free`.
    pub fn getNeighborIds(self: *Self, allocator: std.mem.Allocator, node_id: i64) ![]i64 {
        const sql = "SELECT to_id FROM neighbor_of WHERE from_id = ?1";
        const stmt = try self.prepare(sql);
        defer _ = c.sqlite3_finalize(stmt);
        _ = c.sqlite3_bind_int64(stmt, 1, node_id);

        var ids: std.ArrayListUnmanaged(i64) = .{};
        errdefer ids.deinit(allocator);
        while (try step(stmt)) {
            try ids.append(allocator, c.sqlite3_column_int64(stmt, 0));
        }
        return ids.toOwnedSlice(allocator);
    }

    /// Fetch all ContextNode IDs with a non-empty embedding (for KNN scan).
    /// Returns a caller-owned slice of `{id, lod4_name}` suitable for pre-filtering.
    pub fn knnSearch(self: *Self, allocator: std.mem.Allocator, query_vec: []const f32, k: usize) ![]KnnHit {
        var pipeline = HydrationPipeline.init(allocator, self);
        return pipeline.knnSearch(query_vec, k);
    }

    /// BFS traversal from root_id up to max_depth hops.
    /// Returns allocator-owned slice of ContextNode (all LOD strings owned by allocator).
    /// Caller frees with: for (nodes) |*n| n.free(allocator); allocator.free(nodes);
    pub fn traverseFrom(self: *Self, arena: std.mem.Allocator, root_id: i64, max_depth: u8) ![]ContextNode {
        const stmt = try self.prepare(schema.QUERY_NEIGHBOR_BFS);
        defer _ = c.sqlite3_finalize(stmt);
        _ = c.sqlite3_bind_int64(stmt, 1, root_id);

        var nodes: std.ArrayListUnmanaged(ContextNode) = .{};
        errdefer {
            for (nodes.items) |*n| n.free(arena);
            nodes.deinit(arena);
        }

        while (try step(stmt)) {
            const node_id = c.sqlite3_column_int64(stmt, 0);
            const dist: u8 = @intCast(@min(255, c.sqlite3_column_int(stmt, 1)));
            if (dist > max_depth) continue;

            // Fetch the full node using the arena allocator
            const prev_alloc = self.allocator;
            self.allocator = arena;
            const maybe_node = self.fetchNode(node_id) catch {
                self.allocator = prev_alloc;
                continue;
            };
            self.allocator = prev_alloc;

            if (maybe_node) |node| {
                try nodes.append(arena, node);
            }
        }

        return nodes.toOwnedSlice(arena);
    }

    /// Look up a node id by exact lod4 (name) match.
    /// Returns null if not found.
    pub fn findNodeByName(self: *Self, name: []const u8) !?i64 {
        const sql = "SELECT id FROM context_nodes WHERE lod4 = ?1 LIMIT 1";
        const stmt = try self.prepare(sql);
        defer _ = c.sqlite3_finalize(stmt);
        _ = c.sqlite3_bind_text(stmt, 1, name.ptr, @intCast(name.len), SQLITE_STATIC);
        if (!try step(stmt)) return null;
        return c.sqlite3_column_int64(stmt, 0);
    }

    // ------------------------------------------------------------------
    // Duck-typing capability query (P5.4)
    // ------------------------------------------------------------------

    /// isA returns true when child_id has `parent_name` anywhere in its
    /// transitive rdf:type chain (via entity_types + neighbor_of BFS).
    ///
    /// Uses a single recursive CTE:
    ///   1. Seed with direct type_ids from entity_types for child_id.
    ///   2. Expand through neighbor_of (is_a edges) transitively.
    ///   3. Match lod4 of reached type nodes against parent_name.
    ///
    /// Read-only — no mutex needed (SQLite WAL allows concurrent readers).
    pub fn isA(self: *Self, child_id: i64, parent_name: []const u8) !bool {
        const sql =
            \\WITH RECURSIVE type_chain(id) AS (
            \\    SELECT type_id FROM entity_types WHERE entity_id = ?1
            \\    UNION
            \\    SELECT et.type_id FROM entity_types et
            \\    JOIN type_chain tc ON tc.id = et.entity_id
            \\)
            \\SELECT 1 FROM type_chain tc
            \\JOIN context_nodes cn ON cn.id = tc.id
            \\WHERE cn.lod4 = ?2
            \\LIMIT 1
        ;
        const stmt = try self.prepare(sql);
        defer _ = c.sqlite3_finalize(stmt);
        _ = c.sqlite3_bind_int64(stmt, 1, child_id);
        _ = c.sqlite3_bind_text(stmt, 2, parent_name.ptr, @intCast(parent_name.len), SQLITE_STATIC);
        return try step(stmt);
    }

    // ------------------------------------------------------------------
    // YAGO ingestion helpers
    // ------------------------------------------------------------------

    pub fn insertEntityType(self: *Self, entity_id: i64, type_id: i64) !void {
        const sql = "INSERT OR IGNORE INTO entity_types (entity_id, type_id) VALUES (?1, ?2)";
        const stmt = try self.prepare(sql);
        defer _ = c.sqlite3_finalize(stmt);
        _ = c.sqlite3_bind_int64(stmt, 1, entity_id);
        _ = c.sqlite3_bind_int64(stmt, 2, type_id);
        _ = try step(stmt);
    }

    pub fn insertRdfEdge(self: *Self, from_id: i64, to_id: i64, predicate: []const u8) !void {
        const pred_short = if (predicate.len > 200) predicate[0..200] else predicate;
        const sql =
            \\INSERT OR REPLACE INTO neighbor_of (from_id, to_id, distance, edge_type)
            \\VALUES (?1, ?2, 1.0, ?3)
        ;
        const stmt = try self.prepare(sql);
        defer _ = c.sqlite3_finalize(stmt);
        _ = c.sqlite3_bind_int64(stmt, 1, from_id);
        _ = c.sqlite3_bind_int64(stmt, 2, to_id);
        _ = c.sqlite3_bind_text(stmt, 3, pred_short.ptr, @intCast(pred_short.len), SQLITE_STATIC);
        _ = try step(stmt);
    }

    // ------------------------------------------------------------------
    // Provenance
    // ------------------------------------------------------------------

    pub fn insertProvenance(self: *Self, provenance_id: i32, source: []const u8, authority: []const u8) !void {
        const sql =
            \\INSERT OR REPLACE INTO provenance_registry
            \\    (provenance_id, source, imported_at, authority)
            \\VALUES (?1, ?2, ?3, ?4)
        ;
        const stmt = try self.prepare(sql);
        defer _ = c.sqlite3_finalize(stmt);
        _ = c.sqlite3_bind_int64(stmt, 1, provenance_id);
        _ = c.sqlite3_bind_text(stmt, 2, source.ptr, @intCast(source.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_double(stmt, 3, @floatFromInt(std.time.timestamp()));
        _ = c.sqlite3_bind_text(stmt, 4, authority.ptr, @intCast(authority.len), SQLITE_STATIC);
        _ = try step(stmt);
    }

    // ------------------------------------------------------------------
    // Approval workflow
    // ------------------------------------------------------------------

    pub fn insertApproval(self: *Self, node_id: i64, confidence_before: i32) !void {
        const sql =
            \\INSERT OR REPLACE INTO approval_workflow
            \\    (node_id, status, confidence_before, confidence_after)
            \\VALUES (?1, 'pending', ?2, 0)
        ;
        const stmt = try self.prepare(sql);
        defer _ = c.sqlite3_finalize(stmt);
        _ = c.sqlite3_bind_int64(stmt, 1, node_id);
        _ = c.sqlite3_bind_int64(stmt, 2, confidence_before);
        _ = try step(stmt);
    }

    pub fn updateApproval(self: *Self, node_id: i64, status: []const u8, reviewed_by: []const u8, confidence_after: i32) !void {
        const sql =
            \\INSERT OR REPLACE INTO approval_workflow
            \\    (node_id, status, reviewed_by, reviewed_at, confidence_before, confidence_after)
            \\VALUES (?1, ?2, ?3, ?4, 0, ?5)
        ;
        const stmt = try self.prepare(sql);
        defer _ = c.sqlite3_finalize(stmt);
        _ = c.sqlite3_bind_int64(stmt, 1, node_id);
        _ = c.sqlite3_bind_text(stmt, 2, status.ptr, @intCast(status.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 3, reviewed_by.ptr, @intCast(reviewed_by.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_double(stmt, 4, @floatFromInt(std.time.timestamp()));
        _ = c.sqlite3_bind_int64(stmt, 5, confidence_after);
        _ = try step(stmt);
    }

    // ------------------------------------------------------------------
    // Contradictions
    // ------------------------------------------------------------------

    pub fn insertContradiction(self: *Self, node_a: i64, node_b: i64, predicate: []const u8, value_a: []const u8, value_b: []const u8) !void {
        const sql =
            \\INSERT OR REPLACE INTO contradictions
            \\    (node_a, node_b, predicate, value_a, value_b, detected_at)
            \\VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        ;
        const stmt = try self.prepare(sql);
        defer _ = c.sqlite3_finalize(stmt);
        _ = c.sqlite3_bind_int64(stmt, 1, node_a);
        _ = c.sqlite3_bind_int64(stmt, 2, node_b);
        _ = c.sqlite3_bind_text(stmt, 3, predicate.ptr, @intCast(predicate.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 4, value_a.ptr, @intCast(value_a.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 5, value_b.ptr, @intCast(value_b.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_double(stmt, 6, @floatFromInt(std.time.timestamp()));
        _ = try step(stmt);
    }
};

// ---------------------------------------------------------------------------
// §3.3 HydrationPipeline — in-Zig KNN → persist neighbor edges
// ---------------------------------------------------------------------------

/// Implements the 3-step hydration pipeline:
///   1. Accept embedding vector (from edge LLM via HTTP)
///   2. KNN: fetch all embeddings from SQLite, compute cosine in Zig, take top-K
///   3. Persist NEIGHBOR_OF edges into SQLite for graph traversal
pub const HydrationPipeline = struct {
    const Self = @This();

    allocator: std.mem.Allocator,
    library: *Library,

    pub fn init(allocator: std.mem.Allocator, library: *Library) Self {
        return .{ .allocator = allocator, .library = library };
    }

    /// Cosine distance between two equal-length float slices.
    /// Returns value in [0.0, 2.0] (0 = identical directions).
    pub fn cosineDistance(a: []const f32, b: []const f32) f32 {
        if (a.len == 0 or b.len == 0 or a.len != b.len) return 2.0;
        var dot: f32 = 0;
        var norm_a: f32 = 0;
        var norm_b: f32 = 0;
        for (a, b) |ai, bi| {
            dot += ai * bi;
            norm_a += ai * ai;
            norm_b += bi * bi;
        }
        const denom = @sqrt(norm_a) * @sqrt(norm_b);
        if (denom == 0) return 2.0;
        return 1.0 - (dot / denom);
    }

    /// KNN search — fetch all non-empty embeddings from SQLite, compute
    /// cosine distances in Zig, return top-K hits sorted by distance.
    pub fn knnSearch(self: *Self, query_vec: []const f32, k: usize) ![]KnnHit {
        const sql = "SELECT id, lod4, embedding FROM context_nodes WHERE length(embedding) > 0";
        const stmt = try self.library.prepare(sql);
        defer _ = c.sqlite3_finalize(stmt);

        var hits: std.ArrayListUnmanaged(KnnHit) = .{};
        defer hits.deinit(self.allocator);

        while (try Library.step(stmt)) {
            const node_id = c.sqlite3_column_int64(stmt, 0);

            const name_ptr: [*c]const u8 = c.sqlite3_column_text(stmt, 1);
            const name_len: usize = @intCast(c.sqlite3_column_bytes(stmt, 1));
            const name = if (name_ptr != null) name_ptr[0..name_len] else "";

            const blob_ptr = c.sqlite3_column_blob(stmt, 2);
            const blob_len: usize = @intCast(c.sqlite3_column_bytes(stmt, 2));
            var emb_buf: std.ArrayListUnmanaged(f32) = .{};
            defer emb_buf.deinit(self.allocator);
            if (blob_ptr != null and blob_len >= 4) {
                const raw = @as([*]const u8, @ptrCast(blob_ptr))[0..blob_len];
                const n = blob_len / @sizeOf(f32);
                try emb_buf.resize(self.allocator, n);
                for (0..n) |i| {
                    var v: f32 = undefined;
                    @memcpy(std.mem.asBytes(&v), raw[i * 4 .. i * 4 + 4]);
                    emb_buf.items[i] = v;
                }
            }
            const emb: []const f32 = emb_buf.items;

            try hits.append(self.allocator, .{
                .id = node_id,
                .name = name,
                .distance = cosineDistance(query_vec, emb),
            });
        }

        std.mem.sort(KnnHit, hits.items, {}, struct {
            fn lt(_: void, a: KnnHit, b: KnnHit) bool {
                return a.distance < b.distance;
            }
        }.lt);

        const actual_k = @min(k, hits.items.len);
        return try self.allocator.dupe(KnnHit, hits.items[0..actual_k]);
    }

    /// Persist NEIGHBOR_OF edges from KNN hits.
    pub fn persistEdges(self: *Self, knn_hits: []const KnnHit) !void {
        try self.library.persistNeighborEdges(knn_hits);
    }

    /// Full hydration: KNN → persist neighbor edges.  Returns number of nodes found.
    pub fn hydrate(self: *Self, query_vec: []const f32, k: usize) !usize {
        const knn_hits = try self.knnSearch(query_vec, k);
        defer self.allocator.free(knn_hits);
        try self.persistEdges(knn_hits);
        return knn_hits.len;
    }
};

// ---------------------------------------------------------------------------
// §3.4 ContextPacker — LOD selection algorithm
// ---------------------------------------------------------------------------

/// Assigns LOD levels to nodes based on BFS hop distance from semantic center.
/// distance 0 → lod0 (full text)
/// distance 1 → lod1 (summary, falls back to lod0)
/// distance 2 → lod2 (brief, falls back to lod4/name)
/// distance 3+ → lod4 (name only)
pub const ContextPacker = struct {
    const Self = @This();

    allocator: std.mem.Allocator,
    library: *Library,
    max_tokens: usize,
    chars_per_token: usize = 4,

    pub fn init(allocator: std.mem.Allocator, library: *Library, max_tokens: usize) Self {
        return .{ .allocator = allocator, .library = library, .max_tokens = max_tokens };
    }

    /// Select which LOD text to use for a node at a given graph distance.
    pub fn selectLod(self: *Self, node: ContextNode, graph_distance: u32) []const u8 {
        _ = self;
        return switch (graph_distance) {
            0 => node.lod[0],
            1 => if (node.lod[1].len > 0) node.lod[1] else node.lod[0],
            2 => if (node.lod[2].len > 0) node.lod[2] else node.lod[4],
            else => node.lod[4],
        };
    }

    /// BFS from `semantic_center_id` using the neighbor_of table.
    /// Returns owned slice of GraphNode (caller must free with allocator.free).
    pub fn getNodesByDistance(self: *Self, semantic_center_id: i64) ![]GraphNode {
        const stmt = try self.library.prepare(schema.QUERY_NEIGHBOR_BFS);
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_int64(stmt, 1, semantic_center_id);

        var nodes: std.ArrayListUnmanaged(GraphNode) = .{};
        errdefer nodes.deinit(self.allocator);

        while (try Library.step(stmt)) {
            const node_id = c.sqlite3_column_int64(stmt, 0);
            const dist: u32 = @intCast(c.sqlite3_column_int(stmt, 1));
            try nodes.append(self.allocator, .{ .id = node_id, .graph_distance = dist });
        }

        return nodes.toOwnedSlice(self.allocator);
    }

    /// Pack context window text for LLM injection, respecting token budget.
    pub fn pack(self: *Self, semantic_center_id: i64) ![]const u8 {
        var context_buffer: std.ArrayListUnmanaged(u8) = .{};
        defer context_buffer.deinit(self.allocator);
        var token_budget = self.max_tokens;

        const graph_nodes = try self.getNodesByDistance(semantic_center_id);
        defer self.allocator.free(graph_nodes);

        for (graph_nodes) |gnode| {
            if (token_budget == 0) break;

            const maybe_node = try self.library.fetchNode(gnode.id);
            if (maybe_node == null) continue;
            var node = maybe_node.?;
            defer node.free(self.library.allocator);

            const selected_text = self.selectLod(node, gnode.graph_distance);
            const estimated_tokens = selected_text.len / self.chars_per_token;

            if (estimated_tokens <= token_budget) {
                try context_buffer.appendSlice(self.allocator, selected_text);
                try context_buffer.appendSlice(self.allocator, "\n---\n");
                token_budget -= estimated_tokens;
            } else {
                const name_tokens = node.lod[4].len / self.chars_per_token;
                if (name_tokens <= token_budget) {
                    try context_buffer.appendSlice(self.allocator, node.lod[4]);
                    try context_buffer.appendSlice(self.allocator, "\n");
                    token_budget -= name_tokens;
                }
            }
        }

        return context_buffer.toOwnedSlice(self.allocator);
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

/// Open an in-memory Library and initialise its schema.
fn testOpenLib(allocator: std.mem.Allocator) !*Library {
    const lib = try Library.init(allocator, .mem, "");
    try lib.initSchema();
    return lib;
}

test "NodeId: is i64" {
    const nid: NodeId = 42;
    try testing.expectEqual(@as(i64, 42), nid);
    const zero: NodeId = 0;
    try testing.expectEqual(@as(i64, 0), zero);
    const neg: NodeId = -1;
    try testing.expectEqual(@as(i64, -1), neg);
}

test "ContextNode init" {
    var node = try ContextNode.init(0xDEADBEEF, "test_node", "Full content.", testing.allocator);
    defer node.free(testing.allocator);
    try testing.expectEqual(@as(i64, 0xDEADBEEF), node.id);
    try testing.expectEqualStrings("test_node", node.lod[4]);
    try testing.expectEqualStrings("Full content.", node.lod[0]);
    try testing.expect(node.valid_to == null);
    try testing.expectEqual(@as(usize, 0), node.embedding.len);
}

test "ContextNode LOD fields" {
    var node = try ContextNode.init(1, "alpha", "Full text here.", testing.allocator);
    defer node.free(testing.allocator);
    node.lod[1] = "Summary.";
    node.lod[2] = "Brief.";
    node.lod[3] = "Tiny.";
    try testing.expectEqualStrings("Full text here.", node.lod[0]);
    try testing.expectEqualStrings("Summary.", node.lod[1]);
    try testing.expectEqualStrings("Brief.", node.lod[2]);
    try testing.expectEqualStrings("Tiny.", node.lod[3]);
}

test "ContextNode getLod/setLod" {
    var node = try ContextNode.init(1, "test", "Full.", testing.allocator);
    defer node.free(testing.allocator);
    try testing.expectEqualStrings("Full.", node.getLod(0));
    try testing.expectEqualStrings("test", node.getLod(4));
    try testing.expectEqualStrings("", node.getLod(5));
    try testing.expectEqualStrings("", node.getLod(7));
}

test "EdgeType relation and label" {
    try testing.expectEqualStrings("depends_on", EdgeType.depends_on.relation());
    try testing.expectEqualStrings("neighbor_of", EdgeType.neighbor_of.relation());
    try testing.expectEqualStrings("neighbor_of", EdgeType.semantic_similarity.relation());
    try testing.expectEqualStrings("semantic_similarity", EdgeType.semantic_similarity.label());
}

test "HydrationPipeline: cosine distance" {
    const a = [_]f32{ 1, 0, 0 };
    const b = [_]f32{ 0, 1, 0 };
    const c2 = [_]f32{ 1, 0, 0 };
    const dist_ab = HydrationPipeline.cosineDistance(&a, &b);
    try testing.expect(@abs(dist_ab - 1.0) < 0.001);
    const dist_ac = HydrationPipeline.cosineDistance(&a, &c2);
    try testing.expect(@abs(dist_ac - 0.0) < 0.001);
}

test "HydrationPipeline: empty vectors return max distance" {
    const a = [_]f32{};
    const b = [_]f32{};
    try testing.expectEqual(@as(f32, 2.0), HydrationPipeline.cosineDistance(&a, &b));
}

test "HydrationPipeline: mismatched lengths return max distance" {
    const a = [_]f32{ 1, 2 };
    const b = [_]f32{1};
    try testing.expectEqual(@as(f32, 2.0), HydrationPipeline.cosineDistance(&a, &b));
}

test "HydrationPipeline: cosine distance — anti-parallel vectors" {
    const a = [_]f32{ 1, 0 };
    const b = [_]f32{ -1, 0 };
    const dist = HydrationPipeline.cosineDistance(&a, &b);
    try testing.expect(@abs(dist - 2.0) < 0.001);
}

test "HydrationPipeline: cosine distance — equal unit vectors" {
    const a = [_]f32{ 0.6, 0.8 };
    const b = [_]f32{ 0.6, 0.8 };
    try testing.expect(@abs(HydrationPipeline.cosineDistance(&a, &b) - 0.0) < 0.001);
}

test "ContextPacker: LOD selection" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const lib = try testOpenLib(allocator);
    defer lib.deinit();

    var packer = ContextPacker.init(allocator, lib, 1000);

    var node = try ContextNode.init(1, "test", "Full content here for testing purposes.", testing.allocator);
    defer node.free(testing.allocator);
    node.lod[1] = "Summary text.";
    node.lod[2] = "Brief.";
    node.lod[3] = "Tiny";

    try testing.expectEqualStrings("Full content here for testing purposes.", packer.selectLod(node, 0));
    try testing.expectEqualStrings("Summary text.", packer.selectLod(node, 1));
    try testing.expectEqualStrings("Brief.", packer.selectLod(node, 2));
    try testing.expectEqualStrings("test", packer.selectLod(node, 3));
    try testing.expectEqualStrings("test", packer.selectLod(node, 100));
}

test "ContextPacker: empty LOD fallbacks" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const lib = try testOpenLib(allocator);
    defer lib.deinit();

    var packer = ContextPacker.init(allocator, lib, 1000);
    var node = try ContextNode.init(1, "minimal", "Full text only.", testing.allocator);
    defer node.free(testing.allocator);

    try testing.expectEqualStrings("Full text only.", packer.selectLod(node, 1));
    try testing.expectEqualStrings("minimal", packer.selectLod(node, 2));
}

test "Library: open in-memory, init schema" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const lib = try testOpenLib(allocator);
    defer lib.deinit();
    try testing.expect(lib.initialized);
}

test "Library: insert and fetch ContextNode" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const lib = try testOpenLib(allocator);
    defer lib.deinit();

    var node = try ContextNode.init(0xABCDEF01, "hello", "Hello world.", testing.allocator);
    defer node.free(testing.allocator);
    try lib.insertNode(node);

    var fetched = (try lib.fetchNode(@as(i64, 0xABCDEF01))).?;
    defer fetched.free(lib.allocator);
    try testing.expectEqualStrings("hello", fetched.lod[4]);
    try testing.expectEqualStrings("Hello world.", fetched.lod[0]);
}

test "Library: insert target and depends_on edge" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const lib = try testOpenLib(allocator);
    defer lib.deinit();

    var interner = @import("common").interner.StringInterner.init(allocator);
    defer interner.deinit();
    var dep_empty = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(allocator, 4);
    defer dep_empty.deinit(allocator);
    var prov_one = try interner.internAndGetBitSet(allocator, &[_][]const u8{"run"});
    defer prov_one.deinit(allocator);
    var dep_one = try interner.internAndGetBitSet(allocator, &[_][]const u8{"run"});
    defer dep_one.deinit(allocator);
    var prov_two = try interner.internAndGetBitSet(allocator, &[_][]const u8{"confuse"});
    defer prov_two.deinit(allocator);

    try lib.insertTarget(0x01, "cat", &dep_empty, &prov_one, interner.count(), false);
    try lib.insertTarget(0x02, "confuse_a_cat", &dep_one, &prov_two, interner.count(), true);
    try lib.insertDependsOn(0x02, 0x01);
}

test "Library: neighbor_of edge insertion" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const lib = try testOpenLib(allocator);
    defer lib.deinit();

    var n1 = try ContextNode.init(0x10, "center", "Semantic center.", lib.allocator);
    defer n1.free(lib.allocator);
    try lib.insertNode(n1);

    var n2 = try ContextNode.init(0x11, "neighbor", "Nearby node.", lib.allocator);
    defer n2.free(lib.allocator);
    try lib.insertNode(n2);

    try lib.insertNeighborOf(0x10, 0x11, 0.15, .neighbor_of);
}

test "Library: fetchNode returns null for unknown id" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const lib = try testOpenLib(allocator);
    defer lib.deinit();

    const result = try lib.fetchNode(0x0DEA_D000_0000_0000);
    try testing.expect(result == null);
}

test "Library: upsert overwrites existing node" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const lib = try testOpenLib(allocator);
    defer lib.deinit();

    var node = try ContextNode.init(0x42, "original", "First content.", lib.allocator);
    defer node.free(lib.allocator);
    try lib.insertNode(node);

    var updated = try ContextNode.init(0x42, "updated", "Second content.", lib.allocator);
    defer updated.free(lib.allocator);
    updated.lod[1] = "Short summary.";
    try lib.insertNode(updated);

    var fetched = (try lib.fetchNode(0x42)).?;
    defer fetched.free(lib.allocator);
    try testing.expectEqualStrings("updated", fetched.lod[4]);
    try testing.expectEqualStrings("Second content.", fetched.lod[0]);
}

test "Library: initSchema is idempotent" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const lib = try testOpenLib(allocator);
    defer lib.deinit();
    try lib.initSchema();
    try testing.expect(lib.initialized);
}

test "WasmTool: editable mixin is zero size" {
    try testing.expectEqual(@as(usize, 0), @sizeOf(reflection.Editable(WasmTool)));
}

test "WasmTool: reflective get on scalar fields" {
    var tool = WasmTool{
        .id = 0xABCD,
        .test_passed = true,
        .created_at = 1.5,
        .schema_hash = "abc123",
    };
    const hp = try tool.editable.get(testing.allocator, "test_passed", .coder);
    defer testing.allocator.free(hp);
    try testing.expectEqualStrings("true", hp);

    const hsh = try tool.editable.get(testing.allocator, "schema_hash", .coder);
    defer testing.allocator.free(hsh);
    try testing.expectEqualStrings("abc123", hsh);
}

test "Library: insert and fetch WasmTool" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const lib = try testOpenLib(allocator);
    defer lib.deinit();

    const tool = WasmTool{
        .id = 0xBEEF_0001,
        .target_id = 0x01,
        .wasm_b64 = "AGFzbQ==",
        .schema_hash = "deadbeef",
        .test_passed = true,
        .created_at = 1700000000.0,
    };
    try lib.insertWasmTool(tool);

    const fetched = try lib.fetchWasmTool(@as(i64, 0xBEEF_0001));
    try testing.expect(fetched != null);
    defer lib.freeWasmTool(fetched.?);

    try testing.expectEqual(@as(i64, 0xBEEF_0001), fetched.?.id);
    try testing.expectEqual(@as(i64, 0x01), fetched.?.target_id);
    try testing.expectEqualStrings("AGFzbQ==", fetched.?.wasm_b64);
    try testing.expectEqualStrings("deadbeef", fetched.?.schema_hash);
    try testing.expect(fetched.?.test_passed);
}

test "Library: fetchWasmTool returns null for unknown id" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const lib = try testOpenLib(allocator);
    defer lib.deinit();

    const result = try lib.fetchWasmTool(@as(i64, 0x0DEA_DDEA_DDEA));
    try testing.expect(result == null);
}

test "Library: insertWasmTool upsert overwrites existing record" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const lib = try testOpenLib(allocator);
    defer lib.deinit();

    try lib.insertWasmTool(.{ .id = 0x42, .schema_hash = "v1", .test_passed = false });
    try lib.insertWasmTool(.{ .id = 0x42, .schema_hash = "v2", .test_passed = true });

    const fetched = try lib.fetchWasmTool(@as(i64, 0x42));
    try testing.expect(fetched != null);
    defer lib.freeWasmTool(fetched.?);
    try testing.expectEqualStrings("v2", fetched.?.schema_hash);
    try testing.expect(fetched.?.test_passed);
}

test "Library: persistNeighborEdges with empty slice is a no-op" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const lib = try testOpenLib(allocator);
    defer lib.deinit();
    try lib.persistNeighborEdges(&[_]KnnHit{});
}

test "Library: persistNeighborEdges single-element slice is also a no-op" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const lib = try testOpenLib(allocator);
    defer lib.deinit();

    var n = try ContextNode.init(0xAA, "center", "Center node.", lib.allocator);
    defer n.free(lib.allocator);
    try lib.insertNode(n);
    const hits = [_]KnnHit{.{ .id = 0xAA, .name = "center", .distance = 0.0 }};
    try lib.persistNeighborEdges(&hits);
}

test "Library: all EdgeType variants can be inserted" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const lib = try testOpenLib(allocator);
    defer lib.deinit();

    var n1 = try ContextNode.init(0x20, "n1", "Node 1.", lib.allocator);
    defer n1.free(lib.allocator);
    try lib.insertNode(n1);

    var n2 = try ContextNode.init(0x21, "n2", "Node 2.", lib.allocator);
    defer n2.free(lib.allocator);
    try lib.insertNode(n2);

    const edge_types = [_]EdgeType{
        .neighbor_of,
        .semantic_similarity,
        .temporal_sequence,
    };
    for (edge_types) |et| {
        try lib.insertNeighborOf(0x20, 0x21, 0.5, et);
    }
}

test "ContextPacker: selectLod distance 3 always returns name" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const lib = try testOpenLib(allocator);
    defer lib.deinit();

    var packer = ContextPacker.init(allocator, lib, 500);
    var node = try ContextNode.init(99, "n", "Full.", testing.allocator);
    defer node.free(testing.allocator);
    node.lod[3] = "Tiny";

    try testing.expectEqualStrings("n", packer.selectLod(node, 3));
    try testing.expectEqualStrings("n", packer.selectLod(node, 50));
}

test "Library: KNN search returns top-K hits sorted by distance" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const lib = try testOpenLib(allocator);
    defer lib.deinit();

    // Insert 3 nodes with float32 embeddings stored as BLOB
    const emb_a = [_]f32{ 1, 0, 0 };
    const emb_b = [_]f32{ 0.9, 0.1, 0 };
    const emb_c = [_]f32{ 0, 1, 0 };

    inline for (.{
        .{ .id = 0xA1, .name = "a", .text = "Node A", .emb = &emb_a },
        .{ .id = 0xA2, .name = "b", .text = "Node B", .emb = &emb_b },
        .{ .id = 0xA3, .name = "c", .text = "Node C", .emb = &emb_c },
    }) |entry| {
        var node = try ContextNode.init(entry.id, entry.name, entry.text, lib.allocator);
        defer node.free(lib.allocator);
        node.embedding = entry.emb;
        try lib.insertNode(node);
    }

    var pipeline = lib.createHydrationPipeline();
    const query = [_]f32{ 1, 0, 0 };
    const hits = try pipeline.knnSearch(&query, 3);
    defer allocator.free(hits);

    try testing.expectEqual(@as(usize, 3), hits.len);
    // Node A is closest to [1,0,0], C is furthest
    try testing.expectEqual(@as(i64, 0xA1), hits[0].id);
    try testing.expect(hits[0].distance < hits[1].distance);
    try testing.expect(hits[1].distance < hits[2].distance);
}

test "Library: BFS distance via ContextPacker.getNodesByDistance" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const lib = try testOpenLib(allocator);
    defer lib.deinit();

    // Build a small graph: center → hop1 → hop2
    var center = try ContextNode.init(0x100, "center", "Center.", lib.allocator);
    defer center.free(lib.allocator);
    try lib.insertNode(center);
    var hop1 = try ContextNode.init(0x101, "hop1", "Hop1.", lib.allocator);
    defer hop1.free(lib.allocator);
    try lib.insertNode(hop1);
    var hop2 = try ContextNode.init(0x102, "hop2", "Hop2.", lib.allocator);
    defer hop2.free(lib.allocator);
    try lib.insertNode(hop2);

    try lib.insertNeighborOf(0x100, 0x101, 0.1, .neighbor_of);
    try lib.insertNeighborOf(0x101, 0x102, 0.2, .neighbor_of);

    var packer = lib.createContextPacker(10_000);
    const nodes = try packer.getNodesByDistance(0x100);
    defer allocator.free(nodes);

    // center (dist 0), hop1 (dist 1), hop2 (dist 2)
    try testing.expectEqual(@as(usize, 3), nodes.len);
    try testing.expectEqual(@as(u32, 0), nodes[0].graph_distance);
    try testing.expectEqual(@as(u32, 1), nodes[1].graph_distance);
    try testing.expectEqual(@as(u32, 2), nodes[2].graph_distance);
}

test "Library: concurrent insert+read, 4 threads" {
    // spawn 4 threads: 2 insert (with mutex) + 2 read (lock-free)
    // verify no panic/data races
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const lib = try testOpenLib(allocator);
    defer lib.deinit();

    const Ctx = struct {
        lib: *Library,
        alloc: std.mem.Allocator,
    };

    const insertFn = struct {
        fn run(ctx: *Ctx) void {
            var node = ContextNode.init(0x999, "concurrent_node", "Content.", ctx.alloc) catch return;
            defer node.free(ctx.alloc);
            ctx.lib.insertNode(node) catch {};
        }
    }.run;

    const readFn = struct {
        fn run(ctx: *Ctx) void {
            _ = ctx.lib.findNodeByName("concurrent_node") catch {};
        }
    }.run;

    var ctx = Ctx{ .lib = lib, .alloc = allocator };

    const t1 = try std.Thread.spawn(.{}, insertFn, .{&ctx});
    const t2 = try std.Thread.spawn(.{}, insertFn, .{&ctx});
    const t3 = try std.Thread.spawn(.{}, readFn, .{&ctx});
    const t4 = try std.Thread.spawn(.{}, readFn, .{&ctx});

    t1.join();
    t2.join();
    t3.join();
    t4.join();
    // If we reach here without panic, the test passes.
    try testing.expect(true);
}

