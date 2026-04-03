//! guidance SQLite vector search database (cosine similarity via BLOB storage).
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
const vector = @import("root.zig");
const common = @import("common");
const simhash = @import("simhash.zig");
const log = std.log.scoped(.guidance_db);

pub const c = @cImport({
    @cInclude("sqlite3.h");
});

pub const SQLITE_STATIC: c.sqlite3_destructor_type = null;
const BUSY_TIMEOUT_MS: c_int = 5000;

pub const SCHEMA_VERSION: u32 = 2;

/// Minimum cosine similarity to include a result from vectorSearch().
/// Below this threshold the match is semantic noise for 384-dim embeddings.
pub const MIN_VECTOR_THRESHOLD: f32 = 0.35; // was 0.28

// ---------------------------------------------------------------------------
// Semantic aliases for query expansion
// ---------------------------------------------------------------------------

/// A single alias entry mapping a key to expansion values.
pub const SemanticAlias = struct {
    key: []const u8,
    values: []const []const u8,
};

/// A keyword match result from the keyword index search.
pub const KeywordMatch = struct {
    keyword: []const u8,
    score: f32,
};

/// A module match result from keyword-to-module lookup.
pub const ModuleMatch = struct {
    module_id: i64,
    relevance: f32,
};

/// A keyword associated with a capability (from capability_keywords table).
pub const CapabilityKeyword = struct {
    keyword: []const u8,
    relevance: f32,
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

/// Synchronizes the database by loading data from the specified path using the allocator and embedding provider.
pub fn syncDatabase(
    allocator: std.mem.Allocator,
    guidance_dir: []const u8,
    db_path: []const u8,
    embedder: vector.EmbeddingProvider,
    capabilities_dir: ?[]const u8,
    aliases: ?SemanticAliases,
    cache_limit: u32,
) !void {
    var db = try GuidanceDb.init(allocator, db_path, embedder);
    defer db.deinit();

    const src_dir_path = try std.fmt.allocPrint(allocator, "{s}/src", .{guidance_dir});
    defer allocator.free(src_dir_path);

    try db.syncFromDir(allocator, src_dir_path);

    // Resolve capability-mapping.json path (lives alongside guidance_dir)
    const mapping_path = try std.fmt.allocPrint(allocator, "{s}/capability-mapping.json", .{guidance_dir});
    defer allocator.free(mapping_path);

    if (capabilities_dir) |cap_dir| {
        db.syncCapabilities(allocator, cap_dir, mapping_path) catch |err| {
            log.warn("capabilities sync failed: {s}", .{@errorName(err)});
        };
        db.syncCapabilityKeywords(allocator, mapping_path) catch |err| {
            log.warn("capability_keywords sync failed: {s}", .{@errorName(err)});
        };
    }

    if (aliases) |ali| {
        std.debug.print("syncDatabase: syncing {d} semantic aliases...\n", .{ali.aliases.len});
        db.syncAliasEmbeddings(allocator, ali) catch |err| {
            std.debug.print("error: alias embeddings sync failed: {s}\n", .{@errorName(err)});
        };
    } else {
        std.debug.print("syncDatabase: no semantic aliases provided\n", .{});
    }

    // Recompute IDF weights so rare keywords score higher than common ones.
    _ = db.rebuildKeywordIdf() catch |err| {
        log.warn("rebuildKeywordIdf failed: {s}", .{@errorName(err)});
    };

    // Trim embedding cache to configured limit
    db.trimCache(cache_limit);
}

// ---------------------------------------------------------------------------
// DbSyncBuilder — fluent wrapper around syncDatabase
// ---------------------------------------------------------------------------
//
// Usage:
//   try vector_db.DbSyncBuilder.init(allocator, guidance_dir, db_path, embedder)
//       .withCapabilities(cap_dir)   // optional
//       .withAliases(aliases)         // optional
//       .cacheLimit(500)              // optional (default: 0 = unlimited)
//       .sync();

/// Manages database sync operations with a fixed-size buffer pool; owned by the module; ensures consistent state across invocations.
pub const DbSyncBuilder = struct {
    allocator: std.mem.Allocator,
    guidance_dir: []const u8,
    db_path: []const u8,
    embedder: vector.EmbeddingProvider,
    capabilities_dir: ?[]const u8 = null,
    aliases: ?SemanticAliases = null,
    cache_limit: u32 = 0,

    /// Create a builder bound to the required arguments.
    pub fn init(
        allocator: std.mem.Allocator,
        guidance_dir: []const u8,
        db_path: []const u8,
        embedder: vector.EmbeddingProvider,
    ) DbSyncBuilder {
        return .{
            .allocator = allocator,
            .guidance_dir = guidance_dir,
            .db_path = db_path,
            .embedder = embedder,
        };
    }

    /// Set the directory to scan for CAPABILITY.md files.
    pub fn withCapabilities(self: DbSyncBuilder, dir: []const u8) DbSyncBuilder {
        var b = self;
        b.capabilities_dir = dir;
        return b;
    }

    /// Attach pre-loaded semantic aliases for embedding-based query steering.
    pub fn withAliases(self: DbSyncBuilder, aliases: SemanticAliases) DbSyncBuilder {
        var b = self;
        b.aliases = aliases;
        return b;
    }

    /// Maximum entries in the embedding cache (0 = unlimited).
    pub fn cacheLimit(self: DbSyncBuilder, limit: u32) DbSyncBuilder {
        var b = self;
        b.cache_limit = limit;
        return b;
    }

    /// Execute the sync.  Terminal method — equivalent to calling syncDatabase directly.
    pub fn sync(self: DbSyncBuilder) !void {
        return syncDatabase(
            self.allocator,
            self.guidance_dir,
            self.db_path,
            self.embedder,
            self.capabilities_dir,
            self.aliases,
            self.cache_limit,
        );
    }
};

// ---------------------------------------------------------------------------
// GuidanceDb — the database handle
// ---------------------------------------------------------------------------

/// Manages guidance data structures with fixed-size buffers; owned by the system; ensures consistent state across operations.
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
            \\  created_at    INTEGER NOT NULL DEFAULT (strftime('%s','now')),
            \\  atime         INTEGER NOT NULL DEFAULT (strftime('%s','now'))
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
        // Note: atime uses NULL default (not DEFAULT function) because SQLite
        // doesn't allow ALTER TABLE with non-constant defaults. Existing rows
        // are updated via a separate UPDATE statement below.
        const alter_sqls = [_][:0]const u8{
            "ALTER TABLE ast_nodes ADD COLUMN embedding BLOB",
            "ALTER TABLE ast_nodes ADD COLUMN embedding_model TEXT",
            "ALTER TABLE ast_nodes ADD COLUMN source TEXT",
            "ALTER TABLE embedding_cache ADD COLUMN atime INTEGER",
        };
        for (alter_sqls) |alter_sql| {
            var alter_err: [*c]u8 = null;
            _ = c.sqlite3_exec(self.db, alter_sql, null, null, &alter_err);
            if (alter_err) |msg| c.sqlite3_free(msg);
        }

        // Update existing embedding_cache rows to set atime = created_at where atime is NULL
        // This handles the migration from pre-atime databases
        {
            var update_err: [*c]u8 = null;
            const update_sql = "UPDATE embedding_cache SET atime = created_at WHERE atime IS NULL";
            _ = c.sqlite3_exec(self.db, update_sql, null, null, &update_err);
            if (update_err) |msg| c.sqlite3_free(msg);
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

        // ── Step 2c: semantic_alias_embeddings table ──
        // Stores pre-computed embeddings for semantic alias keys for lightweight
        // query steering: natural language → matching alias keys → expanded tokens
        {
            const alias_emb_sql =
                \\CREATE TABLE IF NOT EXISTS semantic_alias_embeddings (
                \\  alias_key     TEXT PRIMARY KEY,
                \\  embedding     BLOB    NOT NULL,
                \\  embedding_model TEXT  NOT NULL,
                \\  created_at    INTEGER NOT NULL DEFAULT (strftime('%s','now'))
                \\);
            ;
            var alias_err: [*c]u8 = null;
            const alias_rc = c.sqlite3_exec(self.db, alias_emb_sql, null, null, &alias_err);
            if (alias_rc != c.SQLITE_OK) {
                logSqliteErr("migrate", "CREATE TABLE semantic_alias_embeddings", alias_rc, alias_err, self.db);
                if (alias_err) |msg| c.sqlite3_free(msg);
                return error.MigrationFailed;
            }
        }

        // ── Step 2d: keyword_index table (semantic index for module discovery) ──
        // Keywords are embedded (not content) for lightweight vector search.
        // Query → keyword match → module lookup → detail retrieval.
        {
            const kw_idx_sql =
                \\CREATE TABLE IF NOT EXISTS keyword_index (
                \\  keyword       TEXT PRIMARY KEY,
                \\  embedding     BLOB    NOT NULL,
                \\  embedding_model TEXT  NOT NULL,
                \\  created_at    INTEGER NOT NULL DEFAULT (strftime('%s','now'))
                \\);
                \\CREATE INDEX IF NOT EXISTS idx_kw_created ON keyword_index(created_at);
            ;
            var kw_idx_err: [*c]u8 = null;
            const kw_idx_rc = c.sqlite3_exec(self.db, kw_idx_sql, null, null, &kw_idx_err);
            if (kw_idx_rc != c.SQLITE_OK) {
                logSqliteErr("migrate", "CREATE TABLE keyword_index", kw_idx_rc, kw_idx_err, self.db);
                if (kw_idx_err) |msg| c.sqlite3_free(msg);
                return error.MigrationFailed;
            }
        }

        // ── Step 2e: keyword_modules table (keyword → module mapping) ──
        // Links keywords to modules for retrieval after keyword match.
        {
            const kw_mod_sql =
                \\CREATE TABLE IF NOT EXISTS keyword_modules (
                \\  keyword       TEXT    NOT NULL,
                \\  module_id     INTEGER NOT NULL,
                \\  relevance     REAL    DEFAULT 1.0,
                \\  PRIMARY KEY (keyword, module_id),
                \\  FOREIGN KEY (module_id) REFERENCES ast_nodes(id) ON DELETE CASCADE
                \\);
                \\CREATE INDEX IF NOT EXISTS idx_km_keyword ON keyword_modules(keyword);
                \\CREATE INDEX IF NOT EXISTS idx_km_module  ON keyword_modules(module_id);
            ;
            var kw_mod_err: [*c]u8 = null;
            const kw_mod_rc = c.sqlite3_exec(self.db, kw_mod_sql, null, null, &kw_mod_err);
            if (kw_mod_rc != c.SQLITE_OK) {
                logSqliteErr("migrate", "CREATE TABLE keyword_modules", kw_mod_rc, kw_mod_err, self.db);
                if (kw_mod_err) |msg| c.sqlite3_free(msg);
                return error.MigrationFailed;
            }
        }

        // ── Step 2f: add detail column to ast_nodes ──
        // Stores comprehensive module documentation (TEXT, not embedded).
        {
            var detail_err: [*c]u8 = null;
            _ = c.sqlite3_exec(self.db, "ALTER TABLE ast_nodes ADD COLUMN detail TEXT", null, null, &detail_err);
            if (detail_err) |msg| c.sqlite3_free(msg);
        }

        // ── Step 2g: capability_keywords table ──
        // Maps capability names to AST-level keywords for query-guided keyword search.
        // Natural-language queries match capability embeddings; keywords are then used
        // for direct SQL LIKE searches on enriched AST records.
        {
            const ck_sql =
                \\CREATE TABLE IF NOT EXISTS capability_keywords (
                \\  capability_name TEXT NOT NULL,
                \\  keyword         TEXT NOT NULL,
                \\  relevance       REAL DEFAULT 1.0,
                \\  PRIMARY KEY (capability_name, keyword)
                \\);
                \\CREATE INDEX IF NOT EXISTS idx_ck_cap ON capability_keywords(capability_name);
                \\CREATE INDEX IF NOT EXISTS idx_ck_kw  ON capability_keywords(keyword);
            ;
            var ck_err: [*c]u8 = null;
            const ck_rc = c.sqlite3_exec(self.db, ck_sql, null, null, &ck_err);
            if (ck_rc != c.SQLITE_OK) {
                logSqliteErr("migrate", "CREATE TABLE capability_keywords", ck_rc, ck_err, self.db);
                if (ck_err) |msg| c.sqlite3_free(msg);
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

        // ── Step 4: simhash_index table ──
        // Cascade-deletes keep the index consistent when ast_nodes rows are removed.
        {
            var err_msg: [*c]u8 = null;
            const sh_sql =
                \\CREATE TABLE IF NOT EXISTS simhash_index (
                \\    node_id  INTEGER NOT NULL PRIMARY KEY,
                \\    simhash  INTEGER NOT NULL,
                \\    FOREIGN KEY (node_id) REFERENCES ast_nodes(id) ON DELETE CASCADE
                \\);
                \\CREATE INDEX IF NOT EXISTS idx_sh ON simhash_index(simhash);
            ;
            const rc = c.sqlite3_exec(self.db, sh_sql, null, null, &err_msg);
            if (rc != c.SQLITE_OK) {
                logSqliteErr("migrate", "CREATE TABLE simhash_index", rc, err_msg, self.db);
                if (err_msg) |msg| c.sqlite3_free(msg);
                // Non-fatal: SimHash is a performance hint only.
            }
        }

        // ── Step 2h: query_count column on ast_nodes (hot files tracking) ──
        // Incremented each time an ast_nodes row is returned in a keyword search.
        {
            var qc_err: [*c]u8 = null;
            _ = c.sqlite3_exec(
                self.db,
                "ALTER TABLE ast_nodes ADD COLUMN query_count INTEGER NOT NULL DEFAULT 0",
                null,
                null,
                &qc_err,
            );
            if (qc_err) |msg| c.sqlite3_free(msg);
        }

        // ── Step 2i: query_log table (telemetry) ──
        {
            const ql_sql =
                \\CREATE TABLE IF NOT EXISTS query_log (
                \\  id           INTEGER PRIMARY KEY,
                \\  query        TEXT    NOT NULL,
                \\  timestamp    INTEGER NOT NULL,
                \\  latency_ms   INTEGER,
                \\  result_count INTEGER,
                \\  tier         TEXT
                \\);
                \\CREATE INDEX IF NOT EXISTS idx_ql_ts  ON query_log(timestamp);
                \\CREATE INDEX IF NOT EXISTS idx_ql_q   ON query_log(query);
            ;
            var ql_err: [*c]u8 = null;
            const ql_rc = c.sqlite3_exec(self.db, ql_sql, null, null, &ql_err);
            if (ql_rc != c.SQLITE_OK) {
                logSqliteErr("migrate", "CREATE TABLE query_log", ql_rc, ql_err, self.db);
                if (ql_err) |msg| c.sqlite3_free(msg);
                // Non-fatal: telemetry is best-effort.
            }
        }

        // ── Step 2j: llm_cache table (synthesis cache) ──
        {
            const lc_sql =
                \\CREATE TABLE IF NOT EXISTS llm_cache (
                \\  query_hash     TEXT PRIMARY KEY,
                \\  response       TEXT NOT NULL,
                \\  created_at     INTEGER NOT NULL,
                \\  signature_hash TEXT NOT NULL
                \\);
            ;
            var lc_err: [*c]u8 = null;
            const lc_rc = c.sqlite3_exec(self.db, lc_sql, null, null, &lc_err);
            if (lc_rc != c.SQLITE_OK) {
                logSqliteErr("migrate", "CREATE TABLE llm_cache", lc_rc, lc_err, self.db);
                if (lc_err) |msg| c.sqlite3_free(msg);
                // Non-fatal.
            }
        }

        // ── Step 2k: capability_sources table (capability ↔ source file joins) ──
        // Links capabilities to source files with confidence scores for discovery.
        {
            const cs_sql =
                \\CREATE TABLE IF NOT EXISTS capability_sources (
                \\  capability_name TEXT NOT NULL,
                \\  source_path    TEXT NOT NULL,
                \\  confidence     REAL NOT NULL,
                \\  reason         TEXT NOT NULL,
                \\  updated_at     INTEGER NOT NULL,
                \\  PRIMARY KEY (capability_name, source_path)
                \\);
                \\CREATE INDEX IF NOT EXISTS idx_cs_cap    ON capability_sources(capability_name);
                \\CREATE INDEX IF NOT EXISTS idx_cs_source ON capability_sources(source_path);
                \\CREATE INDEX IF NOT EXISTS idx_cs_conf   ON capability_sources(confidence);
            ;
            var cs_err: [*c]u8 = null;
            const cs_rc = c.sqlite3_exec(self.db, cs_sql, null, null, &cs_err);
            if (cs_rc != c.SQLITE_OK) {
                logSqliteErr("migrate", "CREATE TABLE capability_sources", cs_rc, cs_err, self.db);
                if (cs_err) |msg| c.sqlite3_free(msg);
                // Non-fatal.
            }
        }

        // ── Step 5: schema version ──
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
        // Derive workspace root: src_dir_path = "{workspace}/.guidance/src"
        // Two dirname levels up: .guidance/src → .guidance → workspace
        const guidance_dir = std.fs.path.dirname(src_dir_path) orelse ".";
        const workspace = std.fs.path.dirname(guidance_dir) orelse ".";

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

            self.indexFile(fa, rel_path, mtime_sec, workspace) catch |err| {
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
    /// Sync CAPABILITY.md files from cap_dir into the capabilities table.
    /// mapping_path: optional path to capability-mapping.json; when provided, keywords
    /// are injected into the embedding text so natural-language queries route to them.
    pub fn syncCapabilities(
        self: *Self,
        allocator: std.mem.Allocator,
        cap_dir: []const u8,
        mapping_path: ?[]const u8,
    ) !void {
        std.debug.print("[sync-capabilities] scanning {s}\n", .{cap_dir});
        var dir = std.fs.cwd().openDir(cap_dir, .{ .iterate = true }) catch |err| {
            if (err == error.FileNotFound) {
                std.debug.print("[sync-capabilities] dir not found: {s}\n", .{cap_dir});
                return;
            }
            std.debug.print("[sync-capabilities] error opening dir: {s}\n", .{@errorName(err)});
            return err;
        };
        defer dir.close();

        // Pre-load capability keywords from mapping file for embedding enrichment.
        // This is a best-effort load — missing file is not an error.
        var mapping_arena = std.heap.ArenaAllocator.init(allocator);
        defer mapping_arena.deinit();
        const mapping_alloc = mapping_arena.allocator();

        // Map: capability_name → [][]const u8 of keywords (owned by mapping_arena).
        var kw_map: std.StringHashMapUnmanaged([]const []const u8) = .{};
        defer kw_map.deinit(mapping_alloc);

        if (mapping_path) |mp| {
            loadCapabilityKeywordsMap(mapping_alloc, mp, &kw_map) catch {};
        }

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

            // Derive capability name from directory stem for keyword lookup.
            const cap_name = capabilityNameFromPath(abs_path);
            const keywords: []const []const u8 = kw_map.get(cap_name) orelse &[_][]const u8{};

            self.indexCapability(fa, abs_path, mtime_sec, keywords) catch |err| {
                log.warn("indexCapability({s}): {s}", .{ abs_path, @errorName(err) });
                continue;
            };
            synced += 1;
        }

        if (synced > 0) log.info("capabilities synced: {d}", .{synced});
    }

    /// Parse a CAPABILITY.md file and upsert it into the capabilities table.
    /// keywords: AST-level search terms from capability-mapping.json; injected into
    /// the embedding text so NL queries route to the right keyword searches.
    fn indexCapability(
        self: *Self,
        allocator: std.mem.Allocator,
        file_path: []const u8,
        mtime: i64,
        keywords: []const []const u8,
    ) !void {
        const content = try std.fs.cwd().readFileAlloc(allocator, file_path, 512 * 1024);
        defer allocator.free(content);

        // Parse YAML-ish frontmatter: ---\nname: ...\ndescription: ...\n---
        var name: []const u8 = capabilityNameFromPath(file_path);
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

        // Build query-oriented embedding text.
        // The embedding represents *the kinds of NL queries* that should match this
        // capability, so that query → capability match → capability keywords → SQL search.
        // Format: description. NL question variants. AST-level search terms.
        var emb_buf: std.ArrayList(u8) = .{};
        defer emb_buf.deinit(allocator);

        try emb_buf.appendSlice(allocator, description);
        if (description.len > 0) try emb_buf.appendSlice(allocator, ". ");
        try emb_buf.appendSlice(allocator, "How does ");
        try emb_buf.appendSlice(allocator, name);
        try emb_buf.appendSlice(allocator, " work? What is ");
        try emb_buf.appendSlice(allocator, name);
        try emb_buf.appendSlice(allocator, "? Understanding ");
        try emb_buf.appendSlice(allocator, name);
        try emb_buf.appendSlice(allocator, " implementation.");

        // Append keywords so both identifier and prose forms match.
        if (keywords.len > 0) {
            try emb_buf.appendSlice(allocator, " Search terms: ");
            for (keywords, 0..) |kw, i| {
                if (i > 0) try emb_buf.appendSlice(allocator, ", ");
                try emb_buf.appendSlice(allocator, kw);
            }
            try emb_buf.append(allocator, '.');
        }

        // Append a short excerpt of the body for extra semantic signal.
        const body_excerpt = body[0..@min(300, body.len)];
        if (body_excerpt.len > 0) {
            try emb_buf.append(allocator, ' ');
            try emb_buf.appendSlice(allocator, body_excerpt);
        }

        const emb_text = try emb_buf.toOwnedSlice(allocator);
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

    // ── Capability-keyword methods ─────────────────────────────────

    /// Sync capability_keywords table from capability-mapping.json.
    /// Idempotent: existing rows are skipped via INSERT OR IGNORE.
    pub fn syncCapabilityKeywords(
        self: *Self,
        allocator: std.mem.Allocator,
        mapping_path: []const u8,
    ) !void {
        var arena = std.heap.ArenaAllocator.init(allocator);
        defer arena.deinit();
        const fa = arena.allocator();

        var kw_map: std.StringHashMapUnmanaged([]const []const u8) = .{};
        defer kw_map.deinit(fa);

        loadCapabilityKeywordsMap(fa, mapping_path, &kw_map) catch return;

        const sql = "INSERT OR IGNORE INTO capability_keywords(capability_name, keyword, relevance) VALUES (?1, ?2, ?3)";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        var inserted: usize = 0;
        var it = kw_map.iterator();
        while (it.next()) |entry| {
            const cap_name = entry.key_ptr.*;
            const keywords = entry.value_ptr.*;
            for (keywords) |kw| {
                _ = c.sqlite3_reset(stmt);
                _ = c.sqlite3_bind_text(stmt, 1, cap_name.ptr, @intCast(cap_name.len), SQLITE_STATIC);
                _ = c.sqlite3_bind_text(stmt, 2, kw.ptr, @intCast(kw.len), SQLITE_STATIC);
                _ = c.sqlite3_bind_double(stmt, 3, 1.0);
                _ = c.sqlite3_step(stmt);
                inserted += 1;
            }
        }

        if (inserted > 0) log.info("capability_keywords synced: {d} rows", .{inserted});
    }

    /// Find AST-level keywords associated with capabilities that are semantically
    /// similar to `query_embedding`.  Returns an owned, deduplicated slice of
    /// keyword strings (caller must free each string and the slice).
    ///
    /// Threshold: capabilities with cosine similarity below `threshold` are skipped.
    /// max_capabilities: how many top capabilities to query (default: 3).
    pub fn findCapabilityKeywordsForQuery(
        self: *Self,
        allocator: std.mem.Allocator,
        query_embedding: []const f32,
        threshold: f32,
        max_capabilities: usize,
    ) ![][]const u8 {
        // Step 1: scan capabilities table for similar embeddings.
        const cap_sql = "SELECT name, embedding FROM capabilities WHERE embedding IS NOT NULL";
        var cap_stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, cap_sql, -1, &cap_stmt, null) != c.SQLITE_OK) {
            return allocator.alloc([]const u8, 0);
        }
        defer _ = c.sqlite3_finalize(cap_stmt);

        const ScoredCap = struct { name: []const u8, score: f32 };
        var scored: std.ArrayList(ScoredCap) = .{};
        errdefer {
            for (scored.items) |sc| allocator.free(sc.name);
            scored.deinit(allocator);
        }

        while (c.sqlite3_step(cap_stmt) == c.SQLITE_ROW) {
            const cap_name = try dupeCol(cap_stmt.?, 0, allocator);
            const emb_blob = c.sqlite3_column_blob(cap_stmt.?, 1);
            const emb_len: usize = @intCast(c.sqlite3_column_bytes(cap_stmt.?, 1));
            if (emb_blob == null or emb_len == 0) {
                allocator.free(cap_name);
                continue;
            }
            const bytes: []const u8 = @as([*]const u8, @ptrCast(emb_blob))[0..emb_len];
            const stored_emb = vector.bytesToVec(allocator, bytes) catch {
                allocator.free(cap_name);
                continue;
            };
            defer allocator.free(stored_emb);

            const sim = vector.cosineSimilarity(query_embedding, stored_emb);
            if (sim >= threshold) {
                try scored.append(allocator, .{ .name = cap_name, .score = sim });
            } else {
                allocator.free(cap_name);
            }
        }

        // Sort by score descending and take top N.
        std.sort.block(ScoredCap, scored.items, {}, struct {
            fn lessThan(_: void, a: ScoredCap, b: ScoredCap) bool {
                return a.score > b.score;
            }
        }.lessThan);

        const take = @min(max_capabilities, scored.items.len);

        // Step 2: for each matched capability, fetch its keywords.
        var seen: std.StringHashMapUnmanaged(void) = .{};
        defer {
            var sit = seen.keyIterator();
            while (sit.next()) |k| allocator.free(k.*);
            seen.deinit(allocator);
        }

        var keywords: std.ArrayList([]const u8) = .{};
        errdefer {
            for (keywords.items) |kw| allocator.free(kw);
            keywords.deinit(allocator);
        }

        const kw_sql = "SELECT keyword FROM capability_keywords WHERE capability_name = ?1 ORDER BY relevance DESC";
        var kw_stmt: ?*c.sqlite3_stmt = null;
        _ = c.sqlite3_prepare_v2(self.db, kw_sql, -1, &kw_stmt, null);
        defer {
            if (kw_stmt) |s| _ = c.sqlite3_finalize(s);
        }

        for (0..take) |i| {
            const cap_name = scored.items[i].name;
            if (kw_stmt) |s| {
                _ = c.sqlite3_reset(s);
                _ = c.sqlite3_bind_text(s, 1, cap_name.ptr, @intCast(cap_name.len), SQLITE_STATIC);
                while (c.sqlite3_step(s) == c.SQLITE_ROW) {
                    const kw = try dupeCol(s, 0, allocator);
                    if (seen.contains(kw)) {
                        allocator.free(kw);
                        continue;
                    }
                    const kw_lower = try std.ascii.allocLowerString(allocator, kw);
                    allocator.free(kw);
                    if (seen.contains(kw_lower)) {
                        allocator.free(kw_lower);
                        continue;
                    }
                    try seen.put(allocator, try allocator.dupe(u8, kw_lower), {});
                    try keywords.append(allocator, kw_lower);
                }
            }
        }

        // Free unused capability names.
        for (take..scored.items.len) |i| allocator.free(scored.items[i].name);
        for (0..take) |i| allocator.free(scored.items[i].name);
        scored.deinit(allocator);

        return keywords.toOwnedSlice(allocator);
    }

    /// Return the names of capabilities that matched the query above threshold.
    /// The query text is embedded (cached) and cosine-compared to capabilities.embedding.
    /// Caller owns the returned slice and each name string; free with allocator.
    /// Returns an empty slice when no embedder is configured or no capabilities match.
    pub fn findMatchedCapabilityNamesForQuery(
        self: *Self,
        allocator: std.mem.Allocator,
        query_text: []const u8,
        threshold: f32,
        max_capabilities: usize,
    ) ![][]const u8 {
        const query_emb = try self.getOrComputeEmbedding(allocator, query_text);
        defer if (query_emb) |e| allocator.free(e);
        const emb = query_emb orelse return allocator.alloc([]const u8, 0);

        const sql = "SELECT name, embedding FROM capabilities WHERE embedding IS NOT NULL";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) {
            return allocator.alloc([]const u8, 0);
        }
        defer _ = c.sqlite3_finalize(stmt);

        const ScoredCap = struct { name: []const u8, score: f32 };
        var scored: std.ArrayList(ScoredCap) = .{};
        defer {
            for (scored.items) |sc| allocator.free(sc.name);
            scored.deinit(allocator);
        }

        while (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            const cap_name = try dupeCol(stmt.?, 0, allocator);
            const emb_blob = c.sqlite3_column_blob(stmt.?, 1);
            const emb_len: usize = @intCast(c.sqlite3_column_bytes(stmt.?, 1));
            if (emb_blob == null or emb_len == 0) {
                allocator.free(cap_name);
                continue;
            }
            const bytes: []const u8 = @as([*]const u8, @ptrCast(emb_blob))[0..emb_len];
            const stored_emb = vector.bytesToVec(allocator, bytes) catch {
                allocator.free(cap_name);
                continue;
            };
            defer allocator.free(stored_emb);

            const sim = vector.cosineSimilarity(emb, stored_emb);
            if (sim >= threshold) {
                try scored.append(allocator, .{ .name = cap_name, .score = sim });
            } else {
                allocator.free(cap_name);
            }
        }

        std.sort.block(ScoredCap, scored.items, {}, struct {
            fn lessThan(_: void, a: ScoredCap, b: ScoredCap) bool {
                return a.score > b.score;
            }
        }.lessThan);

        const take = @min(max_capabilities, scored.items.len);
        var names: std.ArrayList([]const u8) = .{};
        errdefer {
            for (names.items) |n| allocator.free(n);
            names.deinit(allocator);
        }
        for (0..take) |i| {
            try names.append(allocator, try allocator.dupe(u8, scored.items[i].name));
        }
        return names.toOwnedSlice(allocator);
    }

    // ── Capability sources methods ─────────────────────────────────

    /// Source file linked to a capability with confidence score.
    pub const CapabilitySource = struct {
        source_path: []const u8,
        confidence: f32,
        reason: []const u8,
    };

    /// Upsert a capability-source mapping into the capability_sources table.
    /// Returns true if a new row was inserted, false if updated.
    pub fn upsertCapabilitySource(
        self: *Self,
        capability_name: []const u8,
        source_path: []const u8,
        confidence: f32,
        reason: []const u8,
    ) !bool {
        const timestamp: i64 = @intCast(std.time.nanoTimestamp());
        const sql = "INSERT OR REPLACE INTO capability_sources" ++
            "(capability_name, source_path, confidence, reason, updated_at)" ++
            " VALUES (?1, ?2, ?3, ?4, ?5)";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, capability_name.ptr, @intCast(capability_name.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 2, source_path.ptr, @intCast(source_path.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_double(stmt, 3, confidence);
        _ = c.sqlite3_bind_text(stmt, 4, reason.ptr, @intCast(reason.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_int64(stmt, 5, timestamp);

        if (c.sqlite3_step(stmt) != c.SQLITE_DONE) return error.StepFailed;
        return true;
    }

    /// Get all source files for a capability with confidence >= min_confidence.
    /// Caller owns the returned slice and each string; free with allocator.
    pub fn getCapabilitySources(
        self: *Self,
        allocator: std.mem.Allocator,
        capability_name: []const u8,
        min_confidence: f32,
    ) ![]CapabilitySource {
        const sql = "SELECT source_path, confidence, reason FROM capability_sources" ++
            " WHERE capability_name = ?1 AND confidence >= ?2 ORDER BY confidence DESC";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) {
            return allocator.alloc(CapabilitySource, 0);
        }
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, capability_name.ptr, @intCast(capability_name.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_double(stmt, 2, min_confidence);

        var sources: std.ArrayList(CapabilitySource) = .{};
        errdefer {
            for (sources.items) |s| {
                allocator.free(s.source_path);
                allocator.free(s.reason);
            }
            sources.deinit(allocator);
        }

        while (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            const sp = try dupeCol(stmt.?, 0, allocator);
            const conf: f32 = @floatCast(c.sqlite3_column_double(stmt.?, 1));
            const reason = try dupeCol(stmt.?, 2, allocator);
            try sources.append(allocator, .{
                .source_path = sp,
                .confidence = conf,
                .reason = reason,
            });
        }

        return sources.toOwnedSlice(allocator);
    }

    /// Clear all capability-source mappings for a specific capability.
    /// Used when re-discovering sources after capability modification.
    pub fn clearCapabilitySources(self: *Self, capability_name: []const u8) !void {
        const sql = "DELETE FROM capability_sources WHERE capability_name = ?1";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, capability_name.ptr, @intCast(capability_name.len), SQLITE_STATIC);
        _ = c.sqlite3_step(stmt);
    }

    /// Get all capabilities that a source file belongs to.
    /// Caller owns the returned slice and each string; free with allocator.
    pub fn getCapabilitiesForSource(
        self: *Self,
        allocator: std.mem.Allocator,
        source_path: []const u8,
    ) ![][]const u8 {
        const sql = "SELECT capability_name FROM capability_sources WHERE source_path = ?1 ORDER BY confidence DESC";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) {
            return allocator.alloc([]const u8, 0);
        }
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, source_path.ptr, @intCast(source_path.len), SQLITE_STATIC);

        var caps: std.ArrayList([]const u8) = .{};
        errdefer {
            for (caps.items) |cap_item| allocator.free(cap_item);
            caps.deinit(allocator);
        }

        while (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            try caps.append(allocator, try dupeCol(stmt.?, 0, allocator));
        }

        return caps.toOwnedSlice(allocator);
    }

    /// Capability source entry returned by getAllCapabilitySources.
    pub const CapabilitySourceEntry = struct {
        source_path: []const u8,
        capability_name: []const u8,
    };

    /// Get ALL capability-source mappings as (source_path, capability_name) pairs.
    /// M4: Used to populate capabilities_map in SyncProcessor.
    /// Caller owns the returned slice and each string; free with allocator.
    pub fn getAllCapabilitySources(self: *Self, allocator: std.mem.Allocator) ![]CapabilitySourceEntry {
        const sql = "SELECT source_path, capability_name FROM capability_sources ORDER BY source_path, confidence DESC";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) {
            return allocator.alloc(CapabilitySourceEntry, 0);
        }
        defer _ = c.sqlite3_finalize(stmt);

        var entries: std.ArrayList(CapabilitySourceEntry) = .{};
        errdefer {
            for (entries.items) |e| {
                allocator.free(e.source_path);
                allocator.free(e.capability_name);
            }
            entries.deinit(allocator);
        }

        while (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            const sp = try dupeCol(stmt.?, 0, allocator);
            const cn = try dupeCol(stmt.?, 1, allocator);
            try entries.append(allocator, .{ .source_path = sp, .capability_name = cn });
        }

        return entries.toOwnedSlice(allocator);
    }

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

    fn indexFile(self: *Self, allocator: std.mem.Allocator, file_path: []const u8, mtime: i64, workspace: []const u8) !void {
        const file_data = try std.fs.cwd().readFileAlloc(allocator, file_path, 8 * 1024 * 1024);
        defer allocator.free(file_data);

        const parsed = try parseGuidanceJson(allocator, file_data);

        // For Zig files, read source text so insertModule/insertMember can
        // fall back to /// and //! inline comments when JSON has none.
        const source_text: ?[]u8 = if (std.mem.eql(u8, parsed.language, "zig")) blk: {
            const src_path = try std.fmt.allocPrint(allocator, "{s}/{s}", .{ workspace, parsed.source });
            defer allocator.free(src_path);
            break :blk std.fs.cwd().readFileAlloc(allocator, src_path, 5 * 1024 * 1024) catch null;
        } else null;
        defer if (source_text) |t| allocator.free(t);

        try self.execSimple("BEGIN");
        errdefer _ = self.execSimpleNoErr("ROLLBACK");

        try self.deleteFileRecords(file_path);
        try self.insertModule(allocator, file_path, parsed, mtime, source_text);
        for (parsed.members) |m| {
            try self.insertMember(allocator, file_path, parsed, m, mtime, source_text);
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

    // ── SimHash index ──────────────────────────────────────────────

    /// Upsert a SimHash entry for a node with an embedding.
    /// Called after each INSERT INTO ast_nodes when an embedding was computed.
    fn upsertSimhash(self: *Self, node_id: i64, h: u64) void {
        const sql = "INSERT OR REPLACE INTO simhash_index(node_id, simhash) VALUES(?1, ?2)";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return;
        defer _ = c.sqlite3_finalize(stmt);
        _ = c.sqlite3_bind_int64(stmt, 1, node_id);
        _ = c.sqlite3_bind_int64(stmt, 2, @bitCast(h));
        _ = c.sqlite3_step(stmt);
    }

    /// Return candidate node IDs whose SimHash is within max_hamming bits of query_hash.
    ///
    /// This is the hot path: loads 8 bytes per node (versus 4 × DIMS bytes per embedding),
    /// filters by Hamming distance (single XOR + popcount instruction), and returns only
    /// the candidates that warrant a full cosine evaluation.
    fn getSimhashCandidates(
        self: *Self,
        allocator: std.mem.Allocator,
        query_hash: u64,
        max_hamming: u7,
    ) ![]i64 {
        const sql = "SELECT node_id, simhash FROM simhash_index";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) {
            return allocator.alloc(i64, 0);
        }
        defer _ = c.sqlite3_finalize(stmt);

        var candidates: std.ArrayList(i64) = .{};
        errdefer candidates.deinit(allocator);

        while (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            const node_id = c.sqlite3_column_int64(stmt, 0);
            const h: u64 = @bitCast(c.sqlite3_column_int64(stmt, 1));
            if (simhash.hammingDistance(h, query_hash) <= max_hamming) {
                try candidates.append(allocator, node_id);
            }
        }
        return candidates.toOwnedSlice(allocator);
    }

    /// Fetch the embedding BLOB for a single node by its SQLite row id.
    /// Returns null when the node has no embedding.  Caller frees the slice.
    fn getEmbeddingById(self: *Self, allocator: std.mem.Allocator, node_id: i64) !?[]f32 {
        const sql = "SELECT embedding FROM ast_nodes WHERE id = ?1";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return null;
        defer _ = c.sqlite3_finalize(stmt);
        _ = c.sqlite3_bind_int64(stmt, 1, node_id);
        if (c.sqlite3_step(stmt) != c.SQLITE_ROW) return null;
        const emb_raw = c.sqlite3_column_blob(stmt, 0);
        const emb_len: usize = @intCast(c.sqlite3_column_bytes(stmt, 0));
        if (emb_raw == null or emb_len == 0) return null;
        const emb_bytes: []const u8 = @as([*]const u8, @ptrCast(emb_raw))[0..emb_len];
        return try vector.bytesToVec(allocator, emb_bytes);
    }

    /// Rebuild the entire simhash_index from the current ast_nodes embeddings.
    /// Safe to run at any time — clears and repopulates the table.
    /// Call after a full sync to ensure the index is current.
    pub fn rebuildSimhashIndex(self: *Self, allocator: std.mem.Allocator) !usize {
        // Clear existing index.
        _ = self.execSimpleNoErr("DELETE FROM simhash_index");

        const sql =
            "SELECT id, embedding FROM ast_nodes " ++
            "WHERE embedding IS NOT NULL AND node_type != 'test_decl'";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) {
            return error.PrepareFailed;
        }
        defer _ = c.sqlite3_finalize(stmt);

        var count: usize = 0;
        while (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            const node_id = c.sqlite3_column_int64(stmt, 0);
            const emb_raw = c.sqlite3_column_blob(stmt, 1);
            const emb_len: usize = @intCast(c.sqlite3_column_bytes(stmt, 1));
            if (emb_raw == null or emb_len == 0) continue;

            const emb_bytes: []const u8 = @as([*]const u8, @ptrCast(emb_raw))[0..emb_len];
            const emb = vector.bytesToVec(allocator, emb_bytes) catch continue;
            defer allocator.free(emb);

            const h = simhash.simhash(emb);
            self.upsertSimhash(node_id, h);
            count += 1;
        }
        log.info("simhash_index rebuilt: {d} entries", .{count});
        return count;
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
    ///
    /// Format: descriptor-bag "name · module · type · comment_nouns · params · context"
    /// Uses `·` (U+00B7) as separator — rare in code, clear to sentence-transformers.
    /// Boilerplate prefixes are stripped from comments before embedding.
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
            // Module: "<prose> · module · <stripped_comment>"
            try buf.appendSlice(allocator, prose_module);
            try buf.appendSlice(allocator, " · module");
            if (comment) |cm| {
                const stripped = common.stripBoilerplate(cm);
                if (stripped.len > 0) {
                    try buf.appendSlice(allocator, " · ");
                    try buf.appendSlice(allocator, stripped);
                }
            }
        } else {
            // Member: "<name> · <prose> · <noun> · <stripped_comment> · <params> · <context>"
            try buf.appendSlice(allocator, name);
            try buf.appendSlice(allocator, " · ");
            try buf.appendSlice(allocator, prose_module);
            try buf.appendSlice(allocator, " · ");
            try buf.appendSlice(allocator, noun);

            if (comment) |cm| {
                const stripped = common.stripBoilerplate(cm);
                if (stripped.len > 0) {
                    try buf.appendSlice(allocator, " · ");
                    try buf.appendSlice(allocator, stripped);
                }
            }

            // Parameter names (stripped of types) — semantic signal for callers
            if (signature) |sig| {
                if (try extractParamNames(allocator, sig)) |params| {
                    defer allocator.free(params);
                    if (params.len > 0) {
                        try buf.appendSlice(allocator, " · ");
                        try buf.appendSlice(allocator, params);
                    }
                }
            }

            // Parent module context — helps queries find members via module purpose
            if (parent_comment) |pc| {
                if (pc.len > 0 and pc.len < 200) {
                    const stripped_pc = common.stripBoilerplate(pc);
                    if (stripped_pc.len > 0) {
                        try buf.appendSlice(allocator, " · ");
                        try buf.appendSlice(allocator, stripped_pc);
                    }
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

        // Update atime on cache hit
        const update_sql = "UPDATE embedding_cache SET atime = strftime('%s','now') WHERE content_hash = ?1";
        var upd_stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, update_sql, -1, &upd_stmt, null) == c.SQLITE_OK) {
            defer _ = c.sqlite3_finalize(upd_stmt);
            _ = c.sqlite3_bind_text(upd_stmt, 1, content_hash.ptr, @intCast(content_hash.len), SQLITE_STATIC);
            _ = c.sqlite3_step(upd_stmt);
        }

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

        const sql = "INSERT OR REPLACE INTO embedding_cache(content_hash, embedding, model, created_at, atime) VALUES(?1,?2,?3,strftime('%s','now'),strftime('%s','now'))";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, content_hash.ptr, @intCast(content_hash.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_blob(stmt, 2, bytes.ptr, @intCast(bytes.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 3, model_name.ptr, @intCast(model_name.len), SQLITE_STATIC);
        _ = c.sqlite3_step(stmt);
    }

    /// Trim embedding cache to keep at most `limit` entries.
    /// Deletes oldest entries (lowest atime) first.
    /// Does nothing if limit is 0 (unlimited).
    fn trimCache(self: *Self, limit: u32) void {
        if (limit == 0) {
            log.debug("trimCache: limit is 0 (unlimited), skipping", .{});
            return;
        }

        // Count entries
        var count_stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, "SELECT COUNT(*) FROM embedding_cache", -1, &count_stmt, null) != c.SQLITE_OK) return;
        defer _ = c.sqlite3_finalize(count_stmt);

        if (c.sqlite3_step(count_stmt) != c.SQLITE_ROW) return;
        const count: u32 = @intCast(c.sqlite3_column_int(count_stmt, 0));

        log.debug("trimCache: count={d}, limit={d}", .{ count, limit });

        if (count <= limit) return;

        // Delete oldest entries to bring count down to limit
        const delete_count = count - limit;
        const delete_sql = "DELETE FROM embedding_cache WHERE content_hash IN (SELECT content_hash FROM embedding_cache ORDER BY atime ASC LIMIT ?1)";
        var delete_stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, delete_sql, -1, &delete_stmt, null) != c.SQLITE_OK) {
            log.debug("trimCache: prepare failed", .{});
            return;
        }
        defer _ = c.sqlite3_finalize(delete_stmt);

        _ = c.sqlite3_bind_int(delete_stmt, 1, @intCast(delete_count));
        const rc = c.sqlite3_step(delete_stmt);
        log.debug("trimCache: delete step rc={d}, deleted {d} entries", .{ rc, c.sqlite3_changes(self.db) });
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

        // Normalize to lowercase for case-insensitive embedding matching
        const text_lower = try std.ascii.allocLowerString(allocator, text);
        defer allocator.free(text_lower);

        const hash = vector.contentHashWithModel(text_lower, model_name);
        const hash_str: []const u8 = &hash;

        // Cache hit?
        if (try self.getCachedEmbedding(allocator, hash_str)) |cached| {
            return cached;
        }

        // Call embedding API with lowercase text
        const emb = self.embedder.embed(allocator, text_lower) catch |err| {
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

    // ── Source comment readers (Zig only) ──────────────────────────

    /// Extract `//!` module doc lines from Zig source.
    /// Returns an allocator-owned string or null.
    fn extractSourceModuleDoc(allocator: std.mem.Allocator, source: []const u8) !?[]u8 {
        var lines: std.ArrayList([]const u8) = .empty;
        defer lines.deinit(allocator);
        var it = std.mem.splitScalar(u8, source, '\n');
        while (it.next()) |line| {
            const trimmed = std.mem.trimLeft(u8, line, " \t");
            if (std.mem.startsWith(u8, trimmed, "//!")) {
                const after = trimmed[3..];
                const text = if (after.len > 0 and after[0] == ' ') after[1..] else after;
                try lines.append(allocator, text);
            } else {
                if (lines.items.len > 0) break;
            }
        }
        if (lines.items.len == 0) return null;
        var buf: std.ArrayList(u8) = .empty;
        errdefer buf.deinit(allocator);
        for (lines.items, 0..) |dl, i| {
            if (i > 0) try buf.append(allocator, '\n');
            try buf.appendSlice(allocator, dl);
        }
        const result = try buf.toOwnedSlice(allocator);
        if (result.len == 0) {
            allocator.free(result);
            return null;
        }
        return result;
    }

    /// Extract `///` doc comment above the declaration at 1-based `line`.
    /// Returns an allocator-owned string or null.
    fn extractSourceMemberDoc(allocator: std.mem.Allocator, source: []const u8, line: u32) !?[]u8 {
        if (line == 0) return null;
        var all_lines: std.ArrayList([]const u8) = .empty;
        defer all_lines.deinit(allocator);
        var it = std.mem.splitScalar(u8, source, '\n');
        while (it.next()) |ln| try all_lines.append(allocator, ln);
        if (line > all_lines.items.len) return null;

        var doc_lines: std.ArrayList([]const u8) = .empty;
        defer doc_lines.deinit(allocator);
        var idx = line - 1; // 0-based index of declaration
        while (idx > 0) {
            idx -= 1;
            const prev = std.mem.trimLeft(u8, all_lines.items[idx], " \t");
            if (!std.mem.startsWith(u8, prev, "///")) break;
            const after = prev[3..];
            const text = if (after.len > 0 and after[0] == ' ') after[1..] else after;
            try doc_lines.append(allocator, text);
        }
        if (doc_lines.items.len == 0) return null;
        std.mem.reverse([]const u8, doc_lines.items);

        var buf: std.ArrayList(u8) = .empty;
        errdefer buf.deinit(allocator);
        for (doc_lines.items, 0..) |dl, i| {
            if (i > 0) try buf.append(allocator, '\n');
            try buf.appendSlice(allocator, dl);
        }
        const result = try buf.toOwnedSlice(allocator);
        if (result.len == 0) {
            allocator.free(result);
            return null;
        }
        return result;
    }

    // ── Insert helpers ─────────────────────────────────────────────

    fn insertModule(self: *Self, allocator: std.mem.Allocator, file_path: []const u8, doc: ParsedDoc, mtime: i64, source_text: ?[]const u8) !void {
        // When JSON has no module comment (stripped for Zig source-code-first
        // workflow), read //! lines directly from the source file.
        const source_comment: ?[]u8 = if (doc.module_comment == null)
            if (source_text) |src| try extractSourceModuleDoc(allocator, src) else null
        else
            null;
        defer if (source_comment) |sc| allocator.free(sc);
        const effective_comment: ?[]const u8 = doc.module_comment orelse source_comment;

        const name = effective_comment orelse doc.module;

        // Build embedding text - use SHORT comment only (searchable phrase)
        // Do NOT embed full detail - it's comprehensive docs, not a search phrase
        // Keywords from detail are embedded separately in keyword_index table
        const emb_text = try buildEmbeddingText(allocator, doc.module, name, "module", effective_comment, null, null);
        defer allocator.free(emb_text);

        // Only generate embedding for modules with short searchable content
        // Embedding should be for "what does X do?" style queries
        const has_semantic_content = effective_comment != null and effective_comment.?.len > 0;
        const emb: ?[]f32 = if (has_semantic_content)
            try self.getOrComputeEmbedding(allocator, emb_text)
        else
            null;
        defer if (emb) |e| allocator.free(e);

        const emb_bytes: ?[]u8 = if (emb) |e| try vector.vecToBytes(allocator, e) else null;
        defer if (emb_bytes) |b| allocator.free(b);

        const model_name = self.embedder.getName();

        const sql =
            "INSERT INTO ast_nodes(" ++
            "  file_path, source, module, node_type, name, signature," ++
            "  comment, line, used_by, language, file_type, file_hash, last_modified," ++
            "  embedding, embedding_model, detail" ++
            ") VALUES (?1,?2,?3,'module',?4,NULL,?5,NULL,?6,?7,'source',?8,?9,?10,?11,?12)";

        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, file_path.ptr, @intCast(file_path.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 2, doc.source.ptr, @intCast(doc.source.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 3, doc.module.ptr, @intCast(doc.module.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 4, name.ptr, @intCast(name.len), SQLITE_STATIC);
        if (effective_comment) |cm|
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

        if (doc.detail) |d|
            _ = c.sqlite3_bind_text(stmt, 12, d.ptr, @intCast(d.len), SQLITE_STATIC)
        else
            _ = c.sqlite3_bind_null(stmt, 12);

        if (c.sqlite3_step(stmt) != c.SQLITE_DONE) return error.StepFailed;

        // Get the row ID for keyword linking
        const module_id = c.sqlite3_last_insert_rowid(self.db);

        // Sync keywords to keyword_index and keyword_modules tables
        if (doc.keywords.len > 0) {
            // Extract member names for direct-match filtering
            var member_names: std.ArrayList([]const u8) = .{};
            defer member_names.deinit(allocator);
            for (doc.members) |m| {
                try member_names.append(allocator, m.name);
            }
            try self.syncModuleKeywords(allocator, doc.keywords, module_id, member_names.items);
        }
    }

    /// Sync keywords for a module: embed each keyword and link to module.
    fn syncModuleKeywords(
        self: *Self,
        allocator: std.mem.Allocator,
        keywords: []const []const u8,
        module_id: i64,
        member_names: []const []const u8,
    ) !void {
        // Clear existing keyword links for this module
        try self.clearKeywordLinksForModule(module_id);

        for (keywords) |kw| {
            // Normalize keyword to lowercase for case-insensitive matching
            const kw_lower = std.ascii.allocLowerString(allocator, kw) catch continue;
            defer allocator.free(kw_lower);

            // Skip keywords that directly match AST names (deterministic search handles these)
            var is_direct_match = false;
            for (member_names) |name| {
                const name_lower = std.ascii.allocLowerString(allocator, name) catch continue;
                defer allocator.free(name_lower);
                if (std.mem.eql(u8, kw_lower, name_lower)) {
                    is_direct_match = true;
                    break;
                }
            }
            if (is_direct_match) {
                log.debug("skipping keyword '{s}' - direct match for AST name", .{kw});
                continue;
            }

            // Embed the keyword (lowercase for case-insensitive matching)
            const emb = self.embedder.embed(allocator, kw_lower) catch |err| {
                log.debug("failed to embed keyword '{s}': {s}", .{ kw, @errorName(err) });
                continue;
            };
            defer allocator.free(emb);

            // Store keyword embedding (idempotent, use lowercase keyword)
            self.storeKeywordEmbedding(allocator, kw_lower, emb) catch |err| {
                log.debug("failed to store keyword '{s}': {s}", .{ kw, @errorName(err) });
                continue;
            };

            // Link keyword to module (use lowercase keyword)
            self.linkKeywordToModule(kw_lower, module_id, 1.0) catch |err| {
                log.debug("failed to link keyword '{s}': {s}", .{ kw, @errorName(err) });
                continue;
            };
        }
    }

    fn insertMember(self: *Self, allocator: std.mem.Allocator, file_path: []const u8, doc: ParsedDoc, m: ParsedMember, mtime: i64, source_text: ?[]const u8) !void {
        // When JSON has no member comment (stripped for Zig source-code-first
        // workflow), read /// lines directly from the source file at m.line.
        const source_comment: ?[]u8 = if (m.comment == null)
            if (source_text) |src| if (m.line) |ln| try extractSourceMemberDoc(allocator, src, ln) else null else null
        else
            null;
        defer if (source_comment) |sc| allocator.free(sc);
        const effective_comment: ?[]const u8 = m.comment orelse source_comment;

        // Skip embeddings for test declarations - they add noise without semantic value
        const is_test = std.mem.eql(u8, m.node_type, "test_decl");

        // Only embed nodes with semantic content:
        // - Has a comment (primary signal for "what does this do?")
        // - Is a top-level struct/function (high-value navigation targets)
        // Module comment provides context for members without their own comment
        const has_comment = effective_comment != null and effective_comment.?.len > 0;
        const has_module_context = doc.module_comment != null and doc.module_comment.?.len > 0;
        const is_top_level = std.mem.eql(u8, m.node_type, "struct") or
            std.mem.eql(u8, m.node_type, "enum") or
            std.mem.eql(u8, m.node_type, "fn_decl");

        // Skip noisy comments: auto-generated tables, hex dumps, numeric arrays.
        const comment_is_noisy = if (effective_comment) |cm| common.isNoisyComment(cm) else false;

        // Skip embedding for: tests, members without comments, nested members without module context, noisy
        const should_embed = !is_test and !comment_is_noisy and (has_comment or (is_top_level and has_module_context));

        const emb: ?[]f32 = if (should_embed) blk: {
            const emb_text = try buildEmbeddingText(allocator, doc.module, m.name, m.node_type, effective_comment, m.signature, doc.module_comment);
            defer allocator.free(emb_text);
            break :blk try self.getOrComputeEmbedding(allocator, emb_text);
        } else null;
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
        if (effective_comment) |cm|
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

        // Index the embedding in the SimHash table for ANN pre-filtering.
        if (emb) |e| {
            const node_id = c.sqlite3_last_insert_rowid(self.db);
            const h = simhash.simhash(e);
            self.upsertSimhash(node_id, h);
        }
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
        detail: ?[]const u8 = null,
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
        if (r.detail) |d| allocator.free(d);
        for (r.used_by) |ub| allocator.free(ub);
        allocator.free(r.used_by);
        allocator.free(r.language);
    }

    /// Search with optional semantic alias expansion (alias → expanded tokens).
    ///
    /// Two-phase alias matching:
    /// 1. Embedding-based: Find alias keys similar to query embedding (cosine >= 0.75)
    /// 2. Token-based: Expand any exact token matches from semantic-aliases.json
    pub fn searchWithAliases(
        self: *Self,
        allocator: std.mem.Allocator,
        query_text: []const u8,
        limit: usize,
        aliases: ?SemanticAliases,
    ) ![]SearchResult {
        return self.searchWithAliasesOriginal(allocator, query_text, query_text, limit, aliases);
    }

    /// Search with separate original query for deterministic matching.
    pub fn searchWithAliasesOriginal(
        self: *Self,
        allocator: std.mem.Allocator,
        query_text: []const u8,
        original_query: []const u8,
        limit: usize,
        aliases: ?SemanticAliases,
    ) ![]SearchResult {
        const trimmed = std.mem.trim(u8, query_text, " \t\n\r");
        if (trimmed.len == 0) return allocator.alloc(SearchResult, 0);

        // Phase 1: Embedding-based alias key matching (query steering)
        var embedding_expansion: std.ArrayList([]const u8) = .{};
        defer {
            for (embedding_expansion.items) |t| allocator.free(t);
            embedding_expansion.deinit(allocator);
        }

        const query_emb = try self.getOrComputeEmbedding(allocator, trimmed);
        defer if (query_emb) |e| allocator.free(e);

        if (query_emb) |emb| {
            // Phase 1a: Embedding-based alias key matching — expand query tokens.
            const similar_keys = self.findSimilarAliasKeys(allocator, emb, 0.65, 3) catch &[_][]const u8{}; // was 0.75
            defer {
                for (similar_keys) |k| allocator.free(k);
                allocator.free(similar_keys);
            }

            // Get the values for each similar key from aliases (case-insensitive match)
            if (aliases) |ali| {
                for (similar_keys) |key| {
                    for (ali.aliases) |alias| {
                        // Keys are stored lowercase in DB, compare insensitively
                        if (std.ascii.eqlIgnoreCase(key, alias.key)) {
                            for (alias.values) |val| {
                                try embedding_expansion.append(allocator, try allocator.dupe(u8, val));
                            }
                            break;
                        }
                    }
                }
            }

            // Phase 1b: Capability-guided keyword expansion.
            // Match query embedding against capability embeddings; inject their
            // AST-level keywords into the search expansion so that NL queries
            // route to the correct identifiers in the enriched AST records.
            const cap_keywords = self.findCapabilityKeywordsForQuery(allocator, emb, 0.45, 3) catch &[_][]const u8{};
            defer {
                for (cap_keywords) |kw| allocator.free(kw);
                allocator.free(cap_keywords);
            }
            for (cap_keywords) |kw| {
                try embedding_expansion.append(allocator, try allocator.dupe(u8, kw));
            }
        }

        // Phase 2: Token-based alias expansion (existing logic)
        var expanded_tokens: std.ArrayList([]const u8) = .empty;
        defer expanded_tokens.deinit(allocator);

        // Add original query tokens
        var it = std.mem.tokenizeAny(u8, trimmed, " \t\n\r");
        while (it.next()) |tok| {
            try expanded_tokens.append(allocator, try allocator.dupe(u8, tok));
        }

        // Add embedding-based expansion tokens
        for (embedding_expansion.items) |tok| {
            try expanded_tokens.append(allocator, try allocator.dupe(u8, tok));
        }

        // Token-based alias expansion
        if (aliases) |ali| {
            const expanded = try ali.expandTokens(allocator, expanded_tokens.items);
            defer {
                for (expanded) |tok| allocator.free(tok);
                allocator.free(expanded);
            }

            // Build final query: original + embedding expansions + token expansions
            var final_query: std.ArrayList(u8) = .empty;
            defer final_query.deinit(allocator);

            // Original terms
            try final_query.appendSlice(allocator, trimmed);

            // Token-based expansions (from semantic-aliases.json)
            // Only add terms not already in the query
            var seen: std.StringHashMapUnmanaged(void) = .{};
            defer seen.deinit(allocator);
            for (expanded_tokens.items) |t| {
                try seen.put(allocator, t, {});
            }
            for (expanded) |tok| {
                if (!seen.contains(tok)) {
                    try final_query.append(allocator, ' ');
                    try final_query.appendSlice(allocator, tok);
                    try seen.put(allocator, tok, {});
                }
            }

            // Free temporary tokens
            for (expanded_tokens.items) |t| allocator.free(t);

            // Use searchWithOriginal to pass original query for deterministic matching
            return self.searchWithOriginal(allocator, final_query.items, original_query, limit);
        }

        // No aliases - build query from original + embedding expansions
        var final_query: std.ArrayList(u8) = .empty;
        defer final_query.deinit(allocator);

        try final_query.appendSlice(allocator, trimmed);
        for (embedding_expansion.items) |tok| {
            try final_query.append(allocator, ' ');
            try final_query.appendSlice(allocator, tok);
        }

        // Free original tokens
        for (expanded_tokens.items) |t| allocator.free(t);

        // Use searchWithOriginal to pass original query for deterministic matching
        return self.searchWithOriginal(allocator, final_query.items, original_query, limit);
    }

    pub fn search(
        self: *Self,
        allocator: std.mem.Allocator,
        query_text: []const u8,
        limit: usize,
    ) ![]SearchResult {
        return self.searchWithOriginal(allocator, query_text, query_text, limit);
    }

    /// Search with separate original query for deterministic matching.
    /// The original_query is used for Phase 1 (exact name match), while query_text
    /// is used for Phase 2-3 (vector/keyword search). This prevents expanded queries
    /// from accidentally matching AST names that were added by LLM expansion.
    pub fn searchWithOriginal(
        self: *Self,
        allocator: std.mem.Allocator,
        query_text: []const u8,
        original_query: []const u8,
        limit: usize,
    ) ![]SearchResult {
        const trimmed = std.mem.trim(u8, query_text, " \t\n\r");
        if (trimmed.len == 0) return allocator.alloc(SearchResult, 0);

        // ── Phases 1-3: Primary search (labeled block collects result) ─────────
        const primary: []SearchResult = primary: {
            // Phase 1: Deterministic name match (case-sensitive AST lookup).
            // Use ORIGINAL query so expanded tokens don't pollute exact matching.
            const orig_trimmed = std.mem.trim(u8, original_query, " \t\n\r");
            var query_tokens: std.ArrayList([]const u8) = .{};
            defer {
                for (query_tokens.items) |t| allocator.free(t);
                query_tokens.deinit(allocator);
            }
            {
                var tok_it = std.mem.tokenizeAny(u8, orig_trimmed, " \t\n\r_-");
                while (tok_it.next()) |tok| {
                    if (tok.len >= 2 and looksLikeIdentifier(tok)) {
                        try query_tokens.append(allocator, try allocator.dupe(u8, tok));
                    }
                }
            }
            for (query_tokens.items) |token| {
                const exact_match = self.findExactNameMatch(allocator, token, limit) catch continue;
                if (exact_match.len > 0) {
                    log.debug("deterministic name match for token '{s}' in query '{s}'", .{ token, orig_trimmed });
                    break :primary exact_match;
                }
            }

            // Phase 2: Keyword vector search (semantic index, case-insensitive).
            // Strip NL interrogative prefix before embedding.
            const embed_query = stripNlPrefix(trimmed);
            const query_emb = try self.getOrComputeEmbedding(allocator, embed_query);
            defer if (query_emb) |e| allocator.free(e);

            if (query_emb) |_| {
                if (self.keywordIndexSearch(allocator, embed_query, limit)) |kw_results| {
                    if (kw_results.len > 0) {
                        log.debug("keyword vector search found {d} results for '{s}'", .{ kw_results.len, embed_query });
                        break :primary kw_results;
                    }
                } else |_| {}
            }

            // Phase 3: Hybrid search (vector + keyword LIKE).
            const has_embeddings = query_emb != null and query_emb.?.len > 0;
            break :primary if (has_embeddings)
                try self.hybridSearch(allocator, embed_query, query_emb.?, limit)
            else
                try self.keywordSearch(allocator, embed_query, limit);
        };

        // ── Phase 4: One-hop see-also expansion ──────────────────────────────────
        // When top result has sufficient confidence and there is room in the limit,
        // append module rows from the top result's `used_by` edge list.
        if (primary.len == 0 or primary.len >= limit) return primary;

        const extras = self.seeAlsoExpand(allocator, primary, limit - primary.len) catch return primary;
        if (extras.len == 0) {
            allocator.free(extras);
            return primary;
        }

        // Merge: allocate a flat slice and memcpy both halves.
        // allocator.free on a SearchResult slice only frees the backing array,
        // not the strings inside — ownership of those transfers to `merged`.
        const merged = allocator.alloc(SearchResult, primary.len + extras.len) catch {
            for (extras) |r| GuidanceDb.freeSearchResult(allocator, r);
            allocator.free(extras);
            return primary;
        };
        @memcpy(merged[0..primary.len], primary);
        allocator.free(primary);
        @memcpy(merged[primary.len..], extras);
        allocator.free(extras);
        return merged;
    }

    /// Delegates to src/common/str.looksLikeIdentifier.
    const looksLikeIdentifier = common.looksLikeIdentifier;
    /// Delegates to src/common/str.stripNlPrefix.
    const stripNlPrefix = common.stripNlPrefix;

    /// One-hop "see also" expansion: given primary results, fetch the module rows
    /// of modules that the top result's module lists in its `used_by` field.
    ///
    /// Returns an owned slice of additional SearchResult items.  Each result's
    /// score is `top_score × SEE_ALSO_DECAY`.  Deduplicates against `primary`.
    /// Returns an empty slice (not an error) on any lookup failure.
    fn seeAlsoExpand(
        self: *Self,
        allocator: std.mem.Allocator,
        primary: []const SearchResult,
        limit: usize,
    ) ![]SearchResult {
        if (primary.len == 0 or limit == 0) return allocator.alloc(SearchResult, 0);

        const SEE_ALSO_DECAY: f32 = 0.6;
        const top = primary[0];
        const top_score: f32 = @floatCast(top.score);

        // Only expand when top result has meaningful similarity
        if (top_score < 0.40) return allocator.alloc(SearchResult, 0);

        // 1. Fetch the used_by JSON array for the top result's module node
        const ub_sql = "SELECT used_by FROM ast_nodes WHERE module = ?1 AND node_type = 'module' LIMIT 1";
        var ub_stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, ub_sql, -1, &ub_stmt, null) != c.SQLITE_OK)
            return allocator.alloc(SearchResult, 0);
        defer _ = c.sqlite3_finalize(ub_stmt);

        _ = c.sqlite3_bind_text(ub_stmt, 1, top.module.ptr, @intCast(top.module.len), SQLITE_STATIC);
        if (c.sqlite3_step(ub_stmt) != c.SQLITE_ROW) return allocator.alloc(SearchResult, 0);

        const ub = try parseUsedByCol(ub_stmt.?, 0, allocator);
        defer {
            for (ub) |m| allocator.free(m);
            allocator.free(ub);
        }
        if (ub.len == 0) return allocator.alloc(SearchResult, 0);

        // 2. Build set of already-returned modules for deduplication
        var seen: std.StringHashMapUnmanaged(void) = .{};
        defer seen.deinit(allocator);
        for (primary) |r| {
            try seen.put(allocator, r.module, {});
        }

        // 3. For each referenced module, fetch its module row
        const mod_sql = "SELECT file_path, source, module, node_type, name, signature, " ++
            "       comment, line, used_by, language, detail " ++
            "FROM ast_nodes WHERE module = ?1 AND node_type = 'module' LIMIT 1";
        var mod_stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, mod_sql, -1, &mod_stmt, null) != c.SQLITE_OK)
            return allocator.alloc(SearchResult, 0);
        defer _ = c.sqlite3_finalize(mod_stmt);

        var extras: std.ArrayList(SearchResult) = .{};
        errdefer {
            for (extras.items) |r| GuidanceDb.freeSearchResult(allocator, r);
            extras.deinit(allocator);
        }

        for (ub) |mod_name| {
            if (extras.items.len >= limit) break;
            if (seen.contains(mod_name)) continue;

            _ = c.sqlite3_reset(mod_stmt);
            _ = c.sqlite3_bind_text(mod_stmt, 1, mod_name.ptr, @intCast(mod_name.len), SQLITE_STATIC);
            if (c.sqlite3_step(mod_stmt) != c.SQLITE_ROW) continue;

            var r = try readRowResult(mod_stmt.?, allocator);
            r.score = top_score * SEE_ALSO_DECAY;
            try extras.append(allocator, r);
            try seen.put(allocator, r.module, {});
        }

        log.debug("seeAlsoExpand: added {d} see-also results for module '{s}'", .{ extras.items.len, top.module });
        return extras.toOwnedSlice(allocator);
    }

    /// Find exact case-sensitive name match in AST.
    /// Returns results if the name exists in the database.
    pub fn findExactNameMatch(
        self: *Self,
        allocator: std.mem.Allocator,
        name: []const u8,
        limit: usize,
    ) ![]SearchResult {
        const sql = "SELECT file_path, source, module, node_type, name, signature," ++
            "       comment, line, used_by, language, detail " ++
            "FROM ast_nodes WHERE name = ?1 AND node_type != 'test_decl' " ++
            "ORDER BY last_modified DESC LIMIT ?2";

        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) {
            return allocator.alloc(SearchResult, 0);
        }
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, name.ptr, @intCast(name.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_int64(stmt, 2, @intCast(limit));

        var results: std.ArrayList(SearchResult) = .{};
        errdefer {
            for (results.items) |r| freeSearchResult(allocator, r);
            results.deinit(allocator);
        }

        while (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            var r = try readRowResult(stmt.?, allocator);
            r.score = 1.0; // Exact match gets highest score
            try results.append(allocator, r);
        }

        return results.toOwnedSlice(allocator);
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
        // ScoredRow: holds id + score, then we resolve later
        const ScoredRow = struct {
            id: i64,
            score: f32,
        };
        var scored: std.ArrayList(ScoredRow) = .{};
        defer scored.deinit(allocator);

        // ── SimHash pre-filter (fast path when index is populated) ──────────
        // Compute the query's SimHash and find candidates within maxHamming bits.
        // Falls back to linear scan when the simhash_index is empty (first sync).
        const q_hash = simhash.simhash(query_embedding);
        const max_h = simhash.maxHamming(MIN_VECTOR_THRESHOLD);
        const candidates = self.getSimhashCandidates(allocator, q_hash, max_h) catch &[_]i64{};
        defer if (candidates.len > 0) allocator.free(candidates);

        if (candidates.len > 0) {
            // Fast path: full cosine only on SimHash candidates (typically < 500 of 50K).
            for (candidates) |node_id| {
                const emb = self.getEmbeddingById(allocator, node_id) catch continue orelse continue;
                defer allocator.free(emb);
                const sim = vector.cosineSimilarity(query_embedding, emb);
                if (sim < MIN_VECTOR_THRESHOLD) continue;
                try scored.append(allocator, .{ .id = node_id, .score = sim });
            }
        } else {
            // Slow path: linear scan of all embeddings (used before simhash_index is built).
            // idx_gdb_has_emb covers `embedding IS NOT NULL`.
            // Excluding test_decl saves ~10-15% of the scan.
            const sql =
                "SELECT id, embedding FROM ast_nodes " ++
                "WHERE embedding IS NOT NULL AND node_type != 'test_decl' " ++
                "ORDER BY last_modified DESC LIMIT 2000";

            var stmt: ?*c.sqlite3_stmt = null;
            if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) == c.SQLITE_OK) {
                defer _ = c.sqlite3_finalize(stmt);
                while (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
                    const id = c.sqlite3_column_int64(stmt, 0);
                    const emb_raw = c.sqlite3_column_blob(stmt, 1);
                    const emb_len: usize = @intCast(c.sqlite3_column_bytes(stmt, 1));
                    if (emb_raw == null or emb_len == 0) continue;
                    const emb_bytes: []const u8 = @as([*]const u8, @ptrCast(emb_raw))[0..emb_len];
                    const emb = vector.bytesToVec(allocator, emb_bytes) catch continue;
                    defer allocator.free(emb);
                    const sim = vector.cosineSimilarity(query_embedding, emb);
                    if (sim < MIN_VECTOR_THRESHOLD) continue;
                    try scored.append(allocator, .{ .id = id, .score = sim });
                }
            }
        }

        // Sort by score descending
        std.mem.sortUnstable(ScoredRow, scored.items, {}, struct {
            fn lt(_: void, a: ScoredRow, b: ScoredRow) bool {
                return a.score > b.score;
            }
        }.lt);

        // Adaptive threshold: discard results that are far below the top score.
        // Formula: max(0.30, 0.60 × top_score).  // was max(0.25, 0.75 × top_score)
        // Example: top=0.80 → cutoff=0.48 (was 0.60, less aggressive pruning).
        //          top=0.30 → cutoff=0.30 (was 0.25, slightly higher floor).
        if (scored.items.len > 0) {
            const adaptive_cutoff: f32 = @max(0.30, 0.60 * scored.items[0].score); // was 0.75
            // scored is sorted descending; find the first item below cutoff.
            var keep: usize = scored.items.len;
            for (scored.items, 0..) |sr, i| {
                if (sr.score < adaptive_cutoff) {
                    keep = i;
                    break;
                }
            }
            if (keep < scored.items.len) {
                log.debug("adaptive threshold {d:.3} pruned {d} → {d} candidates", .{
                    adaptive_cutoff, scored.items.len, keep,
                });
                scored.items.len = keep;
            }
        }

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
            "       comment, line, used_by, language, detail, (" ++
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
            // Column 11 is match_score
            const raw_score: f64 = @floatFromInt(c.sqlite3_column_int(stmt, 11));
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
    /// M5.2: Three-way fusion with capability keywords.
    /// Weights: cosine × 0.60 + BM25 × 0.25 + capability × 0.15
    fn hybridSearch(
        self: *Self,
        allocator: std.mem.Allocator,
        query_text: []const u8,
        query_embedding: []const f32,
        limit: usize,
    ) ![]SearchResult {
        // Run all three searches at 3x limit for fusion
        const triple_limit = limit * 3;

        // ── Vector results ──
        const vec_results = self.vectorSearch(allocator, query_embedding, triple_limit) catch &[_]SearchResult{};
        defer {
            for (vec_results) |r| freeSearchResult(allocator, r);
            allocator.free(vec_results);
        }

        // ── Keyword results (BM25 on lod text) ──
        const kw_results = self.keywordSearch(allocator, query_text, triple_limit) catch &[_]SearchResult{};
        defer {
            for (kw_results) |r| freeSearchResult(allocator, r);
            allocator.free(kw_results);
        }

        // ── Capability keyword results ──
        const cap_results = self.capabilityKeywordSearch(allocator, query_text, triple_limit) catch &[_]SearchResult{};
        defer {
            for (cap_results) |r| freeSearchResult(allocator, r);
            allocator.free(cap_results);
        }

        // Build IdScore arrays for three-way merging
        var vec_ids: std.ArrayList(vector.IdScore) = .empty;
        defer vec_ids.deinit(allocator);
        for (vec_results) |r| {
            const row_id = self.lookupId(r.file_path, r.module, r.name, r.node_type) catch continue;
            try vec_ids.append(allocator, .{ .id = row_id, .score = @floatCast(r.score) });
        }

        var kw_ids: std.ArrayList(vector.IdScore) = .empty;
        defer kw_ids.deinit(allocator);
        for (kw_results) |r| {
            const row_id = self.lookupId(r.file_path, r.module, r.name, r.node_type) catch continue;
            try kw_ids.append(allocator, .{ .id = row_id, .score = @floatCast(r.score) });
        }

        var cap_ids: std.ArrayList(vector.IdScore) = .empty;
        defer cap_ids.deinit(allocator);
        for (cap_results) |r| {
            const row_id = self.lookupId(r.file_path, r.module, r.name, r.node_type) catch continue;
            try cap_ids.append(allocator, .{ .id = row_id, .score = @floatCast(r.score) });
        }

        // Three-way hybrid merge (M5.2 weights: 0.60, 0.25, 0.15)
        const merged = try vector.hybridMergeThree(
            allocator,
            vec_ids.items,
            kw_ids.items,
            cap_ids.items,
            0.60, // cosine weight
            0.25, // BM25 weight
            0.15, // capability keyword weight
            triple_limit,
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

    /// Capability keyword search: match query terms against capability_keywords table.
    fn capabilityKeywordSearch(
        self: *Self,
        allocator: std.mem.Allocator,
        query_text: []const u8,
        limit: usize,
    ) ![]SearchResult {
        _ = limit;
        var results: std.ArrayList(SearchResult) = .empty;
        errdefer {
            for (results.items) |r| freeSearchResult(allocator, r);
            results.deinit(allocator);
        }

        // Tokenize query into lowercase keywords
        var keywords: std.ArrayList([]const u8) = .empty;
        defer {
            for (keywords.items) |kw| allocator.free(kw);
            keywords.deinit(allocator);
        }

        var iter = std.mem.tokenizeAny(u8, query_text, " \t\n\r,.;:!?\"'()[]{}/\\");
        while (iter.next()) |token| {
            const lower = try std.ascii.allocLowerString(allocator, token);
            try keywords.append(allocator, lower);
        }

        if (keywords.items.len == 0) return allocator.alloc(SearchResult, 0);

        // Simplified: just match any keyword from capability_keywords
        var stmt: ?*c.sqlite3_stmt = null;
        const simple_sql = "SELECT DISTINCT a.id, a.file_path, a.module, a.name, a.node_type, a.lod0 " ++
            "FROM ast_nodes a " ++
            "JOIN capability_keywords ck ON ck.capability_name = a.name ";

        if (c.sqlite3_prepare_v2(self.db, simple_sql, -1, &stmt, null) != c.SQLITE_OK) {
            return allocator.alloc(SearchResult, 0);
        }
        defer _ = c.sqlite3_finalize(stmt);

        var seen_ids = std.AutoHashMap(i64, void).init(allocator);
        defer seen_ids.deinit();

        var scored_results: std.ArrayList(struct {
            id: i64,
            score: f32,
            file_path: []const u8,
            module: []const u8,
            name: []const u8,
            node_type: []const u8,
            lod0: []const u8,
        }) = .empty;
        defer scored_results.deinit(allocator);

        while (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            const row_id = c.sqlite3_column_int64(stmt, 0);
            if (seen_ids.contains(row_id)) continue;
            try seen_ids.put(row_id, {});

            const fp = try dupeCol(stmt.?, 1, allocator);
            const mod = try dupeCol(stmt.?, 2, allocator);
            const nm = try dupeCol(stmt.?, 3, allocator);
            const nt = try dupeCol(stmt.?, 4, allocator);
            const lod0 = try dupeCol(stmt.?, 5, allocator);

            try scored_results.append(allocator, .{
                .id = row_id,
                .score = 1.0, // Default score for capability match
                .file_path = fp,
                .module = mod,
                .name = nm,
                .node_type = nt,
                .lod0 = lod0,
            });
        }

        for (scored_results.items) |sr| {
            try results.append(allocator, .{
                .file_path = sr.file_path,
                .source = sr.lod0,
                .module = sr.module,
                .name = sr.name,
                .node_type = sr.node_type,
                .signature = null,
                .comment = null,
                .detail = null,
                .line = null,
                .used_by = &[_][]const u8{},
                .language = "unknown",
                .score = sr.score,
            });
        }

        return results.toOwnedSlice(allocator);
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
            "       comment, line, used_by, language, detail " ++
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

    /// Read columns 0..10: file_path, source, module, node_type, name, signature,
    /// comment, line, used_by, language, detail.  Score is set to 0.0 (caller overrides).
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
            .detail = try dupeColNullable(stmt, 10, allocator),
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

    // ── Semantic Alias Embeddings ────────────────────────────────────

    /// Store an embedding for a semantic alias key.
    /// Used during `guidance gen` to pre-compute embeddings for alias expansion.
    pub fn storeAliasEmbedding(
        self: *Self,
        allocator: std.mem.Allocator,
        alias_key: []const u8,
        embedding: []const f32,
    ) !void {
        const model_name = self.embedder.getName();
        const emb_bytes = try vector.vecToBytes(allocator, embedding);
        defer allocator.free(emb_bytes);

        const sql = "INSERT OR REPLACE INTO semantic_alias_embeddings(alias_key, embedding, embedding_model) VALUES(?1,?2,?3)";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, alias_key.ptr, @intCast(alias_key.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_blob(stmt, 2, emb_bytes.ptr, @intCast(emb_bytes.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 3, model_name.ptr, @intCast(model_name.len), SQLITE_STATIC);

        if (c.sqlite3_step(stmt) != c.SQLITE_DONE) return error.StepFailed;
    }

    /// Find semantic alias keys whose embeddings are similar to the query embedding.
    /// Returns keys with cosine similarity >= threshold, sorted by similarity descending.
    /// Caller owns the returned slice of strings.
    pub fn findSimilarAliasKeys(
        self: *Self,
        allocator: std.mem.Allocator,
        query_embedding: []const f32,
        threshold: f32,
        limit: usize,
    ) ![]const []const u8 {
        const sql = "SELECT alias_key, embedding FROM semantic_alias_embeddings";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) {
            return allocator.alloc([]const u8, 0);
        }
        defer _ = c.sqlite3_finalize(stmt);

        const ScoredKey = struct {
            key: []const u8,
            score: f32,
        };

        var scored: std.ArrayList(ScoredKey) = .{};
        errdefer {
            for (scored.items) |sk| allocator.free(sk.key);
            scored.deinit(allocator);
        }

        while (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            const key = try dupeCol(stmt.?, 0, allocator);
            const emb_blob = c.sqlite3_column_blob(stmt.?, 1);
            const emb_len: usize = @intCast(c.sqlite3_column_bytes(stmt.?, 1));
            if (emb_blob == null or emb_len == 0) {
                allocator.free(key);
                continue;
            }
            const bytes: []const u8 = @as([*]const u8, @ptrCast(emb_blob))[0..emb_len];
            const stored_emb = vector.bytesToVec(allocator, bytes) catch {
                allocator.free(key);
                continue;
            };
            defer allocator.free(stored_emb);

            const sim = vector.cosineSimilarity(query_embedding, stored_emb);
            if (sim >= threshold) {
                try scored.append(allocator, .{ .key = key, .score = sim });
            } else {
                allocator.free(key);
            }
        }

        // Sort by score descending
        std.sort.block(ScoredKey, scored.items, {}, struct {
            fn lessThan(_: void, a: ScoredKey, b: ScoredKey) bool {
                return a.score > b.score;
            }
        }.lessThan);

        // Return top N keys (ownership transferred to caller)
        const count = @min(limit, scored.items.len);
        var result = try allocator.alloc([]const u8, count);
        for (0..count) |i| {
            result[i] = scored.items[i].key;
        }
        // Free unused keys
        for (count..scored.items.len) |i| {
            allocator.free(scored.items[i].key);
        }
        scored.deinit(allocator);
        return result;
    }

    /// Sync semantic alias embeddings from the aliases file.
    /// Pre-computes embeddings for all alias keys for fast query steering.
    /// Keys are lowercased for case-insensitive matching.
    pub fn syncAliasEmbeddings(
        self: *Self,
        allocator: std.mem.Allocator,
        aliases: SemanticAliases,
    ) !void {
        var synced: usize = 0;
        var failed: usize = 0;
        var skipped: usize = 0;
        const model_name = self.embedder.getName();
        const total = aliases.aliases.len;
        std.debug.print("syncAliasEmbeddings: processing {d} aliases with embedder '{s}'\n", .{ total, model_name });
        for (aliases.aliases) |alias| {
            // Lowercase the key for case-insensitive embedding comparison
            const key_lower = std.ascii.allocLowerString(allocator, alias.key) catch {
                std.debug.print("error: failed to lowercase alias key '{s}'\n", .{alias.key});
                failed += 1;
                continue;
            };
            defer allocator.free(key_lower);

            // Skip when this (alias_key, embedding_model) pair is already stored.
            const already_stored: bool = blk: {
                const chk = "SELECT COUNT(*) FROM semantic_alias_embeddings WHERE alias_key=?1 AND embedding_model=?2";
                var chk_stmt: ?*c.sqlite3_stmt = null;
                if (c.sqlite3_prepare_v2(self.db, chk, -1, &chk_stmt, null) != c.SQLITE_OK) break :blk false;
                defer _ = c.sqlite3_finalize(chk_stmt);
                _ = c.sqlite3_bind_text(chk_stmt, 1, key_lower.ptr, @intCast(key_lower.len), SQLITE_STATIC);
                _ = c.sqlite3_bind_text(chk_stmt, 2, model_name.ptr, @intCast(model_name.len), SQLITE_STATIC);
                break :blk c.sqlite3_step(chk_stmt) == c.SQLITE_ROW and c.sqlite3_column_int(chk_stmt, 0) > 0;
            };
            if (already_stored) {
                skipped += 1;
                continue;
            }

            // Embed the lowercase alias key
            const emb = self.embedder.embed(allocator, key_lower) catch |err| {
                std.debug.print("error: failed to embed alias key '{s}': {s}\n", .{ alias.key, @errorName(err) });
                failed += 1;
                continue;
            };
            defer allocator.free(emb);

            // Store with lowercase key for consistent lookup
            self.storeAliasEmbedding(allocator, key_lower, emb) catch |err| {
                std.debug.print("error: failed to store alias embedding '{s}': {s}\n", .{ alias.key, @errorName(err) });
                failed += 1;
                continue;
            };
            synced += 1;
        }
        std.debug.print("syncAliasEmbeddings: {d}/{d} synced, {d} skipped, {d} failed\n", .{ synced, total, skipped, failed });
    }

    // ── Keyword Index Methods ─────────────────────────────────────

    /// Store a keyword with its embedding. Idempotent (INSERT OR REPLACE).
    pub fn storeKeywordEmbedding(
        self: *Self,
        allocator: std.mem.Allocator,
        keyword: []const u8,
        embedding: []const f32,
    ) !void {
        const emb_bytes = try vector.vecToBytes(allocator, embedding);
        defer allocator.free(emb_bytes);

        const model_name = self.embedder.getName();
        const sql = "INSERT OR REPLACE INTO keyword_index(keyword, embedding, embedding_model) VALUES (?1, ?2, ?3)";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, keyword.ptr, @intCast(keyword.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_blob(stmt, 2, emb_bytes.ptr, @intCast(emb_bytes.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 3, model_name.ptr, @intCast(model_name.len), SQLITE_STATIC);

        if (c.sqlite3_step(stmt) != c.SQLITE_DONE) return error.StepFailed;
    }

    /// Link a keyword to a module. Idempotent (PRIMARY KEY).
    pub fn linkKeywordToModule(
        self: *Self,
        keyword: []const u8,
        module_id: i64,
        relevance: f32,
    ) !void {
        const sql = "INSERT OR REPLACE INTO keyword_modules(keyword, module_id, relevance) VALUES (?1, ?2, ?3)";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, keyword.ptr, @intCast(keyword.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_int64(stmt, 2, module_id);
        _ = c.sqlite3_bind_double(stmt, 3, @floatCast(relevance));

        if (c.sqlite3_step(stmt) != c.SQLITE_DONE) return error.StepFailed;
    }

    /// Find keywords similar to query embedding (vector search on keyword_index).
    /// Returns owned slice of owned keyword strings.
    pub fn findSimilarKeywords(
        self: *Self,
        allocator: std.mem.Allocator,
        query_embedding: []const f32,
        threshold: f32,
        limit: usize,
    ) ![]KeywordMatch {
        const sql = "SELECT keyword, embedding FROM keyword_index";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        var scored: std.ArrayList(KeywordMatch) = .{};
        errdefer {
            for (scored.items) |s| allocator.free(s.keyword);
            scored.deinit(allocator);
        }

        while (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            const kw_ptr = c.sqlite3_column_text(stmt, 0);
            const kw_len: usize = @intCast(c.sqlite3_column_bytes(stmt, 0));
            if (kw_ptr == null or kw_len == 0) continue;

            const kw = try allocator.dupe(u8, kw_ptr[0..kw_len]);

            const emb_blob = c.sqlite3_column_blob(stmt, 1);
            const emb_len: usize = @intCast(c.sqlite3_column_bytes(stmt, 1));
            if (emb_blob == null or emb_len == 0) {
                allocator.free(kw);
                continue;
            }

            const bytes: []const u8 = @as([*]const u8, @ptrCast(emb_blob))[0..emb_len];
            const stored_emb = vector.bytesToVec(allocator, bytes) catch {
                allocator.free(kw);
                continue;
            };
            defer allocator.free(stored_emb);

            const sim = vector.cosineSimilarity(query_embedding, stored_emb);
            if (sim >= threshold) {
                log.debug("keyword '{s}' similarity {d:.3} >= threshold {d:.3}", .{ kw, sim, threshold });
                try scored.append(allocator, .{ .keyword = kw, .score = sim });
            } else {
                allocator.free(kw);
            }
        }

        // Sort by score descending
        std.sort.block(@TypeOf(scored.items[0]), scored.items, {}, struct {
            fn lessThan(_: void, a: @TypeOf(scored.items[0]), b: @TypeOf(scored.items[0])) bool {
                return a.score > b.score;
            }
        }.lessThan);

        // Trim to limit
        const count = @min(limit, scored.items.len);
        for (count..scored.items.len) |i| {
            allocator.free(scored.items[i].keyword);
        }
        scored.items.len = count;

        return scored.toOwnedSlice(allocator);
    }

    /// Find module IDs for a set of keywords.
    /// Returns owned slice of module IDs with relevance scores.
    pub fn findModulesForKeywords(
        self: *Self,
        allocator: std.mem.Allocator,
        keywords: []const []const u8,
    ) ![]ModuleMatch {
        if (keywords.len == 0) return &[_]ModuleMatch{};

        // Build IN clause
        var in_clause: std.ArrayList(u8) = .{};
        defer in_clause.deinit(allocator);
        const w = in_clause.writer(allocator);
        for (keywords, 0..) |_, i| {
            if (i > 0) try w.writeAll(", ");
            try w.print("?", .{});
        }

        const sql_base = "SELECT DISTINCT module_id, SUM(relevance) as total_rel FROM keyword_modules WHERE keyword IN (";
        const sql_end = ") GROUP BY module_id ORDER BY total_rel DESC";
        const sql = try std.fmt.allocPrint(allocator, "{s}{s}{s}", .{ sql_base, in_clause.items, sql_end });
        defer allocator.free(sql);

        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql.ptr, -1, &stmt, null) != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        for (keywords, 1..) |kw, i| {
            _ = c.sqlite3_bind_text(stmt, @intCast(i), kw.ptr, @intCast(kw.len), SQLITE_STATIC);
        }

        var results: std.ArrayList(ModuleMatch) = .{};
        errdefer results.deinit(allocator);

        while (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            const module_id = c.sqlite3_column_int64(stmt, 0);
            const relevance: f32 = @floatCast(c.sqlite3_column_double(stmt, 1));
            try results.append(allocator, .{ .module_id = module_id, .relevance = relevance });
        }

        return results.toOwnedSlice(allocator);
    }

    /// Fetch module detail by ID.
    /// Returns owned detail string or null.
    pub fn fetchModuleDetail(
        self: *Self,
        allocator: std.mem.Allocator,
        module_id: i64,
    ) !?[]const u8 {
        const sql = "SELECT detail FROM ast_nodes WHERE id = ?1 AND node_type = 'module'";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_int64(stmt, 1, module_id);

        if (c.sqlite3_step(stmt) != c.SQLITE_ROW) return null;

        const detail_ptr = c.sqlite3_column_text(stmt, 0);
        const detail_len: usize = @intCast(c.sqlite3_column_bytes(stmt, 0));
        if (detail_ptr == null or detail_len == 0) return null;

        return allocator.dupe(u8, detail_ptr[0..detail_len]);
    }

    /// Update module detail and comment by ID.
    pub fn updateModuleDetail(
        self: *Self,
        module_id: i64,
        detail: []const u8,
        comment: []const u8,
    ) !void {
        const sql = "UPDATE ast_nodes SET detail = ?1, comment = ?2 WHERE id = ?3";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, detail.ptr, @intCast(detail.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 2, comment.ptr, @intCast(comment.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_int64(stmt, 3, module_id);

        if (c.sqlite3_step(stmt) != c.SQLITE_DONE) return error.StepFailed;
    }

    /// Recompute IDF weights for all keyword → module links.
    ///
    /// IDF(kw) = log₂(total_modules / df) + 1, where df = count of distinct modules
    /// that share this keyword.  Rare keywords (small df) get higher relevance;
    /// keywords present in every module approach 1.0.
    ///
    /// Called once after a full sync pass so weights stay current across incremental
    /// syncs where individual modules are updated independently.
    fn rebuildKeywordIdf(self: *Self) !usize {
        // 1. Total module count
        const total_modules: i64 = blk: {
            var cnt_stmt: ?*c.sqlite3_stmt = null;
            if (c.sqlite3_prepare_v2(self.db, "SELECT COUNT(*) FROM ast_nodes WHERE node_type = 'module'", -1, &cnt_stmt, null) != c.SQLITE_OK)
                return error.PrepareFailed;
            defer _ = c.sqlite3_finalize(cnt_stmt);
            break :blk if (c.sqlite3_step(cnt_stmt) == c.SQLITE_ROW) c.sqlite3_column_int64(cnt_stmt, 0) else 0;
        };
        if (total_modules == 0) return 0;

        // 2. Iterate (keyword, df) and update each row in the same pass.
        //    kw_ptr is valid until the next sqlite3_step on sel_stmt; upd_stmt
        //    is stepped before we advance sel_stmt, so the pointer stays alive.
        const sel_sql = "SELECT keyword, COUNT(DISTINCT module_id) FROM keyword_modules GROUP BY keyword";
        var sel_stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sel_sql, -1, &sel_stmt, null) != c.SQLITE_OK)
            return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(sel_stmt);

        const upd_sql = "UPDATE keyword_modules SET relevance = ?1 WHERE keyword = ?2";
        var upd_stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, upd_sql, -1, &upd_stmt, null) != c.SQLITE_OK)
            return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(upd_stmt);

        var updated: usize = 0;
        const n: f32 = @floatFromInt(total_modules);

        while (c.sqlite3_step(sel_stmt) == c.SQLITE_ROW) {
            const kw_ptr = c.sqlite3_column_text(sel_stmt, 0);
            const kw_len: usize = @intCast(c.sqlite3_column_bytes(sel_stmt, 0));
            if (kw_ptr == null or kw_len == 0) continue;

            const df: i64 = c.sqlite3_column_int64(sel_stmt, 1);
            if (df <= 0) continue;

            // IDF ≥ 1.0 always (log₂ of ratio ≥ 1 when df ≤ n)
            const idf: f32 = @max(1.0, std.math.log2(n / @as(f32, @floatFromInt(df))) + 1.0);

            _ = c.sqlite3_reset(upd_stmt);
            _ = c.sqlite3_bind_double(upd_stmt, 1, @floatCast(idf));
            _ = c.sqlite3_bind_text(upd_stmt, 2, kw_ptr, @intCast(kw_len), SQLITE_STATIC);
            _ = c.sqlite3_step(upd_stmt);
            updated += 1;
        }

        log.debug("rebuildKeywordIdf: updated {d} keyword weights (N={d})", .{ updated, total_modules });
        return updated;
    }

    /// Delete all keyword links for a module (before re-syncing).
    pub fn clearKeywordLinksForModule(self: *Self, module_id: i64) !void {
        const sql = "DELETE FROM keyword_modules WHERE module_id = ?1";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return error.PrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_int64(stmt, 1, module_id);
        _ = c.sqlite3_step(stmt);
    }

    /// Keyword-based search: query → keyword match → module lookup.
    /// Returns SearchResult slice with detail populated.
    pub fn keywordIndexSearch(
        self: *Self,
        allocator: std.mem.Allocator,
        query_text: []const u8,
        limit: usize,
    ) ![]SearchResult {
        // 1. Embed query
        const query_emb = try self.getOrComputeEmbedding(allocator, query_text);
        defer if (query_emb) |e| allocator.free(e);

        if (query_emb == null) return allocator.alloc(SearchResult, 0);

        // 2. Find matching keywords
        const kw_matches = try self.findSimilarKeywords(allocator, query_emb.?, 0.50, 10);
        defer {
            for (kw_matches) |km| allocator.free(km.keyword);
            allocator.free(kw_matches);
        }

        if (kw_matches.len == 0) return allocator.alloc(SearchResult, 0);

        // 3. Extract keyword strings
        var keywords: std.ArrayList([]const u8) = .{};
        defer keywords.deinit(allocator);
        for (kw_matches) |km| {
            try keywords.append(allocator, km.keyword);
        }

        // 4. Find modules for keywords
        const module_matches = try self.findModulesForKeywords(allocator, keywords.items);
        defer allocator.free(module_matches);

        if (module_matches.len == 0) return allocator.alloc(SearchResult, 0);

        // 5. Fetch module details
        var results: std.ArrayList(SearchResult) = .{};
        errdefer {
            for (results.items) |r| freeSearchResult(allocator, r);
            results.deinit(allocator);
        }

        const actual_limit = @min(limit, module_matches.len);
        for (module_matches[0..actual_limit]) |mm| {
            const row = self.fetchById(allocator, mm.module_id) catch continue;
            log.debug("keywordIndexSearch fetched module id={d} module={s} name={s}", .{ mm.module_id, row.module, row.name });
            try results.append(allocator, row);
        }

        log.debug("keywordIndexSearch returning {d} results", .{results.items.len});
        return results.toOwnedSlice(allocator);
    }

    /// Embedding statistics for verbose status output.
    pub const EmbeddingStats = struct {
        ast_nodes_with_embeddings: usize,
        alias_embeddings: usize,
        keyword_embeddings: usize,
        embedding_cache_entries: usize,
    };

    /// Get embedding statistics from the database.
    /// Returns null if database is not open or query fails.
    pub fn getEmbeddingStats(self: *Self) ?EmbeddingStats {
        var stats: EmbeddingStats = .{ .ast_nodes_with_embeddings = 0, .alias_embeddings = 0, .keyword_embeddings = 0, .embedding_cache_entries = 0 };

        var stmt: ?*c.sqlite3_stmt = null;

        // Count ast_nodes with embeddings
        stmt = null;
        if (c.sqlite3_prepare_v2(self.db, "SELECT COUNT(*) FROM ast_nodes WHERE embedding IS NOT NULL", -1, &stmt, null) == c.SQLITE_OK) {
            if (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
                stats.ast_nodes_with_embeddings = @intCast(c.sqlite3_column_int(stmt, 0));
            }
            _ = c.sqlite3_finalize(stmt);
        }

        // Count alias embeddings
        stmt = null;
        if (c.sqlite3_prepare_v2(self.db, "SELECT COUNT(*) FROM semantic_alias_embeddings", -1, &stmt, null) == c.SQLITE_OK) {
            if (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
                stats.alias_embeddings = @intCast(c.sqlite3_column_int(stmt, 0));
            }
            _ = c.sqlite3_finalize(stmt);
        }

        // Count keyword embeddings
        stmt = null;
        if (c.sqlite3_prepare_v2(self.db, "SELECT COUNT(*) FROM keyword_index", -1, &stmt, null) == c.SQLITE_OK) {
            if (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
                stats.keyword_embeddings = @intCast(c.sqlite3_column_int(stmt, 0));
            }
            _ = c.sqlite3_finalize(stmt);
        }

        // Count embedding cache entries
        stmt = null;
        if (c.sqlite3_prepare_v2(self.db, "SELECT COUNT(*) FROM embedding_cache", -1, &stmt, null) == c.SQLITE_OK) {
            if (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
                stats.embedding_cache_entries = @intCast(c.sqlite3_column_int(stmt, 0));
            }
            _ = c.sqlite3_finalize(stmt);
        }

        return stats;
    }

    /// Entry types for show command
    pub const AliasEmbeddingEntry = struct { key: []const u8, model: []const u8 };
    pub const KeywordEmbeddingEntry = struct { keyword: []const u8, model: []const u8 };
    pub const EmbeddingCacheEntry = struct { content_hash: []const u8, model: []const u8 };
    pub const AstNodeEmbeddingEntry = struct { name: []const u8, node_type: []const u8, module: []const u8 };

    pub fn getAllAliasEmbeddings(self: *Self, allocator: std.mem.Allocator) ![]AliasEmbeddingEntry {
        var results: std.ArrayList(AliasEmbeddingEntry) = .{};
        errdefer results.deinit(allocator);

        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, "SELECT alias_key, embedding_model FROM semantic_alias_embeddings ORDER BY alias_key", -1, &stmt, null) != c.SQLITE_OK) {
            if (stmt) |s| _ = c.sqlite3_finalize(s);
            return error.PrepareFailed;
        }
        defer _ = c.sqlite3_finalize(stmt);

        while (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            const key = c.sqlite3_column_text(stmt, 0);
            const model = c.sqlite3_column_text(stmt, 1);
            if (key != null and model != null) {
                try results.append(allocator, .{
                    .key = try allocator.dupe(u8, std.mem.span(key)),
                    .model = try allocator.dupe(u8, std.mem.span(model)),
                });
            }
        }

        return results.toOwnedSlice(allocator);
    }

    pub fn getAllKeywordEmbeddings(self: *Self, allocator: std.mem.Allocator) ![]KeywordEmbeddingEntry {
        var results: std.ArrayList(KeywordEmbeddingEntry) = .{};
        errdefer results.deinit(allocator);

        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, "SELECT keyword, embedding_model FROM keyword_index ORDER BY keyword", -1, &stmt, null) != c.SQLITE_OK) {
            if (stmt) |s| _ = c.sqlite3_finalize(s);
            return error.PrepareFailed;
        }
        defer _ = c.sqlite3_finalize(stmt);

        while (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            const keyword = c.sqlite3_column_text(stmt, 0);
            const model = c.sqlite3_column_text(stmt, 1);
            if (keyword != null and model != null) {
                try results.append(allocator, .{
                    .keyword = try allocator.dupe(u8, std.mem.span(keyword)),
                    .model = try allocator.dupe(u8, std.mem.span(model)),
                });
            }
        }

        return results.toOwnedSlice(allocator);
    }

    pub fn getAllEmbeddingCacheEntries(self: *Self, allocator: std.mem.Allocator) ![]EmbeddingCacheEntry {
        var results: std.ArrayList(EmbeddingCacheEntry) = .{};
        errdefer results.deinit(allocator);

        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, "SELECT content_hash, model FROM embedding_cache ORDER BY content_hash", -1, &stmt, null) != c.SQLITE_OK) {
            if (stmt) |s| _ = c.sqlite3_finalize(s);
            return error.PrepareFailed;
        }
        defer _ = c.sqlite3_finalize(stmt);

        while (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            const hash = c.sqlite3_column_text(stmt, 0);
            const model = c.sqlite3_column_text(stmt, 1);
            if (hash != null and model != null) {
                try results.append(allocator, .{
                    .content_hash = try allocator.dupe(u8, std.mem.span(hash)),
                    .model = try allocator.dupe(u8, std.mem.span(model)),
                });
            }
        }

        return results.toOwnedSlice(allocator);
    }

    pub fn getAllAstNodeEmbeddings(self: *Self, allocator: std.mem.Allocator) ![]AstNodeEmbeddingEntry {
        var results: std.ArrayList(AstNodeEmbeddingEntry) = .{};
        errdefer results.deinit(allocator);

        var stmt: ?*c.sqlite3_stmt = null;
        const sql = "SELECT name, node_type, module FROM ast_nodes WHERE embedding IS NOT NULL ORDER BY module, name";
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) {
            if (stmt) |s| _ = c.sqlite3_finalize(s);
            return error.PrepareFailed;
        }
        defer _ = c.sqlite3_finalize(stmt);

        while (c.sqlite3_step(stmt) == c.SQLITE_ROW) {
            const name = c.sqlite3_column_text(stmt, 0);
            const node_type = c.sqlite3_column_text(stmt, 1);
            const module = c.sqlite3_column_text(stmt, 2);
            if (name != null and node_type != null and module != null) {
                try results.append(allocator, .{
                    .name = try allocator.dupe(u8, std.mem.span(name)),
                    .node_type = try allocator.dupe(u8, std.mem.span(node_type)),
                    .module = try allocator.dupe(u8, std.mem.span(module)),
                });
            }
        }

        return results.toOwnedSlice(allocator);
    }

    // ── Telemetry: query_log ──────────────────────────────────────────────────

    /// Log a query execution to query_log for telemetry.  Best-effort: never returns an error.
    pub fn logQuery(
        self: *Self,
        query: []const u8,
        latency_ms: i64,
        result_count: usize,
        tier: []const u8,
    ) void {
        const sql = "INSERT INTO query_log(query,timestamp,latency_ms,result_count,tier) VALUES(?1,strftime('%s','now'),?2,?3,?4)";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return;
        defer {
            if (stmt) |s| _ = c.sqlite3_finalize(s);
        }
        _ = c.sqlite3_bind_text(stmt.?, 1, query.ptr, @intCast(query.len), c.SQLITE_STATIC);
        _ = c.sqlite3_bind_int64(stmt.?, 2, latency_ms);
        _ = c.sqlite3_bind_int64(stmt.?, 3, @intCast(result_count));
        _ = c.sqlite3_bind_text(stmt.?, 4, tier.ptr, @intCast(tier.len), c.SQLITE_STATIC);
        _ = c.sqlite3_step(stmt.?);
    }

    pub const TelemetryEntry = struct {
        query: []const u8,
        count: i64,
        avg_latency_ms: f64,
        tier: []const u8,
    };

    /// Return the top N most frequent queries from query_log.
    /// Caller owns the returned slice (each TelemetryEntry.query is owned).
    pub fn topQueries(self: *Self, allocator: std.mem.Allocator, limit: usize) ![]TelemetryEntry {
        const sql = "SELECT query, COUNT(*) as cnt, AVG(latency_ms) as avg_lat, COALESCE(tier,'') as tier FROM query_log GROUP BY query ORDER BY cnt DESC LIMIT ?1";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return &.{};
        defer {
            if (stmt) |s| _ = c.sqlite3_finalize(s);
        }
        _ = c.sqlite3_bind_int64(stmt.?, 1, @intCast(limit));

        var results: std.ArrayList(TelemetryEntry) = .{};
        while (c.sqlite3_step(stmt.?) == c.SQLITE_ROW) {
            const qraw = c.sqlite3_column_text(stmt.?, 0);
            const qlen: usize = @intCast(c.sqlite3_column_bytes(stmt.?, 0));
            const entry = TelemetryEntry{
                .query = try allocator.dupe(u8, if (qraw) |p| p[0..qlen] else ""),
                .count = c.sqlite3_column_int64(stmt.?, 1),
                .avg_latency_ms = c.sqlite3_column_double(stmt.?, 2),
                .tier = blk: {
                    const traw = c.sqlite3_column_text(stmt.?, 3);
                    const tlen: usize = @intCast(c.sqlite3_column_bytes(stmt.?, 3));
                    break :blk try allocator.dupe(u8, if (traw) |p| p[0..tlen] else "");
                },
            };
            try results.append(allocator, entry);
        }
        return results.toOwnedSlice(allocator);
    }

    // ── Hot Files: query_count on ast_nodes ──────────────────────────────────

    /// Return the top `limit` hot files: ast_nodes rows ordered by query_count desc.
    /// Caller owns the returned slice and must free each entry with `freeSearchResult`.
    pub fn listHotFiles(self: *Self, allocator: std.mem.Allocator, limit: usize) ![]SearchResult {
        // Column order matches readRowResult (cols 0-10) + query_count as col 11.
        // 0:file_path 1:source 2:module 3:node_type 4:name
        // 5:signature 6:comment 7:line 8:used_by 9:language 10:detail 11:query_count
        const sql =
            \\SELECT file_path, COALESCE(source,''), module, node_type, name,
            \\       signature, comment, line, used_by, language, detail, query_count
            \\FROM ast_nodes
            \\WHERE query_count > 0
            \\ORDER BY query_count DESC
            \\LIMIT ?1
        ;
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return &.{};
        defer {
            if (stmt) |s| _ = c.sqlite3_finalize(s);
        }
        _ = c.sqlite3_bind_int64(stmt.?, 1, @intCast(limit));

        var results: std.ArrayList(SearchResult) = .{};
        errdefer {
            for (results.items) |r| freeSearchResult(allocator, r);
            results.deinit(allocator);
        }
        while (c.sqlite3_step(stmt.?) == c.SQLITE_ROW) {
            var r = try readRowResult(stmt.?, allocator);
            r.score = @floatFromInt(c.sqlite3_column_int64(stmt.?, 11));
            try results.append(allocator, r);
        }
        return results.toOwnedSlice(allocator);
    }

    /// Increment query_count for the given ast_nodes id. Best-effort, no error.
    pub fn incrementQueryCount(self: *Self, node_id: i64) void {
        const sql = "UPDATE ast_nodes SET query_count = query_count + 1 WHERE id = ?1";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return;
        defer {
            if (stmt) |s| _ = c.sqlite3_finalize(s);
        }
        _ = c.sqlite3_bind_int64(stmt.?, 1, node_id);
        _ = c.sqlite3_step(stmt.?);
    }

    /// Increment query_count for all ast_nodes with the given relative source path.
    /// `source_path` matches the `source` column (e.g. "src/guidance/llm_filter.zig").
    /// Best-effort, no error.
    pub fn incrementQueryCountForFile(self: *Self, source_path: []const u8) void {
        const sql = "UPDATE ast_nodes SET query_count = query_count + 1 WHERE source = ?1";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return;
        defer {
            if (stmt) |s| _ = c.sqlite3_finalize(s);
        }
        _ = c.sqlite3_bind_text(stmt.?, 1, source_path.ptr, @intCast(source_path.len), c.SQLITE_STATIC);
        _ = c.sqlite3_step(stmt.?);
    }

    // ── LLM Synthesis Cache ───────────────────────────────────────────────────

    /// Look up a cached LLM synthesis response by query hash.
    /// Returns an allocator-owned string or null on miss. `query_hash` is SHA-256 hex.
    pub fn loadCachedSynthesis(self: *Self, allocator: std.mem.Allocator, query_hash: []const u8) !?[]const u8 {
        const sql = "SELECT response FROM llm_cache WHERE query_hash = ?1 LIMIT 1";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return null;
        defer {
            if (stmt) |s| _ = c.sqlite3_finalize(s);
        }
        _ = c.sqlite3_bind_text(stmt.?, 1, query_hash.ptr, @intCast(query_hash.len), c.SQLITE_STATIC);
        if (c.sqlite3_step(stmt.?) != c.SQLITE_ROW) return null;
        const raw = c.sqlite3_column_text(stmt.?, 0);
        const len: usize = @intCast(c.sqlite3_column_bytes(stmt.?, 0));
        return try allocator.dupe(u8, if (raw) |p| p[0..len] else "");
    }

    /// Store an LLM synthesis response in the cache. `signature_hash` tracks
    /// the set of source files that contributed to this response; used for
    /// invalidation on `guidance gen --force`.
    pub fn storeSynthesisCache(
        self: *Self,
        query_hash: []const u8,
        response: []const u8,
        signature_hash: []const u8,
    ) void {
        const sql =
            \\INSERT OR REPLACE INTO llm_cache(query_hash, response, created_at, signature_hash)
            \\VALUES(?1, ?2, strftime('%s','now'), ?3)
        ;
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return;
        defer {
            if (stmt) |s| _ = c.sqlite3_finalize(s);
        }
        _ = c.sqlite3_bind_text(stmt.?, 1, query_hash.ptr, @intCast(query_hash.len), c.SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt.?, 2, response.ptr, @intCast(response.len), c.SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt.?, 3, signature_hash.ptr, @intCast(signature_hash.len), c.SQLITE_STATIC);
        _ = c.sqlite3_step(stmt.?);
    }

    /// Delete ALL llm_cache entries. Called by `guidance gen --force`.
    pub fn clearSynthesisCache(self: *Self) void {
        _ = c.sqlite3_exec(self.db, "DELETE FROM llm_cache", null, null, null);
    }

    /// Delete all llm_cache entries whose signature_hash matches.
    /// Called by `guidance gen --force` to invalidate stale cached responses.
    pub fn invalidateSynthesisCache(self: *Self, signature_hash: []const u8) void {
        const sql = "DELETE FROM llm_cache WHERE signature_hash = ?1";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK) return;
        defer {
            if (stmt) |s| _ = c.sqlite3_finalize(s);
        }
        _ = c.sqlite3_bind_text(stmt.?, 1, signature_hash.ptr, @intCast(signature_hash.len), c.SQLITE_STATIC);
        _ = c.sqlite3_step(stmt.?);
    }

    /// Return cache statistics: total entries, total bytes. Best-effort.
    pub fn cacheStats(self: *Self) struct { entries: i64, bytes: i64 } {
        const sql = "SELECT COUNT(*), SUM(LENGTH(response)) FROM llm_cache";
        var stmt: ?*c.sqlite3_stmt = null;
        if (c.sqlite3_prepare_v2(self.db, sql, -1, &stmt, null) != c.SQLITE_OK)
            return .{ .entries = 0, .bytes = 0 };
        defer {
            if (stmt) |s| _ = c.sqlite3_finalize(s);
        }
        if (c.sqlite3_step(stmt.?) != c.SQLITE_ROW) return .{ .entries = 0, .bytes = 0 };
        return .{
            .entries = c.sqlite3_column_int64(stmt.?, 0),
            .bytes = c.sqlite3_column_int64(stmt.?, 1),
        };
    }
};

// ---------------------------------------------------------------------------
// JSON parsing — internal structure
// ---------------------------------------------------------------------------

/// Represents parsed member data with ownership and invariants; managed via parsing pipeline.
const ParsedMember = struct {
    node_type: []const u8,
    name: []const u8,
    signature: ?[]const u8,
    comment: ?[]const u8,
    line: ?u32,
};

/// Represents parsed document data with ownership and invariants; managed via parsing lifecycle.
const ParsedDoc = struct {
    module: []const u8,
    source: []const u8,
    language: []const u8,
    module_comment: ?[]const u8,
    detail: ?[]const u8 = null,
    keywords: []const []const u8 = &.{},
    used_by: []const []const u8,
    file_hash: ?[]const u8,
    members: []ParsedMember,
};

// ---------------------------------------------------------------------------
// Capability helpers — module-level (no GuidanceDb state required)
// ---------------------------------------------------------------------------

/// Extract the capability name from an absolute CAPABILITY.md path.
/// Example: "doc/capabilities/vector-search/CAPABILITY.md" → "vector-search"
fn capabilityNameFromPath(file_path: []const u8) []const u8 {
    const dir = std.fs.path.dirname(file_path) orelse return file_path;
    return std.fs.path.basename(dir);
}

/// Load capability_keywords section from capability-mapping.json into map.
/// All map keys and values are allocated on `alloc`.
/// JSON structure: { "capability_keywords": { "cap-name": ["kw1","kw2",...] } }
fn loadCapabilityKeywordsMap(
    alloc: std.mem.Allocator,
    mapping_path: []const u8,
    map: *std.StringHashMapUnmanaged([]const []const u8),
) !void {
    const content = std.fs.cwd().readFileAlloc(alloc, mapping_path, 512 * 1024) catch return;

    const Value = std.json.Value;
    var parsed = try std.json.parseFromSlice(Value, alloc, content, .{ .ignore_unknown_fields = true });
    defer parsed.deinit();

    if (parsed.value != .object) return;
    const ck_val = parsed.value.object.get("capability_keywords") orelse return;
    if (ck_val != .object) return;

    var it = ck_val.object.iterator();
    while (it.next()) |entry| {
        if (entry.value_ptr.* != .array) continue;
        const arr = entry.value_ptr.*.array.items;

        var kws: std.ArrayList([]const u8) = .{};
        for (arr) |item| {
            if (item == .string) {
                try kws.append(alloc, try alloc.dupe(u8, item.string));
            }
        }
        const key = try alloc.dupe(u8, entry.key_ptr.*);
        try map.put(alloc, key, try kws.toOwnedSlice(alloc));
    }
}

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

    const detail: ?[]const u8 = blk: {
        const dv = root.object.get("detail") orelse break :blk null;
        if (dv != .string) break :blk null;
        break :blk dv.string;
    };

    var keywords_list: std.ArrayList([]const u8) = .{};
    if (root.object.get("keywords")) |kwv| {
        if (kwv == .array) {
            for (kwv.array.items) |item| {
                if (item == .string) try keywords_list.append(arena, item.string);
            }
        }
    }

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
            .detail = detail,
            .keywords = try keywords_list.toOwnedSlice(arena),
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
            .detail = detail,
            .keywords = try keywords_list.toOwnedSlice(arena),
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
        .detail = detail,
        .keywords = try keywords_list.toOwnedSlice(arena),
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
    try std.testing.expectEqual(@as(c_int, 2), c.sqlite3_column_int(stmt, 0));
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

test "buildEmbeddingText descriptor-bag format" {
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
    // Descriptor-bag: name is the first token
    try std.testing.expect(std.mem.startsWith(u8, text, "syncDatabase · "));
    // Type noun is present as a bag token
    try std.testing.expect(std.mem.indexOf(u8, text, "function") != null);
    // Comment is embedded
    try std.testing.expect(std.mem.indexOf(u8, text, "Synchronises the SQLite database") != null);
    // Parameter names extracted (not full types); allocator is filtered out
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

// =============================================================================
// DbSyncBuilder tests
// =============================================================================

test "DbSyncBuilder defaults: cache_limit=0, no capabilities, no aliases" {
    var noop: vector.NoopEmbedding = .{};
    const embedder = noop.provider();

    const builder = DbSyncBuilder.init(
        std.testing.allocator,
        ".guidance",
        ".guidance.db",
        embedder,
    );

    try std.testing.expectEqual(@as(u32, 0), builder.cache_limit);
    try std.testing.expect(builder.capabilities_dir == null);
    try std.testing.expect(builder.aliases == null);
    try std.testing.expectEqualStrings(".guidance", builder.guidance_dir);
    try std.testing.expectEqualStrings(".guidance.db", builder.db_path);
}

test "DbSyncBuilder fluent setters return updated values" {
    var noop: vector.NoopEmbedding = .{};
    const embedder = noop.provider();

    const builder = DbSyncBuilder.init(
        std.testing.allocator,
        ".guidance",
        ".guidance.db",
        embedder,
    )
        .withCapabilities(".doc/capabilities")
        .cacheLimit(500);

    try std.testing.expectEqual(@as(u32, 500), builder.cache_limit);
    try std.testing.expect(builder.capabilities_dir != null);
    try std.testing.expectEqualStrings(".doc/capabilities", builder.capabilities_dir.?);
    // aliases still unset
    try std.testing.expect(builder.aliases == null);
}

test "DbSyncBuilder: each setter produces an independent copy (immutable chain)" {
    var noop: vector.NoopEmbedding = .{};
    const embedder = noop.provider();

    const base = DbSyncBuilder.init(std.testing.allocator, "g", "db", embedder);
    const with_cap = base.withCapabilities("cap");
    const with_limit = base.cacheLimit(99);

    // base is unmodified
    try std.testing.expect(base.capabilities_dir == null);
    try std.testing.expectEqual(@as(u32, 0), base.cache_limit);

    // derived builders have their own values
    try std.testing.expectEqualStrings("cap", with_cap.capabilities_dir.?);
    try std.testing.expectEqual(@as(u32, 99), with_limit.cache_limit);
}
