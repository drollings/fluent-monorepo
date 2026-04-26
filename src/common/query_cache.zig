const std = @import("std");
const hash_mod = @import("hash.zig");

pub const c = @cImport({
    @cInclude("sqlite3.h");
});

pub const PersistentQueryCache = struct {
    allocator: std.mem.Allocator,
    db: ?*c.sqlite3,
    db_path: []const u8,
    default_ttl_seconds: u64,
    max_entries: usize,

    pub const Entry = struct {
        query: []const u8,
        result_summary: []const u8,
        timestamp: i128,
        ttl_seconds: u64,
    };

    pub fn init(allocator: std.mem.Allocator, db_path: []const u8, default_ttl_seconds: u64) !PersistentQueryCache {
        return .{
            .allocator = allocator,
            .db = null,
            .db_path = db_path,
            .default_ttl_seconds = default_ttl_seconds,
            .max_entries = 4096,
        };
    }

    pub fn deinit(self: *PersistentQueryCache) void {
        if (self.db) |db| {
            _ = c.sqlite3_close(db);
            self.db = null;
        }
    }

    fn ensureDb(self: *PersistentQueryCache) !*c.sqlite3 {
        if (self.db) |db| return db;

        const db_path_z = try std.fmt.allocPrintSentinel(self.allocator, "{s}", .{self.db_path}, 0);
        defer self.allocator.free(db_path_z);

        var db: ?*c.sqlite3 = null;
        const rc = c.sqlite3_open(db_path_z.ptr, &db);
        if (rc != c.SQLITE_OK) {
            if (db) |d| _ = c.sqlite3_close(d);
            return error.SqliteOpenFailed;
        }
        _ = c.sqlite3_busy_timeout(db, 5000);

        const create_sql =
            \\CREATE TABLE IF NOT EXISTS query_cache (
            \\  key TEXT PRIMARY KEY,
            \\  result_json TEXT NOT NULL,
            \\  timestamp INTEGER NOT NULL,
            \\  ttl_seconds INTEGER NOT NULL
            \\)
        ;
        var err_msg: [*c]u8 = null;
        const create_rc = c.sqlite3_exec(db, create_sql, null, null, &err_msg);
        if (err_msg) |msg| c.sqlite3_free(msg);
        if (create_rc != c.SQLITE_OK) {
            _ = c.sqlite3_close(db);
            return error.SqliteExecFailed;
        }

        self.db = db;
        return db.?;
    }

    fn queryKey(query: []const u8) u64 {
        var lower_buf: [512]u8 = undefined;
        const len = @min(query.len, lower_buf.len);
        for (query[0..len], 0..) |c, i| lower_buf[i] = std.ascii.toLower(c);
        return hash_mod.fnv1a64(lower_buf[0..len]);
    }

    pub fn get(self: *PersistentQueryCache, query: []const u8) !?Entry {
        const db = try self.ensureDb();
        const key = queryKey(query);
        const now_ms = std.time.milliTimestamp();

        var key_buf: [20]u8 = undefined;
        const key_str = try std.fmt.bufPrint(&key_buf, "{d}", .{@as(i64, @bitCast(key))});

        const sql = "SELECT result_json, timestamp, ttl_seconds FROM query_cache WHERE key = ?";
        var stmt: ?*c.sqlite3_stmt = null;
        const prep_rc = c.sqlite3_prepare_v2(db, sql, -1, &stmt, null);
        if (prep_rc != c.SQLITE_OK) return error.SqlitePrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, key_str.ptr, @intCast(key_str.len), c.SQLITE_STATIC);

        const step_rc = c.sqlite3_step(stmt);
        if (step_rc != c.SQLITE_ROW) return null;

        const result_json = c.sqlite3_column_text(stmt, 0);
        const timestamp = c.sqlite3_column_int64(stmt, 1);
        const ttl_seconds = c.sqlite3_column_int64(stmt, 2);

        const result_len: usize = @intCast(c.sqlite3_column_bytes(stmt, 0));
        const result_copy = try self.allocator.dupe(u8, result_json[0..result_len]);

        const ttl_ms: i64 = ttl_seconds * 1000;
        if (now_ms - timestamp > ttl_ms) {
            self.allocator.free(result_copy);
            _ = c.sqlite3_finalize(stmt);

            const del_sql = "DELETE FROM query_cache WHERE key = ?";
            var del_stmt: ?*c.sqlite3_stmt = null;
            _ = c.sqlite3_prepare_v2(db, del_sql, -1, &del_stmt, null);
            if (del_stmt) |ds| {
                _ = c.sqlite3_bind_text(ds, 1, key_str.ptr, @intCast(key_str.len), c.SQLITE_STATIC);
                _ = c.sqlite3_step(ds);
                _ = c.sqlite3_finalize(ds);
            }
            return null;
        }

        const result_summary = try self.allocator.dupe(u8, query);
        return .{
            .query = result_summary,
            .result_summary = result_copy,
            .timestamp = @as(i128, timestamp) * 1_000_000,
            .ttl_seconds = @intCast(ttl_seconds),
        };
    }

    pub fn put(self: *PersistentQueryCache, query: []const u8, result_summary: []const u8, ttl_seconds: ?u64) !void {
        const db = try self.ensureDb();
        const key = queryKey(query);
        const ttl = ttl_seconds orelse self.default_ttl_seconds;
        const now_ms = @as(i64, @intCast(std.time.milliTimestamp()));

        var key_buf: [20]u8 = undefined;
        const key_str = try std.fmt.bufPrint(&key_buf, "{d}", .{@as(i64, @bitCast(key))});

        const sql = "INSERT OR REPLACE INTO query_cache (key, result_json, timestamp, ttl_seconds) VALUES (?, ?, ?, ?)";
        var stmt: ?*c.sqlite3_stmt = null;
        const prep_rc = c.sqlite3_prepare_v2(db, sql, -1, &stmt, null);
        if (prep_rc != c.SQLITE_OK) return error.SqlitePrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, key_str.ptr, @intCast(key_str.len), c.SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 2, result_summary.ptr, @intCast(result_summary.len), c.SQLITE_STATIC);
        _ = c.sqlite3_bind_int64(stmt, 3, now_ms);
        _ = c.sqlite3_bind_int64(stmt, 4, @as(c.sqlite3_int64, @intCast(ttl)));

        const step_rc = c.sqlite3_step(stmt);
        if (step_rc != c.SQLITE_DONE) return error.SqliteExecFailed;

        self.evictIfNeeded(db) catch {};
    }

    fn evictIfNeeded(self: *PersistentQueryCache, db: *c.sqlite3) !void {
        _ = self;
        const count_sql = "SELECT COUNT(*) FROM query_cache";
        var cnt_stmt: ?*c.sqlite3_stmt = null;
        const prep_rc = c.sqlite3_prepare_v2(db, count_sql, -1, &cnt_stmt, null);
        if (prep_rc != c.SQLITE_OK) return;
        defer _ = c.sqlite3_finalize(cnt_stmt);

        const step_rc = c.sqlite3_step(cnt_stmt);
        if (step_rc != c.SQLITE_ROW) return;
        const count = c.sqlite3_column_int64(cnt_stmt, 0);

        if (count > @as(i64, @intCast(@as(usize, @intCast(self.max_entries))))) {
            var err_msg: [*c]u8 = null;
            _ = c.sqlite3_exec(db, "DELETE FROM query_cache WHERE key IN (SELECT key FROM query_cache ORDER BY timestamp ASC LIMIT 100)", null, null, &err_msg);
            if (err_msg) |msg| c.sqlite3_free(msg);
        }
    }

    pub fn clear(self: *PersistentQueryCache) !void {
        const db = try self.ensureDb();
        var err_msg: [*c]u8 = null;
        const rc = c.sqlite3_exec(db, "DELETE FROM query_cache", null, null, &err_msg);
        if (err_msg) |msg| c.sqlite3_free(msg);
        if (rc != c.SQLITE_OK) return error.SqliteExecFailed;
    }

    pub fn expireStale(self: *PersistentQueryCache) !void {
        const db = try self.ensureDb();
        const now_ms = @as(i64, @intCast(std.time.milliTimestamp()));
        const sql = "DELETE FROM query_cache WHERE (timestamp + ttl_seconds * 1000) < ?";
        var stmt: ?*c.sqlite3_stmt = null;
        const prep_rc = c.sqlite3_prepare_v2(db, sql, -1, &stmt, null);
        if (prep_rc != c.SQLITE_OK) return error.SqlitePrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);
        _ = c.sqlite3_bind_int64(stmt, 1, now_ms);
        _ = c.sqlite3_step(stmt);
    }

    pub fn stats(self: *PersistentQueryCache) !struct { total: usize, stale: usize, active: usize } {
        const db = try self.ensureDb();
        const now_ms = @as(i64, @intCast(std.time.milliTimestamp()));

        var total: i64 = 0;
        var stale: i64 = 0;

        const total_sql = "SELECT COUNT(*) FROM query_cache";
        var t_stmt: ?*c.sqlite3_stmt = null;
        _ = c.sqlite3_prepare_v2(db, total_sql, -1, &t_stmt, null);
        if (t_stmt) |ts| {
            defer _ = c.sqlite3_finalize(ts);
            if (c.sqlite3_step(ts) == c.SQLITE_ROW) total = c.sqlite3_column_int64(ts, 0);
        }

        const stale_sql = "SELECT COUNT(*) FROM query_cache WHERE (timestamp + ttl_seconds * 1000) < ?";
        var s_stmt: ?*c.sqlite3_stmt = null;
        _ = c.sqlite3_prepare_v2(db, stale_sql, -1, &s_stmt, null);
        if (s_stmt) |ss| {
            defer _ = c.sqlite3_finalize(ss);
            _ = c.sqlite3_bind_int64(ss, 1, now_ms);
            if (c.sqlite3_step(ss) == c.SQLITE_ROW) stale = c.sqlite3_column_int64(ss, 0);
        }

        return .{
            .total = @intCast(total),
            .stale = @intCast(stale),
            .active = @intCast(@max(total - stale, 0)),
        };
    }
};

const testing = std.testing;

test "PersistentQueryCache put and get" {
    var cache = try PersistentQueryCache.init(testing.allocator, ":memory:", 3600);
    defer cache.deinit();

    try cache.put("filterStages", "Filters pipeline stages by type", null);
    const entry = try cache.get("filterStages");
    try testing.expect(entry != null);
}

test "PersistentQueryCache clear" {
    var cache = try PersistentQueryCache.init(testing.allocator, ":memory:", 3600);
    defer cache.deinit();

    try cache.put("q1", "r1", null);
    try cache.clear();
    try testing.expect(try cache.get("q1") == null);
}