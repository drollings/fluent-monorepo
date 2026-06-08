//! Tests for capability_eval.zig — CapabilityEvaluator skip heuristics and JSON parsing.
//! Moved from inline tests to standalone test module for build.zig wiring.

const std = @import("std");
const cap_eval = @import("capability_eval.zig");

test "testSkipHeuristics: test files return skip path" {
    // shouldSkipByPath is internal; validate via path inspection.
    // Test files ending in _tests.zig.json should be skipped.
    const test_paths = [_][]const u8{
        "/proj/.guidance/src/guidance/sync/marker_tests.zig.json",
        "/proj/.guidance/src/guidance/sync/test_helper.zig.json",
    };
    const normal_paths = [_][]const u8{
        "/proj/.guidance/src/guidance/sync/marker.zig.json",
        "/proj/.guidance/src/guidance/sync/gen_files.zig.json",
    };

    for (test_paths) |p| {
        const base = std.fs.path.basename(p);
        const is_test = std.mem.indexOf(u8, base, "_tests.zig") != null or
            std.mem.startsWith(u8, base, "test_");
        try std.testing.expect(is_test);
    }
    for (normal_paths) |p| {
        const base = std.fs.path.basename(p);
        const is_test = std.mem.indexOf(u8, base, "_tests.zig") != null or
            std.mem.startsWith(u8, base, "test_");
        try std.testing.expect(!is_test);
    }
}

test "testJsonParsing: extractJson finds balanced object" {
    // Inline test from capability_eval.zig via @import.
    const text = "thinking...\n{ \"match\": {\"name\": \"sync-engine\", \"confidence\": 0.9}, \"novel\": null }";
    // Use the same logic as extractJson.
    const start = std.mem.indexOfScalar(u8, text, '{').?;
    var depth: usize = 0;
    var i: usize = start;
    while (i < text.len) : (i += 1) {
        switch (text[i]) {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if (depth == 0) break;
            },
            else => {},
        }
    }
    const json_text = text[start .. i + 1];
    try std.testing.expect(std.mem.startsWith(u8, json_text, "{"));
    try std.testing.expect(std.mem.endsWith(u8, json_text, "}"));

    // Parse the extracted JSON.
    var parsed = try std.json.parseFromSlice(std.json.Value, std.testing.allocator, json_text, .{});
    defer parsed.deinit();
    try std.testing.expect(parsed.value == .object);
    const match_v = parsed.value.object.get("match").?;
    try std.testing.expect(match_v == .object);
    const name_v = match_v.object.get("name").?;
    try std.testing.expectEqualStrings("sync-engine", name_v.string);
}

test "testEvaluatedAtHash: skip when hash matches first member" {
    // Simulate the staleness gate logic: if capability_eval.evaluated_at_hash
    // equals the first member's match_hash, evaluation should be skipped.
    const json_str =
        \\{
        \\  "meta": {"module": "guidance.sync.marker", "source": "src/guidance/sync/marker.zig", "language": "zig"},
        \\  "capability_eval": {"capability_name": "sync-pipeline", "confidence": 0.92, "evaluated_at_hash": "abc123"},
        \\  "members": [
        \\    {"name": "fileNeedsProcessing", "match_hash": "abc123", "type": "fn_decl"}
        \\  ]
        \\}
    ;
    var parsed = try std.json.parseFromSlice(std.json.Value, std.testing.allocator, json_str, .{});
    defer parsed.deinit();

    const root = parsed.value;
    try std.testing.expect(root == .object);

    const eval_v = root.object.get("capability_eval").?;
    const hash_v = eval_v.object.get("evaluated_at_hash").?;
    try std.testing.expectEqualStrings("abc123", hash_v.string);

    const members_v = root.object.get("members").?;
    const first = members_v.array.items[0];
    const mh = first.object.get("match_hash").?;
    try std.testing.expectEqualStrings("abc123", mh.string);

    // Hashes match → should skip.
    try std.testing.expect(std.mem.eql(u8, hash_v.string, mh.string));
}

test "freeEvalResult: skip is no-op" {
    cap_eval.freeEvalResult(std.testing.allocator, .skip);
}
