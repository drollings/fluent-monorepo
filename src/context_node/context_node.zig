/// context_node.zig — Context Node type for managing levels of detail, embedding caches, etc.
///
const std = @import("std");
const schema = @import("schema.zig");
const reflection = @import("common/reflection.zig");

// ---------------------------------------------------------------------------
// §3.5 NodeId — i64 matching DB's native Int type
// ---------------------------------------------------------------------------

/// NodeId is u64
pub const NodeId = u64;

// ---------------------------------------------------------------------------
// DB row extraction helpers
// ---------------------------------------------------------------------------
// These helpers convert a std.json.Value (a single cell returned by DB)
// to a native Zig type.  They are used by fetchNode / fetchWasmTool, and
// eliminate repeated switch-on-tag boilerplate at every call site.
// All take an optional value so callers can pass `DB.colByHeader(…)`
// directly without a separate null check.

/// Dupe a string cell into the given allocator.  Returns "" for null/non-string.
fn colStr(allocator: std.mem.Allocator, val: ?std.json.Value) ![]const u8 {
    if (val) |v| if (v == .string) return allocator.dupe(u8, v.string);
    return allocator.dupe(u8, "");
}

/// Extract an f64.  Returns `default` for null / non-numeric cells.
fn colFloat(val: ?std.json.Value, default: f64) f64 {
    const v = val orelse return default;
    return switch (v) {
        .float => v.float,
        .integer => @floatFromInt(v.integer),
        else => default,
    };
}

/// Extract an optional f64.  Returns null for DB null or absent column.
fn colOptFloat(val: ?std.json.Value) ?f64 {
    const v = val orelse return null;
    return switch (v) {
        .float => v.float,
        .integer => @floatFromInt(v.integer),
        .null => null,
        else => null,
    };
}

/// Extract a bool.  Accepts DB Bool or integer (0/1).
fn colBool(val: ?std.json.Value, default: bool) bool {
    const v = val orelse return default;
    return switch (v) {
        .bool => v.bool,
        .integer => v.integer != 0,
        else => default,
    };
}

/// Extract an i64.  Returns `default` for non-integer cells.
fn colInt(val: ?std.json.Value, default: i64) i64 {
    const v = val orelse return default;
    return switch (v) {
        .integer => v.integer,
        .float => @intFromFloat(v.float),
        else => default,
    };
}

// ---------------------------------------------------------------------------
// §3.1 ContextNode — universal semantic entity
// ---------------------------------------------------------------------------

/// ContextNode - the universal semantic data structure.
/// Stores LOD text pyramid and float embedding vector.
/// id: i64 — DB native Int
/// lod[0]: max detail (full description)
/// lod[1]: summary (condensed but comprehensive)
/// lod[2]: brief (concise key points)
/// lod[3]: tiny (single sentence or key phrase)
/// lod[4]: name (entity name or identifier)
/// lod[5]: minimal (abbreviation or alias)
pub const ContextNode = struct {
    id: i64,
    lod: [schema.LOD_COUNT][]const u8,
    /// Bitmask: bit i is set when lod[i] is allocator-owned and must be freed.
    /// Prevents accidentally freeing string literals assigned directly to lod slots.
    lod_owned: u8 = 0,
    embedding: []const f32,
    valid_from: f64,
    valid_to: ?f64,
    confidence: i32,
    provenance_id: i32,

    /// Initialise a ContextNode with allocator-owned copies of `full_text` (lod0)
    /// and `name` (lod4).  All other LOD slots are set to empty string literals
    /// (not allocated).  Call `free` with the same allocator when done.
    pub fn init(id: i64, name: []const u8, full_text: []const u8, allocator: std.mem.Allocator) !ContextNode {
        var node = ContextNode{
            .id = id,
            .lod = [_][]const u8{ "", "", "", "", "", "" },
            .embedding = &[_]f32{},
            .valid_from = @floatFromInt(std.time.timestamp()),
            .valid_to = null,
            .confidence = 0,
            .provenance_id = 0,
        };
        node.lod[0] = try allocator.dupe(u8, full_text);
        errdefer allocator.free(node.lod[0]);
        node.lod[4] = try allocator.dupe(u8, name);
        node.lod_owned = (1 << 0) | (1 << 4); // lod0 and lod4 are owned
        return node;
    }

    pub fn getLod(self: *const ContextNode, level: u3) []const u8 {
        if (level >= schema.LOD_COUNT) return "";
        return self.lod[level];
    }

    pub fn setLod(self: *ContextNode, level: u3, value: []const u8) void {
        if (level < schema.LOD_COUNT) {
            self.lod[level] = value;
        }
    }

    /// Free allocator-owned LOD strings tracked by `lod_owned`.
    /// Clears each freed slot to "" and its ownership bit, so double-free is safe.
    pub fn free(self: *ContextNode, allocator: std.mem.Allocator) void {
        for (&self.lod, 0..) |*slot, i| {
            if (self.lod_owned & (@as(u8, 1) << @intCast(i)) != 0) {
                allocator.free(slot.*);
                slot.* = "";
                self.lod_owned &= ~(@as(u8, 1) << @intCast(i));
            }
        }
    }
};

/// KnnHit - result from in-Zig cosine similarity search.
/// Used to construct NEIGHBOR_OF edges in DB.
pub const KnnHit = struct {
    id: i64,
    name: []const u8,
    distance: f32, // cosine distance (0 = identical, 2 = opposite)
};

/// EdgeType enum — maps to edge relation names in DB.
pub const EdgeType = enum(i16) {
    depends_on = 0,
    provides_capability = 1,
    neighbor_of = 2,
    semantic_similarity = 3,
    temporal_sequence = 4,

    /// Returns the DB stored-relation name for this edge type.
    /// NOTE: `semantic_similarity` and `temporal_sequence` are logical subtypes;
    /// they are both stored in the `neighbor_of` relation and distinguished by
    /// the `edge_type` column value (see `EdgeType.label()`).
    pub fn cozoRelation(self: EdgeType) []const u8 {
        return switch (self) {
            .depends_on => "depends_on",
            .provides_capability => "provides_capability",
            .neighbor_of, .semantic_similarity, .temporal_sequence => "neighbor_of",
        };
    }

    /// Returns the string label stored in the `edge_type` column.
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

/// GraphNode - a node with its shortest-path distance from semantic center.
/// Used by ContextPacker for LOD routing.
pub const GraphNode = struct {
    id: i64,
    graph_distance: u32,
};

/// WasmTool — the Zig mirror of the `wasm_tools` DB relation.
///
/// `id` and `target_id` are i64 values stored in DB as Int
/// columns (see NodeId).  All other fields map 1-to-1 to DB columns.
///
/// The `editable` mixin gives this struct zero-overhead reflection support:
///   - TUI / REPL field editing with role-based access control
///   - Config-file-based hydration (string → field via Constraint)
///   - DynamicEditable round-trips for DB row import/export
///
/// Ownership: `wasm_b64` and `schema_hash` are allocator-owned when returned
/// by `Library.fetchWasmTool()`; call `Library.freeWasmTool()` to release.
pub const WasmTool = struct {
    id: i64 = 0,
    target_id: i64 = 0,
    wasm_b64: []const u8 = "",
    schema_hash: []const u8 = "",
    test_passed: bool = false,
    created_at: f64 = 0.0,
    /// Zero-size reflection mixin.  Enables field-level set/get, permissions,
    /// and string serialization via the Accessor/Constraint/Editable pattern.
    editable: reflection.Editable(@This()) = .{},
};

// ---------------------------------------------------------------------------
// §3.2 Library — DB database handle
// ---------------------------------------------------------------------------

/// Library - the unified database interface backed by DB.
/// Manages the DB instance, schema initialization, and provides
/// the hydration pipeline and context packing algorithms.
///
/// Replaces both PgPool (cold storage) and the LadybugDB session graph.
/// DB handles both persistent storage and graph traversal via Datalog.
pub const Library = struct {
    const Self = @This();

    allocator: std.mem.Allocator,
    db: DB,
    initialized: bool = false,

    // ------------------------------------------------------------------
    // Lifecycle
    // ------------------------------------------------------------------

    /// Open the DB database at the given path.
    /// engine: .sqlite (persistent) or .mem (testing/ephemeral)
    /// path: file path for sqlite, "" for mem
    pub fn init(
        allocator: std.mem.Allocator,
        engine: StorageEngine,
        path: [:0]const u8,
    ) !*Self {
        const db = try DB.open(allocator, engine, path, "{}");
        const self = try allocator.create(Self);
        self.* = .{
            .allocator = allocator,
            .db = db,
        };
        return self;
    }

    pub fn deinit(self: *Self) void {
        _ = self.db.close();
        self.allocator.destroy(self);
    }

    // ------------------------------------------------------------------
    // Schema initialization
    // ------------------------------------------------------------------

    /// Create all relations if they do not already exist.
    /// DB's :create is idempotent if the relation already exists —
    /// it returns an error if the schema conflicts, so we wrap each statement.
    pub fn initSchema(self: *Self) !void {
        for (schema.SCHEMA_DDL) |ddl| {
            const ddl_z = try self.allocator.dupeZ(u8, ddl);
            defer self.allocator.free(ddl_z);
            self.db.exec(ddl_z) catch |err| {
                // If the relation already exists, DB returns an error.
                // We treat that as a no-op (idempotent init).
                if (err == @import("cozo.zig").CozoError.QueryFailed) {
                    std.log.debug("Schema statement skipped (already exists?): {s}", .{ddl[0..@min(60, ddl.len)]});
                } else {
                    return err;
                }
            };
        }
        self.initialized = true;
        std.log.info("Library: DB schema initialized", .{});
    }

    // ------------------------------------------------------------------
    // ContextNode persistence
    // ------------------------------------------------------------------

    /// Insert or upsert a ContextNode into DB.
    /// String fields are passed via parameterized query to prevent injection.
    pub fn insertNode(self: *Self, node: ContextNode) !void {
        // Build embedding JSON array: [0.1, 0.2, ...]
        var emb_buf: std.ArrayListUnmanaged(u8) = .{};
        defer emb_buf.deinit(self.allocator);
        try emb_buf.appendSlice(self.allocator, "[");
        for (node.embedding, 0..) |v, i| {
            if (i > 0) try emb_buf.appendSlice(self.allocator, ",");
            try std.fmt.format(emb_buf.writer(self.allocator), "{d}", .{v});
        }
        try emb_buf.appendSlice(self.allocator, "]");

        const valid_to_str = if (node.valid_to) |vt|
            try std.fmt.allocPrint(self.allocator, "{d}", .{vt})
        else
            try self.allocator.dupe(u8, "null");
        defer self.allocator.free(valid_to_str);

        // Build params JSON — string fields use $name placeholders to prevent injection.
        // Use std.json.Stringify.valueAlloc for correct escaping of arbitrary string content.
        const params_str = try std.json.Stringify.valueAlloc(self.allocator, .{
            .lod0 = node.lod[0],
            .lod1 = node.lod[1],
            .lod2 = node.lod[2],
            .lod3 = node.lod[3],
            .lod4 = node.lod[4],
            .lod5 = node.lod[5],
        }, .{});
        defer self.allocator.free(params_str);
        // null-terminate for C API
        const params = try self.allocator.dupeZ(u8, params_str);
        defer self.allocator.free(params);

        const script = try std.fmt.allocPrintSentinel(self.allocator,
            \\?[id, lod0, lod1, lod2, lod3, lod4, lod5, embedding, valid_from, valid_to, confidence, provenance_id] <-
            \\    [[{d}, $lod0, $lod1, $lod2, $lod3, $lod4, $lod5, {s}, {d}, {s}, {d}, {d}]]
            \\:put context_nodes {{ id => lod0, lod1, lod2, lod3, lod4, lod5, embedding, valid_from, valid_to, confidence, provenance_id }}
        , .{ node.id, emb_buf.items, node.valid_from, valid_to_str, node.confidence, node.provenance_id }, 0);
        defer self.allocator.free(script);

        try self.db.execWithParams(script, params);
    }

    /// Fetch a ContextNode by i64 id.
    /// Strings in the returned node are allocator-owned; free with node.free(allocator).
    pub fn fetchNode(self: *Self, id: i64) !?ContextNode {
        const params = try std.fmt.allocPrintSentinel(self.allocator,
            \\{{"id": {d}}}
        , .{id}, 0);
        defer self.allocator.free(params);

        const script =
            \\?[lod0, lod1, lod2, lod3, lod4, lod5, valid_from, valid_to, confidence, provenance_id] :=
            \\    *context_nodes{ id: $id, lod0, lod1, lod2, lod3, lod4, lod5, valid_from, valid_to, confidence, provenance_id }
        ;

        var result = try self.db.queryReadWithParams(script, params);
        defer result.deinit();

        const rows = DB.getRows(&result);
        if (rows.len == 0) return null;

        // Use header-based column lookup so positional index changes in the
        // SELECT list can never silently map the wrong value to the wrong field.
        const headers = DB.getHeaders(&result);
        const row = rows[0];

        var node = ContextNode{
            .id = id,
            .lod = [_][]const u8{ "", "", "", "", "", "" },
            .embedding = &[_]f32{},
            .valid_from = @floatCast(colFloat(DB.colByHeader(headers, row, "valid_from"), 0.0)),
            .valid_to = blk: {
                const v = colOptFloat(DB.colByHeader(headers, row, "valid_to"));
                break :blk if (v) |f| @as(f64, @floatCast(f)) else null;
            },
            .confidence = @intCast(colInt(DB.colByHeader(headers, row, "confidence"), 0)),
            .provenance_id = @intCast(colInt(DB.colByHeader(headers, row, "provenance_id"), 0)),
        };

        const lod_names = [_][]const u8{ "lod0", "lod1", "lod2", "lod3", "lod4", "lod5" };
        inline for (lod_names, 0..) |lod_name, i| {
            node.lod[i] = try colStr(self.allocator, DB.colByHeader(headers, row, lod_name));
            // colStr always dupes (returns "" dupe for empty), so all slots are owned.
            node.lod_owned |= @as(u8, 1) << i;
        }

        return node;
    }

    // ------------------------------------------------------------------
    // Target persistence
    // ------------------------------------------------------------------

    /// Insert a target definition into DB.
    /// `depends` and `provides` are stored as raw bitset word arrays ([Int])
    /// so capability sets larger than 63 bits round-trip correctly.
    /// `name` is passed via parameterized query to prevent injection.
    pub fn insertTarget(
        self: *Self,
        id: i64,
        name: []const u8,
        depends: *const std.bit_set.DynamicBitSetUnmanaged,
        provides: *const std.bit_set.DynamicBitSetUnmanaged,
        total_bits: usize,
        is_essential: bool,
    ) !void {
        const bits_per_word = @bitSizeOf(usize);
        const dep_words = (depends.bit_length + bits_per_word - 1) / bits_per_word;
        const prov_words = (provides.bit_length + bits_per_word - 1) / bits_per_word;

        // Build JSON arrays of i64 words for DB [Int] columns.
        var dep_buf: std.ArrayListUnmanaged(u8) = .{};
        defer dep_buf.deinit(self.allocator);
        var prov_buf: std.ArrayListUnmanaged(u8) = .{};
        defer prov_buf.deinit(self.allocator);

        try dep_buf.append(self.allocator, '[');
        for (0..dep_words) |i| {
            if (i > 0) try dep_buf.append(self.allocator, ',');
            const w: i64 = @bitCast(@as(u64, depends.masks[i]));
            try std.fmt.format(dep_buf.writer(self.allocator), "{d}", .{w});
        }
        try dep_buf.append(self.allocator, ']');

        try prov_buf.append(self.allocator, '[');
        for (0..prov_words) |i| {
            if (i > 0) try prov_buf.append(self.allocator, ',');
            const w: i64 = @bitCast(@as(u64, provides.masks[i]));
            try std.fmt.format(prov_buf.writer(self.allocator), "{d}", .{w});
        }
        try prov_buf.append(self.allocator, ']');

        const params_str = try std.json.Stringify.valueAlloc(self.allocator, .{
            .id = id,
            .name = name,
            .total_bits = @as(i64, @intCast(total_bits)),
            .essential = is_essential,
        }, .{});
        defer self.allocator.free(params_str);
        const params = try self.allocator.dupeZ(u8, params_str);
        defer self.allocator.free(params);

        const script = try std.fmt.allocPrintSentinel(self.allocator,
            \\?[id, name, depends_words, provides_words, total_bits, is_essential] <-
            \\    [[$id, $name, {s}, {s}, $total_bits, $essential]]
            \\:put targets {{ id => name, depends_words, provides_words, total_bits, is_essential }}
        , .{ dep_buf.items, prov_buf.items }, 0);
        defer self.allocator.free(script);

        try self.db.execWithParams(script, params);
    }

    // ------------------------------------------------------------------
    // WasmTool persistence
    // ------------------------------------------------------------------

    /// Insert or upsert a WasmTool record into the wasm_tools relation.
    pub fn insertWasmTool(self: *Self, tool: WasmTool) !void {
        const params_str = try std.json.Stringify.valueAlloc(self.allocator, .{
            .id = tool.id,
            .target_id = tool.target_id,
            .wasm_b64 = tool.wasm_b64,
            .schema_hash = tool.schema_hash,
            .test_passed = tool.test_passed,
            .created_at = tool.created_at,
        }, .{});
        defer self.allocator.free(params_str);
        const params = try self.allocator.dupeZ(u8, params_str);
        defer self.allocator.free(params);

        const script =
            \\?[id, target_id, wasm_b64, schema_hash, test_passed, created_at] <-
            \\    [[$id, $target_id, $wasm_b64, $schema_hash, $test_passed, $created_at]]
            \\:put wasm_tools { id => target_id, wasm_b64, schema_hash, test_passed, created_at }
        ;
        try self.db.execWithParams(script, params);
    }

    /// Fetch a WasmTool record by i64 id.
    /// Strings in the returned tool are allocator-owned; free with freeWasmTool().
    pub fn fetchWasmTool(self: *Self, id: i64) !?WasmTool {
        const params = try std.fmt.allocPrintSentinel(self.allocator,
            \\{{"id": {d}}}
        , .{id}, 0);
        defer self.allocator.free(params);

        const script =
            \\?[target_id, wasm_b64, schema_hash, test_passed, created_at] :=
            \\    *wasm_tools{ id: $id, target_id, wasm_b64, schema_hash, test_passed, created_at }
        ;

        var result = try self.db.queryReadWithParams(script, params);
        defer result.deinit();

        const rows = DB.getRows(&result);
        if (rows.len == 0) return null;

        const headers = DB.getHeaders(&result);
        const row = rows[0];

        return WasmTool{
            .id = id,
            .target_id = colInt(DB.colByHeader(headers, row, "target_id"), 0),
            .wasm_b64 = try colStr(self.allocator, DB.colByHeader(headers, row, "wasm_b64")),
            .schema_hash = try colStr(self.allocator, DB.colByHeader(headers, row, "schema_hash")),
            .test_passed = colBool(DB.colByHeader(headers, row, "test_passed"), false),
            .created_at = colFloat(DB.colByHeader(headers, row, "created_at"), 0.0),
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

    /// Insert a DEPENDS_ON edge between two target IDs.
    pub fn insertDependsOn(self: *Self, from_id: i64, to_id: i64) !void {
        const params = try std.fmt.allocPrintSentinel(self.allocator,
            \\{{"from": {d}, "to": {d}}}
        , .{ from_id, to_id }, 0);
        defer self.allocator.free(params);
        const script =
            \\?[from, to] <- [[$from, $to]]
            \\:put depends_on { from, to }
        ;
        try self.db.execWithParams(script, params);
    }

    /// Insert a NEIGHBOR_OF edge with cosine distance and edge type.
    /// `edge_type` label is a known-safe enum string, passed via params for consistency.
    pub fn insertNeighborOf(
        self: *Self,
        from_id: i64,
        to_id: i64,
        distance: f32,
        edge_type: EdgeType,
    ) !void {
        const params = try std.fmt.allocPrintSentinel(self.allocator,
            \\{{"from": {d}, "to": {d}, "distance": {d:.6}, "edge_type": "{s}"}}
        , .{ from_id, to_id, distance, edge_type.label() }, 0);
        defer self.allocator.free(params);
        const script =
            \\?[from, to, distance, edge_type] <-
            \\    [[$from, $to, $distance, $edge_type]]
            \\:put neighbor_of { from, to => distance, edge_type }
        ;
        try self.db.execWithParams(script, params);
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

    /// Persist NEIGHBOR_OF edges from KNN hits into DB.
    /// Call after knnSearch to make the topology durable.
    pub fn persistNeighborEdges(self: *Self, knn_hits: []const KnnHit) !void {
        if (knn_hits.len == 0) return;
        const center = knn_hits[0];
        for (knn_hits[1..]) |hit| {
            try self.insertNeighborOf(center.id, hit.id, hit.distance, .neighbor_of);
        }
    }

    // ------------------------------------------------------------------
    // YAGO ingestion helpers
    // ------------------------------------------------------------------

    /// Insert an entity_types row (entity IRI hashed to i64 → type IRI hashed to i64).
    pub fn insertEntityType(self: *Self, entity_id: i64, type_id: i64) !void {
        const params = try std.fmt.allocPrintSentinel(self.allocator,
            \\{{"entity": {d}, "type": {d}}}
        , .{ entity_id, type_id }, 0);
        defer self.allocator.free(params);
        const script =
            \\?[entity, type] <-
            \\    [[$entity, $type]]
            \\:put entity_types { entity, type }
        ;
        try self.db.execWithParams(script, params);
    }

    /// Insert a generic RDF edge (predicate IRI stored as edge label) using
    /// the neighbor_of relation with edge_type "rdf_property".
    pub fn insertRdfEdge(self: *Self, from_id: i64, to_id: i64, predicate: []const u8) !void {
        // Truncate predicate to 200 chars for storage safety
        const pred_short = if (predicate.len > 200) predicate[0..200] else predicate;
        const params = try std.fmt.allocPrintSentinel(self.allocator,
            \\{{"from": {d}, "to": {d}, "distance": 1.0, "edge_type": "{s}"}}
        , .{ from_id, to_id, pred_short }, 0);
        defer self.allocator.free(params);
        const script =
            \\?[from, to, distance, edge_type] <-
            \\    [[$from, $to, $distance, $edge_type]]
            \\:put neighbor_of { from, to => distance, edge_type }
        ;
        try self.db.execWithParams(script, params);
    }

    // ------------------------------------------------------------------
    // Provenance methods
    // ------------------------------------------------------------------

    /// Insert a provenance record.
    pub fn insertProvenance(self: *Self, provenance_id: i32, source: []const u8, authority: []const u8) !void {
        const params_str = try std.json.Stringify.valueAlloc(self.allocator, .{
            .provenance_id = provenance_id,
            .source = source,
            .authority = authority,
            .imported_at = @as(f64, @floatFromInt(std.time.timestamp())),
        }, .{});
        defer self.allocator.free(params_str);
        const params = try self.allocator.dupeZ(u8, params_str);
        defer self.allocator.free(params);
        const script =
            \\?[provenance_id, source, imported_at, authority] <-
            \\    [[$provenance_id, $source, $imported_at, $authority]]
            \\:put provenance_registry { provenance_id => source, imported_at, authority }
        ;
        try self.db.execWithParams(script, params);
    }

    // ------------------------------------------------------------------
    // Approval workflow methods
    // ------------------------------------------------------------------

    /// Insert an approval workflow record for a node.
    pub fn insertApproval(self: *Self, node_id: i64, confidence_before: i32) !void {
        const params = try std.fmt.allocPrintSentinel(self.allocator,
            \\{{"node": {d}, "status": "pending", "confidence_before": {d}, "confidence_after": 0}}
        , .{ node_id, confidence_before }, 0);
        defer self.allocator.free(params);
        const script =
            \\?[node, status, reviewed_by, reviewed_at, confidence_before, confidence_after] <-
            \\    [[$node, $status, null, null, $confidence_before, $confidence_after]]
            \\:put approval_workflow { node => status, reviewed_by, reviewed_at, confidence_before, confidence_after }
        ;
        try self.db.execWithParams(script, params);
    }

    /// Update approval status after review.
    pub fn updateApproval(self: *Self, node_id: i64, status: []const u8, reviewed_by: []const u8, confidence_after: i32) !void {
        const params_str = try std.json.Stringify.valueAlloc(self.allocator, .{
            .node = node_id,
            .status = status,
            .reviewed_by = reviewed_by,
            .reviewed_at = @as(f64, @floatFromInt(std.time.timestamp())),
            .confidence_after = confidence_after,
        }, .{});
        defer self.allocator.free(params_str);
        const params = try self.allocator.dupeZ(u8, params_str);
        defer self.allocator.free(params);
        const script =
            \\?[node, status, reviewed_by, reviewed_at, confidence_before, confidence_after] <-
            \\    [[$node, $status, $reviewed_by, $reviewed_at, 0, $confidence_after]]
            \\:put approval_workflow { node => status, reviewed_by, reviewed_at, confidence_before, confidence_after }
        ;
        try self.db.execWithParams(script, params);
    }

    // ------------------------------------------------------------------
    // Contradiction methods
    // ------------------------------------------------------------------

    /// Insert a contradiction record.
    pub fn insertContradiction(self: *Self, node_a: i64, node_b: i64, predicate: []const u8, value_a: []const u8, value_b: []const u8) !void {
        const params_str = try std.json.Stringify.valueAlloc(self.allocator, .{
            .node_a = node_a,
            .node_b = node_b,
            .predicate = predicate,
            .value_a = value_a,
            .value_b = value_b,
            .detected_at = @as(f64, @floatFromInt(std.time.timestamp())),
        }, .{});
        defer self.allocator.free(params_str);
        const params = try self.allocator.dupeZ(u8, params_str);
        defer self.allocator.free(params);
        const script =
            \\?[node_a, node_b, predicate, value_a, value_b, detected_at] <-
            \\    [[$node_a, $node_b, $predicate, $value_a, $value_b, $detected_at]]
            \\:put contradictions { node_a, node_b => predicate, value_a, value_b, detected_at }
        ;
        try self.db.execWithParams(script, params);
    }
};

// ---------------------------------------------------------------------------
// §3.3 HydrationPipeline — Cozo KNN → session Cypher
// ---------------------------------------------------------------------------

/// Implements the 3-step hydration pipeline:
///   1. Accept embedding vector (from edge LLM via HTTP)
///   2. KNN: fetch all embeddings from DB, compute cosine in Zig, take top-K
///   3. Persist NEIGHBOR_OF edges into DB for graph traversal
///
/// No external vector index required at edge scale.
pub const HydrationPipeline = struct {
    const Self = @This();

    allocator: std.mem.Allocator,
    library: *Library,

    pub fn init(allocator: std.mem.Allocator, library: *Library) Self {
        return .{ .allocator = allocator, .library = library };
    }

    /// Cosine distance between two equal-length float slices.
    /// Returns value in [0.0, 2.0] (0 = identical directions).
    fn cosineDistance(a: []const f32, b: []const f32) f32 {
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

    /// Step 2: KNN search — fetch embeddings from DB, compute distances in Zig.
    /// Returns top-K hits sorted by cosine distance (ascending).
    pub fn knnSearch(
        self: *Self,
        query_vec: []const f32,
        k: usize,
    ) ![]KnnHit {
        // Fetch only nodes that have a non-empty embedding to avoid distance=2.0 sentinel hits.
        var result = try self.library.db.queryRead(
            "?[id, lod4, embedding] := *context_nodes{id, lod4, embedding}, length(embedding) > 0",
        );
        defer result.deinit();

        const rows = DB.getRows(&result);
        var hits = try std.ArrayList(KnnHit).initCapacity(self.allocator, rows.len);
        defer hits.deinit();

        for (rows) |row| {
            if (row != .array or row.array.items.len < 3) continue;
            const cols = row.array.items;

            const node_id: i64 = if (cols[0] == .integer) cols[0].integer else 0;
            const name = if (cols[1] == .string) cols[1].string else "";

            // Parse embedding list
            var emb = std.ArrayList(f32).init(self.allocator);
            defer emb.deinit();
            if (cols[2] == .array) {
                for (cols[2].array.items) |v| {
                    const fv: f32 = switch (v) {
                        .float => @floatCast(v.float),
                        .integer => @floatFromInt(v.integer),
                        else => 0,
                    };
                    try emb.append(fv);
                }
            }

            const dist = cosineDistance(query_vec, emb.items);
            try hits.append(.{
                .id = node_id,
                .name = name,
                .distance = dist,
            });
        }

        // Sort ascending by distance
        std.mem.sort(KnnHit, hits.items, {}, struct {
            fn lessThan(_: void, a: KnnHit, b: KnnHit) bool {
                return a.distance < b.distance;
            }
        }.lessThan);

        // Return top-K
        const actual_k = @min(k, hits.items.len);
        return try self.allocator.dupe(KnnHit, hits.items[0..actual_k]);
    }

    /// Step 3: Persist NEIGHBOR_OF edges from KNN hits into DB.
    /// The center node (knn_hits[0]) gets edges to all other hits.
    pub fn persistEdges(self: *Self, knn_hits: []const KnnHit) !void {
        try self.library.persistNeighborEdges(knn_hits);
    }

    /// Full hydration: KNN → persist neighbor edges into DB.
    /// Returns number of nodes found.
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

/// Assigns LOD levels to nodes based on graph distance from semantic center.
/// distance 0 → lod0_full (complete text)
/// distance 1 → lod1_summary (800 chars)
/// distance 2 → lod2_brief (240 chars)
/// distance 3+ → lod4_name (name only)
pub const ContextPacker = struct {
    const Self = @This();

    allocator: std.mem.Allocator,
    library: *Library,
    max_tokens: usize,
    chars_per_token: usize = 4,

    pub fn init(allocator: std.mem.Allocator, library: *Library, max_tokens: usize) Self {
        return .{
            .allocator = allocator,
            .library = library,
            .max_tokens = max_tokens,
        };
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

    /// Get nodes ordered by BFS distance from semantic center.
    /// Uses DB's Datalog recursive rule (schema.QUERY_NEIGHBOR_BFS).
    /// The returned slice is owned by the caller and must be freed with `allocator.free`.
    pub fn getNodesByDistance(self: *Self, semantic_center_id: i64) ![]GraphNode {
        // Build params JSON: {"center": <i64>}
        const params = try std.fmt.allocPrintSentinel(self.allocator,
            \\{{"center": {d}}}
        , .{semantic_center_id}, 0);
        defer self.allocator.free(params);

        const query_z: [:0]const u8 = schema.QUERY_NEIGHBOR_BFS[0..schema.QUERY_NEIGHBOR_BFS.len :0];
        var result = try self.library.db.queryWithParams(query_z, params);
        defer result.deinit();

        const rows = DB.getRows(&result);
        var nodes = try std.ArrayList(GraphNode).initCapacity(self.allocator, rows.len);
        errdefer nodes.deinit();

        for (rows) |row| {
            if (row != .array or row.array.items.len < 3) continue;
            const cols = row.array.items;

            const node_id: i64 = if (cols[0] == .integer) cols[0].integer else 0;
            const dist: u32 = if (cols[1] == .integer) @intCast(cols[1].integer) else 0;

            try nodes.append(.{
                .id = node_id,
                .graph_distance = dist,
            });
        }

        // Already sorted by dist from the CozoScript `:order dist` clause.
        return nodes.toOwnedSlice();
    }

    /// Pack context window text for LLM injection, respecting token budget.
    pub fn pack(self: *Self, semantic_center_id: i64) ![]const u8 {
        var context_buffer = std.ArrayList(u8).init(self.allocator);
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
                try context_buffer.appendSlice(selected_text);
                try context_buffer.appendSlice("\n---\n");
                token_budget -= estimated_tokens;
            } else {
                // Downgrade to LOD4 if selected LOD exceeds budget
                const name_tokens = node.lod[4].len / self.chars_per_token;
                if (name_tokens <= token_budget) {
                    try context_buffer.appendSlice(node.lod[4]);
                    try context_buffer.appendSlice("\n");
                    token_budget -= name_tokens;
                }
            }
        }

        return context_buffer.toOwnedSlice();
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

/// Open an in-memory Library and initialise the schema.
/// Caller owns the returned pointer and must call lib.deinit().
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
    // Out of bounds (u3 max is 7) returns empty string
    try testing.expectEqualStrings("", node.getLod(7));
}

test "EdgeType cozoRelation and label" {
    try testing.expectEqualStrings("depends_on", EdgeType.depends_on.cozoRelation());
    try testing.expectEqualStrings("neighbor_of", EdgeType.neighbor_of.cozoRelation());
    try testing.expectEqualStrings("neighbor_of", EdgeType.semantic_similarity.cozoRelation());
    try testing.expectEqualStrings("semantic_similarity", EdgeType.semantic_similarity.label());
}

test "HydrationPipeline: cosine distance" {
    // Test internal cosine distance via the public KNN path is too heavy for unit test.
    // Validate the math directly.
    const a = [_]f32{ 1, 0, 0 };
    const b = [_]f32{ 0, 1, 0 };
    const c = [_]f32{ 1, 0, 0 };

    // Orthogonal vectors: cosine distance = 1.0
    const dist_ab = HydrationPipeline.cosineDistance(&a, &b);
    try testing.expect(@abs(dist_ab - 1.0) < 0.001);

    // Identical vectors: cosine distance = 0.0
    const dist_ac = HydrationPipeline.cosineDistance(&a, &c);
    try testing.expect(@abs(dist_ac - 0.0) < 0.001);
}

test "HydrationPipeline: empty vectors return max distance" {
    const a = [_]f32{};
    const b = [_]f32{};
    const dist = HydrationPipeline.cosineDistance(&a, &b);
    try testing.expectEqual(@as(f32, 2.0), dist);
}

test "HydrationPipeline: mismatched lengths return max distance" {
    const a = [_]f32{ 1, 2 };
    const b = [_]f32{1};
    const dist = HydrationPipeline.cosineDistance(&a, &b);
    try testing.expectEqual(@as(f32, 2.0), dist);
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

    // Distance 1: fallback to full text when summary is empty
    try testing.expectEqualStrings("Full text only.", packer.selectLod(node, 1));

    // Distance 2: fallback to name when brief is empty
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

    // Build trivial bitsets via the interner helpers.
    var interner = @import("common/interner.zig").StringInterner.init(allocator);
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

    // Overwrite with updated content at the same id.
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
    // Calling initSchema twice must not error.
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const lib = try testOpenLib(allocator);
    defer lib.deinit();
    try lib.initSchema(); // second call — should silently succeed
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

    // Must not error when passed an empty slice.
    try lib.persistNeighborEdges(&[_]KnnHit{});
}

test "Library: persistNeighborEdges single-element slice is also a no-op" {
    // With only one hit (the center itself), there are no edges to write.
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

test "HydrationPipeline: cosine distance — anti-parallel vectors" {
    const a = [_]f32{ 1, 0 };
    const b = [_]f32{ -1, 0 };
    // cosine distance = 1 - (-1) = 2.0
    const dist = HydrationPipeline.cosineDistance(&a, &b);
    try testing.expect(@abs(dist - 2.0) < 0.001);
}

test "HydrationPipeline: cosine distance — equal unit vectors" {
    const a = [_]f32{ 0.6, 0.8 }; // already a unit vector
    const b = [_]f32{ 0.6, 0.8 };
    const dist = HydrationPipeline.cosineDistance(&a, &b);
    try testing.expect(@abs(dist - 0.0) < 0.001);
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

    // Distances ≥ 3 must always resolve to lod[4] (name).
    try testing.expectEqualStrings("n", packer.selectLod(node, 3));
    try testing.expectEqualStrings("n", packer.selectLod(node, 50));
}
