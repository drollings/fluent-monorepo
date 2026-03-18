//! guidance LanceDB-style vector search database.
//!
//! Produces `.guidance.db` — a SQLite database that mirrors the `.explain.db`
//! schema but adds a vector embedding column for semantic (cosine) search.
//!
//! This is a **parallel** implementation to db.zig (FTS5).  Both read the same
//! JSON guidance files under `.guidance/src/`; NullClaw's explain tool can point
//! to either database.  Once guidance.db search quality matches explain.db, it
//! can fully replace it.
//!
//! Public API:
//!   pub fn syncDatabase(allocator, guidance_dir, db_path, embedder) !void
//!   pub fn openDb(allocator, db_path, embedder) !GuidanceDb
//!
//! Schema (version 1):
//!   schema_version   — single-row version table
//!   ast_nodes        — relational table: same columns as explain.db + embedding BLOB
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
// Public entry points
// ---------------------------------------------------------------------------

/// Open (or create) `db_path`, synchronise it with every JSON file under
/// `<guidance_dir>/src/`, embedding each node via `embedder`.
pub fn syncDatabase(
    allocator: std.mem.Allocator,
    guidance_dir: []const u8,
    db_path: []const u8,
    embedder: vector.EmbeddingProvider,
) !void {
    var db = try GuidanceDb.init(allocator, db_path, embedder);
    defer db.deinit();

    const src_dir_path = try std.fmt.allocPrint(allocator, "{s}/src", .{guidance_dir});
    defer allocator.free(src_dir_path);

    try db.syncFromDir(allocator, src_dir_path);
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
        const sql =
            \\-- Schema version table
            \\CREATE TABLE IF NOT EXISTS schema_version (
            \\  version INTEGER PRIMARY KEY
            \\);
            \\
            \\-- Main node table: one row per indexed node (module or member).
            \\-- Mirrors explain.db ast_nodes schema + embedding BLOB for vector search.
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
            \\  last_modified INTEGER NOT NULL,
            \\  embedding     BLOB,
            \\  embedding_model TEXT
            \\);
            \\CREATE INDEX IF NOT EXISTS idx_gdb_file      ON ast_nodes(file_path);
            \\CREATE INDEX IF NOT EXISTS idx_gdb_mtime     ON ast_nodes(file_path, last_modified);
            \\CREATE INDEX IF NOT EXISTS idx_gdb_lang      ON ast_nodes(language);
            \\CREATE INDEX IF NOT EXISTS idx_gdb_module    ON ast_nodes(module);
            \\CREATE INDEX IF NOT EXISTS idx_gdb_node_type ON ast_nodes(node_type);
            \\CREATE INDEX IF NOT EXISTS idx_gdb_has_emb   ON ast_nodes(id) WHERE embedding IS NOT NULL;
            \\
            \\-- Embedding cache: content_hash → embedding blob.
            \\-- Prevents redundant embedding API calls when nodes haven't changed.
            \\CREATE TABLE IF NOT EXISTS embedding_cache (
            \\  content_hash TEXT PRIMARY KEY,
            \\  embedding     BLOB NOT NULL,
            \\  model         TEXT NOT NULL,
            \\  created_at    INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            \\);
        ;
        var err_msg: [*c]u8 = null;
        const rc = c.sqlite3_exec(self.db, sql, null, null, &err_msg);
        if (rc != c.SQLITE_OK) {
            logSqliteErr("migrate", "CREATE TABLE/indexes", rc, err_msg, self.db);
            if (err_msg) |msg| c.sqlite3_free(msg);
            return error.MigrationFailed;
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

    /// Build the text used for embedding a node.
    /// Format: "module: name (node_type) — comment. signature"
    ///
    /// The goal is to capture the semantic meaning of each node so that
    /// natural-language queries ("find database sync logic") surface the
    /// right code even without exact token matches.
    fn buildEmbeddingText(
        allocator: std.mem.Allocator,
        module: []const u8,
        name: []const u8,
        node_type: []const u8,
        comment: ?[]const u8,
        signature: ?[]const u8,
    ) ![]const u8 {
        var buf: std.ArrayList(u8) = .empty;
        errdefer buf.deinit(allocator);

        try buf.appendSlice(allocator, module);
        try buf.appendSlice(allocator, ": ");
        try buf.appendSlice(allocator, name);
        try buf.appendSlice(allocator, " (");
        try buf.appendSlice(allocator, node_type);
        try buf.append(allocator, ')');
        if (comment) |cm| {
            if (cm.len > 0) {
                try buf.appendSlice(allocator, " — ");
                try buf.appendSlice(allocator, cm);
            }
        }
        if (signature) |sig| {
            if (sig.len > 0) {
                try buf.append(allocator, ' ');
                try buf.appendSlice(allocator, sig);
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

        const emb_text = try buildEmbeddingText(allocator, doc.module, name, "module", doc.module_comment, null);
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
        const emb_text = try buildEmbeddingText(allocator, doc.module, m.name, m.node_type, m.comment, m.signature);
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
        const sql =
            "SELECT id, file_path, source, module, node_type, name, signature," ++
            "       comment, line, used_by, language, embedding " ++
            "FROM ast_nodes " ++
            "WHERE embedding IS NOT NULL " ++
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

    /// Keyword search: SQL LIKE on name, comment, module, signature.
    pub fn keywordSearch(
        self: *Self,
        allocator: std.mem.Allocator,
        query_text: []const u8,
        limit: usize,
    ) ![]SearchResult {
        // Build %pattern%
        const pattern = try std.fmt.allocPrint(allocator, "%{s}%", .{query_text});
        defer allocator.free(pattern);

        const sql =
            "SELECT file_path, source, module, node_type, name, signature," ++
            "       comment, line, used_by, language " ++
            "FROM ast_nodes " ++
            "WHERE name LIKE ?1 OR comment LIKE ?1 OR module LIKE ?1 OR signature LIKE ?1 " ++
            "ORDER BY last_modified DESC " ++
            "LIMIT ?2";

        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) {
            return allocator.alloc(SearchResult, 0);
        }
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, pattern.ptr, @intCast(pattern.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_int64(stmt, 2, @intCast(limit * 2));

        var results: std.ArrayList(SearchResult) = .empty;
        errdefer {
            for (results.items) |r| freeSearchResult(allocator, r);
            results.deinit(allocator);
        }

        while (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            var r = try readRowResult(stmt.?, allocator);
            r.score = 1.0; // flat keyword score; reranking will differentiate
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
// JSON parsing — shared with db.zig structure
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

test "buildEmbeddingText format" {
    const allocator = std.testing.allocator;
    const text = try GuidanceDb.buildEmbeddingText(
        allocator,
        "src.foo",
        "doThing",
        "fn_decl",
        "Does the thing.",
        "fn doThing() void",
    );
    defer allocator.free(text);
    try std.testing.expectEqualStrings(
        "src.foo: doThing (fn_decl) — Does the thing. fn doThing() void",
        text,
    );
}
