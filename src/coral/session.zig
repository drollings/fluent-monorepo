/// session.zig — Coral Session Persistence (SQLite + FTS5)
///
/// Stores session metadata and messages in a dedicated SQLite database,
/// separate from the main coral db to avoid circular imports.
///
/// Schema:
///   sessions(id, source, model, parent_session_id, created_at)
///   messages(id, session_id, role, content, tokens, created_at)
///   messages_fts — FTS5 virtual table over messages.content
const std = @import("std");

const c = @cImport({
    @cInclude("sqlite3.h");
});

const SQLITE_STATIC: c.sqlite3_destructor_type = null;
const BUSY_TIMEOUT_MS: c_int = 5000;

// ---------------------------------------------------------------------------
// DDL
// ---------------------------------------------------------------------------

const DDL_SESSIONS: []const u8 =
    \\CREATE TABLE IF NOT EXISTS sessions (
    \\    id INTEGER PRIMARY KEY,
    \\    source TEXT NOT NULL DEFAULT '',
    \\    model TEXT NOT NULL DEFAULT '',
    \\    parent_session_id INTEGER,
    \\    created_at REAL NOT NULL DEFAULT 0.0
    \\)
;

const DDL_MESSAGES: []const u8 =
    \\CREATE TABLE IF NOT EXISTS messages (
    \\    id INTEGER PRIMARY KEY,
    \\    session_id INTEGER NOT NULL,
    \\    role TEXT NOT NULL DEFAULT '',
    \\    content TEXT NOT NULL DEFAULT '',
    \\    tokens INTEGER NOT NULL DEFAULT 0,
    \\    created_at REAL NOT NULL DEFAULT 0.0
    \\)
;

const DDL_MESSAGES_INDEX: []const u8 =
    \\CREATE INDEX IF NOT EXISTS messages_by_session ON messages(session_id)
;

const DDL_MESSAGES_FTS: []const u8 =
    \\CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts
    \\USING fts5(content, content='messages', content_rowid='id')
;

const DDL_MESSAGES_FTS_TRIGGER_INSERT: []const u8 =
    \\CREATE TRIGGER IF NOT EXISTS messages_fts_insert
    \\AFTER INSERT ON messages BEGIN
    \\    INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
    \\END
;

const DDL_MESSAGES_FTS_TRIGGER_DELETE: []const u8 =
    \\CREATE TRIGGER IF NOT EXISTS messages_fts_delete
    \\BEFORE DELETE ON messages BEGIN
    \\    INSERT INTO messages_fts(messages_fts, rowid, content)
    \\    VALUES ('delete', old.id, old.content);
    \\END
;

const SCHEMA_DDL = [_][]const u8{
    DDL_SESSIONS,
    DDL_MESSAGES,
    DDL_MESSAGES_INDEX,
    DDL_MESSAGES_FTS,
    DDL_MESSAGES_FTS_TRIGGER_INSERT,
    DDL_MESSAGES_FTS_TRIGGER_DELETE,
};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A row returned by searchMessages.
pub const Message = struct {
    id: i64,
    session_id: i64,
    role: []const u8,
    content: []const u8,
};

// ---------------------------------------------------------------------------
// SessionDB
// ---------------------------------------------------------------------------

/// Manages database session state with fixed-size buffers; owned by the session; ensures consistent access patterns.
pub const SessionDB = struct {
    db: ?*c.sqlite3,
    allocator: std.mem.Allocator,

    const Self = @This();

    // ------------------------------------------------------------------
    // Lifecycle
    // ------------------------------------------------------------------

    /// Open (or create) a SQLite database at `path` and initialize the schema.
    /// Pass ":memory:" for an in-process ephemeral database useful in tests.
    pub fn init(path: []const u8, allocator: std.mem.Allocator) !Self {
        const path_z = try allocator.dupeZ(u8, path);
        defer allocator.free(path_z);

        var db: ?*c.sqlite3 = null;
        const rc = c.sqlite3_open(path_z.ptr, &db);
        if (rc != c.SQLITE_OK) {
            if (db) |d| _ = c.sqlite3_close(d);
            return error.SqliteOpenFailed;
        }
        _ = c.sqlite3_busy_timeout(db, BUSY_TIMEOUT_MS);
        _ = c.sqlite3_exec(db, "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;", null, null, null);

        var self = Self{ .db = db, .allocator = allocator };
        try self.initSchema();
        return self;
    }

    pub fn deinit(self: *Self) void {
        if (self.db) |db| {
            _ = c.sqlite3_close(db);
            self.db = null;
        }
    }

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    fn exec(self: *Self, sql: []const u8) !void {
        const sql_z = try self.allocator.dupeZ(u8, sql);
        defer self.allocator.free(sql_z);
        const rc = c.sqlite3_exec(self.db, sql_z.ptr, null, null, null);
        if (rc != c.SQLITE_OK) return error.SqliteExecFailed;
    }

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
    // Schema
    // ------------------------------------------------------------------

    fn initSchema(self: *Self) !void {
        for (SCHEMA_DDL) |ddl| {
            try self.exec(ddl);
        }
    }

    // ------------------------------------------------------------------
    // Sessions
    // ------------------------------------------------------------------

    /// Insert a new session row and return its rowid.
    /// `parent_session_id` may be null for root sessions.
    pub fn createSession(
        self: *Self,
        source: []const u8,
        model: []const u8,
        parent_session_id: ?i64,
    ) !i64 {
        const sql =
            \\INSERT INTO sessions (source, model, parent_session_id, created_at)
            \\VALUES (?1, ?2, ?3, ?4)
        ;
        const stmt = try self.prepare(sql);
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, source.ptr, @intCast(source.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 2, model.ptr, @intCast(model.len), SQLITE_STATIC);
        if (parent_session_id) |pid| {
            _ = c.sqlite3_bind_int64(stmt, 3, pid);
        } else {
            _ = c.sqlite3_bind_null(stmt, 3);
        }
        const now: f64 = @floatFromInt(std.time.milliTimestamp());
        _ = c.sqlite3_bind_double(stmt, 4, now / 1000.0);

        _ = try step(stmt);
        return c.sqlite3_last_insert_rowid(self.db);
    }

    // ------------------------------------------------------------------
    // Messages
    // ------------------------------------------------------------------

    /// Append a message to `session_id` and return its rowid.
    /// The FTS5 trigger keeps messages_fts in sync automatically.
    pub fn addMessage(
        self: *Self,
        session_id: i64,
        role: []const u8,
        content: []const u8,
        tokens: i64,
    ) !i64 {
        const sql =
            \\INSERT INTO messages (session_id, role, content, tokens, created_at)
            \\VALUES (?1, ?2, ?3, ?4, ?5)
        ;
        const stmt = try self.prepare(sql);
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_int64(stmt, 1, session_id);
        _ = c.sqlite3_bind_text(stmt, 2, role.ptr, @intCast(role.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 3, content.ptr, @intCast(content.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_int64(stmt, 4, tokens);
        const now: f64 = @floatFromInt(std.time.milliTimestamp());
        _ = c.sqlite3_bind_double(stmt, 5, now / 1000.0);

        _ = try step(stmt);
        return c.sqlite3_last_insert_rowid(self.db);
    }

    /// Full-text search over message content.
    /// Returns at most `limit` results ordered by FTS5 rank.
    /// Caller must free the returned slice and all strings inside it.
    pub fn searchMessages(
        self: *Self,
        query: []const u8,
        limit: usize,
        allocator: std.mem.Allocator,
    ) ![]Message {
        const sql =
            \\SELECT m.id, m.session_id, m.role, m.content
            \\FROM messages_fts f
            \\JOIN messages m ON m.id = f.rowid
            \\WHERE messages_fts MATCH ?1
            \\ORDER BY rank
            \\LIMIT ?2
        ;
        const stmt = try self.prepare(sql);
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, query.ptr, @intCast(query.len), SQLITE_STATIC);
        _ = c.sqlite3_bind_int64(stmt, 2, @intCast(limit));

        var results: std.ArrayList(Message) = .empty;
        errdefer {
            for (results.items) |msg| {
                allocator.free(msg.role);
                allocator.free(msg.content);
            }
            results.deinit(allocator);
        }

        while (try step(stmt)) {
            const id = c.sqlite3_column_int64(stmt, 0);
            const session_id = c.sqlite3_column_int64(stmt, 1);

            const role_ptr: [*c]const u8 = c.sqlite3_column_text(stmt, 2);
            const role_len: usize = @intCast(c.sqlite3_column_bytes(stmt, 2));
            const role = try allocator.dupe(u8, role_ptr[0..role_len]);

            const content_ptr: [*c]const u8 = c.sqlite3_column_text(stmt, 3);
            const content_len: usize = @intCast(c.sqlite3_column_bytes(stmt, 3));
            const content = try allocator.dupe(u8, content_ptr[0..content_len]);

            try results.append(allocator, .{
                .id = id,
                .session_id = session_id,
                .role = role,
                .content = content,
            });
        }

        return results.toOwnedSlice(allocator);
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "SessionDB init and deinit (in-memory)" {
    var db = try SessionDB.init(":memory:", testing.allocator);
    defer db.deinit();
}

test "createSession returns valid id" {
    var db = try SessionDB.init(":memory:", testing.allocator);
    defer db.deinit();

    const id = try db.createSession("cli", "gpt-4o", null);
    try testing.expect(id > 0);
}

test "createSession with parent_session_id" {
    var db = try SessionDB.init(":memory:", testing.allocator);
    defer db.deinit();

    const parent = try db.createSession("cli", "gpt-4o", null);
    const child = try db.createSession("cli", "gpt-4o", parent);
    try testing.expect(child > parent);
}

test "addMessage returns valid id" {
    var db = try SessionDB.init(":memory:", testing.allocator);
    defer db.deinit();

    const sid = try db.createSession("cli", "gpt-4o", null);
    const mid = try db.addMessage(sid, "user", "Hello, world!", 4);
    try testing.expect(mid > 0);
}

test "searchMessages returns matching messages" {
    var db = try SessionDB.init(":memory:", testing.allocator);
    defer db.deinit();

    const sid = try db.createSession("cli", "gpt-4o", null);
    _ = try db.addMessage(sid, "user", "The quick brown fox", 5);
    _ = try db.addMessage(sid, "assistant", "jumps over the lazy dog", 6);
    _ = try db.addMessage(sid, "user", "An unrelated statement about cats", 6);

    const results = try db.searchMessages("fox", 10, testing.allocator);
    defer {
        for (results) |msg| {
            testing.allocator.free(msg.role);
            testing.allocator.free(msg.content);
        }
        testing.allocator.free(results);
    }

    try testing.expectEqual(@as(usize, 1), results.len);
    try testing.expectEqualStrings("user", results[0].role);
    try testing.expect(std.mem.indexOf(u8, results[0].content, "fox") != null);
}

test "searchMessages limit is respected" {
    var db = try SessionDB.init(":memory:", testing.allocator);
    defer db.deinit();

    const sid = try db.createSession("test", "model", null);
    _ = try db.addMessage(sid, "user", "alpha search term here", 4);
    _ = try db.addMessage(sid, "user", "beta search term here", 4);
    _ = try db.addMessage(sid, "user", "gamma search term here", 4);

    const results = try db.searchMessages("search", 2, testing.allocator);
    defer {
        for (results) |msg| {
            testing.allocator.free(msg.role);
            testing.allocator.free(msg.content);
        }
        testing.allocator.free(results);
    }

    try testing.expect(results.len <= 2);
}

test "searchMessages no results for missing term" {
    var db = try SessionDB.init(":memory:", testing.allocator);
    defer db.deinit();

    const sid = try db.createSession("cli", "gpt-4o", null);
    _ = try db.addMessage(sid, "user", "Hello world", 2);

    const results = try db.searchMessages("xyzzy_nonexistent", 10, testing.allocator);
    defer testing.allocator.free(results);

    try testing.expectEqual(@as(usize, 0), results.len);
}

test "SessionDB persists to tmpDir" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const dir_path = try tmp.dir.realpathAlloc(testing.allocator, ".");
    defer testing.allocator.free(dir_path);

    const db_path = try std.fs.path.join(testing.allocator, &.{ dir_path, "session_test.db" });
    defer testing.allocator.free(db_path);

    {
        var db = try SessionDB.init(db_path, testing.allocator);
        defer db.deinit();
        const sid = try db.createSession("cli", "gpt-4o", null);
        _ = try db.addMessage(sid, "user", "persisted message", 3);
    }

    // Reopen and verify schema is intact
    {
        var db = try SessionDB.init(db_path, testing.allocator);
        defer db.deinit();
        const results = try db.searchMessages("persisted", 5, testing.allocator);
        defer {
            for (results) |msg| {
                testing.allocator.free(msg.role);
                testing.allocator.free(msg.content);
            }
            testing.allocator.free(results);
        }
        try testing.expectEqual(@as(usize, 1), results.len);
    }
}
