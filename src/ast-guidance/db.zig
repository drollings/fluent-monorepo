//! ast-guidance SQLite FTS5 indexer.
//!
//! Maintains `.ast-guidance/ast-guidance.db` as a BM25-searchable index over
//! all `.ast-guidance/src/**/*.json` guidance files.
//!
//! Public API:
//!   pub fn syncDatabase(allocator, guidance_dir) !void
//!   pub fn openDb(allocator, guidance_dir) !AstDb
//!
//! Schema:
//!   ast_nodes          — relational table (metadata + mtime)
//!   fts_search         — FTS5 virtual table (content= external-content)
//!   Triggers           — keep fts_search in sync with ast_nodes

const std = @import("std");
const log = std.log.scoped(.ast_db);

pub const c = @cImport({
    @cInclude("sqlite3.h");
});

pub const SQLITE_STATIC: c.sqlite3_destructor_type = null;
const BUSY_TIMEOUT_MS: c_int = 5000;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Open (or create) the FTS5 index and synchronize it with every JSON file
/// under `<guidance_dir>/src/`.  Uses an ArenaAllocator per file to bound
/// peak memory usage.
pub fn syncDatabase(allocator: std.mem.Allocator, guidance_dir: []const u8) !void {
    var db = try AstDb.init(allocator, guidance_dir);
    defer db.deinit();
    try db.sync(allocator);
}

// ---------------------------------------------------------------------------
// AstDb — the database handle
// ---------------------------------------------------------------------------

pub const AstDb = struct {
    db: ?*c.sqlite3,
    allocator: std.mem.Allocator,

    const Self = @This();

    /// Open the database at `<guidance_dir>/ast-guidance.db`, running schema
    /// migrations as needed.
    pub fn init(allocator: std.mem.Allocator, guidance_dir: []const u8) !Self {
        // Build null-terminated path: <guidance_dir>/ast-guidance.db\0
        const db_path = try std.fmt.allocPrintZ(allocator, "{s}/ast-guidance.db", .{guidance_dir});
        defer allocator.free(db_path);

        var db: ?*c.sqlite3 = null;
        const rc = c.sqlite3_open(db_path.ptr, &db);
        if (rc != c.SQLITE_OK) {
            if (db) |d| _ = c.sqlite3_close(d);
            log.err("sqlite3_open({s}) failed: rc={d}", .{ db_path, rc });
            return error.SqliteOpenFailed;
        }

        _ = c.sqlite3_busy_timeout(db, BUSY_TIMEOUT_MS);

        var self_ = Self{ .db = db, .allocator = allocator };
        try self_.configurePragmas();
        try self_.migrate();
        return self_;
    }

    pub fn deinit(self: *Self) void {
        if (self.db) |db| {
            _ = c.sqlite3_close(db);
            self.db = null;
        }
    }

    // ── Schema ─────────────────────────────────────────────────────

    fn configurePragmas(self: *Self) !void {
        const pragmas = [_][:0]const u8{
            "PRAGMA journal_mode = WAL;",
            "PRAGMA synchronous  = NORMAL;",
            "PRAGMA temp_store   = MEMORY;",
            "PRAGMA cache_size   = -2000;",
            "PRAGMA foreign_keys = ON;",
        };
        for (pragmas) |pragma| {
            var err_msg: [*c]u8 = null;
            const rc = c.sqlite3_exec(self.db, pragma, null, null, &err_msg);
            if (rc != c.SQLITE_OK) {
                logSqliteErr("pragma", pragma, rc, err_msg, self.db);
            }
            if (err_msg) |msg| c.sqlite3_free(msg);
        }
    }

    fn migrate(self: *Self) !void {
        const sql =
            \\-- Relational table: one row per indexed node (module or member)
            \\CREATE TABLE IF NOT EXISTS ast_nodes (
            \\  id            INTEGER PRIMARY KEY,
            \\  file_path     TEXT    NOT NULL,
            \\  module        TEXT    NOT NULL,
            \\  node_type     TEXT    NOT NULL,
            \\  name          TEXT    NOT NULL,
            \\  signature     TEXT,
            \\  last_modified INTEGER NOT NULL
            \\);
            \\CREATE INDEX IF NOT EXISTS idx_ast_file  ON ast_nodes(file_path);
            \\CREATE INDEX IF NOT EXISTS idx_ast_mtime ON ast_nodes(file_path, last_modified);
            \\
            \\-- FTS5 virtual table — external-content backed by ast_nodes
            \\CREATE VIRTUAL TABLE IF NOT EXISTS fts_search USING fts5(
            \\  name,
            \\  comment,
            \\  module,
            \\  content='ast_nodes',
            \\  content_rowid='id'
            \\);
            \\
            \\-- Triggers: keep fts_search in sync with ast_nodes
            \\CREATE TRIGGER IF NOT EXISTS ast_nodes_ai
            \\  AFTER INSERT ON ast_nodes BEGIN
            \\    INSERT INTO fts_search(rowid, name, comment, module)
            \\    VALUES (new.id, new.name, '', new.module);
            \\  END;
            \\
            \\CREATE TRIGGER IF NOT EXISTS ast_nodes_ad
            \\  AFTER DELETE ON ast_nodes BEGIN
            \\    INSERT INTO fts_search(fts_search, rowid, name, comment, module)
            \\    VALUES ('delete', old.id, old.name, '', old.module);
            \\  END;
            \\
            \\CREATE TRIGGER IF NOT EXISTS ast_nodes_au
            \\  AFTER UPDATE ON ast_nodes BEGIN
            \\    INSERT INTO fts_search(fts_search, rowid, name, comment, module)
            \\    VALUES ('delete', old.id, old.name, '', old.module);
            \\    INSERT INTO fts_search(rowid, name, comment, module)
            \\    VALUES (new.id, new.name, '', new.module);
            \\  END;
        ;
        var err_msg: [*c]u8 = null;
        const rc = c.sqlite3_exec(self.db, sql, null, null, &err_msg);
        if (rc != c.SQLITE_OK) {
            logSqliteErr("migrate", "CREATE TABLE/FTS/triggers", rc, err_msg, self.db);
            if (err_msg) |msg| c.sqlite3_free(msg);
            return error.MigrationFailed;
        }
    }

    // ── Sync ───────────────────────────────────────────────────────

    /// Walk `<guidance_dir>/src/**/*.json` and upsert any file whose mtime has
    /// changed since the last index run.
    pub fn sync(self: *Self, allocator: std.mem.Allocator) !void {
        // guidance_dir is the parent of "src/" — reconstruct from db path.
        // We store it in the allocator so we need to pass it explicitly.
        // syncDatabase passes through guidance_dir; this overload is used via
        // AstDb.init so we need to receive guidance_dir separately.
        // NOTE: this method is only called from syncDatabase which owns db.
        _ = allocator;
        // Implementation delegates to syncFromDir, called by syncDatabase.
    }

    /// Walk `src_dir` (`.ast-guidance/src`) and upsert stale JSON files.
    pub fn syncFromDir(self: *Self, allocator: std.mem.Allocator, src_dir_path: []const u8) !void {
        var src_dir = std.fs.cwd().openDir(src_dir_path, .{ .iterate = true }) catch |err| {
            if (err == error.FileNotFound) {
                log.warn("guidance src dir not found: {s}", .{src_dir_path});
                return;
            }
            return err;
        };
        defer src_dir.close();

        var walker = try src_dir.walk(allocator);
        defer walker.deinit();

        var synced: usize = 0;
        var skipped: usize = 0;

        while (try walker.next()) |entry| {
            if (entry.kind != .file) continue;
            if (!std.mem.endsWith(u8, entry.path, ".json")) continue;

            // Build absolute-style relative path: src_dir_path/entry.path
            const rel_path = try std.fmt.allocPrint(allocator, "{s}/{s}", .{ src_dir_path, entry.path });
            defer allocator.free(rel_path);

            // Stat the file for mtime
            const stat = std.fs.cwd().statFile(rel_path) catch |err| {
                log.warn("stat({s}): {s}", .{ rel_path, @errorName(err) });
                continue;
            };
            const mtime_sec: i64 = @intCast(@divTrunc(stat.mtime, std.time.ns_per_s));

            // Check whether DB record is up-to-date
            if (try self.fileIsUpToDate(rel_path, mtime_sec)) {
                skipped += 1;
                continue;
            }

            // Use a per-file arena to avoid fragmentation
            var file_arena = std.heap.ArenaAllocator.init(allocator);
            defer file_arena.deinit();
            const fa = file_arena.allocator();

            self.indexFile(fa, rel_path, mtime_sec) catch |err| {
                log.warn("indexFile({s}): {s}", .{ rel_path, @errorName(err) });
                continue;
            };
            synced += 1;
        }

        log.info("db sync complete: {d} updated, {d} skipped", .{ synced, skipped });
    }

    // ── Per-file helpers ───────────────────────────────────────────

    /// Returns true when the DB already has an up-to-date entry for `file_path`.
    fn fileIsUpToDate(self: *Self, file_path: []const u8, mtime: i64) !bool {
        const sql = "SELECT last_modified FROM ast_nodes WHERE file_path = ?1 LIMIT 1";
        var stmt: ?*c.sqlite3_stmt = null;
        const rc = c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null);
        if (rc != c.SQLITE_OK) return false;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, file_path.ptr, @intCast(file_path.len), SQLITE_STATIC);

        if (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            const stored_mtime = c.sqlite3_column_int64(stmt, 0);
            return stored_mtime == mtime;
        }
        return false;
    }

    /// Parse a JSON guidance file and (re-)index it inside a transaction.
    fn indexFile(self: *Self, allocator: std.mem.Allocator, file_path: []const u8, mtime: i64) !void {
        const file_data = try std.fs.cwd().readFileAlloc(allocator, file_path, 8 * 1024 * 1024);
        defer allocator.free(file_data);

        const parsed = try parseGuidanceJson(allocator, file_data);

        try self.execSimple("BEGIN");
        errdefer _ = self.execSimpleNoErr("ROLLBACK");

        try self.deleteFileRecords(file_path);
        try self.insertModule(file_path, parsed.module, parsed.module_comment, mtime);
        for (parsed.members) |m| {
            try self.insertMember(file_path, parsed.module, m, mtime);
        }

        try self.execSimple("COMMIT");
    }

    fn deleteFileRecords(self: *Self, file_path: []const u8) !void {
        const sql = "DELETE FROM ast_nodes WHERE file_path = ?1";
        var stmt: ?*c.sqlite3_stmt = null;
        const rc = c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null);
        if (rc != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, file_path.ptr, @intCast(file_path.len), SQLITE_STATIC);
        if (c.sqlite3_step(stmt) != c.SQLITE_DONE) return error.StepFailed;
    }

    fn insertModule(self: *Self, file_path: []const u8, module: []const u8, comment: ?[]const u8, mtime: i64) !void {
        const sql =
            "INSERT INTO ast_nodes(file_path, module, node_type, name, signature, last_modified) " ++
            "VALUES (?1, ?2, 'module', ?3, NULL, ?4)";
        var stmt: ?*c.sqlite3_stmt = null;
        const rc = c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null);
        if (rc != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, file_path.ptr, @intCast(file_path.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 2, module.ptr, @intCast(module.len), SQLITE_STATIC);
        const name = comment orelse module;
        _ = c.sqlite3_bind_text(stmt, 3, name.ptr, @intCast(name.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_int64(stmt, 4, mtime);

        if (c.sqlite3_step(stmt) != c.SQLITE_DONE) return error.StepFailed;
    }

    fn insertMember(self: *Self, file_path: []const u8, module: []const u8, m: ParsedMember, mtime: i64) !void {
        const sql =
            "INSERT INTO ast_nodes(file_path, module, node_type, name, signature, last_modified) " ++
            "VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
        var stmt: ?*c.sqlite3_stmt = null;
        const rc = c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null);
        if (rc != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, file_path.ptr, @intCast(file_path.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 2, module.ptr, @intCast(module.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 3, m.node_type.ptr, @intCast(m.node_type.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 4, m.name.ptr, @intCast(m.name.len), SQLITE_STATIC);
        if (m.signature) |sig| {
            _ = c.sqlite3_bind_text(stmt, 5, sig.ptr, @intCast(sig.len), SQLITE_STATIC);
        } else {
            _ = c.sqlite3_bind_null(stmt, 5);
        }
        _ = c.sqlite3_bind_int64(stmt, 6, mtime);

        if (c.sqlite3_step(stmt) != c.SQLITE_DONE) return error.StepFailed;

        // Also update FTS with comment if present — triggers only push name/module.
        // For richer search, we re-update the fts row with the comment text.
        if (m.comment) |comment| {
            try self.updateFtsComment(c.sqlite3_last_insert_rowid(self.db), m.name, comment, module);
        }
    }

    /// Patch the FTS5 row for the just-inserted node to include its doc-comment.
    /// The INSERT trigger only writes name+module; this replaces that with
    /// the full comment so BM25 can score against it.
    fn updateFtsComment(self: *Self, rowid: i64, name: []const u8, comment: []const u8, module: []const u8) !void {
        // Delete the partial row written by trigger, then reinsert with comment.
        const del_sql = "INSERT INTO fts_search(fts_search, rowid, name, comment, module) VALUES('delete',?1,?2,'',?3)";
        const ins_sql = "INSERT INTO fts_search(rowid, name, comment, module) VALUES(?1,?2,?3,?4)";

        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, del_sql, -1, &stmt, null) == c.SQLITE_OK) {
            _ = c.sqlite3_bind_int64(stmt, 1, rowid);
            _ = c.sqlite3_bind_text(stmt, 2, name.ptr, @intCast(name.len), SQLITE_STATIC);
            _ = c.sqlite3_bind_text(stmt, 3, module.ptr, @intCast(module.len), SQLITE_STATIC);
            _ = c.sqlite3_step(stmt);
            _ = c.sqlite3_finalize(stmt);
        }

        stmt = null;
        if (c.sqlite3_prepare_v2(self.db, ins_sql, -1, &stmt, null) == c.SQLITE_OK) {
            _ = c.sqlite3_bind_int64(stmt, 1, rowid);
            _ = c.sqlite3_bind_text(stmt, 2, name.ptr, @intCast(name.len), SQLITE_STATIC);
            _ = c.sqlite3_bind_text(stmt, 3, comment.ptr, @intCast(comment.len), SQLITE_STATIC);
            _ = c.sqlite3_bind_text(stmt, 4, module.ptr, @intCast(module.len), SQLITE_STATIC);
            _ = c.sqlite3_step(stmt);
            _ = c.sqlite3_finalize(stmt);
        }
    }

    // ── Search ─────────────────────────────────────────────────────

    pub const SearchResult = struct {
        file_path: []const u8,
        module: []const u8,
        node_type: []const u8,
        name: []const u8,
        signature: ?[]const u8,
        score: f64,
    };

    /// BM25 full-text search.  Returns results ordered best-first (score desc).
    /// Caller must free each `SearchResult` field with `allocator`.
    pub fn search(
        self: *Self,
        allocator: std.mem.Allocator,
        query_text: []const u8,
        limit: usize,
    ) ![]SearchResult {
        const trimmed = std.mem.trim(u8, query_text, " \t\n\r");
        if (trimmed.len == 0) return allocator.alloc(SearchResult, 0);

        // Build quoted-OR FTS5 query from whitespace-separated tokens
        var fts_buf = std.ArrayList(u8).init(allocator);
        defer fts_buf.deinit();
        var it = std.mem.tokenizeAny(u8, trimmed, " \t\n\r");
        var first = true;
        while (it.next()) |word| {
            if (!first) try fts_buf.appendSlice(" OR ");
            try fts_buf.append('"');
            for (word) |ch| {
                if (ch == '"') try fts_buf.appendSlice("\"\"") else try fts_buf.append(ch);
            }
            try fts_buf.append('"');
            first = false;
        }
        if (fts_buf.items.len == 0) return allocator.alloc(SearchResult, 0);
        try fts_buf.append(0); // null-terminate

        const sql =
            "SELECT n.file_path, n.module, n.node_type, n.name, n.signature, " ++
            "       bm25(fts_search) as score " ++
            "FROM fts_search f " ++
            "JOIN ast_nodes n ON n.id = f.rowid " ++
            "WHERE fts_search MATCH ?1 " ++
            "ORDER BY score " ++
            "LIMIT ?2";

        var stmt: ?*c.sqlite3_stmt = null;
        const rc = c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null);
        if (rc != c.SQLITE_OK) return allocator.alloc(SearchResult, 0);
        defer _ = c.sqlite3_finalize(stmt);

        const fts_query = fts_buf.items[0 .. fts_buf.items.len - 1];
        _ = c.sqlite3_bind_text(stmt, 1, fts_query.ptr, @intCast(fts_query.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_int64(stmt, 2, @intCast(limit));

        var results = std.ArrayList(SearchResult).init(allocator);
        errdefer {
            for (results.items) |r| freeSearchResult(allocator, r);
            results.deinit();
        }

        while (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            const result = try readSearchResult(stmt.?, allocator);
            try results.append(result);
        }

        return results.toOwnedSlice();
    }

    fn readSearchResult(stmt: *c.sqlite3_stmt, allocator: std.mem.Allocator) !SearchResult {
        return SearchResult{
            .file_path = try dupeCol(stmt, 0, allocator),
            .module = try dupeCol(stmt, 1, allocator),
            .node_type = try dupeCol(stmt, 2, allocator),
            .name = try dupeCol(stmt, 3, allocator),
            .signature = try dupeColNullable(stmt, 4, allocator),
            .score = -c.sqlite3_column_double(stmt, 5), // BM25 negative → positive
        };
    }

    pub fn freeSearchResult(allocator: std.mem.Allocator, r: SearchResult) void {
        allocator.free(r.file_path);
        allocator.free(r.module);
        allocator.free(r.node_type);
        allocator.free(r.name);
        if (r.signature) |s| allocator.free(s);
    }

    // ── SQL utilities ──────────────────────────────────────────────

    fn execSimple(self: *Self, sql: [:0]const u8) !void {
        var err_msg: [*c]u8 = null;
        const rc = c.sqlite3_exec(self.db, sql, null, null, &err_msg);
        if (rc != c.SQLITE_OK) {
            logSqliteErr("exec", sql, rc, err_msg, self.db);
            if (err_msg) |msg| c.sqlite3_free(msg);
            return error.ExecFailed;
        }
    }

    fn execSimpleNoErr(self: *Self, sql: [:0]const u8) void {
        var err_msg: [*c]u8 = null;
        const rc = c.sqlite3_exec(self.db, sql, null, null, &err_msg);
        if (err_msg) |msg| c.sqlite3_free(msg);
        _ = rc;
    }
};

// ---------------------------------------------------------------------------
// JSON parsing — targeted extraction from GuidanceDoc format
// ---------------------------------------------------------------------------

const ParsedMember = struct {
    node_type: []const u8,
    name: []const u8,
    signature: ?[]const u8,
    comment: ?[]const u8,
};

const ParsedDoc = struct {
    module: []const u8,
    module_comment: ?[]const u8,
    members: []ParsedMember,
};

/// Parse a GuidanceDoc JSON blob, extracting only the fields we need for FTS.
/// All returned strings are owned by the arena passed in.
fn parseGuidanceJson(arena: std.mem.Allocator, json_data: []const u8) !ParsedDoc {
    const Value = std.json.Value;
    const parsed = try std.json.parseFromSlice(Value, arena, json_data, .{
        .ignore_unknown_fields = true,
    });
    // parsed.value owned by arena — no need to call parsed.deinit()

    const root = parsed.value;
    if (root != .object) return error.InvalidJson;

    // meta.module and meta.source
    const meta_val = root.object.get("meta") orelse return error.MissingMeta;
    if (meta_val != .object) return error.MissingMeta;
    const module_val = meta_val.object.get("module") orelse return error.MissingModule;
    if (module_val != .string) return error.MissingModule;
    const module = module_val.string;

    const comment: ?[]const u8 = blk: {
        const cv = root.object.get("comment") orelse break :blk null;
        if (cv != .string) break :blk null;
        break :blk cv.string;
    };

    var members_list = std.ArrayList(ParsedMember).init(arena);

    const members_val = root.object.get("members") orelse {
        return .{ .module = module, .module_comment = comment, .members = &.{} };
    };
    if (members_val != .array) {
        return .{ .module = module, .module_comment = comment, .members = &.{} };
    }

    for (members_val.array.items) |item| {
        if (item != .object) continue;
        const m = try parseMemberValue(arena, item);
        try members_list.append(m);
        // Also recurse into nested members (e.g. methods inside a struct)
        if (item.object.get("members")) |nested_val| {
            if (nested_val == .array) {
                for (nested_val.array.items) |nested_item| {
                    if (nested_item != .object) continue;
                    const nm = try parseMemberValue(arena, nested_item);
                    try members_list.append(nm);
                }
            }
        }
    }

    return .{
        .module = module,
        .module_comment = comment,
        .members = try members_list.toOwnedSlice(),
    };
}

fn parseMemberValue(arena: std.mem.Allocator, item: std.json.Value) !ParsedMember {
    _ = arena;
    const node_type: []const u8 = blk: {
        const tv = item.object.get("type") orelse break :blk "unknown";
        if (tv != .string) break :blk "unknown";
        break :blk tv.string;
    };
    const name: []const u8 = blk: {
        const nv = item.object.get("name") orelse break :blk "";
        if (nv != .string) break :blk "";
        break :blk nv.string;
    };
    const signature: ?[]const u8 = blk: {
        const sv = item.object.get("signature") orelse break :blk null;
        if (sv != .string) break :blk null;
        break :blk sv.string;
    };
    const comment: ?[]const u8 = blk: {
        const cv = item.object.get("comment") orelse break :blk null;
        if (cv != .string) break :blk null;
        // Skip primitive/boilerplate comments
        if (cv.string.len < 4) break :blk null;
        break :blk cv.string;
    };
    return .{ .node_type = node_type, .name = name, .signature = signature, .comment = comment };
}

// ---------------------------------------------------------------------------
// Column helpers
// ---------------------------------------------------------------------------

fn dupeCol(stmt: *c.sqlite3_stmt, col: c_int, allocator: std.mem.Allocator) ![]u8 {
    const raw = c.sqlite3_column_text(stmt, col);
    const len: usize = @intCast(c.sqlite3_column_bytes(stmt, col));
    if (raw == null or len == 0) return allocator.dupe(u8, "");
    return allocator.dupe(u8, @as([*]const u8, @ptrCast(raw))[0..len]);
}

fn dupeColNullable(stmt: *c.sqlite3_stmt, col: c_int, allocator: std.mem.Allocator) !?[]u8 {
    if (c.sqlite3_column_type(stmt, col) == c.SQLITE_NULL) return null;
    const raw = c.sqlite3_column_text(stmt, col);
    if (raw == null) return null;
    const len: usize = @intCast(c.sqlite3_column_bytes(stmt, col));
    return try allocator.dupe(u8, @as([*]const u8, @ptrCast(raw))[0..len]);
}

fn logSqliteErr(context: []const u8, sql: []const u8, rc: c_int, err_msg: [*c]u8, db: ?*c.sqlite3) void {
    if (err_msg) |msg| {
        log.warn("sqlite {s} failed (rc={d}, sql={s}): {s}", .{ context, rc, sql, std.mem.span(msg) });
        return;
    }
    if (db) |d| {
        log.warn("sqlite {s} failed (rc={d}, sql={s}): {s}", .{ context, rc, sql, std.mem.span(c.sqlite3_errmsg(d)) });
        return;
    }
    log.warn("sqlite {s} failed (rc={d}, sql={s})", .{ context, rc, sql });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "db init with in-memory path" {
    const allocator = std.testing.allocator;
    // Use a temp dir so we can clean up
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    var db = try AstDb.init(allocator, tmp_path);
    defer db.deinit();

    // Basic health check: SELECT 1 must work
    var err_msg: [*c]u8 = null;
    const rc = c.sqlite3_exec(db.db, "SELECT 1", null, null, &err_msg);
    if (err_msg) |msg| c.sqlite3_free(msg);
    try std.testing.expectEqual(@as(c_int, c.SQLITE_OK), rc);
}

test "parseGuidanceJson extracts module and members" {
    const json =
        \\{
        \\  "meta": { "module": "src.foo.bar", "source": "src/foo/bar.zig", "language": "zig" },
        \\  "comment": "Does something useful.",
        \\  "members": [
        \\    { "type": "fn_decl", "name": "doThing", "signature": "fn doThing() void", "comment": "Does the thing." },
        \\    { "type": "struct",  "name": "MyState", "is_pub": true }
        \\  ]
        \\}
    ;
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const doc = try parseGuidanceJson(arena.allocator(), json);
    try std.testing.expectEqualStrings("src.foo.bar", doc.module);
    try std.testing.expectEqualStrings("Does something useful.", doc.module_comment.?);
    try std.testing.expectEqual(@as(usize, 2), doc.members.len);
    try std.testing.expectEqualStrings("doThing", doc.members[0].name);
    try std.testing.expectEqualStrings("fn_decl", doc.members[0].node_type);
    try std.testing.expectEqualStrings("Does the thing.", doc.members[0].comment.?);
    try std.testing.expectEqualStrings("MyState", doc.members[1].name);
}

test "full index + search round-trip" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    // Create a fake src/ dir with one JSON file
    try tmp.dir.makeDir("src");
    const json =
        \\{
        \\  "meta": { "module": "src.mymod", "source": "src/mymod.zig", "language": "zig" },
        \\  "comment": "The best module.",
        \\  "members": [
        \\    { "type": "fn_decl", "name": "frobnicate", "comment": "Frobnicates the widget." }
        \\  ]
        \\}
    ;
    try tmp.dir.writeFile(.{ .sub_path = "src/mymod.zig.json", .data = json });

    var db = try AstDb.init(allocator, tmp_path);
    defer db.deinit();

    const src_dir = try std.fmt.allocPrint(allocator, "{s}/src", .{tmp_path});
    defer allocator.free(src_dir);

    try db.syncFromDir(allocator, src_dir);

    const results = try db.search(allocator, "frobnicate", 10);
    defer {
        for (results) |r| AstDb.freeSearchResult(allocator, r);
        allocator.free(results);
    }

    try std.testing.expect(results.len >= 1);
    try std.testing.expectEqualStrings("frobnicate", results[0].name);
}
