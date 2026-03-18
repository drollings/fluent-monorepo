//! guidance LanceDB-style vector search database.
//!
//! Produces `.guidance.db` — a SQLite database with vector embeddings for
//! semantic (cosine similarity) search, powering the `explain` subcommand.
//!
//! Public API:
//!   pub fn syncDatabase(allocator, guidance_dir, db_path, embedder) !void
//!   pub fn openDb(allocator, db_path, embedder) !GuidanceDb
//!   pub fn loadSemanticAliases(allocator, path) !?SemanticAliases
//!
//! Schema (version 1):
//!   schema_version   — single-row version table
//!   ast_nodes        — relational table with metadata + embedding BLOB
//!   embedding_cache  — content-hash → embedding blob (avoids redundant API calls)
//!
//! Search modes:
//!   vectorSearch    — cosine similarity over stored embeddings (semantic)
//!   keywordSearch   — SQL LIKE on name / comment / module / signature
//!   search          — hybrid: vector + keyword, fused by weighted score

const std = @import("std");
const vector = @import("vector/root.zig");
const log = std.log.scoped(.guidance_db);

pub const c = @cImport({
    @cInclude("sqlite3.h");
});

pub const SQLITE_STATIC: c.sqlite3_destructor_type = null;
const BUSY_TIMEOUT_MS: c_int = 5000;

pub const SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Semantic aliases for query expansion
// ---------------------------------------------------------------------------

/// A single alias entry mapping a key to expansion values.
pub const SemanticAlias = struct {
    key: []const u8,
    values: []const []const u8,
};

/// Loaded semantic aliases (owned by caller).
pub const SemanticAliases = struct {
    aliases: []SemanticAlias,
    allocator: std.mem.Allocator,

    pub fn deinit(self: *@This()) void {
        for (self.aliases) |a| {
            self.allocator.free(a.key);
            for (a.values) |v| self.allocator.free(v);
            self.allocator.free(a.values);
        }
        self.allocator.free(self.aliases);
    }

    /// Expand query tokens using aliases. Returns owned slice of owned strings.
    /// Caller must free the returned slice and each string.
    pub fn expandTokens(
        self: @This(),
        allocator: std.mem.Allocator,
        tokens: []const []const u8,
    ) ![]const []const u8 {
        var expanded: std.ArrayList([]const u8) = .{};
        errdefer {
            for (expanded.items) |t| allocator.free(t);
            expanded.deinit(allocator);
        }

        var seen_lowercase: std.StringHashMapUnmanaged(void) = .{};
        defer {
            var it = seen_lowercase.keyIterator();
            while (it.next()) |k| allocator.free(k.*);
            seen_lowercase.deinit(allocator);
        }

        for (tokens) |tok| {
            const lower = try std.ascii.allocLowerString(allocator, tok);
            const contains_lower = seen_lowercase.contains(lower);
            if (!contains_lower) {
                try seen_lowercase.put(allocator, lower, {});
            }
            if (contains_lower) {
                allocator.free(lower);
                continue;
            }

            try expanded.append(allocator, try allocator.dupe(u8, tok));

            for (self.aliases) |alias| {
                if (std.ascii.eqlIgnoreCase(tok, alias.key)) {
                    for (alias.values) |val| {
                        const val_lower = try std.ascii.allocLowerString(allocator, val);
                        if (seen_lowercase.contains(val_lower)) {
                            allocator.free(val_lower);
                            continue;
                        }
                        try seen_lowercase.put(allocator, val_lower, {});
                        try expanded.append(allocator, try allocator.dupe(u8, val));
                    }
                }
            }
        }

        return try expanded.toOwnedSlice(allocator);
    }
};

/// Load semantic aliases from a JSON file.
/// Returns null if file doesn't exist.
pub fn loadSemanticAliases(allocator: std.mem.Allocator, path: []const u8) !?SemanticAliases {
    const file = std.fs.openFileAbsolute(path, .{}) catch return null;
    defer file.close();

    const content = file.readToEndAlloc(allocator, 64 * 1024) catch return null;
    defer allocator.free(content);

    const Value = std.json.Value;
    var parsed = std.json.parseFromSlice(Value, allocator, content, .{ .ignore_unknown_fields = true }) catch return null;
    defer parsed.deinit();

    if (parsed.value != .object) return null;
    const aliases_arr = parsed.value.object.get("aliases") orelse return null;
    if (aliases_arr != .array) return null;

    var out: std.ArrayList(SemanticAlias) = .{};
    errdefer {
        for (out.items) |a| {
            allocator.free(a.key);
            for (a.values) |v| allocator.free(v);
            allocator.free(a.values);
        }
        out.deinit(allocator);
    }

    for (aliases_arr.array.items) |item| {
        if (item != .object) continue;
        const key_val = item.object.get("key") orelse continue;
        const values_val = item.object.get("values") orelse continue;
        if (key_val != .string or values_val != .array) continue;

        var vals: std.ArrayList([]const u8) = .{};
        errdefer {
            for (vals.items) |v| allocator.free(v);
            vals.deinit(allocator);
        }

        for (values_val.array.items) |v| {
            if (v == .string) {
                try vals.append(allocator, try allocator.dupe(u8, v.string));
            }
        }

        try out.append(allocator, .{
            .key = try allocator.dupe(u8, key_val.string),
            .values = try vals.toOwnedSlice(allocator),
        });
    }

    return .{
        .aliases = try out.toOwnedSlice(allocator),
        .allocator = allocator,
    };
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Open (or create) `db_path`, synchronise it with every JSON file under
/// `<guidance_dir>/src/`, embedding each node via `embedder`.
/// If `capabilities_dir` is non-null, also indexes CAPABILITY.md files from
/// that directory into the `capabilities` table.
pub fn syncDatabase(
    allocator: std.mem.Allocator,
    guidance_dir: []const u8,
    db_path: []const u8,
    embedder: vector.EmbeddingProvider,
    capabilities_dir: ?[]const u8,
) !void {
    var db = try GuidanceDb.init(allocator, db_path, embedder);
    defer db.deinit();

    const src_dir_path = try std.fmt.allocPrint(allocator, "{s}/src", .{guidance_dir});
    defer allocator.free(src_dir_path);

    try db.syncFromDir(allocator, src_dir_path);

    if (capabilities_dir) |cap_dir| {
        db.syncCapabilities(allocator, cap_dir) catch |err| {
            log.warn("capabilities sync failed: {s}", .{@errorName(err)});
        };
    }
}

// ---------------------------------------------------------------------------
// GuidanceDb — the database handle
// ---------------------------------------------------------------------------

pub const GuidanceDb = struct {
    db: ?*c.sqlite3,
    allocator: std.mem.Allocator,
    /// Embedding provider — caller owns this; GuidanceDb does NOT call deinit.
    embedder: vector.EmbeddingProvider,

    const Self = @This();

    pub fn init(
        allocator: std.mem.Allocator,
        db_path: []const u8,
        embedder: vector.EmbeddingProvider,
    ) !Self {
        const db_path_z = try std.fmt.allocPrintSentinel(allocator, "{s}", .{db_path}, 0);
        defer allocator.free(db_path_z);

        var db: ?*c.sqlite3 = null;
        const rc = c.sqlite3_open(db_path_z.ptr, &db);
        if (rc != c.SQLITE_OK) {
            if (db) |d| _ = c.sqlite3_close(d);
            log.err("sqlite3_open({s}) failed: rc={d}", .{ db_path, rc });
            return error.SqliteOpenFailed;
        }

        _ = c.sqlite3_busy_timeout(db, BUSY_TIMEOUT_MS);

        var self_ = Self{ .db = db, .allocator = allocator, .embedder = embedder };
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
            if (rc != c.SQLITE_OK) logSqliteErr("pragma", pragma, rc, err_msg, self.db);
            if (err_msg) |msg| c.sqlite3_free(msg);
        }
    }

    fn migrate(self: *Self) !void {
        // ── Step 1: base tables and relational indexes (no embedding column yet) ──
        // We separate embedding-column creation into ALTER TABLE steps so this
        // migration is safe to run on a database that was previously created by
        // an older guidance.db version. CREATE TABLE IF NOT EXISTS is idempotent;
        // ALTER TABLE ADD COLUMN silently succeeds even if the column already exists
        // because we ignore the "duplicate column name" error code.
        const base_sql =
            \\CREATE TABLE IF NOT EXISTS schema_version (
            \\  version INTEGER PRIMARY KEY
            \\);
            \\CREATE TABLE IF NOT EXISTS ast_nodes (
            \\  id            INTEGER PRIMARY KEY,
            \\  file_path     TEXT    NOT NULL,
            \\  source        TEXT,
            \\  module        TEXT    NOT NULL,
            \\  node_type     TEXT    NOT NULL,
            \\  name          TEXT    NOT NULL,
            \\  signature     TEXT,
            \\  comment       TEXT,
            \\  line          INTEGER,
            \\  used_by       TEXT,
            \\  language      TEXT    NOT NULL DEFAULT 'zig',
            \\  file_type     TEXT    NOT NULL DEFAULT 'source',
            \\  file_hash     TEXT,
            \\  last_modified INTEGER NOT NULL
            \\);
            \\CREATE INDEX IF NOT EXISTS idx_gdb_file      ON ast_nodes(file_path);
            \\CREATE INDEX IF NOT EXISTS idx_gdb_mtime     ON ast_nodes(file_path, last_modified);
            \\CREATE INDEX IF NOT EXISTS idx_gdb_lang      ON ast_nodes(language);
            \\CREATE INDEX IF NOT EXISTS idx_gdb_module    ON ast_nodes(module);
            \\CREATE INDEX IF NOT EXISTS idx_gdb_node_type ON ast_nodes(node_type);
            \\CREATE TABLE IF NOT EXISTS embedding_cache (
            \\  content_hash TEXT PRIMARY KEY,
            \\  embedding     BLOB NOT NULL,
            \\  model         TEXT NOT NULL,
            \\  created_at    INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            \\);
        ;
        {
            var err_msg: [*c]u8 = null;
            const rc = c.sqlite3_exec(self.db, base_sql, null, null, &err_msg);
            if (rc != c.SQLITE_OK) {
                logSqliteErr("migrate", "CREATE TABLE/base-indexes", rc, err_msg, self.db);
                if (err_msg) |msg| c.sqlite3_free(msg);
                return error.MigrationFailed;
            }
        }

        // ── Step 2: add embedding columns (idempotent ALTER TABLE) ──
        // SQLite returns SQLITE_ERROR ("duplicate column name") if the column
        // already exists — we intentionally ignore that specific error.
        const alter_sqls = [_][:0]const u8{
            "ALTER TABLE ast_nodes ADD COLUMN embedding BLOB",
            "ALTER TABLE ast_nodes ADD COLUMN embedding_model TEXT",
            "ALTER TABLE ast_nodes ADD COLUMN source TEXT",
        };
        for (alter_sqls) |alter_sql| {
            var alter_err: [*c]u8 = null;
            _ = c.sqlite3_exec(self.db, alter_sql, null, null, &alter_err);
            if (alter_err) |msg| c.sqlite3_free(msg);
        }

        // ── Step 2b: capabilities table ──
        {
            const cap_sql =
                \\CREATE TABLE IF NOT EXISTS capabilities (
                \\  id            INTEGER PRIMARY KEY,
                \\  name          TEXT    NOT NULL,
                \\  description   TEXT,
                \\  content       TEXT,
                \\  file_path     TEXT    NOT NULL,
                \\  last_modified INTEGER NOT NULL,
                \\  embedding     BLOB,
                \\  embedding_model TEXT
                \\);
                \\CREATE UNIQUE INDEX IF NOT EXISTS idx_gdb_cap_name ON capabilities(name);
                \\CREATE INDEX IF NOT EXISTS idx_gdb_cap_fp ON capabilities(file_path);
            ;
            var cap_err: [*c]u8 = null;
            const cap_rc = c.sqlite3_exec(self.db, cap_sql, null, null, &cap_err);
            if (cap_rc != c.SQLITE_OK) {
                logSqliteErr("migrate", "CREATE TABLE capabilities", cap_rc, cap_err, self.db);
                if (cap_err) |msg| c.sqlite3_free(msg);
                return error.MigrationFailed;
            }
        }

        // ── Step 3: conditional index on embedding (requires column to exist) ──
        {
            var err_msg: [*c]u8 = null;
            const rc = c.sqlite3_exec(
                self.db,
                "CREATE INDEX IF NOT EXISTS idx_gdb_has_emb ON ast_nodes(id) WHERE embedding IS NOT NULL",
                null,
                null,
                &err_msg,
            );
            if (rc != c.SQLITE_OK) {
                // Log but don't fail — partial index is a performance hint only.
                logSqliteErr("migrate", "CREATE INDEX idx_gdb_has_emb", rc, err_msg, self.db);
            }
            if (err_msg) |msg| c.sqlite3_free(msg);
        }

        // ── Step 4: schema version ──
        const ver_sql = "INSERT OR REPLACE INTO schema_version(version) VALUES(?1)";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, ver_sql, -1, &stmt, null) == c.SQLITE_OK) {
            _ = c.sqlite3_bind_int64(stmt, 1, SCHEMA_VERSION);
            _ = c.sqlite3_step(stmt);
            _ = c.sqlite3_finalize(stmt);
        }
    }

    // ── Sync ───────────────────────────────────────────────────────

    /// Walk `src_dir_path` and upsert stale JSON files, embedding each node.
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

            const rel_path = try std.fmt.allocPrint(allocator, "{s}/{s}", .{ src_dir_path, entry.path });
            defer allocator.free(rel_path);

            const stat = std.fs.cwd().statFile(rel_path) catch |err| {
                log.warn("stat({s}): {s}", .{ rel_path, @errorName(err) });
                continue;
            };
            const mtime_sec: i64 = @intCast(@divTrunc(stat.mtime, std.time.ns_per_s));

            if (try self.fileIsUpToDate(rel_path, mtime_sec)) {
                skipped += 1;
                continue;
            }

            var file_arena = std.heap.ArenaAllocator.init(allocator);
            defer file_arena.deinit();
            const fa = file_arena.allocator();

            self.indexFile(fa, rel_path, mtime_sec) catch |err| {
                log.warn("indexFile({s}): {s}", .{ rel_path, @errorName(err) });
                continue;
            };
            synced += 1;
        }

        log.info("guidance.db sync complete: {d} updated, {d} skipped", .{ synced, skipped });
    }

    /// Walk `cap_dir` for `CAPABILITY.md` files and upsert them into the
    /// `capabilities` table.  Each capability is embedded as a single row
    /// using the frontmatter name + description + first 500 chars of content.
    pub fn syncCapabilities(self: *Self, allocator: std.mem.Allocator, cap_dir: []const u8) !void {
        var dir = std.fs.cwd().openDir(cap_dir, .{ .iterate = true }) catch |err| {
            if (err == error.FileNotFound) {
                log.info("capabilities dir not found: {s}", .{cap_dir});
                return;
            }
            return err;
        };
        defer dir.close();

        var walker = try dir.walk(allocator);
        defer walker.deinit();

        var synced: usize = 0;

        while (try walker.next()) |entry| {
            if (entry.kind != .file) continue;
            if (!std.mem.endsWith(u8, entry.path, "CAPABILITY.md")) continue;

            const abs_path = try std.fmt.allocPrint(allocator, "{s}/{s}", .{ cap_dir, entry.path });
            defer allocator.free(abs_path);

            const stat = std.fs.cwd().statFile(abs_path) catch continue;
            const mtime_sec: i64 = @intCast(@divTrunc(stat.mtime, std.time.ns_per_s));

            // Check if already up-to-date
            const up_to_date = blk: {
                const chk_sql = "SELECT last_modified FROM capabilities WHERE file_path = ?1 LIMIT 1";
                var chk_stmt: ?*c.sqlite3_stmt = null;
                if (c.sqlite3_prepare_v2(self.db, chk_sql, -1, &chk_stmt, null) != c.SQLITE_OK) break :blk false;
                defer _ = c.sqlite3_finalize(chk_stmt);
                _ = c.sqlite3_bind_text(chk_stmt, 1, abs_path.ptr, @intCast(abs_path.len), SQLITE_STATIC);
                if (c.sqlite3_step(chk_stmt) == c.SQLITE_ROW) {
                    break :blk c.sqlite3_column_int64(chk_stmt, 0) == mtime_sec;
                }
                break :blk false;
            };
            if (up_to_date) continue;

            var arena = std.heap.ArenaAllocator.init(allocator);
            defer arena.deinit();
            const fa = arena.allocator();

            self.indexCapability(fa, abs_path, mtime_sec) catch |err| {
                log.warn("indexCapability({s}): {s}", .{ abs_path, @errorName(err) });
                continue;
            };
            synced += 1;
        }

        if (synced > 0) log.info("capabilities synced: {d}", .{synced});
    }

    /// Parse a CAPABILITY.md file and upsert it into the capabilities table.
    fn indexCapability(self: *Self, allocator: std.mem.Allocator, file_path: []const u8, mtime: i64) !void {
        const content = try std.fs.cwd().readFileAlloc(allocator, file_path, 512 * 1024);
        defer allocator.free(content);

        // Parse YAML-ish frontmatter: ---\nname: ...\ndescription: ...\n---
        var name: []const u8 = std.fs.path.stem(std.fs.path.dirname(file_path) orelse file_path);
        var description: []const u8 = "";
        var body: []const u8 = content;

        if (std.mem.startsWith(u8, content, "---")) {
            const end = std.mem.indexOf(u8, content[3..], "\n---") orelse 0;
            if (end > 0) {
                const fm = content[3 .. end + 3];
                body = content[end + 7 ..]; // skip "---\n"
                var fm_it = std.mem.splitScalar(u8, fm, '\n');
                while (fm_it.next()) |line| {
                    const trimmed = std.mem.trim(u8, line, " \r");
                    if (std.mem.startsWith(u8, trimmed, "name:")) {
                        name = std.mem.trim(u8, trimmed["name:".len..], " ");
                    } else if (std.mem.startsWith(u8, trimmed, "description:")) {
                        description = std.mem.trim(u8, trimmed["description:".len..], " ");
                    }
                }
            }
        }

        // Build embedding text: description + first 500 chars of body
        const body_excerpt = body[0..@min(500, body.len)];
        const emb_text = try std.fmt.allocPrint(
            allocator,
            "capability {s}: {s}. {s}",
            .{ name, description, body_excerpt },
        );
        defer allocator.free(emb_text);

        const emb = try self.getOrComputeEmbedding(allocator, emb_text);
        defer if (emb) |e| allocator.free(e);

        const emb_bytes: ?[]u8 = if (emb) |e| try vector.vecToBytes(allocator, e) else null;
        defer if (emb_bytes) |b| allocator.free(b);

        const model_name = self.embedder.getName();

        const sql =
            "INSERT OR REPLACE INTO capabilities" ++
            "(name, description, content, file_path, last_modified, embedding, embedding_model)" ++
            " VALUES (?1,?2,?3,?4,?5,?6,?7)";

        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, name.ptr, @intCast(name.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 2, description.ptr, @intCast(description.len), SQLITE_STATIC);
        const body_trunc = body[0..@min(4096, body.len)];
        _ = c.sqlite3_bind_text(stmt, 3, body_trunc.ptr, @intCast(body_trunc.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 4, file_path.ptr, @intCast(file_path.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_int64(stmt, 5, mtime);
        if (emb_bytes) |b|
            _ = c.sqlite3_bind_blob(stmt, 6, b.ptr, @intCast(b.len), SQLITE_STATIC)
        else
            _ = c.sqlite3_bind_null(stmt, 6);
        _ = c.sqlite3_bind_text(stmt, 7, model_name.ptr, @intCast(model_name.len), SQLITE_STATIC);

        if (c.sqlite3_step(stmt) != c.SQLITE_DONE) return error.StepFailed;
    }

    // ── Per-file helpers ───────────────────────────────────────────

    fn fileIsUpToDate(self: *Self, file_path: []const u8, mtime: i64) !bool {
        const sql = "SELECT last_modified FROM ast_nodes WHERE file_path = ?1 LIMIT 1";
        var stmt: ?*c.sqlite3_stmt = null;
        const rc = c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null);
        if (rc != c.SQLITE_OK) return false;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, file_path.ptr, @intCast(file_path.len), SQLITE_STATIC);
        if (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            return c.sqlite3_column_int64(stmt, 0) == mtime;
        }
        return false;
    }

    fn indexFile(self: *Self, allocator: std.mem.Allocator, file_path: []const u8, mtime: i64) !void {
        const file_data = try std.fs.cwd().readFileAlloc(allocator, file_path, 8 * 1024 * 1024);
        defer allocator.free(file_data);

        const parsed = try parseGuidanceJson(allocator, file_data);

        try self.execSimple("BEGIN");
        errdefer _ = self.execSimpleNoErr("ROLLBACK");

        try self.deleteFileRecords(file_path);
        try self.insertModule(allocator, file_path, parsed, mtime);
        for (parsed.members) |m| {
            try self.insertMember(allocator, file_path, parsed, m, mtime);
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

    // ── Embedding text builder ─────────────────────────────────────

    /// Convert a dotted module path to human-readable prose.
    /// "src.guidance.db" → "guidance database"
    /// "src.guidance.vector.math" → "guidance vector math"
    /// Only includes the last 3 path components to keep text concise.
    fn moduleToProse(allocator: std.mem.Allocator, module: []const u8) ![]u8 {
        // Collect path components, skipping "src" prefix
        var parts: [8][]const u8 = undefined;
        var n: usize = 0;
        var it = std.mem.splitScalar(u8, module, '.');
        while (it.next()) |part| {
            if (part.len == 0) continue;
            if (n == 0 and std.mem.eql(u8, part, "src")) continue;
            if (n < parts.len) {
                parts[n] = part;
                n += 1;
            }
        }
        if (n == 0) return allocator.dupe(u8, module);
        // Take last 3 components for conciseness
        const start: usize = if (n > 3) n - 3 else 0;
        var buf: std.ArrayList(u8) = .empty;
        errdefer buf.deinit(allocator);
        for (parts[start..n], 0..) |part, i| {
            if (i > 0) try buf.append(allocator, ' ');
            try buf.appendSlice(allocator, part);
        }
        return buf.toOwnedSlice(allocator);
    }

    /// Map node_type to a plain English noun for prose embedding.
    fn nodeTypeToNoun(node_type: []const u8) []const u8 {
        if (std.mem.eql(u8, node_type, "fn_decl")) return "function";
        if (std.mem.eql(u8, node_type, "method")) return "method";
        if (std.mem.eql(u8, node_type, "method_private")) return "private method";
        if (std.mem.eql(u8, node_type, "struct")) return "struct";
        if (std.mem.eql(u8, node_type, "enum")) return "enum";
        if (std.mem.eql(u8, node_type, "const")) return "constant";
        if (std.mem.eql(u8, node_type, "type")) return "type alias";
        if (std.mem.eql(u8, node_type, "module")) return "module";
        if (std.mem.eql(u8, node_type, "test_decl")) return "test";
        return node_type;
    }

    /// Extract parameter names from a signature string, stripping types.
    /// "fn foo(allocator: Allocator, x: u32) u32" → "allocator, x"
    /// Returns null when no params or parsing is too complex.
    fn extractParamNames(allocator: std.mem.Allocator, signature: []const u8) !?[]u8 {
        // Find opening paren
        const open = std.mem.indexOfScalar(u8, signature, '(') orelse return null;
        const close = std.mem.lastIndexOfScalar(u8, signature, ')') orelse return null;
        if (close <= open) return null;
        const params_str = std.mem.trim(u8, signature[open + 1 .. close], " \t");
        if (params_str.len == 0) return null;

        var buf: std.ArrayList(u8) = .empty;
        errdefer buf.deinit(allocator);

        var first = true;
        var param_it = std.mem.splitScalar(u8, params_str, ',');
        while (param_it.next()) |param| {
            const trimmed = std.mem.trim(u8, param, " \t");
            if (trimmed.len == 0) continue;
            // Take the part before ':' (the name), or the whole thing if no colon
            const name_end = std.mem.indexOfScalar(u8, trimmed, ':') orelse trimmed.len;
            const name = std.mem.trim(u8, trimmed[0..name_end], " \t*[]");
            if (name.len == 0 or std.mem.eql(u8, name, "_")) continue;
            // Skip "self" and "allocator" — they add no semantic value
            if (std.mem.eql(u8, name, "self") or std.mem.eql(u8, name, "allocator")) continue;
            if (!first) try buf.appendSlice(allocator, ", ");
            try buf.appendSlice(allocator, name);
            first = false;
        }
        if (buf.items.len == 0) {
            buf.deinit(allocator);
            return null;
        }
        const owned = try buf.toOwnedSlice(allocator);
        return owned;
    }

    /// Build rich prose embedding text for an AST node.
    ///
    /// Format (members):
    ///   "<module prose> — <noun> <name>: <comment>. Parameters: <param names>."
    ///   followed by parent module context when available.
    ///
    /// Format (modules):
    ///   "<module prose> module: <module comment>."
    ///
    /// The prose format ("guidance database — function syncDatabase: syncs the
    /// SQLite database") embeds far better than dotted-path tokens because
    /// sentence transformers weight natural language more heavily than
    /// identifier fragments.
    ///
    /// `parent_comment` is the top-level module doc comment — including it in
    /// member embeddings means queries about the module's purpose also surface
    /// individual functions, not just the module row.
    fn buildEmbeddingText(
        allocator: std.mem.Allocator,
        module: []const u8,
        name: []const u8,
        node_type: []const u8,
        comment: ?[]const u8,
        signature: ?[]const u8,
        parent_comment: ?[]const u8,
    ) ![]const u8 {
        const prose_module = try moduleToProse(allocator, module);
        defer allocator.free(prose_module);

        const noun = nodeTypeToNoun(node_type);

        var buf: std.ArrayList(u8) = .empty;
        errdefer buf.deinit(allocator);

        if (std.mem.eql(u8, node_type, "module")) {
            // Module row: "<prose> module: <comment>"
            try buf.appendSlice(allocator, prose_module);
            try buf.appendSlice(allocator, " module");
            if (comment) |cm| {
                if (cm.len > 0) {
                    try buf.appendSlice(allocator, ": ");
                    try buf.appendSlice(allocator, cm);
                }
            }
        } else {
            // Member row: "<prose> — <noun> <name>: <comment>. Parameters: <params>."
            try buf.appendSlice(allocator, prose_module);
            try buf.appendSlice(allocator, " — ");
            try buf.appendSlice(allocator, noun);
            try buf.append(allocator, ' ');
            try buf.appendSlice(allocator, name);

            if (comment) |cm| {
                if (cm.len > 0) {
                    try buf.appendSlice(allocator, ": ");
                    try buf.appendSlice(allocator, cm);
                }
            }

            // Parameter names (stripped of types) — semantic signal for callers
            if (signature) |sig| {
                if (try extractParamNames(allocator, sig)) |params| {
                    defer allocator.free(params);
                    if (params.len > 0) {
                        try buf.appendSlice(allocator, ". Parameters: ");
                        try buf.appendSlice(allocator, params);
                        try buf.append(allocator, '.');
                    }
                }
            }

            // Inject parent module context — helps queries about module purpose
            // find individual members even when they lack their own comment
            if (parent_comment) |pc| {
                if (pc.len > 0 and pc.len < 200) {
                    try buf.appendSlice(allocator, " Context: ");
                    try buf.appendSlice(allocator, pc);
                }
            }
        }

        return buf.toOwnedSlice(allocator);
    }

    // ── Embedding cache ────────────────────────────────────────────

    fn getCachedEmbedding(
        self: *Self,
        allocator: std.mem.Allocator,
        content_hash: []const u8,
    ) !?[]f32 {
        const sql = "SELECT embedding FROM embedding_cache WHERE content_hash = ?1";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return null;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, content_hash.ptr, @intCast(content_hash.len), SQLITE_STATIC);
        if (c.sqlite3_step(stmt) != c.SQLITE_ROW) return null;

        const raw = c.sqlite3_column_blob(stmt, 0);
        const len: usize = @intCast(c.sqlite3_column_bytes(stmt, 0));
        if (raw == null or len == 0) return null;

        const bytes: []const u8 = @as([*]const u8, @ptrCast(raw))[0..len];
        return try vector.bytesToVec(allocator, bytes);
    }

    fn cacheEmbedding(
        self: *Self,
        content_hash: []const u8,
        model_name: []const u8,
        embedding: []const f32,
    ) void {
        const bytes = vector.vecToBytes(self.allocator, embedding) catch return;
        defer self.allocator.free(bytes);

        const sql = "INSERT OR REPLACE INTO embedding_cache(content_hash, embedding, model) VALUES(?1,?2,?3)";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, content_hash.ptr, @intCast(content_hash.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_blob(stmt, 2, bytes.ptr, @intCast(bytes.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 3, model_name.ptr, @intCast(model_name.len), SQLITE_STATIC);
        _ = c.sqlite3_step(stmt);
    }

    /// Get embedding for text, using cache. Returns null-length slice for noop.
    /// Caller owns returned slice.
    fn getOrComputeEmbedding(
        self: *Self,
        allocator: std.mem.Allocator,
        text: []const u8,
    ) !?[]f32 {
        const model_name = self.embedder.getName();

        // Noop embedder: skip caching entirely
        if (std.mem.eql(u8, model_name, "none")) {
            return null;
        }

        const hash = vector.contentHashWithModel(text, model_name);
        const hash_str: []const u8 = &hash;

        // Cache hit?
        if (try self.getCachedEmbedding(allocator, hash_str)) |cached| {
            return cached;
        }

        // Call embedding API
        const emb = self.embedder.embed(allocator, text) catch |err| {
            log.warn("embedding failed: {s}", .{@errorName(err)});
            return null;
        };

        if (emb.len == 0) {
            allocator.free(emb);
            return null;
        }

        // Cache it
        self.cacheEmbedding(hash_str, model_name, emb);

        return emb;
    }

    // ── Insert helpers ─────────────────────────────────────────────

    fn insertModule(self: *Self, allocator: std.mem.Allocator, file_path: []const u8, doc: ParsedDoc, mtime: i64) !void {
        const name = doc.module_comment orelse doc.module;

        const emb_text = try buildEmbeddingText(allocator, doc.module, name, "module", doc.module_comment, null, null);
        defer allocator.free(emb_text);

        const emb = try self.getOrComputeEmbedding(allocator, emb_text);
        defer if (emb) |e| allocator.free(e);

        const emb_bytes: ?[]u8 = if (emb) |e| try vector.vecToBytes(allocator, e) else null;
        defer if (emb_bytes) |b| allocator.free(b);

        const model_name = self.embedder.getName();

        const sql =
            "INSERT INTO ast_nodes(" ++
            "  file_path, source, module, node_type, name, signature," ++
            "  comment, line, used_by, language, file_type, file_hash, last_modified," ++
            "  embedding, embedding_model" ++
            ") VALUES (?1,?2,?3,'module',?4,NULL,?5,NULL,?6,?7,'source',?8,?9,?10,?11)";

        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, file_path.ptr, @intCast(file_path.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 2, doc.source.ptr, @intCast(doc.source.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 3, doc.module.ptr, @intCast(doc.module.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 4, name.ptr, @intCast(name.len), SQLITE_STATIC);
        if (doc.module_comment) |cm|
            _ = c.sqlite3_bind_text(stmt, 5, cm.ptr, @intCast(cm.len), SQLITE_STATIC)
        else
            _ = c.sqlite3_bind_null(stmt, 5);

        const ub_json = try serializeUsedBy(self.allocator, doc.used_by);
        defer self.allocator.free(ub_json);
        if (ub_json.len > 2)
            _ = c.sqlite3_bind_text(stmt, 6, ub_json.ptr, @intCast(ub_json.len), SQLITE_STATIC)
        else
            _ = c.sqlite3_bind_null(stmt, 6);

        _ = c.sqlite3_bind_text(stmt, 7, doc.language.ptr, @intCast(doc.language.len), SQLITE_STATIC);
        if (doc.file_hash) |fh|
            _ = c.sqlite3_bind_text(stmt, 8, fh.ptr, @intCast(fh.len), SQLITE_STATIC)
        else
            _ = c.sqlite3_bind_null(stmt, 8);
        _ = c.sqlite3_bind_int64(stmt, 9, mtime);

        if (emb_bytes) |b|
            _ = c.sqlite3_bind_blob(stmt, 10, b.ptr, @intCast(b.len), SQLITE_STATIC)
        else
            _ = c.sqlite3_bind_null(stmt, 10);
        _ = c.sqlite3_bind_text(stmt, 11, model_name.ptr, @intCast(model_name.len), SQLITE_STATIC);

        if (c.sqlite3_step(stmt) != c.SQLITE_DONE) return error.StepFailed;
    }

    fn insertMember(self: *Self, allocator: std.mem.Allocator, file_path: []const u8, doc: ParsedDoc, m: ParsedMember, mtime: i64) !void {
        const emb_text = try buildEmbeddingText(allocator, doc.module, m.name, m.node_type, m.comment, m.signature, doc.module_comment);
        defer allocator.free(emb_text);

        const emb = try self.getOrComputeEmbedding(allocator, emb_text);
        defer if (emb) |e| allocator.free(e);

        const emb_bytes: ?[]u8 = if (emb) |e| try vector.vecToBytes(allocator, e) else null;
        defer if (emb_bytes) |b| allocator.free(b);

        const model_name = self.embedder.getName();

        const sql =
            "INSERT INTO ast_nodes(" ++
            "  file_path, source, module, node_type, name, signature," ++
            "  comment, line, used_by, language, file_type, file_hash, last_modified," ++
            "  embedding, embedding_model" ++
            ") VALUES (?1,?2,?3,?4,?5,?6,?7,?8,NULL,?9,'source',?10,?11,?12,?13)";

        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, file_path.ptr, @intCast(file_path.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 2, doc.source.ptr, @intCast(doc.source.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 3, doc.module.ptr, @intCast(doc.module.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 4, m.node_type.ptr, @intCast(m.node_type.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 5, m.name.ptr, @intCast(m.name.len), SQLITE_STATIC);
        if (m.signature) |sig|
            _ = c.sqlite3_bind_text(stmt, 6, sig.ptr, @intCast(sig.len), SQLITE_STATIC)
        else
            _ = c.sqlite3_bind_null(stmt, 6);
        if (m.comment) |cm|
            _ = c.sqlite3_bind_text(stmt, 7, cm.ptr, @intCast(cm.len), SQLITE_STATIC)
        else
            _ = c.sqlite3_bind_null(stmt, 7);
        if (m.line) |ln|
            _ = c.sqlite3_bind_int64(stmt, 8, @intCast(ln))
        else
            _ = c.sqlite3_bind_null(stmt, 8);
        _ = c.sqlite3_bind_text(stmt, 9, doc.language.ptr, @intCast(doc.language.len), SQLITE_STATIC);
        if (doc.file_hash) |fh|
            _ = c.sqlite3_bind_text(stmt, 10, fh.ptr, @intCast(fh.len), SQLITE_STATIC)
        else
            _ = c.sqlite3_bind_null(stmt, 10);
        _ = c.sqlite3_bind_int64(stmt, 11, mtime);

        if (emb_bytes) |b|
            _ = c.sqlite3_bind_blob(stmt, 12, b.ptr, @intCast(b.len), SQLITE_STATIC)
        else
            _ = c.sqlite3_bind_null(stmt, 12);
        _ = c.sqlite3_bind_text(stmt, 13, model_name.ptr, @intCast(model_name.len), SQLITE_STATIC);

        if (c.sqlite3_step(stmt) != c.SQLITE_DONE) return error.StepFailed;
    }

    // ── Search ─────────────────────────────────────────────────────

    pub const SearchResult = struct {
        file_path: []const u8,
        source: []const u8,
        module: []const u8,
        node_type: []const u8,
        name: []const u8,
        signature: ?[]const u8,
        comment: ?[]const u8,
        line: ?u32,
        used_by: [][]const u8,
        language: []const u8,
        score: f64,
    };

    pub fn freeSearchResult(allocator: std.mem.Allocator, r: SearchResult) void {
        allocator.free(r.file_path);
        allocator.free(r.source);
        allocator.free(r.module);
        allocator.free(r.node_type);
        allocator.free(r.name);
        if (r.signature) |s| allocator.free(s);
        if (r.comment) |cm| allocator.free(cm);
        for (r.used_by) |ub| allocator.free(ub);
        allocator.free(r.used_by);
        allocator.free(r.language);
    }

    /// Hybrid search: vector (cosine) + keyword (LIKE), fused by weighted score.
    ///
    /// When the embedder is noop (no model configured), falls back to keyword-only.
    /// When the query has no embedding, falls back to keyword-only.
    /// Results are reranked by node_type (boosts structs/functions, penalizes tests).
    /// Search with optional semantic alias expansion (alias → expanded tokens).
    pub fn searchWithAliases(
        self: *Self,
        allocator: std.mem.Allocator,
        query_text: []const u8,
        limit: usize,
        aliases: ?SemanticAliases,
    ) ![]SearchResult {
        if (aliases) |ali| {
            // Expand aliases: tokenise query, run alias expansion, rejoin.
            var tokens: std.ArrayList([]const u8) = .empty;
            defer tokens.deinit(allocator);
            var it = std.mem.tokenizeAny(u8, query_text, " \t\n\r");
            while (it.next()) |tok| try tokens.append(allocator, tok);

            const expanded = try ali.expandTokens(allocator, tokens.items);
            defer allocator.free(expanded);

            var expanded_query: std.ArrayList(u8) = .empty;
            defer expanded_query.deinit(allocator);
            for (expanded, 0..) |tok, idx| {
                if (idx > 0) try expanded_query.append(allocator, ' ');
                try expanded_query.appendSlice(allocator, tok);
            }
            return self.search(allocator, expanded_query.items, limit);
        }
        return self.search(allocator, query_text, limit);
    }

    pub fn search(
        self: *Self,
        allocator: std.mem.Allocator,
        query_text: []const u8,
        limit: usize,
    ) ![]SearchResult {
        const trimmed = std.mem.trim(u8, query_text, " \t\n\r");
        if (trimmed.len == 0) return allocator.alloc(SearchResult, 0);

        // Build query embedding
        const query_emb = try self.getOrComputeEmbedding(allocator, trimmed);
        defer if (query_emb) |e| allocator.free(e);

        const has_embeddings = query_emb != null and query_emb.?.len > 0;

        if (has_embeddings) {
            return self.hybridSearch(allocator, trimmed, query_emb.?, limit);
        } else {
            return self.keywordSearch(allocator, trimmed, limit);
        }
    }

    /// Vector-only cosine similarity search.
    /// Loads all stored embeddings (up to 2000), computes cosine similarity,
    /// returns top-k by score.
    pub fn vectorSearch(
        self: *Self,
        allocator: std.mem.Allocator,
        query_embedding: []const f32,
        limit: usize,
    ) ![]SearchResult {
        // idx_gdb_has_emb covers `embedding IS NOT NULL`.
        // Excluding test_decl saves ~10-15% of the scan and avoids polluting
        // results with test implementation details.
        const sql =
            "SELECT id, file_path, source, module, node_type, name, signature," ++
            "       comment, line, used_by, language, embedding " ++
            "FROM ast_nodes " ++
            "WHERE embedding IS NOT NULL AND node_type != 'test_decl' " ++
            "ORDER BY last_modified DESC " ++
            "LIMIT 2000";

        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) {
            return allocator.alloc(SearchResult, 0);
        }
        defer _ = c.sqlite3_finalize(stmt);

        // ScoredRow: holds id + score, then we resolve later
        const ScoredRow = struct {
            id: i64,
            score: f32,
        };
        var scored: std.ArrayList(ScoredRow) = .empty;
        defer scored.deinit(allocator);

        while (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            const id = c.sqlite3_column_int64(stmt, 0);
            const emb_raw = c.sqlite3_column_blob(stmt, 11);
            const emb_len: usize = @intCast(c.sqlite3_column_bytes(stmt, 11));
            if (emb_raw == null or emb_len == 0) continue;

            const emb_bytes: []const u8 = @as([*]const u8, @ptrCast(emb_raw))[0..emb_len];
            const emb = vector.bytesToVec(allocator, emb_bytes) catch continue;
            defer allocator.free(emb);

            const sim = vector.cosineSimilarity(query_embedding, emb);
            try scored.append(allocator, .{ .id = id, .score = sim });
        }

        // Sort by score descending
        std.mem.sortUnstable(ScoredRow, scored.items, {}, struct {
            fn lt(_: void, a: ScoredRow, b: ScoredRow) bool {
                return a.score > b.score;
            }
        }.lt);

        // Fetch full records for top results
        const fetch_count = @min(limit * 2, scored.items.len);
        var results: std.ArrayList(SearchResult) = .empty;
        errdefer {
            for (results.items) |r| freeSearchResult(allocator, r);
            results.deinit(allocator);
        }

        for (scored.items[0..fetch_count]) |sr| {
            const row = self.fetchById(allocator, sr.id) catch continue;
            var r = row;
            r.score = @floatCast(sr.score);
            try results.append(allocator, r);
        }

        rankByNodeType(results.items);

        const result_slice = try results.toOwnedSlice(allocator);
        const actual = @min(limit, result_slice.len);
        if (result_slice.len > actual) {
            for (result_slice[actual..]) |r| freeSearchResult(allocator, r);
        }
        return allocator.realloc(result_slice, actual) catch result_slice[0..actual];
    }

    /// Keyword search: multi-token LIKE over name, comment, module, signature.
    ///
    /// Splits the query on whitespace and applies each token as a separate LIKE
    /// predicate with OR semantics across all columns, scoring each row by how
    /// many tokens match.  This outperforms a single `LIKE '%multi word%'`
    /// pattern which only hits exact phrase occurrences.
    ///
    /// Example: query "database sync" → finds rows matching "database" OR "sync"
    /// then sorts by match count (rows matching both score higher via reranking).
    pub fn keywordSearch(
        self: *Self,
        allocator: std.mem.Allocator,
        query_text: []const u8,
        limit: usize,
    ) ![]SearchResult {
        // Collect non-trivial tokens
        var tokens: std.ArrayList([]const u8) = .empty;
        defer tokens.deinit(allocator);
        var tok_it = std.mem.tokenizeAny(u8, query_text, " \t\n\r_-");
        while (tok_it.next()) |tok| {
            if (tok.len < 2) continue;
            try tokens.append(allocator, tok);
        }
        if (tokens.items.len == 0) {
            // Fallback: treat whole query as single token
            try tokens.append(allocator, query_text);
        }

        // Build dynamic SQL: for each token, add a clause checking all text columns.
        // WHERE (name LIKE ?1 OR comment LIKE ?1 OR module LIKE ?1 OR sig LIKE ?1)
        //   AND (name LIKE ?2 OR comment LIKE ?2 OR module LIKE ?2 OR sig LIKE ?2) ...
        // For more than 1 token we use OR (any match) so short queries still find results.
        // We also compute a match_count expression for scoring.
        const cols = "name LIKE ? OR comment LIKE ? OR module LIKE ? OR signature LIKE ?";
        var where_buf: std.ArrayList(u8) = .empty;
        defer where_buf.deinit(allocator);

        // Score: sum of per-token hit counts
        var score_buf: std.ArrayList(u8) = .empty;
        defer score_buf.deinit(allocator);

        for (tokens.items, 0..) |_, i| {
            if (i > 0) {
                try where_buf.appendSlice(allocator, " OR ");
                try score_buf.appendSlice(allocator, " + ");
            }
            try where_buf.appendSlice(allocator, "(");
            try where_buf.appendSlice(allocator, cols);
            try where_buf.appendSlice(allocator, ")");
            // Score contribution for this token
            try score_buf.appendSlice(allocator, "(CASE WHEN name LIKE ? THEN 2 ELSE 0 END");
            try score_buf.appendSlice(allocator, " + CASE WHEN comment LIKE ? THEN 1 ELSE 0 END");
            try score_buf.appendSlice(allocator, " + CASE WHEN module LIKE ? THEN 1 ELSE 0 END)");
        }
        try where_buf.append(allocator, 0); // sentinel
        try score_buf.append(allocator, 0);

        var sql_buf: std.ArrayList(u8) = .empty;
        defer sql_buf.deinit(allocator);
        try sql_buf.appendSlice(allocator, "SELECT file_path, source, module, node_type, name, signature," ++
            "       comment, line, used_by, language, (" ++
            "");
        try sql_buf.appendSlice(allocator, score_buf.items[0 .. score_buf.items.len - 1]);
        try sql_buf.appendSlice(allocator, ") AS match_score " ++
            "FROM ast_nodes WHERE (");
        try sql_buf.appendSlice(allocator, where_buf.items[0 .. where_buf.items.len - 1]);
        try sql_buf.appendSlice(allocator, ") AND node_type != 'test_decl' " ++
            "ORDER BY match_score DESC, last_modified DESC LIMIT ?");
        try sql_buf.append(allocator, 0); // sentinel

        const sql_z: [:0]const u8 = sql_buf.items[0 .. sql_buf.items.len - 1 :0];

        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql_z.ptr, -1, &stmt, null) != c.SQLITE_OK) {
            return allocator.alloc(SearchResult, 0);
        }
        defer _ = c.sqlite3_finalize(stmt);

        // Bind parameters: 4 per token in WHERE + 3 per token in score + 1 limit
        var param_idx: c_int = 1;
        for (tokens.items) |tok| {
            const pattern = try std.fmt.allocPrint(allocator, "%{s}%", .{tok});
            defer allocator.free(pattern);
            // 4 bindings for WHERE (name, comment, module, signature)
            for (0..4) |_| {
                _ = c.sqlite3_bind_text(stmt, param_idx, pattern.ptr, @intCast(pattern.len), c.SQLITE_TRANSIENT);
                param_idx += 1;
            }
        }
        for (tokens.items) |tok| {
            const pattern = try std.fmt.allocPrint(allocator, "%{s}%", .{tok});
            defer allocator.free(pattern);
            // 3 bindings for score (name×2, comment, module)
            for (0..3) |_| {
                _ = c.sqlite3_bind_text(stmt, param_idx, pattern.ptr, @intCast(pattern.len), c.SQLITE_TRANSIENT);
                param_idx += 1;
            }
        }
        _ = c.sqlite3_bind_int64(stmt, param_idx, @intCast(limit * 2));

        var results: std.ArrayList(SearchResult) = .empty;
        errdefer {
            for (results.items) |r| freeSearchResult(allocator, r);
            results.deinit(allocator);
        }

        while (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            // Column 10 is match_score
            const raw_score: f64 = @floatFromInt(c.sqlite3_column_int(stmt, 10));
            var r = try readRowResult(stmt.?, allocator);
            r.score = @max(1.0, raw_score);
            try results.append(allocator, r);
        }

        rankByNodeType(results.items);

        const result_slice = try results.toOwnedSlice(allocator);
        const actual = @min(limit, result_slice.len);
        if (result_slice.len > actual) {
            for (result_slice[actual..]) |r| freeSearchResult(allocator, r);
        }
        return allocator.realloc(result_slice, actual) catch result_slice[0..actual];
    }

    /// Hybrid search: fuse vector and keyword results.
    fn hybridSearch(
        self: *Self,
        allocator: std.mem.Allocator,
        query_text: []const u8,
        query_embedding: []const f32,
        limit: usize,
    ) ![]SearchResult {
        // Run both searches at 2x limit for fusion
        const double_limit = limit * 2;

        // ── Vector results ──
        const vec_results = self.vectorSearch(allocator, query_embedding, double_limit) catch &[_]SearchResult{};
        defer {
            for (vec_results) |r| freeSearchResult(allocator, r);
            allocator.free(vec_results);
        }

        // ── Keyword results ──
        const kw_results = self.keywordSearch(allocator, query_text, double_limit) catch &[_]SearchResult{};
        defer {
            for (kw_results) |r| freeSearchResult(allocator, r);
            allocator.free(kw_results);
        }

        // Build IdScore arrays for merging (use row id)
        var vec_ids: std.ArrayList(vector.IdScore) = .empty;
        defer vec_ids.deinit(allocator);
        for (vec_results) |r| {
            // We need the actual row id — look it up via file_path + name + module
            const row_id = self.lookupId(r.file_path, r.module, r.name, r.node_type) catch continue;
            try vec_ids.append(allocator, .{ .id = row_id, .score = @floatCast(r.score) });
        }

        var kw_ids: std.ArrayList(vector.IdScore) = .empty;
        defer kw_ids.deinit(allocator);
        for (kw_results) |r| {
            const row_id = self.lookupId(r.file_path, r.module, r.name, r.node_type) catch continue;
            try kw_ids.append(allocator, .{ .id = row_id, .score = @floatCast(r.score) });
        }

        const merged = try vector.hybridMerge(
            allocator,
            vec_ids.items,
            kw_ids.items,
            0.65, // vector weight
            0.35, // keyword weight
            double_limit,
        );
        defer allocator.free(merged);

        // Fetch full rows in merged order
        var results: std.ArrayList(SearchResult) = .empty;
        errdefer {
            for (results.items) |r| freeSearchResult(allocator, r);
            results.deinit(allocator);
        }

        for (merged) |sr| {
            var row = self.fetchById(allocator, sr.id) catch continue;
            row.score = @floatCast(sr.final_score);
            try results.append(allocator, row);
        }

        rankByNodeType(results.items);

        const result_slice = try results.toOwnedSlice(allocator);
        const actual = @min(limit, result_slice.len);
        if (result_slice.len > actual) {
            for (result_slice[actual..]) |r| freeSearchResult(allocator, r);
        }
        return allocator.realloc(result_slice, actual) catch result_slice[0..actual];
    }

    fn lookupId(
        self: *Self,
        file_path: []const u8,
        module: []const u8,
        name: []const u8,
        node_type: []const u8,
    ) !i64 {
        const sql = "SELECT id FROM ast_nodes WHERE file_path=?1 AND module=?2 AND name=?3 AND node_type=?4 LIMIT 1";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, file_path.ptr, @intCast(file_path.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 2, module.ptr, @intCast(module.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 3, name.ptr, @intCast(name.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 4, node_type.ptr, @intCast(node_type.len), SQLITE_STATIC);

        if (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            return c.sqlite3_column_int64(stmt, 0);
        }
        return error.NotFound;
    }

    fn fetchById(self: *Self, allocator: std.mem.Allocator, id: i64) !SearchResult {
        const sql =
            "SELECT file_path, source, module, node_type, name, signature," ++
            "       comment, line, used_by, language " ++
            "FROM ast_nodes WHERE id = ?1";

        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_int64(stmt, 1, id);
        if (c.sqlite3_step(stmt) != c.SQLITE_ROW) return error.NotFound;

        return readRowResult(stmt.?, allocator);
    }

    // ── Node-type reranking ────────────────────────────────────────

    /// Adjust scores based on node_type and presence of documentation.
    ///
    /// Multipliers (applied in order):
    ///   - Definitions (struct/fn/enum): ×1.5 — high information density
    ///   - Methods: ×1.2 — useful but less discoverable than top-level fns
    ///   - Modules: ×0.8 — coarse-grained; specific members are more useful
    ///   - test_decl: ×0.3 — implementation detail; rarely the answer
    ///   - Has comment: ×1.15 — documented nodes produce better answers
    fn rankByNodeType(results: []SearchResult) void {
        for (results) |*r| {
            if (std.mem.eql(u8, r.node_type, "struct") or
                std.mem.eql(u8, r.node_type, "fn_decl") or
                std.mem.eql(u8, r.node_type, "enum") or
                std.mem.eql(u8, r.node_type, "const") or
                std.mem.eql(u8, r.node_type, "type"))
            {
                r.score *= 1.5;
            } else if (std.mem.eql(u8, r.node_type, "method") or
                std.mem.eql(u8, r.node_type, "method_private"))
            {
                r.score *= 1.2;
            } else if (std.mem.eql(u8, r.node_type, "test_decl")) {
                r.score *= 0.3;
            } else if (std.mem.eql(u8, r.node_type, "module")) {
                r.score *= 0.8;
            }
            // Boost documented nodes: they carry more semantic signal
            if (r.comment) |cm| {
                if (cm.len > 0) r.score *= 1.15;
            }
        }
        std.sort.block(SearchResult, results, {}, struct {
            fn lessThan(_: void, a: SearchResult, b: SearchResult) bool {
                return a.score > b.score;
            }
        }.lessThan);
    }

    // ── Row reading ────────────────────────────────────────────────

    /// Read columns 0..9: file_path, source, module, node_type, name, signature,
    /// comment, line, used_by, language.  Score is set to 0.0 (caller overrides).
    fn readRowResult(stmt: *c.sqlite3_stmt, allocator: std.mem.Allocator) !SearchResult {
        const line_type = c.sqlite3_column_type(stmt, 7);
        const line: ?u32 = if (line_type == c.SQLITE_NULL)
            null
        else
            @intCast(c.sqlite3_column_int(stmt, 7));

        const used_by = try parseUsedByCol(stmt, 8, allocator);

        return SearchResult{
            .file_path = try dupeCol(stmt, 0, allocator),
            .source = try dupeCol(stmt, 1, allocator),
            .module = try dupeCol(stmt, 2, allocator),
            .node_type = try dupeCol(stmt, 3, allocator),
            .name = try dupeCol(stmt, 4, allocator),
            .signature = try dupeColNullable(stmt, 5, allocator),
            .comment = try dupeColNullable(stmt, 6, allocator),
            .line = line,
            .used_by = used_by,
            .language = try dupeCol(stmt, 9, allocator),
            .score = 0.0,
        };
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
// JSON parsing — internal structure
// ---------------------------------------------------------------------------

const ParsedMember = struct {
    node_type: []const u8,
    name: []const u8,
    signature: ?[]const u8,
    comment: ?[]const u8,
    line: ?u32,
};

const ParsedDoc = struct {
    module: []const u8,
    source: []const u8,
    language: []const u8,
    module_comment: ?[]const u8,
    used_by: []const []const u8,
    file_hash: ?[]const u8,
    members: []ParsedMember,
};

fn parseGuidanceJson(arena: std.mem.Allocator, json_data: []const u8) !ParsedDoc {
    const Value = std.json.Value;
    const parsed = try std.json.parseFromSlice(Value, arena, json_data, .{
        .ignore_unknown_fields = true,
    });

    const root = parsed.value;
    if (root != .object) return error.InvalidJson;

    const meta_val = root.object.get("meta") orelse return error.MissingMeta;
    if (meta_val != .object) return error.MissingMeta;

    const module: []const u8 = blk: {
        const v = meta_val.object.get("module") orelse return error.MissingModule;
        if (v != .string) return error.MissingModule;
        break :blk v.string;
    };
    const source: []const u8 = blk: {
        const v = meta_val.object.get("source") orelse break :blk module;
        if (v != .string) break :blk module;
        break :blk v.string;
    };
    const language: []const u8 = blk: {
        const v = meta_val.object.get("language") orelse break :blk "zig";
        if (v != .string) break :blk "zig";
        break :blk v.string;
    };
    const module_comment: ?[]const u8 = blk: {
        const cv = root.object.get("comment") orelse break :blk null;
        if (cv != .string) break :blk null;
        break :blk cv.string;
    };

    var used_by_list: std.ArrayList([]const u8) = .{};
    if (root.object.get("used_by")) |ubv| {
        if (ubv == .array) {
            for (ubv.array.items) |item| {
                if (item == .string) try used_by_list.append(arena, item.string);
            }
        }
    }
    const file_hash: ?[]const u8 = blk: {
        const v = root.object.get("file_hash") orelse break :blk null;
        if (v != .string) break :blk null;
        break :blk v.string;
    };

    var members_list: std.ArrayList(ParsedMember) = .{};
    const members_val = root.object.get("members") orelse {
        return .{
            .module = module,
            .source = source,
            .language = language,
            .module_comment = module_comment,
            .used_by = try used_by_list.toOwnedSlice(arena),
            .file_hash = file_hash,
            .members = &.{},
        };
    };
    if (members_val != .array) {
        return .{
            .module = module,
            .source = source,
            .language = language,
            .module_comment = module_comment,
            .used_by = try used_by_list.toOwnedSlice(arena),
            .file_hash = file_hash,
            .members = &.{},
        };
    }

    for (members_val.array.items) |item| {
        if (item != .object) continue;
        try members_list.append(arena, parseMemberValue(item));
        if (item.object.get("members")) |nested_val| {
            if (nested_val == .array) {
                for (nested_val.array.items) |nested_item| {
                    if (nested_item != .object) continue;
                    try members_list.append(arena, parseMemberValue(nested_item));
                }
            }
        }
    }

    return .{
        .module = module,
        .source = source,
        .language = language,
        .module_comment = module_comment,
        .used_by = try used_by_list.toOwnedSlice(arena),
        .file_hash = file_hash,
        .members = try members_list.toOwnedSlice(arena),
    };
}

fn parseMemberValue(item: std.json.Value) ParsedMember {
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
        if (cv.string.len < 4) break :blk null;
        break :blk cv.string;
    };
    const line: ?u32 = blk: {
        const lv = item.object.get("line") orelse break :blk null;
        switch (lv) {
            .integer => |i| break :blk if (i >= 0) @intCast(i) else null,
            else => break :blk null,
        }
    };
    return .{
        .node_type = node_type,
        .name = name,
        .signature = signature,
        .comment = comment,
        .line = line,
    };
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn serializeUsedBy(allocator: std.mem.Allocator, items: []const []const u8) ![]u8 {
    var buf: std.ArrayList(u8) = .{};
    errdefer buf.deinit(allocator);
    try buf.append(allocator, '[');
    for (items, 0..) |item, i| {
        if (i > 0) try buf.appendSlice(allocator, ",");
        try buf.append(allocator, '"');
        for (item) |ch| {
            switch (ch) {
                '"' => try buf.appendSlice(allocator, "\\\""),
                '\\' => try buf.appendSlice(allocator, "\\\\"),
                else => try buf.append(allocator, ch),
            }
        }
        try buf.append(allocator, '"');
    }
    try buf.append(allocator, ']');
    return buf.toOwnedSlice(allocator);
}

fn parseUsedByCol(stmt: *c.sqlite3_stmt, col: c_int, allocator: std.mem.Allocator) ![][]const u8 {
    if (c.sqlite3_column_type(stmt, col) == c.SQLITE_NULL) return &.{};
    const raw = c.sqlite3_column_text(stmt, col);
    if (raw == null) return &.{};
    const len: usize = @intCast(c.sqlite3_column_bytes(stmt, col));
    const json_text = @as([*]const u8, @ptrCast(raw))[0..len];

    var parsed = std.json.parseFromSlice(std.json.Value, allocator, json_text, .{}) catch return &.{};
    defer parsed.deinit();

    if (parsed.value != .array) return &.{};
    var out: std.ArrayList([]const u8) = .{};
    errdefer {
        for (out.items) |s| allocator.free(s);
        out.deinit(allocator);
    }
    for (parsed.value.array.items) |item| {
        if (item != .string) continue;
        try out.append(allocator, try allocator.dupe(u8, item.string));
    }
    return try out.toOwnedSlice(allocator);
}

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

test "GuidanceDb init and schema" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    const db_path = try std.fmt.allocPrint(allocator, "{s}/test.guidance.db", .{tmp_path});
    defer allocator.free(db_path);

    var noop: vector.NoopEmbedding = .{};
    var db = try GuidanceDb.init(allocator, db_path, noop.provider());
    defer db.deinit();

    // Verify schema_version row was inserted.
    var stmt: ?*c.sqlite3_stmt = null;
    const rc = c.sqlite3_prepare_v2(db.db, "SELECT version FROM schema_version", -1, &stmt, null);
    try std.testing.expectEqual(@as(c_int, c.SQLITE_OK), rc);
    defer _ = c.sqlite3_finalize(stmt);
    try std.testing.expectEqual(@as(c_int, c.SQLITE_ROW), c.sqlite3_step(stmt));
    try std.testing.expectEqual(@as(c_int, 1), c.sqlite3_column_int(stmt, 0));
}

test "GuidanceDb index and keyword search round-trip" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    const db_path = try std.fmt.allocPrint(allocator, "{s}/test.guidance.db", .{tmp_path});
    defer allocator.free(db_path);

    try tmp.dir.makeDir("src");
    const json =
        \\{
        \\  "meta": { "module": "src.mymod", "source": "src/mymod.zig", "language": "zig" },
        \\  "comment": "The best module.",
        \\  "used_by": ["src/main.zig"],
        \\  "members": [
        \\    { "type": "fn_decl", "name": "frobnicate",
        \\      "signature": "fn frobnicate(x: u32) u32",
        \\      "comment": "Frobnicates the widget.", "line": 7 }
        \\  ]
        \\}
    ;
    try tmp.dir.writeFile(.{ .sub_path = "src/mymod.zig.json", .data = json });

    var noop: vector.NoopEmbedding = .{};
    var db = try GuidanceDb.init(allocator, db_path, noop.provider());
    defer db.deinit();

    const src_dir = try std.fmt.allocPrint(allocator, "{s}/src", .{tmp_path});
    defer allocator.free(src_dir);

    try db.syncFromDir(allocator, src_dir);

    // Keyword search by name
    const results = try db.keywordSearch(allocator, "frobnicate", 10);
    defer {
        for (results) |r| GuidanceDb.freeSearchResult(allocator, r);
        allocator.free(results);
    }

    try std.testing.expect(results.len >= 1);
    try std.testing.expectEqualStrings("frobnicate", results[0].name);
    try std.testing.expectEqualStrings("zig", results[0].language);
    try std.testing.expectEqualStrings("Frobnicates the widget.", results[0].comment.?);
    try std.testing.expectEqualStrings("fn frobnicate(x: u32) u32", results[0].signature.?);
    try std.testing.expectEqual(@as(?u32, 7), results[0].line);
}

test "GuidanceDb search falls back to keyword when noop embedder" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    const db_path = try std.fmt.allocPrint(allocator, "{s}/test.guidance.db", .{tmp_path});
    defer allocator.free(db_path);

    try tmp.dir.makeDir("src");
    const json =
        \\{
        \\  "meta": { "module": "src.alpha", "source": "src/alpha.zig", "language": "zig" },
        \\  "members": [
        \\    { "type": "struct", "name": "Widget", "comment": "A useful widget.", "line": 1 }
        \\  ]
        \\}
    ;
    try tmp.dir.writeFile(.{ .sub_path = "src/alpha.zig.json", .data = json });

    var noop: vector.NoopEmbedding = .{};
    var db = try GuidanceDb.init(allocator, db_path, noop.provider());
    defer db.deinit();

    const src_dir = try std.fmt.allocPrint(allocator, "{s}/src", .{tmp_path});
    defer allocator.free(src_dir);

    try db.syncFromDir(allocator, src_dir);

    const results = try db.search(allocator, "Widget", 10);
    defer {
        for (results) |r| GuidanceDb.freeSearchResult(allocator, r);
        allocator.free(results);
    }

    try std.testing.expect(results.len >= 1);
    try std.testing.expectEqualStrings("Widget", results[0].name);
}

test "GuidanceDb skips unchanged files" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    const db_path = try std.fmt.allocPrint(allocator, "{s}/test.guidance.db", .{tmp_path});
    defer allocator.free(db_path);

    try tmp.dir.makeDir("src");
    const json =
        \\{
        \\  "meta": { "module": "src.beta", "source": "src/beta.zig", "language": "zig" },
        \\  "members": []
        \\}
    ;
    try tmp.dir.writeFile(.{ .sub_path = "src/beta.zig.json", .data = json });

    var noop: vector.NoopEmbedding = .{};

    {
        var db = try GuidanceDb.init(allocator, db_path, noop.provider());
        defer db.deinit();
        const src_dir = try std.fmt.allocPrint(allocator, "{s}/src", .{tmp_path});
        defer allocator.free(src_dir);
        try db.syncFromDir(allocator, src_dir);
    }

    // Count rows after first sync
    {
        var db = try GuidanceDb.init(allocator, db_path, noop.provider());
        defer db.deinit();
        const src_dir = try std.fmt.allocPrint(allocator, "{s}/src", .{tmp_path});
        defer allocator.free(src_dir);
        // Second sync should not duplicate rows
        try db.syncFromDir(allocator, src_dir);

        var stmt: ?*c.sqlite3_stmt = null;
        _ = c.sqlite3_prepare_v2(db.db, "SELECT COUNT(*) FROM ast_nodes WHERE module='src.beta'", -1, &stmt, null);
        defer _ = c.sqlite3_finalize(stmt);
        _ = c.sqlite3_step(stmt);
        const count = c.sqlite3_column_int(stmt, 0);
        try std.testing.expectEqual(@as(c_int, 1), count); // only the module row
    }
}

test "buildEmbeddingText prose format" {
    const allocator = std.testing.allocator;
    // Member with comment, signature, and parent module context
    const text = try GuidanceDb.buildEmbeddingText(
        allocator,
        "src.guidance.db",
        "syncDatabase",
        "fn_decl",
        "Synchronises the SQLite database.",
        "fn syncDatabase(allocator: std.mem.Allocator, guidance_dir: []const u8) !void",
        "guidance database module.",
    );
    defer allocator.free(text);
    // Should be prose, not dotted-path
    try std.testing.expect(std.mem.indexOf(u8, text, "function syncDatabase") != null);
    try std.testing.expect(std.mem.indexOf(u8, text, "Synchronises the SQLite database") != null);
    // Parameter names extracted (not full types)
    try std.testing.expect(std.mem.indexOf(u8, text, "guidance_dir") != null);
    // No raw type annotations in params section
    try std.testing.expect(std.mem.indexOf(u8, text, "std.mem.Allocator") == null);
    // Parent context injected
    try std.testing.expect(std.mem.indexOf(u8, text, "guidance database") != null);
}

test "buildEmbeddingText module row" {
    const allocator = std.testing.allocator;
    const text = try GuidanceDb.buildEmbeddingText(
        allocator,
        "src.guidance.db",
        "guidance db",
        "module",
        "Produces .guidance.db for NullClaw.",
        null,
        null,
    );
    defer allocator.free(text);
    try std.testing.expect(std.mem.indexOf(u8, text, "module") != null);
    try std.testing.expect(std.mem.indexOf(u8, text, "Produces") != null);
}

test "moduleToProse strips src prefix and limits depth" {
    const allocator = std.testing.allocator;
    const prose = try GuidanceDb.moduleToProse(allocator, "src.guidance.vector.math");
    defer allocator.free(prose);
    // "src" stripped, last 3 parts: guidance vector math
    try std.testing.expectEqualStrings("guidance vector math", prose);
}

test "extractParamNames strips types" {
    const allocator = std.testing.allocator;
    const params = try GuidanceDb.extractParamNames(allocator, "fn foo(allocator: std.mem.Allocator, x: u32, y: []const u8) void");
    try std.testing.expect(params != null);
    defer allocator.free(params.?);
    // allocator is skipped, x and y remain
    try std.testing.expect(std.mem.indexOf(u8, params.?, "x") != null);
    try std.testing.expect(std.mem.indexOf(u8, params.?, "y") != null);
    try std.testing.expect(std.mem.indexOf(u8, params.?, "allocator") == null);
}
