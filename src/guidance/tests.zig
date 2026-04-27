//! Unit tests for src/guidance — json_store merge logic, sync, config, and commit helpers.
//!
//! Run with: zig build test-guidance
const std = @import("std");
const types = @import("types.zig");
const json_store = @import("sync/json_store.zig");
const main = @import("main.zig");
const sync_mod = @import("sync.zig");
const config_mod = @import("config.zig");

// Pull in inline tests from new source-code-first comment management modules.
comptime {
    _ = @import("sync/line_verify.zig");
    _ = @import("comments/core.zig");
    _ = @import("comments/inserter.zig");
    _ = @import("comments/header.zig");
    _ = @import("comments/sync.zig");
    // M9: Pull in doc_parser tests (parseDocContent, anchors, frontmatter)
    _ = @import("doc_parser.zig");
    // M9: Pull in staged tests (formatStaged, capability_doc, See Also cap)
    _ = @import("staged.zig");
    // Phase 2 DRY: core/ modules
    _ = @import("core/intent.zig");
    _ = @import("core/ranking.zig");
    _ = @import("core/excerpt.zig");
    _ = @import("core/skill_loader.zig");
    _ = @import("core/metadata.zig");
    _ = @import("core/format.zig");
    _ = @import("core/drift.zig");
    // Phase 0/1/1.5/2a/2b: codehealth and call_extractor tests
    _ = @import("codehealth/main.zig");
    _ = @import("codehealth/extractor.zig");
    _ = @import("codehealth/orphan.zig");
    _ = @import("codehealth/build_validation.zig");
    _ = @import("codehealth/test_audit.zig");
    _ = @import("codehealth/test_mover.zig");
}

// ---------------------------------------------------------------------------
// parseHunkRanges
// ---------------------------------------------------------------------------

test "parseHunkRanges returns empty for non-hunk input" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const chunk = "diff --git a/foo.zig b/foo.zig\nindex abc..def 100644\n--- a/foo.zig\n+++ b/foo.zig\n";
    const ranges = try main.parseHunkRangesPub(allocator, chunk);
    defer allocator.free(ranges);
    try std.testing.expect(ranges.len == 0);
}

test "parseHunkRanges parses single @@ header" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const chunk = "diff --git a/foo.zig b/foo.zig\n@@ -10,6 +15,8 @@ fn foo() {\n+added line\n";
    const ranges = try main.parseHunkRangesPub(allocator, chunk);
    defer allocator.free(ranges);

    try std.testing.expect(ranges.len == 1);
    try std.testing.expectEqual(@as(u32, 15), ranges[0][0]); // start
    try std.testing.expectEqual(@as(u32, 23), ranges[0][1]); // 15 + 8
}

test "parseHunkRanges parses multiple @@ headers" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const chunk =
        \\diff --git a/foo.zig b/foo.zig
        \\@@ -1,3 +1,4 @@ fn first() {
        \\ unchanged
        \\+added
        \\@@ -50,2 +51,3 @@ fn second() {
        \\ other
        \\+also added
    ;
    const ranges = try main.parseHunkRangesPub(allocator, chunk);
    defer allocator.free(ranges);

    try std.testing.expect(ranges.len == 2);
    try std.testing.expectEqual(@as(u32, 1), ranges[0][0]);
    try std.testing.expectEqual(@as(u32, 5), ranges[0][1]); // 1 + 4
    try std.testing.expectEqual(@as(u32, 51), ranges[1][0]);
    try std.testing.expectEqual(@as(u32, 54), ranges[1][1]); // 51 + 3
}

test "parseHunkRanges handles @@ without count (implicit 1)" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    // Some diffs omit the count when it is 1: @@ -5 +7 @@
    const chunk = "@@ -5 +7 @@ fn single() {}\n+line\n";
    const ranges = try main.parseHunkRangesPub(allocator, chunk);
    defer allocator.free(ranges);

    try std.testing.expect(ranges.len == 1);
    try std.testing.expectEqual(@as(u32, 7), ranges[0][0]);
    try std.testing.expectEqual(@as(u32, 8), ranges[0][1]); // 7 + 1
}

// ---------------------------------------------------------------------------
// loadChangedMembers
// ---------------------------------------------------------------------------

test "loadChangedMembers returns empty for missing JSON file" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    const members = try main.loadChangedMembersPub(allocator, tmp_path, "src/nonexistent.zig", &.{});
    defer {
        for (members) |m| m.deinit(allocator);
        allocator.free(members);
    }
    try std.testing.expect(members.len == 0);
}

test "loadChangedMembers returns all members when hunk_ranges is empty" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    // Create .guidance/src directory structure (matching what loadChangedMembers expects)
    try tmp.dir.makePath(".guidance/src");
    const json_content =
        \\{
        \\  "meta": {"module": "foo", "source": "src/foo.zig", "language": "zig"},
        \\  "comment": "Foo module.",
        \\  "members": [
        \\    {"type": "fn_decl", "name": "alpha", "line": 10, "is_pub": true,
        \\     "comment": "Does alpha.", "signature": "fn alpha() void",
        \\     "tags": [], "patterns": [], "members": []},
        \\    {"type": "fn_decl", "name": "beta",  "line": 30, "is_pub": true,
        \\     "comment": "Does beta.",  "signature": "fn beta() void",
        \\     "tags": [], "patterns": [], "members": []}
        \\  ]
        \\}
    ;
    const f = try tmp.dir.createFile(".guidance/src/foo.zig.json", .{});
    try f.writeAll(json_content);
    f.close();

    // guidance_root should be the .guidance directory
    const guidance_root = try std.fs.path.join(allocator, &.{ tmp_path, ".guidance" });
    defer allocator.free(guidance_root);

    // rel_path is just "foo.zig" (without src prefix), function adds src/
    const members = try main.loadChangedMembersPub(allocator, guidance_root, "foo.zig", &.{});
    defer {
        for (members) |m| m.deinit(allocator);
        allocator.free(members);
    }

    try std.testing.expect(members.len == 2);
    try std.testing.expectEqualStrings("alpha", members[0].name);
    try std.testing.expectEqualStrings("Does alpha.", members[0].comment);
    try std.testing.expectEqual(@as(?u32, 10), members[0].line);
    try std.testing.expectEqualStrings("beta", members[1].name);
}

test "loadChangedMembers filters by hunk range with context window" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    try tmp.dir.makePath(".guidance/src");
    const json_content =
        \\{
        \\  "meta": {"module": "bar", "source": "src/bar.zig", "language": "zig"},
        \\  "comment": "",
        \\  "members": [
        \\    {"type": "fn_decl", "name": "near",  "line": 20, "is_pub": true,
        \\     "comment": "Near the hunk.", "signature": "fn near() void",
        \\     "tags": [], "patterns": [], "members": []},
        \\    {"type": "fn_decl", "name": "far",   "line": 200, "is_pub": true,
        \\     "comment": "Far from hunk.", "signature": "fn far() void",
        \\     "tags": [], "patterns": [], "members": []}
        \\  ]
        \\}
    ;
    const f = try tmp.dir.createFile(".guidance/src/bar.zig.json", .{});
    try f.writeAll(json_content);
    f.close();

    const guidance_root = try std.fs.path.join(allocator, &.{ tmp_path, ".guidance" });
    defer allocator.free(guidance_root);

    // Hunk touches new-file lines 25–35 → "near" (line 20) is within ±15 context, "far" (200) is not.
    const hunk_ranges = [_][2]u32{.{ 25, 35 }};
    const members = try main.loadChangedMembersPub(allocator, guidance_root, "bar.zig", &hunk_ranges);
    defer {
        for (members) |m| m.deinit(allocator);
        allocator.free(members);
    }

    try std.testing.expect(members.len == 1);
    try std.testing.expectEqualStrings("near", members[0].name);
}

// ---------------------------------------------------------------------------
// isExactNameMatch
// ---------------------------------------------------------------------------

test "isExactNameMatch matches case-insensitively" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");

    const terms = [_][]const u8{ "cmdexplain", "sync" };
    try std.testing.expect(main.isExactNameMatchPub("cmdExplain", &terms));
    try std.testing.expect(main.isExactNameMatchPub("CMDEXPLAIN", &terms));
    try std.testing.expect(!main.isExactNameMatchPub("other", &terms));
}

test "isExactNameMatch handles empty terms" {
    const terms = [_][]const u8{};
    try std.testing.expect(!main.isExactNameMatchPub("anything", &terms));
}

// ---------------------------------------------------------------------------
// loadSkillsFromJson
// ---------------------------------------------------------------------------

test "loadSkillsFromJson returns null for missing file" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const result = main.loadSkillsFromJsonPub(allocator, "/nonexistent/path/file.json");
    try std.testing.expect(result == null);
}

test "loadSkillsFromJson extracts skill refs from array" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    const json_content =
        \\{"skills": ["zig-current", "gof-patterns"], "members": []}
    ;
    const f = try tmp.dir.createFile("test.json", .{});
    try f.writeAll(json_content);
    f.close();

    const json_path = try std.fs.path.join(allocator, &.{ tmp_path, "test.json" });
    defer allocator.free(json_path);

    const result = main.loadSkillsFromJsonPub(allocator, json_path);
    try std.testing.expect(result != null);
    defer allocator.free(result.?);

    try std.testing.expect(std.mem.indexOf(u8, result.?, "zig-current") != null);
    try std.testing.expect(std.mem.indexOf(u8, result.?, "gof-patterns") != null);
}

test "loadSkillsFromJson handles object format with ref field" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    const json_content =
        \\{"skills": [{"ref": "zig-current", "context": "API changes"}], "members": []}
    ;
    const f = try tmp.dir.createFile("test2.json", .{});
    try f.writeAll(json_content);
    f.close();

    const json_path = try std.fs.path.join(allocator, &.{ tmp_path, "test2.json" });
    defer allocator.free(json_path);

    const result = main.loadSkillsFromJsonPub(allocator, json_path);
    try std.testing.expect(result != null);
    defer allocator.free(result.?);

    try std.testing.expect(std.mem.indexOf(u8, result.?, "zig-current") != null);
}

// ---------------------------------------------------------------------------
// loadUsedByFromJson
// ---------------------------------------------------------------------------

test "loadUsedByFromJson returns null for missing file" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const result = main.loadUsedByFromJsonPub(allocator, "/nonexistent/path/file.json");
    try std.testing.expect(result == null);
}

test "loadUsedByFromJson extracts used_by array" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    const json_content =
        \\{"used_by": ["src/main.zig", "src/db.zig"], "members": []}
    ;
    const f = try tmp.dir.createFile("test3.json", .{});
    try f.writeAll(json_content);
    f.close();

    const json_path = try std.fs.path.join(allocator, &.{ tmp_path, "test3.json" });
    defer allocator.free(json_path);

    const result = main.loadUsedByFromJsonPub(allocator, json_path);
    try std.testing.expect(result != null);
    defer {
        for (result.?) |r| allocator.free(r);
        allocator.free(result.?);
    }

    try std.testing.expect(result.?.len == 2);
    try std.testing.expectEqualStrings("src/main.zig", result.?[0]);
    try std.testing.expectEqualStrings("src/db.zig", result.?[1]);
}

// ---------------------------------------------------------------------------
// loadPublicMemberNames
// ---------------------------------------------------------------------------

test "loadPublicMemberNames returns null for missing file" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const result = main.loadPublicMemberNamesPub(allocator, "/nonexistent/path/file.json");
    try std.testing.expect(result == null);
}

test "loadPublicMemberNames filters public non-test members" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    const json_content =
        \\{"members": [
        \\  {"type": "fn_decl", "name": "publicFunc", "is_pub": true, "line": 1},
        \\  {"type": "fn_decl", "name": "privateFunc", "is_pub": false, "line": 2},
        \\  {"type": "test_decl", "name": "testSomething", "is_pub": true, "line": 3}
        \\]}
    ;
    const f = try tmp.dir.createFile("test4.json", .{});
    try f.writeAll(json_content);
    f.close();

    const json_path = try std.fs.path.join(allocator, &.{ tmp_path, "test4.json" });
    defer allocator.free(json_path);

    const result = main.loadPublicMemberNamesPub(allocator, json_path);
    try std.testing.expect(result != null);
    defer {
        for (result.?) |r| allocator.free(r);
        allocator.free(result.?);
    }

    try std.testing.expect(result.?.len == 1);
    try std.testing.expectEqualStrings("publicFunc", result.?[0]);
}

// ---------------------------------------------------------------------------
// explainExtractExcerpt
// ---------------------------------------------------------------------------

test "explainExtractExcerpt extracts function body" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const src =
        \\fn foo() void {
        \\    const x = 1;
        \\    _ = x;
        \\}
        \\
        \\fn bar() void {
        \\    // another function
        \\}
    ;

    const result = try main.explainExtractExcerptPub(allocator, src, 1, "fn_decl");
    defer allocator.free(result);

    try std.testing.expect(std.mem.indexOf(u8, result, "fn foo()") != null);
    try std.testing.expect(std.mem.indexOf(u8, result, "const x = 1") != null);
    // Should stop at closing brace
    try std.testing.expect(std.mem.indexOf(u8, result, "fn bar()") == null);
}

test "explainExtractExcerpt handles empty source" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const result = try main.explainExtractExcerptPub(allocator, "", 1, "fn_decl");
    defer allocator.free(result);

    try std.testing.expectEqualStrings("", result);
}

// ---------------------------------------------------------------------------
// explainGrepFile
// ---------------------------------------------------------------------------

test "explainGrepFile returns empty for missing file" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const terms = [_][]const u8{"test"};
    const result = try main.explainGrepFilePub(allocator, "/nonexistent/file.zig", &terms, 10);
    defer allocator.free(result);

    try std.testing.expect(result.len == 0);
}

test "explainGrepFile finds matching lines" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    const src =
        \\fn foo() void {
        \\    // test comment
        \\    const x = 42;
        \\}
        \\// Test line
        \\fn bar() void {}
    ;
    const f = try tmp.dir.createFile("test.src", .{});
    try f.writeAll(src);
    f.close();

    const file_path = try std.fs.path.join(allocator, &.{ tmp_path, "test.src" });
    defer allocator.free(file_path);

    const terms = [_][]const u8{ "foo", "bar" };
    const result = try main.explainGrepFilePub(allocator, file_path, &terms, 10);
    defer allocator.free(result);

    // Lines 1 (fn foo) and 6 (fn bar) should match
    try std.testing.expect(result.len >= 1);
    try std.testing.expect(result[0] == 1); // fn foo is on line 1

    // The test line should not match (starts with //)
    for (result) |line| {
        try std.testing.expect(line != 2); // // test comment should not match
        try std.testing.expect(line != 5); // // Test line should not match
    }
}

// ---------------------------------------------------------------------------
// isShortQuery
// ---------------------------------------------------------------------------

test "isShortQuery returns true for short queries" {
    try std.testing.expect(main.isShortQueryPub("sync"));
    try std.testing.expect(main.isShortQueryPub("cmdExplain"));
    try std.testing.expect(main.isShortQueryPub("cosineSimilarity"));
}

test "isShortQuery returns false for long queries" {
    try std.testing.expect(!main.isShortQueryPub("get member by name")); // 4 words
    try std.testing.expect(!main.isShortQueryPub("How do I find all the functions that implement a specific pattern in the codebase"));
    try std.testing.expect(!main.isShortQueryPub("What is the relationship between the sync module and the database module"));
}

test "isShortQuery returns false for question queries" {
    try std.testing.expect(!main.isShortQueryPub("sync?")); // ends with ?
    try std.testing.expect(!main.isShortQueryPub("How does sync work")); // starts with How
    try std.testing.expect(!main.isShortQueryPub("Where is the config")); // starts with Where
    try std.testing.expect(!main.isShortQueryPub("What is this")); // starts with What
    try std.testing.expect(!main.isShortQueryPub("If I run gen")); // starts with If
    try std.testing.expect(!main.isShortQueryPub("Why does it fail")); // starts with Why
    try std.testing.expect(!main.isShortQueryPub("When should I")); // starts with When
    try std.testing.expect(!main.isShortQueryPub("Does it support")); // starts with Does
}

// ---------------------------------------------------------------------------
// loadSkillPara
// ---------------------------------------------------------------------------

test "loadSkillPara returns null for missing skill" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    const result = main.loadSkillParaPub(allocator, tmp_path, tmp_path, "nonexistent-skill");
    try std.testing.expect(result == null);
}

test "loadSkillPara extracts first paragraph from SKILL.md" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    // loadSkillPara looks for SKILL.md in {guidance_dir}/skills/{name}/SKILL.md
    // or {cwd}/doc/skills/{name}/SKILL.md
    // Create skills/test-skill/SKILL.md under the guidance_dir
    try tmp.dir.makePath("skills/test-skill");
    const skill_content = "This is the first paragraph.\n\nThis is the second paragraph.";
    const skill_file = try tmp.dir.createFile("skills/test-skill/SKILL.md", .{});
    try skill_file.writeAll(skill_content);
    skill_file.close();

    // The guidance_dir should be where skills/ is located (tmp_path itself in this case)
    const result = main.loadSkillParaPub(allocator, tmp_path, tmp_path, "test-skill");
    try std.testing.expect(result != null);
    defer allocator.free(result.?);

    try std.testing.expect(std.mem.indexOf(u8, result.?, "This is the first paragraph") != null);
    try std.testing.expect(std.mem.indexOf(u8, result.?, "second") == null); // Should stop at paragraph boundary
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Creates a Zig member from a name slice, hash, and doc slice, returning the member object.
fn makeMember(name: []const u8, hash: ?[]const u8, doc: ?[]const u8) types.Member {
    return .{
        .type = .fn_decl,
        .name = name,
        .match_hash = hash,
        .comment = doc,
    };
}

// ---------------------------------------------------------------------------
// dupeMember: every field is independently owned
// ---------------------------------------------------------------------------

test "dupeMember produces independent copies" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var store = json_store.JsonStore.init(allocator);

    const orig = types.Member{
        .type = .fn_decl,
        .name = "hello",
        .match_hash = "abc123",
        .signature = "fn hello() void",
        .comment = "Say hello.",
        .returns = "void",
        .is_pub = true,
        .line = 10,
    };

    const copy = try store.dupeMember(orig);
    defer store.freeMember(copy);

    // Verify values match.
    try std.testing.expectEqualStrings(orig.name, copy.name);
    try std.testing.expectEqualStrings(orig.match_hash.?, copy.match_hash.?);
    try std.testing.expectEqualStrings(orig.signature.?, copy.signature.?);
    try std.testing.expectEqualStrings(orig.comment.?, copy.comment.?);
    try std.testing.expectEqualStrings(orig.returns.?, copy.returns.?);
    try std.testing.expect(copy.is_pub == orig.is_pub);
    try std.testing.expect(copy.line.? == orig.line.?);

    // Verify that pointers are different (truly independent).
    try std.testing.expect(copy.name.ptr != orig.name.ptr);
}

test "dupeMember with params" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var store = json_store.JsonStore.init(allocator);

    const params = [_]types.Param{
        .{ .name = "x", .type = "u32", .default = null },
        .{ .name = "y", .type = null, .default = "0" },
    };
    const orig = types.Member{
        .type = .fn_decl,
        .name = "add",
        .params = &params,
    };

    const copy = try store.dupeMember(orig);
    defer store.freeMember(copy);

    try std.testing.expect(copy.params.len == 2);
    try std.testing.expectEqualStrings("x", copy.params[0].name);
    try std.testing.expectEqualStrings("u32", copy.params[0].type.?);
    try std.testing.expectEqualStrings("y", copy.params[1].name);
    try std.testing.expectEqualStrings("0", copy.params[1].default.?);

    // Pointers must differ from the stack-allocated original slice.
    try std.testing.expect(copy.params.ptr != orig.params.ptr);
}

test "dupeMember with nested members" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var store = json_store.JsonStore.init(allocator);

    const nested = [_]types.Member{
        .{ .type = .method, .name = "init" },
        .{ .type = .method, .name = "deinit" },
    };
    const orig = types.Member{
        .type = .@"struct",
        .name = "Foo",
        .members = &nested,
    };

    const copy = try store.dupeMember(orig);
    defer store.freeMember(copy);

    try std.testing.expect(copy.members.len == 2);
    try std.testing.expectEqualStrings("init", copy.members[0].name);
    try std.testing.expectEqualStrings("deinit", copy.members[1].name);
    try std.testing.expect(copy.members.ptr != orig.members.ptr);
}

// ---------------------------------------------------------------------------
// mergeMembers: ownership and correctness
// ---------------------------------------------------------------------------

test "mergeMembers with no existing produces all new members" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var store = json_store.JsonStore.init(allocator);

    const source = [_]types.Member{
        .{ .type = .fn_decl, .name = "foo", .match_hash = "h1" },
        .{ .type = .fn_decl, .name = "bar", .match_hash = "h2" },
    };

    const result = try store.mergeMembers(&source, &.{}, true);
    defer {
        for (result.members) |m| store.freeMember(m);
        allocator.free(result.members);
    }

    try std.testing.expect(result.members.len == 2);
    try std.testing.expect(result.members_added == 2);
    try std.testing.expect(result.members_removed == 0);
    try std.testing.expect(result.has_changes == true);
    try std.testing.expectEqualStrings("foo", result.members[0].name);
    try std.testing.expectEqualStrings("bar", result.members[1].name);
}

test "mergeMembers preserves comment when hash unchanged" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var store = json_store.JsonStore.init(allocator);

    const source = [_]types.Member{
        .{ .type = .fn_decl, .name = "foo", .match_hash = "same_hash", .comment = null },
    };
    const existing = [_]types.Member{
        .{ .type = .fn_decl, .name = "foo", .match_hash = "same_hash", .comment = "Hand-written doc." },
    };

    const result = try store.mergeMembers(&source, &existing, true);
    defer {
        for (result.members) |m| store.freeMember(m);
        allocator.free(result.members);
    }

    try std.testing.expect(result.members.len == 1);
    try std.testing.expectEqualStrings("Hand-written doc.", result.members[0].comment.?);
    // Hash unchanged — no update counted.
    try std.testing.expect(result.members_updated == 0);
    try std.testing.expect(result.has_changes == false);
}

test "mergeMembers counts update when hash changed" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var store = json_store.JsonStore.init(allocator);

    const source = [_]types.Member{
        .{ .type = .fn_decl, .name = "foo", .match_hash = "new_hash" },
    };
    const existing = [_]types.Member{
        .{ .type = .fn_decl, .name = "foo", .match_hash = "old_hash" },
    };

    const result = try store.mergeMembers(&source, &existing, true);
    defer {
        for (result.members) |m| store.freeMember(m);
        allocator.free(result.members);
    }

    try std.testing.expect(result.members_updated == 1);
    try std.testing.expect(result.has_changes == true);
}

test "mergeMembers counts removed members" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var store = json_store.JsonStore.init(allocator);

    const source = [_]types.Member{
        .{ .type = .fn_decl, .name = "foo" },
    };
    const existing = [_]types.Member{
        .{ .type = .fn_decl, .name = "foo" },
        .{ .type = .fn_decl, .name = "old_func" },
    };

    const result = try store.mergeMembers(&source, &existing, true);
    defer {
        for (result.members) |m| store.freeMember(m);
        allocator.free(result.members);
    }

    // "old_func" not in source → removed.
    try std.testing.expect(result.members_removed == 1);
    try std.testing.expect(result.has_changes == true);
    // Only "foo" in result; old_func is dropped.
    try std.testing.expect(result.members.len == 1);
}

test "mergeMembers no changes when source matches existing exactly" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var store = json_store.JsonStore.init(allocator);

    const source = [_]types.Member{
        .{ .type = .fn_decl, .name = "foo", .match_hash = "h1" },
    };
    const existing = [_]types.Member{
        .{ .type = .fn_decl, .name = "foo", .match_hash = "h1" },
    };

    const result = try store.mergeMembers(&source, &existing, true);
    defer {
        for (result.members) |m| store.freeMember(m);
        allocator.free(result.members);
    }

    try std.testing.expect(result.has_changes == false);
    try std.testing.expect(result.members_added == 0);
    try std.testing.expect(result.members_updated == 0);
    try std.testing.expect(result.members_removed == 0);
}

test "mergeMembers clears stale comment when hash changes" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var store = json_store.JsonStore.init(allocator);

    // Source has no doc comment (return type changed → new hash).
    const source = [_]types.Member{
        .{ .type = .fn_decl, .name = "doSomething", .match_hash = "new_hash", .comment = null },
    };
    // Existing has a comment that is now stale.
    const existing = [_]types.Member{
        .{ .type = .fn_decl, .name = "doSomething", .match_hash = "old_hash", .comment = "Old stale description." },
    };

    const result = try store.mergeMembers(&source, &existing, true);
    defer {
        for (result.members) |m| store.freeMember(m);
        allocator.free(result.members);
    }

    // Comment must be null (blanked for infill), not the stale old one.
    try std.testing.expect(result.members[0].comment == null);
    try std.testing.expect(result.members_stale == 1);
    try std.testing.expect(result.has_changes == true);
}

test "mergeMembers preserves tags when hash unchanged" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var store = json_store.JsonStore.init(allocator);

    const tags = [_][]const u8{ "important", "public-api" };
    const source = [_]types.Member{
        .{ .type = .fn_decl, .name = "foo", .match_hash = "h1", .tags = &.{} },
    };
    const existing = [_]types.Member{
        .{ .type = .fn_decl, .name = "foo", .match_hash = "h1", .tags = &tags },
    };

    const result = try store.mergeMembers(&source, &existing, true);
    defer {
        for (result.members) |m| store.freeMember(m);
        allocator.free(result.members);
    }

    try std.testing.expect(result.members[0].tags.len == 2);
    try std.testing.expectEqualStrings("important", result.members[0].tags[0]);
}

// ---------------------------------------------------------------------------
// freeGuidanceDoc: smoke test (no double-free with GPA)
// ---------------------------------------------------------------------------

test "freeGuidanceDoc frees all fields without double-free" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var store = json_store.JsonStore.init(allocator);

    const doc = types.GuidanceDoc{
        .meta = .{
            .module = try allocator.dupe(u8, "my.module"),
            .source = try allocator.dupe(u8, "src/my.zig"),
        },
        .comment = try allocator.dupe(u8, "Module docs."),
        .skills = try store.dupeSkills(&.{
            .{ .ref = "zig-current", .context = "relevant" },
        }),
        .hashtags = try store.dupeStrings(&.{"#zig"}),
    };

    // Should not crash, no leaks reported by GPA.
    store.freeGuidanceDoc(doc);
}

// ---------------------------------------------------------------------------
// dupeSkills
// ---------------------------------------------------------------------------

test "dupeSkills produces independent copies" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var store = json_store.JsonStore.init(allocator);

    const orig = [_]types.Skill{
        .{ .ref = "zig-current", .context = "API changes" },
        .{ .ref = "gof-patterns", .context = null },
    };

    const copy = try store.dupeSkills(&orig);
    defer {
        for (copy) |s| {
            allocator.free(s.ref);
            if (s.context) |c| allocator.free(c);
        }
        allocator.free(copy);
    }

    try std.testing.expect(copy.len == 2);
    try std.testing.expectEqualStrings("zig-current", copy[0].ref);
    try std.testing.expectEqualStrings("API changes", copy[0].context.?);
    try std.testing.expectEqualStrings("gof-patterns", copy[1].ref);
    try std.testing.expect(copy[1].context == null);
    try std.testing.expect(copy[0].ref.ptr != orig[0].ref.ptr);
}

// ---------------------------------------------------------------------------
// Round-trip: mergeMembers does not alias source / existing strings
// ---------------------------------------------------------------------------

test "mergeMembers result is independent after source freed" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var store = json_store.JsonStore.init(allocator);

    // Build source members on the heap so we can free them independently.
    const src_name = try allocator.dupe(u8, "myFunc");
    const src_hash = try allocator.dupe(u8, "deadbeef");
    const src_sig = try allocator.dupe(u8, "fn myFunc() void");

    const src_members = try allocator.alloc(types.Member, 1);
    src_members[0] = .{
        .type = .fn_decl,
        .name = src_name,
        .match_hash = src_hash,
        .signature = src_sig,
    };

    const result = try store.mergeMembers(src_members, &.{}, true);

    // Free source members; result must remain valid.
    store.freeMember(src_members[0]);
    allocator.free(src_members);

    defer {
        for (result.members) |m| store.freeMember(m);
        allocator.free(result.members);
    }

    try std.testing.expect(result.members.len == 1);
    try std.testing.expectEqualStrings("myFunc", result.members[0].name);
    try std.testing.expectEqualStrings("fn myFunc() void", result.members[0].signature.?);
}

test "mergeMembers result is independent after existing freed" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var store = json_store.JsonStore.init(allocator);

    const ex_name = try allocator.dupe(u8, "stableFunc");
    const ex_hash = try allocator.dupe(u8, "cafebabe");
    const ex_doc = try allocator.dupe(u8, "My doc string.");

    const ex_members = try allocator.alloc(types.Member, 1);
    ex_members[0] = .{
        .type = .fn_decl,
        .name = ex_name,
        .match_hash = ex_hash,
        .comment = ex_doc,
    };

    const src_members = [_]types.Member{
        .{ .type = .fn_decl, .name = "stableFunc", .match_hash = "cafebabe" },
    };

    const result = try store.mergeMembers(&src_members, ex_members, true);

    // Free existing members; result must remain valid.
    store.freeMember(ex_members[0]);
    allocator.free(ex_members);

    defer {
        for (result.members) |m| store.freeMember(m);
        allocator.free(result.members);
    }

    try std.testing.expect(result.members.len == 1);
    // Hash matched — comment from existing should be preserved.
    try std.testing.expectEqualStrings("My doc string.", result.members[0].comment.?);
}

// ---------------------------------------------------------------------------
// Query engine: leak detection tests
//
// Each test creates an isolated temp directory, writes a minimal guidance JSON
// into it, runs QueryEngine.execute(), calls freeQueryResult, and lets GPA
// detect any unreleased memory.
// ---------------------------------------------------------------------------

/// Creates a temporary GUID file path using provided allocator, directory, filename, and module parameters.
fn writeTempGuidance(allocator: std.mem.Allocator, dir_path: []const u8, filename: []const u8, module: []const u8) ![]u8 {
    const path = try std.fs.path.join(allocator, &.{ dir_path, filename });
    errdefer allocator.free(path);

    const json = try std.fmt.allocPrint(allocator,
        \\{{
        \\  "meta": {{ "module": "{s}", "source": "src/fake.zig", "language": "zig" }},
        \\  "comment": "A test module.",
        \\  "skills": [{{"ref": "zig-current"}}],
        \\  "hashtags": ["#test"],
        \\  "members": [
        \\    {{"type": "fn_decl", "name": "doThing", "match_hash": "abc", "is_pub": true, "line": 1,
        \\      "signature": "fn doThing() void", "comment": "Does a thing.", "tags": [], "patterns": [], "members": []}}
        \\  ]
        \\}}
    , .{module});
    defer allocator.free(json);

    const file = try std.Io.Dir.createFileAbsolute(std.Io.Threaded.global_single_threaded.io(), path, .{});
    defer file.close();
    try file.writeAll(json);

    return path;
}

// ---------------------------------------------------------------------------
// M8: infillJsonFile / infillAllJson — cross-language infill sweep
// ---------------------------------------------------------------------------

/// Writes guidance JSON data to a file path, accepting directory, filename, comments, and a flag for member inclusion.
fn writeGuidanceJson(dir: std.fs.Dir, filename: []const u8, comment: ?[]const u8, has_member: bool) !void {
    var buf: [2048]u8 = undefined;
    var fbs = std.io.fixedBufferStream(&buf);
    const w = fbs.writer();
    try w.writeAll("{\"meta\":{\"module\":\"test\",\"source\":\"src/test.zig\"}");
    if (comment) |c| {
        try w.print(",\"comment\":\"{s}\"", .{c});
    }
    if (has_member) {
        try w.writeAll(",\"members\":[{\"type\":\"fn_decl\",\"name\":\"doThing\",\"line\":1,\"is_pub\":true}]");
    } else {
        try w.writeAll(",\"members\":[]");
    }
    try w.writeByte('}');
    const content = fbs.getWritten();
    const f = try dir.createFile(filename, .{});
    defer f.close();
    try f.writeAll(content);
}

test "infillJsonFile returns false when no enhancer configured" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    {
        var tmp = std.testing.tmpDir(.{});
        defer tmp.cleanup();
        const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
        defer allocator.free(tmp_path);

        try writeGuidanceJson(tmp.dir, "a.zig.json", null, false);
        const json_path = try std.fs.path.join(allocator, &.{ tmp_path, "a.zig.json" });
        defer allocator.free(json_path);

        var processor = sync_mod.SyncProcessor.init(allocator, tmp_path, tmp_path, false, false);
        defer processor.deinit();
        // enhancer is null → must return false without crashing.

        const changed = try processor.infillJsonFile(json_path);
        try std.testing.expect(!changed);
    }
}

test "infillJsonFile returns false when no enhancer" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    {
        var tmp = std.testing.tmpDir(.{});
        defer tmp.cleanup();
        const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
        defer allocator.free(tmp_path);

        try writeGuidanceJson(tmp.dir, "b.zig.json", null, false);
        const json_path = try std.fs.path.join(allocator, &.{ tmp_path, "b.zig.json" });
        defer allocator.free(json_path);

        var processor = sync_mod.SyncProcessor.init(allocator, tmp_path, tmp_path, false, false);
        defer processor.deinit();
        // No enhancer — must short-circuit.

        const changed = try processor.infillJsonFile(json_path);
        try std.testing.expect(!changed);
    }
}

test "infillJsonFile returns false for nonexistent path" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    {
        var tmp = std.testing.tmpDir(.{});
        defer tmp.cleanup();
        const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
        defer allocator.free(tmp_path);

        const json_path = try std.fs.path.join(allocator, &.{ tmp_path, "does_not_exist.json" });
        defer allocator.free(json_path);

        var processor = sync_mod.SyncProcessor.init(allocator, tmp_path, tmp_path, false, false);
        defer processor.deinit();
        // No enhancer — safe to call; returns false without error.

        const changed = try processor.infillJsonFile(json_path);
        try std.testing.expect(!changed);
    }
}

test "infillAllJson returns 0 when no enhancer configured" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    {
        var tmp = std.testing.tmpDir(.{});
        defer tmp.cleanup();
        const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
        defer allocator.free(tmp_path);

        try writeGuidanceJson(tmp.dir, "c.zig.json", null, true);

        var processor = sync_mod.SyncProcessor.init(allocator, tmp_path, tmp_path, false, false);
        defer processor.deinit();

        var skip: std.StringHashMapUnmanaged(void) = .empty;
        defer skip.deinit(allocator);

        const count = try processor.infillAllJson(tmp_path, &skip);
        try std.testing.expectEqual(@as(usize, 0), count);
    }
}

test "infillAllJson skips files in skip_paths" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    {
        var tmp = std.testing.tmpDir(.{});
        defer tmp.cleanup();
        const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
        defer allocator.free(tmp_path);

        try writeGuidanceJson(tmp.dir, "e.zig.json", null, true);
        const skip_file = try std.fs.path.join(allocator, &.{ tmp_path, "e.zig.json" });
        defer allocator.free(skip_file);

        var processor = sync_mod.SyncProcessor.init(allocator, tmp_path, tmp_path, false, false);
        defer processor.deinit();

        var skip: std.StringHashMapUnmanaged(void) = .empty;
        defer skip.deinit(allocator);
        try skip.put(allocator, skip_file, {});

        // File is in skip_paths; no enhancer → count 0, no crash.
        const count = try processor.infillAllJson(tmp_path, &skip);
        try std.testing.expectEqual(@as(usize, 0), count);
    }
}

test "infillAllJson ignores non-json files" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    {
        var tmp = std.testing.tmpDir(.{});
        defer tmp.cleanup();
        const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
        defer allocator.free(tmp_path);

        const f = try tmp.dir.createFile("README.md", .{});
        f.close();
        const g = try tmp.dir.createFile("notes.txt", .{});
        g.close();

        var processor = sync_mod.SyncProcessor.init(allocator, tmp_path, tmp_path, false, false);
        defer processor.deinit();

        var skip: std.StringHashMapUnmanaged(void) = .empty;
        defer skip.deinit(allocator);

        const count = try processor.infillAllJson(tmp_path, &skip);
        try std.testing.expectEqual(@as(usize, 0), count);
    }
}

test "infillAllJson processes .py.json files alongside .zig.json files" {
    // Verifies that the walk covers both extension types without crashing.
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    {
        var tmp = std.testing.tmpDir(.{});
        defer tmp.cleanup();
        const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
        defer allocator.free(tmp_path);

        try writeGuidanceJson(tmp.dir, "module.zig.json", "existing", false);
        try writeGuidanceJson(tmp.dir, "script.py.json", null, false);

        var processor = sync_mod.SyncProcessor.init(allocator, tmp_path, tmp_path, false, false);
        defer processor.deinit();
        // No enhancer → returns 0, but both files are visited without error.

        var skip: std.StringHashMapUnmanaged(void) = .empty;
        defer skip.deinit(allocator);

        const count = try processor.infillAllJson(tmp_path, &skip);
        try std.testing.expectEqual(@as(usize, 0), count);
    }
}

// ---------------------------------------------------------------------------
// config.zig: loadConfig — defaults and JSON parsing
// ---------------------------------------------------------------------------

test "loadConfig falls back to built-in defaults when no config file exists" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    // No config file in tmp_path — must use built-in defaults.
    var cfg = try config_mod.loadConfig(allocator, tmp_path);
    defer cfg.deinit();

    // guidance_root = {tmp_path}/.guidance
    const expected_root = try std.fs.path.join(allocator, &.{ tmp_path, ".guidance" });
    defer allocator.free(expected_root);
    try std.testing.expectEqualStrings(expected_root, cfg.guidance_root);

    // json_base == guidance_root
    try std.testing.expectEqualStrings(cfg.guidance_root, cfg.json_base);

    // skills_dir = {cwd}/skills (resolved from repo root, not guidance_root)
    const expected_skills = try std.fs.path.join(allocator, &.{ tmp_path, "skills" });
    defer allocator.free(expected_skills);
    try std.testing.expectEqualStrings(expected_skills, cfg.skills_dir);

    // inbox_dir = {guidance_root}/inbox
    const expected_inbox = try std.fs.path.join(allocator, &.{ expected_root, "inbox" });
    defer allocator.free(expected_inbox);
    try std.testing.expectEqualStrings(expected_inbox, cfg.inbox_dir);

    // Default src_dirs = ["src"]
    try std.testing.expect(cfg.src_dirs.len == 1);
    try std.testing.expectEqualStrings("src", cfg.src_dirs[0]);

    // Default model
    try std.testing.expectEqualStrings(config_mod.DEFAULT_MODEL, cfg.model_default);

    // Default providers
    try std.testing.expect(cfg.providers.len >= 1);
    try std.testing.expectEqualStrings("local", cfg.providers[0].name);
    try std.testing.expectEqualStrings(config_mod.DEFAULT_BASE_URL, cfg.providers[0].base_url);
    try std.testing.expectEqualStrings(config_mod.DEFAULT_CHAT_ENDPOINT, cfg.providers[0].chat_endpoint);
}

test "loadConfig deinit releases all memory (no leaks)" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    {
        var tmp = std.testing.tmpDir(.{});
        defer tmp.cleanup();
        const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
        defer allocator.free(tmp_path);

        var cfg = try config_mod.loadConfig(allocator, tmp_path);
        cfg.deinit();
    }
}

test "loadConfig reads guidance_dir from project config JSON" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    // Write a config JSON with a custom guidance_dir.
    try tmp.dir.makePath(".guidance");
    const cfg_json =
        \\{"guidance_dir": "custom-guidance", "models": {}, "providers": {"local": {"base_url": "http://localhost:11434", "chat_endpoint": "/v1/chat/completions"}}}
    ;
    const cfg_file = try tmp.dir.createFile(".guidance/guidance-config.json", .{});
    try cfg_file.writeAll(cfg_json);
    cfg_file.close();

    var cfg = try config_mod.loadConfig(allocator, tmp_path);
    defer cfg.deinit();

    const expected_root = try std.fs.path.join(allocator, &.{ tmp_path, "custom-guidance" });
    defer allocator.free(expected_root);
    try std.testing.expectEqualStrings(expected_root, cfg.guidance_root);
}

test "loadConfig reads src_dirs array from JSON" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    try tmp.dir.makePath(".guidance");
    const cfg_json =
        \\{"src_dirs": ["src", "lib", "tools"]}
    ;
    const cfg_file = try tmp.dir.createFile(".guidance/guidance-config.json", .{});
    try cfg_file.writeAll(cfg_json);
    cfg_file.close();

    var cfg = try config_mod.loadConfig(allocator, tmp_path);
    defer cfg.deinit();

    try std.testing.expect(cfg.src_dirs.len == 3);
    try std.testing.expectEqualStrings("src", cfg.src_dirs[0]);
    try std.testing.expectEqualStrings("lib", cfg.src_dirs[1]);
    try std.testing.expectEqualStrings("tools", cfg.src_dirs[2]);
}

test "loadConfig reads models.fast for model_fast" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    try tmp.dir.makePath(".guidance");
    const cfg_json =
        \\{"providers": {"local": {"base_url": "http://localhost:11434", "chat_endpoint": "/v1/chat/completions"}}, "models": {"default": "local:other:latest", "fast": "local:mymodel:v2"}}
    ;
    const cfg_file = try tmp.dir.createFile(".guidance/guidance-config.json", .{});
    try cfg_file.writeAll(cfg_json);
    cfg_file.close();

    var cfg = try config_mod.loadConfig(allocator, tmp_path);
    defer cfg.deinit();

    try std.testing.expectEqualStrings("local:mymodel:v2", cfg.model_fast);
    try std.testing.expectEqualStrings("local:other:latest", cfg.model_default);
}

test "loadConfig falls back to models.default when fast absent" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    try tmp.dir.makePath(".guidance");
    const cfg_json =
        \\{"providers": {"local": {"base_url": "http://localhost:11434", "chat_endpoint": "/v1/chat/completions"}}, "models": {"default": "local:default-model:latest"}}
    ;
    const cfg_file = try tmp.dir.createFile(".guidance/guidance-config.json", .{});
    try cfg_file.writeAll(cfg_json);
    cfg_file.close();

    var cfg = try config_mod.loadConfig(allocator, tmp_path);
    defer cfg.deinit();

    try std.testing.expectEqualStrings("local:default-model:latest", cfg.model_default);
}

test "loadConfig constructs providers with base_url and chat_endpoint" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    try tmp.dir.makePath(".guidance");
    const cfg_json =
        \\{"providers": {"myprovider": {"base_url": "http://myhost:9999", "chat_endpoint": "/v1/chat/completions"}}}
    ;
    const cfg_file = try tmp.dir.createFile(".guidance/guidance-config.json", .{});
    try cfg_file.writeAll(cfg_json);
    cfg_file.close();

    var cfg = try config_mod.loadConfig(allocator, tmp_path);
    defer cfg.deinit();

    try std.testing.expect(cfg.providers.len == 1);
    try std.testing.expectEqualStrings("myprovider", cfg.providers[0].name);
    try std.testing.expectEqualStrings("http://myhost:9999", cfg.providers[0].base_url);
    try std.testing.expectEqualStrings("/v1/chat/completions", cfg.providers[0].chat_endpoint);
}

test "loadConfig with invalid JSON falls back to defaults" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    try tmp.dir.makePath(".guidance");
    const cfg_file = try tmp.dir.createFile(".guidance/guidance-config.json", .{});
    try cfg_file.writeAll("not valid json {{{{");
    cfg_file.close();

    // Must not return an error — falls back to built-in defaults.
    var cfg = try config_mod.loadConfig(allocator, tmp_path);
    defer cfg.deinit();

    try std.testing.expectEqualStrings(config_mod.DEFAULT_MODEL, cfg.model_default);
}

// ---------------------------------------------------------------------------
// chunkIsIgnored / chunkFilePath / splitDiffByFile
// ---------------------------------------------------------------------------
//
// These tests guard against the class of bug where filter logic uses the
// wrong prefix (e.g. "guidance/" instead of ".guidance/"), causing
// guidance JSON diffs to leak into the commit prompt or source diffs to
// be silently dropped.

test "chunkIsIgnored: .guidance/ prefix is filtered" {
    const guidance_dir = ".guidance";
    const chunk =
        \\diff --git a/.guidance/src/foo.zig.json b/.guidance/src/foo.zig.json
        \\index 000..111 100644
        \\--- a/.guidance/src/foo.zig.json
        \\+++ b/.guidance/src/foo.zig.json
        \\@@ -1,3 +1,3 @@
    ;
    try std.testing.expect(main.chunkIsIgnoredPub(chunk, guidance_dir));
}

test "chunkIsIgnored: guidance/ prefix is NOT filtered (regression guard)" {
    // Old code filtered "guidance/". That prefix no longer exists in the repo;
    // filtering it would silently drop any future file with that name.
    const guidance_dir = ".guidance";
    const chunk =
        \\diff --git a/guidance/README.md b/guidance/README.md
        \\index 000..111 100644
        \\--- a/guidance/README.md
        \\+++ b/guidance/README.md
        \\@@ -1 +1 @@
    ;
    try std.testing.expect(!main.chunkIsIgnoredPub(chunk, guidance_dir));
}

test "chunkIsIgnored: regular source files are not filtered" {
    const guidance_dir = ".guidance";
    const src_chunk =
        \\diff --git a/src/main.zig b/src/main.zig
        \\index 000..111 100644
        \\--- a/src/main.zig
        \\+++ b/src/main.zig
        \\@@ -10,5 +10,6 @@ fn foo() void {
    ;
    try std.testing.expect(!main.chunkIsIgnoredPub(src_chunk, guidance_dir));

    const bin_chunk =
        \\diff --git a/bin/guidance-py b/bin/guidance-py
        \\index 000..111 100755
        \\--- a/bin/guidance-py
        \\+++ b/bin/guidance-py
        \\@@ -1,2 +1,3 @@
    ;
    try std.testing.expect(!main.chunkIsIgnoredPub(bin_chunk, guidance_dir));
}

test "chunkFilePath: extracts path from diff --git header" {
    const chunk =
        \\diff --git a/src/guidance/sync.zig b/src/guidance/sync.zig
        \\index abc..def 100644
    ;
    const path = main.chunkFilePathPub(chunk);
    try std.testing.expectEqualStrings("src/guidance/sync.zig", path);
}

test "chunkFilePath: returns empty string for malformed chunk" {
    const path = main.chunkFilePathPub("not a diff header\n+added line\n");
    try std.testing.expectEqualStrings("", path);
}

test "splitDiffByFile: single file diff produces one chunk" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const diff =
        \\diff --git a/src/foo.zig b/src/foo.zig
        \\index 000..111 100644
        \\--- a/src/foo.zig
        \\+++ b/src/foo.zig
        \\@@ -1,3 +1,4 @@
        \\ fn foo() void {}
        \\+fn bar() void {}
    ;
    var chunks: std.ArrayList([]const u8) = .empty;
    defer chunks.deinit(allocator);
    try main.splitDiffByFilePub(diff, &chunks, allocator);

    try std.testing.expectEqual(@as(usize, 1), chunks.items.len);
    try std.testing.expectEqualStrings("src/foo.zig", main.chunkFilePathPub(chunks.items[0]));
}

test "splitDiffByFile: multi-file diff splits into correct chunks" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const diff =
        \\diff --git a/src/foo.zig b/src/foo.zig
        \\index 000..111 100644
        \\--- a/src/foo.zig
        \\+++ b/src/foo.zig
        \\@@ -1,2 +1,3 @@
        \\+added to foo
        \\diff --git a/src/bar.zig b/src/bar.zig
        \\index 222..333 100644
        \\--- a/src/bar.zig
        \\+++ b/src/bar.zig
        \\@@ -5,2 +5,3 @@
        \\+added to bar
    ;
    var chunks: std.ArrayList([]const u8) = .empty;
    defer chunks.deinit(allocator);
    try main.splitDiffByFilePub(diff, &chunks, allocator);

    try std.testing.expectEqual(@as(usize, 2), chunks.items.len);
    try std.testing.expectEqualStrings("src/foo.zig", main.chunkFilePathPub(chunks.items[0]));
    try std.testing.expectEqualStrings("src/bar.zig", main.chunkFilePathPub(chunks.items[1]));
}

test "splitDiffByFile: .guidance/ chunks split correctly and are identifiable" {
    // The filter (chunkIsIgnored) runs after splitting, so we verify that
    // guidance chunks split cleanly and are correctly tagged as ignored.
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();
    const guidance_dir = ".guidance";

    const diff =
        \\diff --git a/src/main.zig b/src/main.zig
        \\index 000..111 100644
        \\@@ -1,2 +1,3 @@
        \\+fn new() void {}
        \\diff --git a/.guidance/src/main.zig.json b/.guidance/src/main.zig.json
        \\index 222..333 100644
        \\@@ -1,3 +1,4 @@
        \\+  "comment": "updated"
    ;
    var chunks: std.ArrayList([]const u8) = .empty;
    defer chunks.deinit(allocator);
    try main.splitDiffByFilePub(diff, &chunks, allocator);

    try std.testing.expectEqual(@as(usize, 2), chunks.items.len);
    try std.testing.expect(!main.chunkIsIgnoredPub(chunks.items[0], guidance_dir)); // src/main.zig — keep
    try std.testing.expect(main.chunkIsIgnoredPub(chunks.items[1], guidance_dir)); // .guidance/ — ignore
}

// ---------------------------------------------------------------------------
// M9: Capability lifecycle detection tests
// ---------------------------------------------------------------------------

test "reportCapabilityLifecycle: all new when previous index missing" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    const current = [_]main.CapabilityEntryPub{
        .{
            .name = "foo",
            .description = "Foo capability",
            .anchors = &.{"FooStruct"},
            .keywords = &.{"foo"},
            .source = "foo/CAPABILITY.md",
        },
        .{
            .name = "bar",
            .description = "Bar capability",
            .anchors = &.{"BarFn"},
            .keywords = &.{"bar"},
            .source = "bar/CAPABILITY.md",
        },
    };

    // Use a path that doesn't exist — all caps are NEW
    const result = try main.reportCapabilityLifecyclePub(
        allocator,
        "/nonexistent/path/capability-index.json",
        &current,
        false,
    );
    try std.testing.expectEqual(@as(usize, 2), result.new_count);
    try std.testing.expectEqual(@as(usize, 0), result.updated_count);
    try std.testing.expectEqual(@as(usize, 0), result.removed_count);
    try std.testing.expectEqual(@as(usize, 0), result.unchanged_count);
}

test "reportCapabilityLifecycle: removed caps detected from previous index" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    // Write a fake previous index to a temp file
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const prev_json =
        \\{"version":1,"capabilities":[{"name":"old-cap","anchors":["OldStruct"],"source":"old-cap/CAPABILITY.md"}]}
    ;
    const idx_file = try tmp.dir.createFile("capability-index.json", .{});
    defer idx_file.close();
    var wbuf: [512]u8 = undefined;
    var fw = idx_file.writer(&wbuf);
    try fw.interface.writeAll(prev_json);
    try fw.interface.flush();

    const idx_path = try tmp.dir.realpathAlloc(allocator, "capability-index.json");
    defer allocator.free(idx_path);

    // Current: "old-cap" gone, "new-cap" added
    const current = [_]main.CapabilityEntryPub{
        .{
            .name = "new-cap",
            .description = "New capability",
            .anchors = &.{"NewStruct"},
            .keywords = &.{"new"},
            .source = "new-cap/CAPABILITY.md",
        },
    };

    const result = try main.reportCapabilityLifecyclePub(allocator, idx_path, &current, false);
    try std.testing.expectEqual(@as(usize, 1), result.new_count); // new-cap is new
    try std.testing.expectEqual(@as(usize, 1), result.removed_count); // old-cap removed
    try std.testing.expectEqual(@as(usize, 0), result.unchanged_count);
}

test "reportCapabilityLifecycle: updated cap detected when anchors change" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const prev_json =
        \\{"version":1,"capabilities":[{"name":"my-cap","anchors":["OldAnchor"],"source":"my-cap/CAPABILITY.md"}]}
    ;
    const idx_file = try tmp.dir.createFile("capability-index.json", .{});
    defer idx_file.close();
    var wbuf: [512]u8 = undefined;
    var fw = idx_file.writer(&wbuf);
    try fw.interface.writeAll(prev_json);
    try fw.interface.flush();

    const idx_path = try tmp.dir.realpathAlloc(allocator, "capability-index.json");
    defer allocator.free(idx_path);

    // Same cap name but different anchors → UPDATED
    const current = [_]main.CapabilityEntryPub{
        .{
            .name = "my-cap",
            .description = "My capability",
            .anchors = &.{"NewAnchor"}, // changed
            .keywords = &.{"cap"},
            .source = "my-cap/CAPABILITY.md",
        },
    };

    const result = try main.reportCapabilityLifecyclePub(allocator, idx_path, &current, false);
    try std.testing.expectEqual(@as(usize, 0), result.new_count);
    try std.testing.expectEqual(@as(usize, 1), result.updated_count);
    try std.testing.expectEqual(@as(usize, 0), result.removed_count);
}

// ---------------------------------------------------------------------------
// M6: extractMemberCommentsFromSource tests
// ---------------------------------------------------------------------------

test "extractMemberCommentsFromSource: extracts comment from source" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var store = json_store.JsonStore.init(allocator);

    const source =
        \\/// This is a doc comment.
        \\/// It has multiple lines.
        \\pub fn myFunc() void {}
        \\
        \\pub fn otherFunc() void {}
    ;

    var doc = types.GuidanceDoc{
        .meta = .{
            .module = "test",
            .source = "test.zig",
        },
        .members = &.{
            .{
                .type = .fn_decl,
                .name = "myFunc",
                .line = 3, // myFunc is on line 3 (after the two /// comments)
                .comment = null, // Should be extracted from source
            },
            .{
                .type = .fn_decl,
                .name = "otherFunc",
                .line = 5, // otherFunc is on line 5
                .comment = null, // No comment before this function
            },
        },
    };

    store.extractMemberCommentsFromSource(&doc, source);

    // myFunc should have its comment extracted
    try std.testing.expect(doc.members[0].comment != null);
    try std.testing.expect(std.mem.indexOf(u8, doc.members[0].comment.?, "This is a doc comment") != null);
    try std.testing.expect(std.mem.indexOf(u8, doc.members[0].comment.?, "multiple lines") != null);

    // otherFunc has no doc comment before it
    try std.testing.expect(doc.members[1].comment == null);

    // Cleanup
    if (doc.members[0].comment) |c| allocator.free(c);
}

test "extractMemberCommentsFromSource: preserves existing comments" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var store = json_store.JsonStore.init(allocator);

    const source =
        \\/// Source comment (should be ignored).
        \\pub fn myFunc() void {}
    ;

    var doc = types.GuidanceDoc{
        .meta = .{
            .module = "test",
            .source = "test.zig",
        },
        .members = &.{
            .{
                .type = .fn_decl,
                .name = "myFunc",
                .line = 2,
                .comment = "Existing comment from JSON", // Should be preserved
            },
        },
    };

    store.extractMemberCommentsFromSource(&doc, source);

    // Existing comment should NOT be overwritten
    try std.testing.expect(doc.members[0].comment != null);
    try std.testing.expectEqualStrings("Existing comment from JSON", doc.members[0].comment.?);
}

test "extractMemberCommentsFromSource: handles nested members" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var store = json_store.JsonStore.init(allocator);

    const source =
        \\pub const MyStruct = struct {
        \\    /// Method comment.
        \\    pub fn method() void {}
        \\};
    ;

    var doc = types.GuidanceDoc{
        .meta = .{
            .module = "test",
            .source = "test.zig",
        },
        .members = &.{
            .{
                .type = .@"struct",
                .name = "MyStruct",
                .line = 1,
                .comment = null,
                .members = &.{
                    .{
                        .type = .method,
                        .name = "method",
                        .line = 3,
                        .comment = null, // Should extract "Method comment."
                    },
                },
            },
        },
    };

    store.extractMemberCommentsFromSource(&doc, source);

    // Nested method comment should be extracted
    try std.testing.expect(doc.members[0].members[0].comment != null);
    try std.testing.expectEqualStrings("Method comment.", doc.members[0].members[0].comment.?);

    // Cleanup
    if (doc.members[0].members[0].comment) |c| allocator.free(c);
}
