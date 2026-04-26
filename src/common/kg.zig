const std = @import("std");

pub const c = @cImport({
    @cInclude("sqlite3.h");
});

pub const KgEntity = struct {
    id: []const u8,
    name: []const u8,
    entity_type: []const u8,
    properties: ?[]const u8,
    created_at: i64,
};

pub const KgTriple = struct {
    id: i64,
    subject: []const u8,
    predicate: []const u8,
    object: []const u8,
    valid_from: ?i64,
    valid_to: ?i64,
    confidence: f64,
    source_closet: ?[]const u8,
    source_file: ?[]const u8,
    source_drawer_id: ?[]const u8,
    adapter_name: ?[]const u8,
};

pub const KgStats = struct {
    entity_count: usize,
    triple_count: usize,
    current_facts: usize,
    expired_facts: usize,
    relationship_types: usize,
};

pub const KnowledgeGraph = struct {
    db: ?*c.sqlite3,
    allocator: std.mem.Allocator,

    pub fn init(allocator: std.mem.Allocator, db_path: []const u8) !KnowledgeGraph {
        const db_path_z = try std.fmt.allocPrintSentinel(allocator, "{s}", .{db_path}, 0);
        defer allocator.free(db_path_z);

        var db: ?*c.sqlite3 = null;
        const rc = c.sqlite3_open(db_path_z.ptr, &db);
        if (rc != c.SQLITE_OK) {
            if (db) |d| _ = c.sqlite3_close(d);
            return error.SqliteOpenFailed;
        }
        _ = c.sqlite3_busy_timeout(db, 5000);

        var self = KnowledgeGraph{ .db = db, .allocator = allocator };
        try self.migrate();
        return self;
    }

    pub fn deinit(self: *KnowledgeGraph) void {
        if (self.db) |db| {
            _ = c.sqlite3_close(db);
            self.db = null;
        }
    }

    fn migrate(self: *KnowledgeGraph) !void {
        const db = self.db orelse return error.SqliteOpenFailed;
        const sql =
            \\CREATE TABLE IF NOT EXISTS kg_entities (
            \\  id TEXT PRIMARY KEY,
            \\  name TEXT NOT NULL,
            \\  entity_type TEXT NOT NULL,
            \\  properties TEXT,
            \\  created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            \\);
            \\CREATE TABLE IF NOT EXISTS kg_triples (
            \\  id INTEGER PRIMARY KEY AUTOINCREMENT,
            \\  subject TEXT NOT NULL,
            \\  predicate TEXT NOT NULL,
            \\  object TEXT NOT NULL,
            \\  valid_from INTEGER,
            \\  valid_to INTEGER,
            \\  confidence REAL DEFAULT 1.0,
            \\  source_closet TEXT,
            \\  source_file TEXT,
            \\  source_drawer_id TEXT,
            \\  adapter_name TEXT,
            \\  extracted_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            \\);
            \\CREATE INDEX IF NOT EXISTS kg_triples_subject ON kg_triples(subject);
            \\CREATE INDEX IF NOT EXISTS kg_triples_predicate ON kg_triples(predicate);
            \\CREATE INDEX IF NOT EXISTS kg_triples_object ON kg_triples(object);
            \\CREATE INDEX IF NOT EXISTS kg_triples_valid ON kg_triples(valid_from, valid_to);
        ;
        var err_msg: [*c]u8 = null;
        const rc = c.sqlite3_exec(db, sql, null, null, &err_msg);
        if (err_msg) |msg| c.sqlite3_free(msg);
        if (rc != c.SQLITE_OK) return error.SqliteExecFailed;
    }

    pub fn addEntity(self: *KnowledgeGraph, name: []const u8, entity_type: []const u8, properties: ?[]const u8) !void {
        const db = self.db orelse return error.SqliteOpenFailed;
        var id_buf: [256]u8 = undefined;
        var id_len: usize = 0;
        for (name) |ch| {
            if (id_len < 255) {
                if (std.ascii.isAlphanumeric(ch)) {
                    id_buf[id_len] = std.ascii.toLower(ch);
                } else {
                    id_buf[id_len] = '_';
                }
                id_len += 1;
            }
        }
        const id = id_buf[0..id_len];

        const sql = "INSERT OR IGNORE INTO kg_entities (id, name, entity_type, properties) VALUES (?, ?, ?, ?)";
        var stmt: ?*c.sqlite3_stmt = null;
        const prep_rc = c.sqlite3_prepare_v2(db, sql, -1, &stmt, null);
        if (prep_rc != c.SQLITE_OK) return error.SqlitePrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, id.ptr, @intCast(id.len), c.SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 2, name.ptr, @intCast(name.len), c.SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 3, entity_type.ptr, @intCast(entity_type.len), c.SQLITE_STATIC);
        if (properties) |p| {
            _ = c.sqlite3_bind_text(stmt, 4, p.ptr, @intCast(p.len), c.SQLITE_STATIC);
        } else {
            _ = c.sqlite3_bind_null(stmt, 4);
        }
        _ = c.sqlite3_step(stmt);
    }

    pub fn addTriple(
        self: *KnowledgeGraph,
        subject: []const u8,
        predicate: []const u8,
        object: []const u8,
        valid_from: ?i64,
        valid_to: ?i64,
        confidence: f64,
        source_closet: ?[]const u8,
        source_file: ?[]const u8,
        source_drawer_id: ?[]const u8,
        adapter_name: ?[]const u8,
    ) !void {
        const db = self.db orelse return error.SqliteOpenFailed;

        try self.addEntity(subject, "unknown", null);
        try self.addEntity(object, "unknown", null);

        const dup_sql = "SELECT id FROM kg_triples WHERE subject = ? AND predicate = ? AND object = ? AND valid_to IS NULL";
        var dup_stmt: ?*c.sqlite3_stmt = null;
        _ = c.sqlite3_prepare_v2(db, dup_sql, -1, &dup_stmt, null);
        if (dup_stmt) |ds| {
            _ = c.sqlite3_bind_text(ds, 1, subject.ptr, @intCast(subject.len), c.SQLITE_STATIC);
            _ = c.sqlite3_bind_text(ds, 2, predicate.ptr, @intCast(predicate.len), c.SQLITE_STATIC);
            _ = c.sqlite3_bind_text(ds, 3, object.ptr, @intCast(object.len), c.SQLITE_STATIC);
            const step_rc = c.sqlite3_step(ds);
            _ = c.sqlite3_finalize(ds);
            if (step_rc == c.SQLITE_ROW) return;
        }

        const sql = "INSERT INTO kg_triples (subject, predicate, object, valid_from, valid_to, confidence, source_closet, source_file, source_drawer_id, adapter_name) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";
        var stmt: ?*c.sqlite3_stmt = null;
        const prep_rc = c.sqlite3_prepare_v2(db, sql, -1, &stmt, null);
        if (prep_rc != c.SQLITE_OK) return error.SqlitePrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);

        _ = c.sqlite3_bind_text(stmt, 1, subject.ptr, @intCast(subject.len), c.SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 2, predicate.ptr, @intCast(predicate.len), c.SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 3, object.ptr, @intCast(object.len), c.SQLITE_STATIC);
        if (valid_from) |vf| _ = c.sqlite3_bind_int64(stmt, 4, vf) else _ = c.sqlite3_bind_null(stmt, 4);
        if (valid_to) |vt| _ = c.sqlite3_bind_int64(stmt, 5, vt) else _ = c.sqlite3_bind_null(stmt, 5);
        _ = c.sqlite3_bind_double(stmt, 6, confidence);
        if (source_closet) |sc| _ = c.sqlite3_bind_text(stmt, 7, sc.ptr, @intCast(sc.len), c.SQLITE_STATIC) else _ = c.sqlite3_bind_null(stmt, 7);
        if (source_file) |sf| _ = c.sqlite3_bind_text(stmt, 8, sf.ptr, @intCast(sf.len), c.SQLITE_STATIC) else _ = c.sqlite3_bind_null(stmt, 8);
        if (source_drawer_id) |sd| _ = c.sqlite3_bind_text(stmt, 9, sd.ptr, @intCast(sd.len), c.SQLITE_STATIC) else _ = c.sqlite3_bind_null(stmt, 9);
        if (adapter_name) |an| _ = c.sqlite3_bind_text(stmt, 10, an.ptr, @intCast(an.len), c.SQLITE_STATIC) else _ = c.sqlite3_bind_null(stmt, 10);

        _ = c.sqlite3_step(stmt);
    }

    pub fn invalidate(self: *KnowledgeGraph, subject: []const u8, predicate: []const u8, object: []const u8, ended: i64) !void {
        const db = self.db orelse return error.SqliteOpenFailed;
        const sql = "UPDATE kg_triples SET valid_to = ? WHERE subject = ? AND predicate = ? AND object = ? AND valid_to IS NULL";
        var stmt: ?*c.sqlite3_stmt = null;
        const prep_rc = c.sqlite3_prepare_v2(db, sql, -1, &stmt, null);
        if (prep_rc != c.SQLITE_OK) return error.SqlitePrepareFailed;
        defer _ = c.sqlite3_finalize(stmt);
        _ = c.sqlite3_bind_int64(stmt, 1, ended);
        _ = c.sqlite3_bind_text(stmt, 2, subject.ptr, @intCast(subject.len), c.SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 3, predicate.ptr, @intCast(predicate.len), c.SQLITE_STATIC);
        _ = c.sqlite3_bind_text(stmt, 4, object.ptr, @intCast(object.len), c.SQLITE_STATIC);
        _ = c.sqlite3_step(stmt);
    }

    pub fn queryEntity(self: *KnowledgeGraph, name: []const u8, as_of: ?i64, direction: enum { outgoing, incoming, both }) ![]KgTriple {
        _ = self;
        _ = as_of;
        _ = direction;
        _ = name;
        return &.{};
    }

    pub fn stats(self: *KnowledgeGraph) !KgStats {
        const db = self.db orelse return error.SqliteOpenFailed;
        var entity_count: i64 = 0;
        var triple_count: i64 = 0;
        var current_facts: i64 = 0;
        var expired_facts: i64 = 0;
        var rel_types: i64 = 0;

        const queries = [_]struct { sql: []const u8, result: *i64 }{
            .{ .sql = "SELECT COUNT(*) FROM kg_entities", .result = &entity_count },
            .{ .sql = "SELECT COUNT(*) FROM kg_triples", .result = &triple_count },
            .{ .sql = "SELECT COUNT(*) FROM kg_triples WHERE valid_to IS NULL", .result = &current_facts },
            .{ .sql = "SELECT COUNT(*) FROM kg_triples WHERE valid_to IS NOT NULL", .result = &expired_facts },
            .{ .sql = "SELECT COUNT(DISTINCT predicate) FROM kg_triples", .result = &rel_types },
        };

        for (queries) |q| {
            var stmt: ?*c.sqlite3_stmt = null;
            _ = c.sqlite3_prepare_v2(db, q.sql.ptr, @intCast(q.sql.len), &stmt, null);
            if (stmt) |s| {
                defer _ = c.sqlite3_finalize(s);
                if (c.sqlite3_step(s) == c.SQLITE_ROW) {
                    q.result.* = c.sqlite3_column_int64(s, 0);
                }
            }
        }

        return .{
            .entity_count = @intCast(entity_count),
            .triple_count = @intCast(triple_count),
            .current_facts = @intCast(current_facts),
            .expired_facts = @intCast(expired_facts),
            .relationship_types = @intCast(rel_types),
        };
    }
};

const testing = std.testing;

test "KnowledgeGraph addEntity and stats" {
    var kg = try KnowledgeGraph.init(testing.allocator, ":memory:");
    defer kg.deinit();

    try kg.addEntity("TestEntity", "person", null);
    try kg.addEntity("ProjectX", "project", "version:1.0");

    const s = try kg.stats();
    try testing.expectEqual(@as(usize, 2), s.entity_count);
}

test "KnowledgeGraph addTriple and invalidate" {
    var kg = try KnowledgeGraph.init(testing.allocator, ":memory:");
    defer kg.deinit();

    try kg.addTriple("Alice", "works_on", "ProjectX", null, null, 0.9, null, null, null, null);

    const s = try kg.stats();
    try testing.expect(s.triple_count >= 1);

    try kg.invalidate("Alice", "works_on", "ProjectX", 1700000000);

    const s2 = try kg.stats();
    try testing.expect(s2.expired_facts >= 1);
}