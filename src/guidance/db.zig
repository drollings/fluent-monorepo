//! guidance SQLite FTS5 indexer.
//!
//! Produces `.explain.db` — a BM25-searchable SQLite database consumed by
//! NullClaw's `explain` tool.
//!
//! Public API:
//!   pub fn syncDatabase(allocator, guidance_dir, db_path) !void
//!   pub fn openDb(allocator, db_path) !ExplainDb
//!
//! Schema (version 1):
//!   schema_version     — single-row version table
//!   ast_nodes          — relational table (all metadata + mtime)
//!   fts_search         — FTS5 virtual table (external-content over ast_nodes)
//!   Triggers           — keep fts_search in sync with ast_nodes

const std = @import("std");
const log = std.log.scoped(.explain_db);

pub const c = @cImport({
    @cInclude("sqlite3.h");
});

pub const SQLITE_STATIC: c.sqlite3_destructor_type = null;
const BUSY_TIMEOUT_MS: c_int = 5000;

/// Schema version.  Bump when the table layout changes incompatibly.
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

        // Track lowercase versions to deduplicate (owned by this function)
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

            // Check for aliases
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

/// Open (or create) `.explain.db` at `db_path` and synchronise it with every
/// JSON file under `<guidance_dir>/src/`.  Uses per-file ArenaAllocators to
/// bound peak memory usage.
pub fn syncDatabase(
    allocator: std.mem.Allocator,
    guidance_dir: []const u8,
    db_path: []const u8,
) !void {
    var db = try ExplainDb.init(allocator, db_path);
    defer db.deinit();

    const src_dir_path = try std.fmt.allocPrint(allocator, "{s}/src", .{guidance_dir});
    defer allocator.free(src_dir_path);

    try db.syncFromDir(allocator, src_dir_path);
}

// ---------------------------------------------------------------------------
// ExplainDb — the database handle
// ---------------------------------------------------------------------------

pub const ExplainDb = struct {
    db: ?*c.sqlite3,
    allocator: std.mem.Allocator,

    const Self = @This();

    /// Open (or create) the database at `db_path`, running schema migrations
    /// as needed.
    pub fn init(allocator: std.mem.Allocator, db_path: []const u8) !Self {
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
            \\-- Schema version table
            \\CREATE TABLE IF NOT EXISTS schema_version (
            \\  version INTEGER PRIMARY KEY
            \\);
            \\
            \\-- Relational table: one row per indexed node (module or member)
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
            \\CREATE INDEX IF NOT EXISTS idx_ast_file  ON ast_nodes(file_path);
            \\CREATE INDEX IF NOT EXISTS idx_ast_mtime ON ast_nodes(file_path, last_modified);
            \\CREATE INDEX IF NOT EXISTS idx_ast_lang  ON ast_nodes(language);
            \\
            \\-- FTS5 virtual table — external-content backed by ast_nodes
            \\-- Searchable columns: name, comment, module, signature
            \\CREATE VIRTUAL TABLE IF NOT EXISTS fts_search USING fts5(
            \\  name,
            \\  comment,
            \\  module,
            \\  signature,
            \\  content='ast_nodes',
            \\  content_rowid='id'
            \\);
            \\
            \\-- Triggers: keep fts_search in sync with ast_nodes
            \\CREATE TRIGGER IF NOT EXISTS ast_nodes_ai
            \\  AFTER INSERT ON ast_nodes BEGIN
            \\    INSERT INTO fts_search(rowid, name, comment, module, signature)
            \\    VALUES (
            \\      new.id,
            \\      new.name,
            \\      COALESCE(new.comment, ''),
            \\      new.module,
            \\      COALESCE(new.signature, '')
            \\    );
            \\  END;
            \\
            \\CREATE TRIGGER IF NOT EXISTS ast_nodes_ad
            \\  AFTER DELETE ON ast_nodes BEGIN
            \\    INSERT INTO fts_search(fts_search, rowid, name, comment, module, signature)
            \\    VALUES (
            \\      'delete', old.id,
            \\      old.name,
            \\      COALESCE(old.comment, ''),
            \\      old.module,
            \\      COALESCE(old.signature, '')
            \\    );
            \\  END;
            \\
            \\CREATE TRIGGER IF NOT EXISTS ast_nodes_au
            \\  AFTER UPDATE ON ast_nodes BEGIN
            \\    INSERT INTO fts_search(fts_search, rowid, name, comment, module, signature)
            \\    VALUES (
            \\      'delete', old.id,
            \\      old.name,
            \\      COALESCE(old.comment, ''),
            \\      old.module,
            \\      COALESCE(old.signature, '')
            \\    );
            \\    INSERT INTO fts_search(rowid, name, comment, module, signature)
            \\    VALUES (
            \\      new.id,
            \\      new.name,
            \\      COALESCE(new.comment, ''),
            \\      new.module,
            \\      COALESCE(new.signature, '')
            \\    );
            \\  END;
        ;
        var err_msg: [*c]u8 = null;
        const rc = c.sqlite3_exec(self.db, sql, null, null, &err_msg);
        if (rc != c.SQLITE_OK) {
            logSqliteErr("migrate", "CREATE TABLE/FTS/triggers", rc, err_msg, self.db);
            if (err_msg) |msg| c.sqlite3_free(msg);
            return error.MigrationFailed;
        }

        // Add 'source' column to existing databases that pre-date this schema change.
        // SQLite returns SQLITE_ERROR ("duplicate column name") if the column already exists;
        // we intentionally ignore that specific error.
        {
            var alter_err: [*c]u8 = null;
            _ = c.sqlite3_exec(
                self.db,
                "ALTER TABLE ast_nodes ADD COLUMN source TEXT",
                null,
                null,
                &alter_err,
            );
            if (alter_err) |msg| c.sqlite3_free(msg);
        }

        // Upsert schema version row.
        const ver_sql = "INSERT OR REPLACE INTO schema_version(version) VALUES(?1)";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, ver_sql, -1, &stmt, null) == c.SQLITE_OK) {
            _ = c.sqlite3_bind_int64(stmt, 1, SCHEMA_VERSION);
            _ = c.sqlite3_step(stmt);
            _ = c.sqlite3_finalize(stmt);
        }
    }

    // ── Sync ───────────────────────────────────────────────────────

    /// Walk `src_dir_path` (`.guidance/src`) and upsert stale JSON files.
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
        try self.insertModule(file_path, parsed, mtime);
        for (parsed.members) |m| {
            try self.insertMember(file_path, parsed, m, mtime);
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

    fn insertModule(self: *Self, file_path: []const u8, doc: ParsedDoc, mtime: i64) !void {
        const sql =
            "INSERT INTO ast_nodes(" ++
            "  file_path, source, module, node_type, name, signature," ++
            "  comment, line, used_by, language, file_type, file_hash, last_modified" ++
            ") VALUES (?1,?2,?3,'module',?4,NULL,?5,NULL,?6,?7,'source',?8,?9)";

        var stmt: ?*c.sqlite3_stmt = null;
        const rc = c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null);
        if (rc != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        // ?1 file_path
        _ = c.sqlite3_bind_text(stmt, 1, file_path.ptr, @intCast(file_path.len), SQLITE_STATIC);
        // ?2 source (meta.source — relative source file path)
        _ = c.sqlite3_bind_text(stmt, 2, doc.source.ptr, @intCast(doc.source.len), SQLITE_STATIC);
        // ?3 module
        _ = c.sqlite3_bind_text(stmt, 3, doc.module.ptr, @intCast(doc.module.len), SQLITE_STATIC);
        // ?4 name (module comment or module name)
        const name = doc.module_comment orelse doc.module;
        _ = c.sqlite3_bind_text(stmt, 4, name.ptr, @intCast(name.len), SQLITE_STATIC);
        // ?5 comment
        if (doc.module_comment) |cm| {
            _ = c.sqlite3_bind_text(stmt, 5, cm.ptr, @intCast(cm.len), SQLITE_STATIC);
        } else {
            _ = c.sqlite3_bind_null(stmt, 5);
        }
        // ?6 used_by (JSON array)
        const ub_json = try serializeUsedBy(self.allocator, doc.used_by);
        defer self.allocator.free(ub_json);
        if (ub_json.len > 2) { // more than just "[]"
            _ = c.sqlite3_bind_text(stmt, 6, ub_json.ptr, @intCast(ub_json.len), SQLITE_STATIC);
        } else {
            _ = c.sqlite3_bind_null(stmt, 6);
        }
        // ?7 language
        _ = c.sqlite3_bind_text(stmt, 7, doc.language.ptr, @intCast(doc.language.len), SQLITE_STATIC);
        // ?8 file_hash
        if (doc.file_hash) |fh| {
            _ = c.sqlite3_bind_text(stmt, 8, fh.ptr, @intCast(fh.len), SQLITE_STATIC);
        } else {
            _ = c.sqlite3_bind_null(stmt, 8);
        }
        // ?9 last_modified
        _ = c.sqlite3_bind_int64(stmt, 9, mtime);

        if (c.sqlite3_step(stmt) != c.SQLITE_DONE) return error.StepFailed;
    }

    fn insertMember(self: *Self, file_path: []const u8, doc: ParsedDoc, m: ParsedMember, mtime: i64) !void {
        const sql =
            "INSERT INTO ast_nodes(" ++
            "  file_path, source, module, node_type, name, signature," ++
            "  comment, line, used_by, language, file_type, file_hash, last_modified" ++
            ") VALUES (?1,?2,?3,?4,?5,?6,?7,?8,NULL,?9,'source',?10,?11)";

        var stmt: ?*c.sqlite3_stmt = null;
        const rc = c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null);
        if (rc != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        // ?1 file_path
        _ = c.sqlite3_bind_text(stmt, 1, file_path.ptr, @intCast(file_path.len), SQLITE_STATIC);
        // ?2 source (meta.source — relative source file path)
        _ = c.sqlite3_bind_text(stmt, 2, doc.source.ptr, @intCast(doc.source.len), SQLITE_STATIC);
        // ?3 module
        _ = c.sqlite3_bind_text(stmt, 3, doc.module.ptr, @intCast(doc.module.len), SQLITE_STATIC);
        // ?4 node_type
        _ = c.sqlite3_bind_text(stmt, 4, m.node_type.ptr, @intCast(m.node_type.len), SQLITE_STATIC);
        // ?5 name
        _ = c.sqlite3_bind_text(stmt, 5, m.name.ptr, @intCast(m.name.len), SQLITE_STATIC);
        // ?6 signature
        if (m.signature) |sig| {
            _ = c.sqlite3_bind_text(stmt, 6, sig.ptr, @intCast(sig.len), SQLITE_STATIC);
        } else {
            _ = c.sqlite3_bind_null(stmt, 6);
        }
        // ?7 comment
        if (m.comment) |cm| {
            _ = c.sqlite3_bind_text(stmt, 7, cm.ptr, @intCast(cm.len), SQLITE_STATIC);
        } else {
            _ = c.sqlite3_bind_null(stmt, 7);
        }
        // ?8 line
        if (m.line) |ln| {
            _ = c.sqlite3_bind_int64(stmt, 8, @intCast(ln));
        } else {
            _ = c.sqlite3_bind_null(stmt, 8);
        }
        // ?9 language
        _ = c.sqlite3_bind_text(stmt, 9, doc.language.ptr, @intCast(doc.language.len), SQLITE_STATIC);
        // ?10 file_hash
        if (doc.file_hash) |fh| {
            _ = c.sqlite3_bind_text(stmt, 10, fh.ptr, @intCast(fh.len), SQLITE_STATIC);
        } else {
            _ = c.sqlite3_bind_null(stmt, 10);
        }
        // ?11 last_modified
        _ = c.sqlite3_bind_int64(stmt, 11, mtime);

        if (c.sqlite3_step(stmt) != c.SQLITE_DONE) return error.StepFailed;
    }

    /// Run FTS optimize pass to merge segment files.
    pub fn optimize(self: *Self) void {
        var err_msg: [*c]u8 = null;
        const rc = c.sqlite3_exec(
            self.db,
            "INSERT INTO fts_search(fts_search) VALUES('optimize')",
            null,
            null,
            &err_msg,
        );
        if (err_msg) |msg| c.sqlite3_free(msg);
        if (rc != c.SQLITE_OK) log.warn("fts optimize failed: rc={d}", .{rc});
    }

    // ── Search ─────────────────────────────────────────────────────

    pub const SearchResult = struct {
        file_path: []const u8,
        source: []const u8, // meta.source — relative path to the actual source file
        module: []const u8,
        node_type: []const u8,
        name: []const u8,
        signature: ?[]const u8,
        comment: ?[]const u8,
        line: ?u32,
        used_by: [][]const u8, // owned slice; each element owned
        language: []const u8,
        score: f64,
    };

    /// Return true when `word` is a common English stop word that adds noise to
    /// FTS5 queries.  Case-insensitive; only checks short words (≤ 6 chars).
    fn isStopWord(word: []const u8) bool {
        if (word.len > 6) return false;
        // Stack-allocate lowercase copy (word is ≤ 6 bytes).
        var buf: [6]u8 = undefined;
        const lower = std.ascii.lowerString(buf[0..word.len], word);
        const stops = [_][]const u8{
            "a",    "an",   "and",  "are",  "as",   "at",   "be",   "by",
            "do",   "for",  "get",  "has",  "how",  "i",    "if",   "in",
            "is",   "it",   "its",  "no",   "not",  "of",   "on",   "or",
            "our",  "out",  "so",   "the",  "to",   "use",  "used", "was",
            "what", "when", "with", "do",   "from", "this", "that", "we",
            "can",  "did",  "does", "does", "you",  "will", "why",  "any",
        };
        for (stops) |s| if (std.mem.eql(u8, lower, s)) return true;
        return false;
    }

    /// BM25 full-text search.  Returns results ordered best-first (score desc).
    /// Caller must free each `SearchResult` field and the slice with `allocator`.
    /// If `aliases` is provided, expands query tokens using semantic aliases.
    pub fn search(
        self: *Self,
        allocator: std.mem.Allocator,
        query_text: []const u8,
        limit: usize,
    ) ![]SearchResult {
        return searchWithAliases(self, allocator, query_text, limit, null);
    }

    /// Search with optional semantic alias expansion.
    pub fn searchWithAliases(
        self: *Self,
        allocator: std.mem.Allocator,
        query_text: []const u8,
        limit: usize,
        aliases: ?SemanticAliases,
    ) ![]SearchResult {
        const trimmed = std.mem.trim(u8, query_text, " \t\n\r");
        if (trimmed.len == 0) return allocator.alloc(SearchResult, 0);

        // Build quoted-OR FTS5 query from whitespace-separated tokens.
        // Strip English stop words so natural-language queries ("how does X work")
        // don't poison BM25 scores by matching on "how", "does", "are", etc.
        var tokens: std.ArrayList([]const u8) = .{};
        defer {
            for (tokens.items) |t| allocator.free(t);
            tokens.deinit(allocator);
        }
        var it = std.mem.tokenizeAny(u8, trimmed, " \t\n\r");
        while (it.next()) |word| {
            // Strip trailing punctuation (e.g. "processed?" → "processed").
            var clean = word;
            while (clean.len > 0) {
                const last = clean[clean.len - 1];
                if (last == '?' or last == '.' or last == ',' or last == '!' or last == ':') {
                    clean = clean[0 .. clean.len - 1];
                } else break;
            }
            if (clean.len == 0) continue;
            if (isStopWord(clean)) continue;
            try tokens.append(allocator, try allocator.dupe(u8, clean));
        }

        // Expand tokens with semantic aliases if available.
        const expanded_tokens: []const []const u8 = if (aliases) |a| blk: {
            const exp = a.expandTokens(allocator, tokens.items) catch break :blk tokens.items;
            // Free original tokens since we have expanded ones
            for (tokens.items) |t| allocator.free(t);
            tokens.clearAndFree(allocator);
            break :blk exp;
        } else tokens.items;

        // Build FTS query.
        var fts_buf: std.ArrayList(u8) = .{};
        defer fts_buf.deinit(allocator);
        var first = true;
        for (expanded_tokens) |tok| {
            if (!first) try fts_buf.appendSlice(allocator, " OR ");
            try fts_buf.append(allocator, '"');
            for (tok) |ch| {
                if (ch == '"') try fts_buf.appendSlice(allocator, "\"\"") else try fts_buf.append(allocator, ch);
            }
            try fts_buf.append(allocator, '"');
            first = false;
        }
        if (fts_buf.items.len == 0) {
            try fts_buf.appendSlice(allocator, trimmed);
        }
        try fts_buf.append(allocator, 0);

        const sql =
            "SELECT n.file_path, n.source, n.module, n.node_type, n.name, n.signature," ++
            "       n.comment, n.line, n.used_by, n.language," ++
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
        _ = c.sqlite3_bind_int64(stmt, 2, @intCast(limit * 2)); // Fetch extra forreranking

        var results: std.ArrayList(SearchResult) = .{};
        errdefer {
            for (results.items) |r| freeSearchResult(allocator, r);
            results.deinit(allocator);
        }

        while (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            const result = try readSearchResult(stmt.?, allocator);
            try results.append(allocator, result);
        }

        // Rerank by node type: boost structs, functions; penalize tests.
        rankByNodeType(results.items);

        // Free expanded tokens if we allocated them (aliases case).
        // Note: original tokens are freed by the defer block above.
        if (aliases != null and expanded_tokens.ptr != tokens.items.ptr) {
            for (expanded_tokens) |t| allocator.free(t);
            allocator.free(@constCast(expanded_tokens));
        }

        // Return at most `limit` results.
        const result_slice = try results.toOwnedSlice(allocator);
        if (result_slice.len <= limit) return result_slice;
        for (result_slice[limit..]) |r| freeSearchResult(allocator, r);
        @memset(result_slice[limit..], result_slice[0]);
        const final = allocator.realloc(result_slice, limit) catch result_slice[0..limit];
        return @constCast(final);
    }

    /// Adjust scores based on node_type: boost definitions, penalize tests.
    fn rankByNodeType(results: []SearchResult) void {
        for (results) |*r| {
            // Strong boost for struct/function/type definitions
            if (std.mem.eql(u8, r.node_type, "struct") or
                std.mem.eql(u8, r.node_type, "fn_decl") or
                std.mem.eql(u8, r.node_type, "enum") or
                std.mem.eql(u8, r.node_type, "const") or
                std.mem.eql(u8, r.node_type, "type"))
            {
                r.score *= 1.5;
            }
            // Moderate boost for method definitions
            else if (std.mem.eql(u8, r.node_type, "method") or
                std.mem.eql(u8, r.node_type, "method_private"))
            {
                r.score *= 1.2;
            }
            // Penalize test declarations
            else if (std.mem.eql(u8, r.node_type, "test_decl")) {
                r.score *= 0.3;
            }
            // Slight penalty for module-level (less specific)
            else if (std.mem.eql(u8, r.node_type, "module")) {
                r.score *= 0.8;
            }
        }
        // Re-sort by adjusted score (descending)
        std.sort.block(SearchResult, results, {}, struct {
            fn lessThan(_: void, a: SearchResult, b: SearchResult) bool {
                return a.score > b.score;
            }
        }.lessThan);
    }

    fn readSearchResult(stmt: *c.sqlite3_stmt, allocator: std.mem.Allocator) !SearchResult {
        // col 7: line
        const line_type = c.sqlite3_column_type(stmt, 7);
        const line: ?u32 = if (line_type == c.SQLITE_NULL)
            null
        else
            @intCast(c.sqlite3_column_int(stmt, 7));

        // col 8: used_by — stored as JSON array ["a","b"] or NULL
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
            .score = -c.sqlite3_column_double(stmt, 10), // BM25 negative → positive
        };
    }

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

/// Parse a GuidanceDoc JSON blob, extracting all fields needed for the DB.
/// All returned strings are slices into the arena-owned JSON parse tree.
fn parseGuidanceJson(arena: std.mem.Allocator, json_data: []const u8) !ParsedDoc {
    const Value = std.json.Value;
    const parsed = try std.json.parseFromSlice(Value, arena, json_data, .{
        .ignore_unknown_fields = true,
    });

    const root = parsed.value;
    if (root != .object) return error.InvalidJson;

    // ── meta ──────────────────────────────────────────────────────
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

    // ── top-level comment ─────────────────────────────────────────
    const module_comment: ?[]const u8 = blk: {
        const cv = root.object.get("comment") orelse break :blk null;
        if (cv != .string) break :blk null;
        break :blk cv.string;
    };

    // ── used_by ───────────────────────────────────────────────────
    var used_by_list: std.ArrayList([]const u8) = .{};
    if (root.object.get("used_by")) |ubv| {
        if (ubv == .array) {
            for (ubv.array.items) |item| {
                if (item == .string) try used_by_list.append(arena, item.string);
            }
        }
    }

    // ── file_hash (optional) ──────────────────────────────────────
    const file_hash: ?[]const u8 = blk: {
        const v = root.object.get("file_hash") orelse break :blk null;
        if (v != .string) break :blk null;
        break :blk v.string;
    };

    // ── members ───────────────────────────────────────────────────
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
        const m = parseMemberValue(item);
        try members_list.append(arena, m);
        // Recurse into nested members (e.g. methods inside a struct).
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
        if (cv.string.len < 4) break :blk null; // skip trivial stubs
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
// Helpers
// ---------------------------------------------------------------------------

/// Serialize a slice of strings into a JSON array: ["a","b",...].
/// Caller owns the returned allocation.
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

/// Parse a JSON-array column (e.g. `["a","b"]`) into an owned slice of owned strings.
/// Returns an empty slice when the column is NULL or not a JSON array.
fn parseUsedByCol(stmt: *c.sqlite3_stmt, col: c_int, allocator: std.mem.Allocator) ![][]const u8 {
    if (c.sqlite3_column_type(stmt, col) == c.SQLITE_NULL) return &.{};
    const raw = c.sqlite3_column_text(stmt, col);
    if (raw == null) return &.{};
    const len: usize = @intCast(c.sqlite3_column_bytes(stmt, col));
    const json_text = @as([*]const u8, @ptrCast(raw))[0..len];

    // Parse JSON array — tolerate errors gracefully.
    var parsed = std.json.parseFromSlice(std.json.Value, allocator, json_text, .{}) catch return &.{};
    defer parsed.deinit();

    if (parsed.value != .array) return &.{};
    const arr = parsed.value.array.items;
    var out: std.ArrayList([]const u8) = .{};
    errdefer {
        for (out.items) |s| allocator.free(s);
        out.deinit(allocator);
    }
    for (arr) |item| {
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

test "db init and schema_version" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    const db_path = try std.fmt.allocPrint(allocator, "{s}/test.explain.db", .{tmp_path});
    defer allocator.free(db_path);

    var db = try ExplainDb.init(allocator, db_path);
    defer db.deinit();

    // Verify schema_version row was inserted.
    var stmt: ?*c.sqlite3_stmt = null;
    const rc = c.sqlite3_prepare_v2(db.db, "SELECT version FROM schema_version", -1, &stmt, null);
    try std.testing.expectEqual(@as(c_int, c.SQLITE_OK), rc);
    defer _ = c.sqlite3_finalize(stmt);
    try std.testing.expectEqual(@as(c_int, c.SQLITE_ROW), c.sqlite3_step(stmt));
    const ver = c.sqlite3_column_int(stmt, 0);
    try std.testing.expectEqual(@as(c_int, 1), ver);
}

test "parseGuidanceJson extracts all fields" {
    const json =
        \\{
        \\  "meta": { "module": "src.foo.bar", "source": "src/foo/bar.zig", "language": "zig" },
        \\  "comment": "Does something useful.",
        \\  "used_by": ["src/main.zig", "src/other.zig"],
        \\  "members": [
        \\    { "type": "fn_decl", "name": "doThing", "signature": "fn doThing() void",
        \\      "comment": "Does the thing.", "line": 42 },
        \\    { "type": "struct",  "name": "MyState", "is_pub": true, "line": 10 }
        \\  ]
        \\}
    ;
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const doc = try parseGuidanceJson(arena.allocator(), json);
    try std.testing.expectEqualStrings("src.foo.bar", doc.module);
    try std.testing.expectEqualStrings("src/foo/bar.zig", doc.source);
    try std.testing.expectEqualStrings("zig", doc.language);
    try std.testing.expectEqualStrings("Does something useful.", doc.module_comment.?);
    try std.testing.expectEqual(@as(usize, 2), doc.used_by.len);
    try std.testing.expectEqualStrings("src/main.zig", doc.used_by[0]);
    try std.testing.expectEqual(@as(usize, 2), doc.members.len);
    try std.testing.expectEqualStrings("doThing", doc.members[0].name);
    try std.testing.expectEqualStrings("fn_decl", doc.members[0].node_type);
    try std.testing.expectEqualStrings("Does the thing.", doc.members[0].comment.?);
    try std.testing.expectEqualStrings("fn doThing() void", doc.members[0].signature.?);
    try std.testing.expectEqual(@as(?u32, 42), doc.members[0].line);
    try std.testing.expectEqual(@as(?u32, 10), doc.members[1].line);
}

test "full index + search round-trip with new schema" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    const db_path = try std.fmt.allocPrint(allocator, "{s}/test.explain.db", .{tmp_path});
    defer allocator.free(db_path);

    // Create a fake src/ dir with one JSON guidance file.
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

    var db = try ExplainDb.init(allocator, db_path);
    defer db.deinit();

    const src_dir = try std.fmt.allocPrint(allocator, "{s}/src", .{tmp_path});
    defer allocator.free(src_dir);

    try db.syncFromDir(allocator, src_dir);
    db.optimize();

    // Search by name.
    const results = try db.search(allocator, "frobnicate", 10);
    defer {
        for (results) |r| ExplainDb.freeSearchResult(allocator, r);
        allocator.free(results);
    }

    try std.testing.expect(results.len >= 1);
    try std.testing.expectEqualStrings("frobnicate", results[0].name);
    try std.testing.expectEqualStrings("zig", results[0].language);
    try std.testing.expectEqualStrings("Frobnicates the widget.", results[0].comment.?);
    try std.testing.expectEqualStrings("fn frobnicate(x: u32) u32", results[0].signature.?);
    try std.testing.expectEqual(@as(?u32, 7), results[0].line);

    // Also confirm comment text is searchable.
    const comment_results = try db.search(allocator, "widget", 10);
    defer {
        for (comment_results) |r| ExplainDb.freeSearchResult(allocator, r);
        allocator.free(comment_results);
    }
    try std.testing.expect(comment_results.len >= 1);
}

test "serializeUsedBy produces valid JSON array" {
    const allocator = std.testing.allocator;
    const items = [_][]const u8{ "src/a.zig", "src/b.zig" };
    const result = try serializeUsedBy(allocator, &items);
    defer allocator.free(result);
    try std.testing.expectEqualStrings("[\"src/a.zig\",\"src/b.zig\"]", result);
}
