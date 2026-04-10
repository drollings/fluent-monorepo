//! Tests for vector_db.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const vector = @import("root.zig");
const vector_db_mod = @import("vector_db.zig");

test "GuidanceDb init and schema" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    const db_path = try std.fmt.allocPrint(allocator, "{s}/test.guidance.db", .{tmp_path});
    defer allocator.free(db_path);

    var noop: vector.NoopEmbedding = .{};
    var db = try vector_db_mod.GuidanceDb.init(allocator, db_path, noop.provider());
    defer db.deinit();

    // Verify schema_version row was inserted.
    var stmt: ?*vector_db_mod.c.sqlite3_stmt = null;
    const rc = vector_db_mod.c.sqlite3_prepare_v2(db.db, "SELECT version FROM schema_version", -1, &stmt, null);
    try std.testing.expectEqual(@as(c_int, vector_db_mod.c.SQLITE_OK), rc);
    defer _ = vector_db_mod.c.sqlite3_finalize(stmt);
    try std.testing.expectEqual(@as(c_int, vector_db_mod.c.SQLITE_ROW), vector_db_mod.c.sqlite3_step(stmt));
    try std.testing.expectEqual(@as(c_int, 2), vector_db_mod.c.sqlite3_column_int(stmt, 0));
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
    var db = try vector_db_mod.GuidanceDb.init(allocator, db_path, noop.provider());
    defer db.deinit();

    const src_dir = try std.fmt.allocPrint(allocator, "{s}/src", .{tmp_path});
    defer allocator.free(src_dir);

    try db.syncFromDir(allocator, src_dir);

    // Keyword search by name
    const results = try db.keywordSearch(allocator, "frobnicate", 10);
    defer {
        for (results) |r| vector_db_mod.GuidanceDb.freeSearchResult(allocator, r);
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
    var db = try vector_db_mod.GuidanceDb.init(allocator, db_path, noop.provider());
    defer db.deinit();

    const src_dir = try std.fmt.allocPrint(allocator, "{s}/src", .{tmp_path});
    defer allocator.free(src_dir);

    try db.syncFromDir(allocator, src_dir);

    const results = try db.search(allocator, "Widget", 10);
    defer {
        for (results) |r| vector_db_mod.GuidanceDb.freeSearchResult(allocator, r);
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
        var db = try vector_db_mod.GuidanceDb.init(allocator, db_path, noop.provider());
        defer db.deinit();
        const src_dir = try std.fmt.allocPrint(allocator, "{s}/src", .{tmp_path});
        defer allocator.free(src_dir);
        try db.syncFromDir(allocator, src_dir);
    }

    // Count rows after first sync
    {
        var db = try vector_db_mod.GuidanceDb.init(allocator, db_path, noop.provider());
        defer db.deinit();
        const src_dir = try std.fmt.allocPrint(allocator, "{s}/src", .{tmp_path});
        defer allocator.free(src_dir);
        // Second sync should not duplicate rows
        try db.syncFromDir(allocator, src_dir);

        var stmt: ?*vector_db_mod.c.sqlite3_stmt = null;
        _ = vector_db_mod.c.sqlite3_prepare_v2(db.db, "SELECT COUNT(*) FROM ast_nodes WHERE module='src.beta'", -1, &stmt, null);
        defer _ = vector_db_mod.c.sqlite3_finalize(stmt);
        _ = vector_db_mod.c.sqlite3_step(stmt);
        const count = vector_db_mod.c.sqlite3_column_int(stmt, 0);
        try std.testing.expectEqual(@as(c_int, 1), count); // only the module row
    }
}
test "buildEmbeddingText descriptor-bag format" {
    const allocator = std.testing.allocator;
    // Member with comment, signature, and parent module context
    const text = try vector_db_mod.GuidanceDb.buildEmbeddingText(
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
    const text = try vector_db_mod.GuidanceDb.buildEmbeddingText(
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
    const prose = try vector_db_mod.GuidanceDb.moduleToProse(allocator, "src.guidance.vector.math");
    defer allocator.free(prose);
    // "src" stripped, last 3 parts: guidance vector math
    try std.testing.expectEqualStrings("guidance vector math", prose);
}
test "extractParamNames strips types" {
    const allocator = std.testing.allocator;
    const params = try vector_db_mod.GuidanceDb.extractParamNames(allocator, "fn foo(allocator: std.mem.Allocator, x: u32, y: []const u8) void");
    try std.testing.expect(params != null);
    defer allocator.free(params.?);
    // allocator is skipped, x and y remain
    try std.testing.expect(std.mem.indexOf(u8, params.?, "x") != null);
    try std.testing.expect(std.mem.indexOf(u8, params.?, "y") != null);
    try std.testing.expect(std.mem.indexOf(u8, params.?, "allocator") == null);
}
test "DbSyncBuilder defaults: cache_limit=0, no capabilities, no aliases" {
    var noop: vector.NoopEmbedding = .{};
    const embedder = noop.provider();

    const builder = vector_db_mod.DbSyncBuilder.init(
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

    const builder = vector_db_mod.DbSyncBuilder.init(
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

    const base = vector_db_mod.DbSyncBuilder.init(std.testing.allocator, "g", "db", embedder);
    const with_cap = base.withCapabilities("cap");
    const with_limit = base.cacheLimit(99);

    // base is unmodified
    try std.testing.expect(base.capabilities_dir == null);
    try std.testing.expectEqual(@as(u32, 0), base.cache_limit);

    // derived builders have their own values
    try std.testing.expectEqualStrings("cap", with_cap.capabilities_dir.?);
    try std.testing.expectEqual(@as(u32, 99), with_limit.cache_limit);
}
test "parseCodehealthDirective: ignore" {
    const dir = vector_db_mod.parseCodehealthDirective("/// CODEHEALTH: ignore vtable-impl\n/// Invoked by reactor loop.").?;
    try std.testing.expectEqualStrings("vtable-impl", dir.ignore_reason);
}
test "parseCodehealthDirective: milestone" {
    const dir = vector_db_mod.parseCodehealthDirective("/// CODEHEALTH: milestone v2.0\n/// Planned for distributed caching.").?;
    try std.testing.expectEqualStrings("v2.0", dir.milestone);
}
test "parseCodehealthDirective: deprecated" {
    const dir = vector_db_mod.parseCodehealthDirective("/// CODEHEALTH: deprecated use searchOptimized instead").?;
    try std.testing.expectEqualStrings("use searchOptimized instead", dir.deprecated_by);
}
test "parseCodehealthDirective: no directive returns null" {
    try std.testing.expect(vector_db_mod.parseCodehealthDirective("/// Regular doc comment") == null);
    try std.testing.expect(vector_db_mod.parseCodehealthDirective("") == null);
}
test "parseCodehealthDirective: multiline comment extracts first line only" {
    const dir = vector_db_mod.parseCodehealthDirective("/// CODEHEALTH: ignore test-helper\n/// other stuff here").?;
    try std.testing.expectEqualStrings("test-helper", dir.ignore_reason);
}
