/// cozo.zig — CozoDB C-binding wrapper for Coral Context
///
/// Thin, zero-cost Zig wrapper around the CozoDB C API (cozo_c.h).
/// CozoDB is the unified backend replacing both pgvector (cold storage)
/// and LadybugDB (session graph). It supports:
///   - Datalog queries (replaces Cypher)
///   - Persistent storage via SQLite or RocksDB backends
///   - Time travel (built-in versioning, replaces timescaledb)
///   - Graph relations (replaces DEPENDS_ON/NEIGHBOR_OF Cypher tables)
///
/// API surface (from cozo_c.h):
///   cozo_open_db(engine, path, options, &db_id) -> ?err_cstr
///   cozo_close_db(db_id) -> bool
///   cozo_run_query(db_id, script, params_json, immutable) -> result_cstr
///   cozo_import_relations(db_id, json) -> result_cstr
///   cozo_export_relations(db_id, json) -> result_cstr
///   cozo_free_str(cstr)
///
/// Memory contract:
///   Every C-string returned by the Cozo API MUST be freed via cozo_free_str().
///   This wrapper enforces that via RAII (CozoResult.deinit()).
const std = @import("std");

// ---------------------------------------------------------------------------
// C extern declarations (mirrors cozo_c.h)
// ---------------------------------------------------------------------------

pub const c = struct {
    pub extern fn cozo_open_db(
        engine: [*:0]const u8,
        path: [*:0]const u8,
        options: [*:0]const u8,
        db_id: *i32,
    ) ?[*:0]u8;

    pub extern fn cozo_close_db(db_id: i32) bool;

    pub extern fn cozo_run_query(
        db_id: i32,
        script_raw: [*:0]const u8,
        params_raw: [*:0]const u8,
        immutable_query: bool,
    ) [*:0]u8;

    pub extern fn cozo_import_relations(
        db_id: i32,
        json_payload: [*:0]const u8,
    ) [*:0]u8;

    pub extern fn cozo_export_relations(
        db_id: i32,
        json_payload: [*:0]const u8,
    ) [*:0]u8;

    pub extern fn cozo_backup(db_id: i32, out_path: [*:0]const u8) [*:0]u8;

    pub extern fn cozo_restore(db_id: i32, in_path: [*:0]const u8) [*:0]u8;

    pub extern fn cozo_free_str(s: [*:0]u8) void;
};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

pub const CozoError = error{
    OpenFailed,
    QueryFailed,
    ImportFailed,
    BackupFailed,
    RestoreFailed,
};

// ---------------------------------------------------------------------------
// StorageEngine
// ---------------------------------------------------------------------------

pub const StorageEngine = enum {
    mem,
    sqlite,
    rocksdb,

    pub fn cStr(self: StorageEngine) [*:0]const u8 {
        return switch (self) {
            .mem => "mem",
            .sqlite => "sqlite",
            .rocksdb => "rocksdb",
        };
    }
};

// ---------------------------------------------------------------------------
// CozoDB — the main database handle
// ---------------------------------------------------------------------------

/// Wraps a single CozoDB instance identified by its integer db_id.
/// Lifecycle: open → query* → close.
/// Thread safety: CozoDB itself is thread-safe; this struct is not.
pub const CozoDB = struct {
    const Self = @This();

    db_id: i32,
    allocator: std.mem.Allocator,

    // ------------------------------------------------------------------
    // Open / Close
    // ------------------------------------------------------------------

    /// Open (or create) a CozoDB database.
    ///
    /// engine: .mem (no path), .sqlite (path = file.db), .rocksdb (path = dir/)
    /// path:   UTF-8 filesystem path (empty string for .mem)
    /// options: JSON string for engine-specific options (pass "{}" for defaults)
    pub fn open(
        allocator: std.mem.Allocator,
        engine: StorageEngine,
        path: [:0]const u8,
        options: [:0]const u8,
    ) !Self {
        var db_id: i32 = 0;
        const err_ptr = c.cozo_open_db(engine.cStr(), path.ptr, options.ptr, &db_id);
        if (err_ptr) |err| {
            const msg = std.mem.span(err);
            std.log.err("cozo_open_db failed: {s}", .{msg});
            c.cozo_free_str(err);
            return CozoError.OpenFailed;
        }
        return .{ .db_id = db_id, .allocator = allocator };
    }

    /// Close the database. Returns true if it was open.
    pub fn close(self: Self) bool {
        return c.cozo_close_db(self.db_id);
    }

    // ------------------------------------------------------------------
    // Query
    // ------------------------------------------------------------------

    /// Run a CozoScript query. Returns parsed JSON as a std.json.Parsed(Value).
    /// Caller must call result.deinit() when done.
    ///
    /// params_json: a JSON object string, e.g. "{}" or "{\"name\": \"foo\"}"
    /// immutable:   true for read-only queries (slightly faster)
    pub fn query(
        self: Self,
        script: [:0]const u8,
        params_json: [:0]const u8,
        immutable: bool,
    ) !std.json.Parsed(std.json.Value) {
        const raw = c.cozo_run_query(self.db_id, script.ptr, params_json.ptr, immutable);
        defer c.cozo_free_str(raw);

        const json_slice = std.mem.span(raw);
        var parsed = try std.json.parseFromSlice(
            std.json.Value,
            self.allocator,
            json_slice,
            .{ .allocate = .alloc_always },
        );

        // Check Cozo's "ok" field
        if (parsed.value == .object) {
            if (parsed.value.object.get("ok")) |ok_val| {
                if (ok_val == .bool and !ok_val.bool) {
                    const msg = if (parsed.value.object.get("message")) |m|
                        if (m == .string) m.string else "unknown error"
                    else
                        "unknown error";
                    std.log.debug("CozoScript failed: {s}\nScript: {s}", .{ msg, script });
                    parsed.deinit();
                    return CozoError.QueryFailed;
                }
            }
        }

        return parsed;
    }

    /// Run a write query with no params. Convenience wrapper.
    pub fn exec(self: Self, script: [:0]const u8) !void {
        var result = try self.query(script, "{}", false);
        result.deinit();
    }

    /// Run a read-only query with no params. Convenience wrapper.
    pub fn queryRead(self: Self, script: [:0]const u8) !std.json.Parsed(std.json.Value) {
        return self.query(script, "{}", true);
    }

    /// Run a query with a pre-built params JSON string.
    /// Convenience wrapper for parameterized queries.
    pub fn queryWithParams(
        self: Self,
        script: [:0]const u8,
        params_json: [:0]const u8,
    ) !std.json.Parsed(std.json.Value) {
        return self.query(script, params_json, false);
    }

    /// Run a write query with a pre-built params JSON string, discarding result.
    pub fn execWithParams(self: Self, script: [:0]const u8, params_json: [:0]const u8) !void {
        var result = try self.query(script, params_json, false);
        result.deinit();
    }

    /// Run a read-only query with a pre-built params JSON string.
    pub fn queryReadWithParams(
        self: Self,
        script: [:0]const u8,
        params_json: [:0]const u8,
    ) !std.json.Parsed(std.json.Value) {
        return self.query(script, params_json, true);
    }

    // ------------------------------------------------------------------
    // Import / Export
    // ------------------------------------------------------------------

    /// Import data from a JSON payload (same format as export).
    pub fn importRelations(self: Self, json_payload: [:0]const u8) !void {
        const raw = c.cozo_import_relations(self.db_id, json_payload.ptr);
        defer c.cozo_free_str(raw);
        const s = std.mem.span(raw);
        var parsed = try std.json.parseFromSlice(std.json.Value, self.allocator, s, .{});
        defer parsed.deinit();
        if (parsed.value == .object) {
            if (parsed.value.object.get("ok")) |ok| {
                if (ok == .bool and !ok.bool) return CozoError.ImportFailed;
            }
        }
    }

    /// Backup the database to a file path.
    pub fn backup(self: Self, out_path: [:0]const u8) !void {
        const raw = c.cozo_backup(self.db_id, out_path.ptr);
        defer c.cozo_free_str(raw);
        const s = std.mem.span(raw);
        var parsed = try std.json.parseFromSlice(std.json.Value, self.allocator, s, .{});
        defer parsed.deinit();
        if (parsed.value == .object) {
            if (parsed.value.object.get("ok")) |ok| {
                if (ok == .bool and !ok.bool) return CozoError.BackupFailed;
            }
        }
    }

    /// Restore the database from a backup file.
    pub fn restore(self: Self, in_path: [:0]const u8) !void {
        const raw = c.cozo_restore(self.db_id, in_path.ptr);
        defer c.cozo_free_str(raw);
        const s = std.mem.span(raw);
        var parsed = try std.json.parseFromSlice(std.json.Value, self.allocator, s, .{});
        defer parsed.deinit();
        if (parsed.value == .object) {
            if (parsed.value.object.get("ok")) |ok| {
                if (ok == .bool and !ok.bool) return CozoError.RestoreFailed;
            }
        }
    }

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    /// Extract rows from a Cozo query result.
    /// Cozo returns: {"ok": true, "rows": [[...], ...], "headers": [...]}
    /// Returns a slice of row arrays. Caller owns the parsed lifetime.
    pub fn getRows(result: *const std.json.Parsed(std.json.Value)) []std.json.Value {
        if (result.value != .object) return &[_]std.json.Value{};
        const rows_val = result.value.object.get("rows") orelse return &[_]std.json.Value{};
        if (rows_val != .array) return &[_]std.json.Value{};
        return rows_val.array.items;
    }

    /// Extract the column headers array from a Cozo query result.
    /// Returns an empty slice if the result is malformed or has no headers key.
    /// Use together with colByHeader() to access columns by name instead of
    /// by fragile positional index.
    pub fn getHeaders(result: *const std.json.Parsed(std.json.Value)) []const std.json.Value {
        if (result.value != .object) return &[_]std.json.Value{};
        const h = result.value.object.get("headers") orelse return &[_]std.json.Value{};
        if (h != .array) return &[_]std.json.Value{};
        return h.array.items;
    }

    /// Look up a cell in a result row by column header name.
    /// Returns null if the header is absent, the column index is out of range,
    /// or the row itself is not an array.
    ///
    /// Eliminates positional indexing fragility: callers no longer need to
    /// hard-code `cols[0]`, `cols[5]`, etc., which silently break when the
    /// SELECT column order changes.
    ///
    /// Example:
    ///   const headers = CozoDB.getHeaders(&result);
    ///   const row     = rows[0];
    ///   const name    = CozoDB.colByHeader(headers, row, "lod4_name");
    pub fn colByHeader(
        headers: []const std.json.Value,
        row: std.json.Value,
        col_name: []const u8,
    ) ?std.json.Value {
        if (row != .array) return null;
        const cols = row.array.items;
        for (headers, 0..) |h, i| {
            if (h == .string and std.mem.eql(u8, h.string, col_name)) {
                return if (i < cols.len) cols[i] else null;
            }
        }
        return null;
    }
};

// ---------------------------------------------------------------------------
// Tests (in-memory engine, no file system required)
// ---------------------------------------------------------------------------

const testing = std.testing;

test "CozoDB: open and close in-memory" {
    const db = try CozoDB.open(testing.allocator, .mem, "", "{}");
    try testing.expect(db.close());
}

test "CozoDB: simple scalar query" {
    const db = try CozoDB.open(testing.allocator, .mem, "", "{}");
    defer _ = db.close();

    var result = try db.queryRead("?[] <- [[1, 2, 3]]");
    defer result.deinit();

    const rows = CozoDB.getRows(&result);
    try testing.expectEqual(@as(usize, 1), rows.len);
    try testing.expectEqual(@as(usize, 3), rows[0].array.items.len);
}

test "CozoDB: create stored relation and insert" {
    const db = try CozoDB.open(testing.allocator, .mem, "", "{}");
    defer _ = db.close();

    try db.exec(":create test_rel {id: Int => name: String}");

    try db.exec("?[id, name] <- [[1, \"alpha\"], [2, \"beta\"]] :put test_rel {id => name}");

    var result = try db.queryRead("?[id, name] := *test_rel[id, name]");
    defer result.deinit();

    const rows = CozoDB.getRows(&result);
    try testing.expectEqual(@as(usize, 2), rows.len);
}

test "CozoDB: query with no results returns empty rows" {
    const db = try CozoDB.open(testing.allocator, .mem, "", "{}");
    defer _ = db.close();

    try db.exec(":create empty_rel {id: Int => val: String}");

    var result = try db.queryRead("?[id, val] := *empty_rel[id, val]");
    defer result.deinit();

    const rows = CozoDB.getRows(&result);
    try testing.expectEqual(@as(usize, 0), rows.len);
}

test "CozoDB: invalid query returns error" {
    const db = try CozoDB.open(testing.allocator, .mem, "", "{}");
    defer _ = db.close();

    const result = db.exec("this is not valid cozoscript");
    try testing.expectError(CozoError.QueryFailed, result);
}

test "CozoDB: getHeaders returns column names" {
    const db = try CozoDB.open(testing.allocator, .mem, "", "{}");
    defer _ = db.close();

    var result = try db.queryRead("?[x, y] <- [[1, 2]]");
    defer result.deinit();

    const headers = CozoDB.getHeaders(&result);
    try testing.expectEqual(@as(usize, 2), headers.len);
    try testing.expectEqualStrings("x", headers[0].string);
    try testing.expectEqualStrings("y", headers[1].string);
}

test "CozoDB: colByHeader finds value by name" {
    const db = try CozoDB.open(testing.allocator, .mem, "", "{}");
    defer _ = db.close();

    try db.exec(":create named_test {id: Int => label: String}");
    try db.exec("?[id, label] <- [[42, \"hello\"]] :put named_test {id => label}");

    var result = try db.queryRead("?[id, label] := *named_test[id, label]");
    defer result.deinit();

    const headers = CozoDB.getHeaders(&result);
    const rows = CozoDB.getRows(&result);
    try testing.expectEqual(@as(usize, 1), rows.len);

    const id_val = CozoDB.colByHeader(headers, rows[0], "id");
    const label_val = CozoDB.colByHeader(headers, rows[0], "label");
    const missing = CozoDB.colByHeader(headers, rows[0], "nonexistent");

    try testing.expect(id_val != null);
    try testing.expectEqual(@as(i64, 42), id_val.?.integer);
    try testing.expect(label_val != null);
    try testing.expectEqualStrings("hello", label_val.?.string);
    try testing.expect(missing == null);
}

test "CozoDB: getHeaders on empty result does not crash" {
    const db = try CozoDB.open(testing.allocator, .mem, "", "{}");
    defer _ = db.close();
    // A relation with no rows still returns headers.
    try db.exec(":create hdr_test {id: Int => val: String}");
    var result = try db.queryRead("?[id, val] := *hdr_test[id, val]");
    defer result.deinit();
    const headers = CozoDB.getHeaders(&result);
    try testing.expectEqual(@as(usize, 2), headers.len);
}
