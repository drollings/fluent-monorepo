//! Tests for enhancer.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const enhancer_mod = @import("enhancer.zig");

test "scoreDocstring empty" {
    try std.testing.expectEqual(@as(u32, 0), enhancer_mod.Enhancer.scoreDocstring(""));
}
test "scoreDocstring quality" {
    const good = "Parses JSON from a slice.\n\nArgs:\n  data: input slice\nReturns: parsed value\nRaises: error on malformed input";
    const score = enhancer_mod.Enhancer.scoreDocstring(good);
    try std.testing.expect(score >= 4);
}
test "extractTags parses hashtags" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    var e = try enhancer_mod.Enhancer.init(gpa.allocator(), .{
        .api_url = "http://localhost:11434/v1/chat/completions",
        .model = "test",
    });
    defer e.deinit();
    const alloc = gpa.allocator();

    const text = "Parses a JSON slice and returns a Value.\nTags: #json #parser #zig";
    const tags = try e.extractTags(text);
    defer {
        for (tags) |t| alloc.free(t);
        alloc.free(tags);
    }
    try std.testing.expectEqual(@as(usize, 3), tags.len);
    try std.testing.expectEqualStrings("json", tags[0]);
    try std.testing.expectEqualStrings("parser", tags[1]);
    try std.testing.expectEqualStrings("zig", tags[2]);
}
test "extractTags returns empty when no Tags line" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    var e = try enhancer_mod.Enhancer.init(gpa.allocator(), .{
        .api_url = "http://localhost:11434/v1/chat/completions",
        .model = "test",
    });
    defer e.deinit();
    const alloc = gpa.allocator();

    const tags = try e.extractTags("Just a plain description with no tags.");
    defer {
        for (tags) |t| alloc.free(t);
        alloc.free(tags);
    }
    try std.testing.expectEqual(@as(usize, 0), tags.len);
}
test "stripTagsLine removes Tags line" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    var e = try enhancer_mod.Enhancer.init(gpa.allocator(), .{
        .api_url = "http://localhost:11434/v1/chat/completions",
        .model = "test",
    });
    defer e.deinit();
    const alloc = gpa.allocator();

    const text = "Parses JSON from a byte slice.\nTags: #json #zig";
    const stripped = try e.stripTagsLine(text);
    defer if (stripped) |s| alloc.free(s);
    try std.testing.expectEqualStrings("Parses JSON from a byte slice.", stripped.?);
}
test "fallbackPhrases extracts identifiers" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    var e = try enhancer_mod.Enhancer.init(gpa.allocator(), .{
        .api_url = "http://localhost:11434/v1/chat/completions",
        .model = "test",
    });
    defer e.deinit();

    const comment = "Manages database connection pool using SQLiteConnection struct.";
    const result = try e.fallbackPhrases(comment);
    defer result.deinit(gpa.allocator());

    // Should extract SQLiteConnection (CamelCase identifier)
    var found_sqlite = false;
    for (result.phrases) |p| {
        if (std.mem.indexOf(u8, p, "SQLite") != null) found_sqlite = true;
    }
    try std.testing.expect(found_sqlite);
}
test "parsePhrasesResponse handles comma-separated" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    var e = try enhancer_mod.Enhancer.init(gpa.allocator(), .{
        .api_url = "http://localhost:11434/v1/chat/completions",
        .model = "test",
    });
    defer e.deinit();

    const tagged = "cosine similarity, vector search, embedding";
    const result = try e.parsePhrasesResponse(tagged);
    defer result.deinit(gpa.allocator());

    try std.testing.expectEqual(@as(usize, 3), result.phrases.len);
    try std.testing.expectEqualStrings("cosine similarity", result.phrases[0]);
    try std.testing.expectEqualStrings("vector search", result.phrases[1]);
    try std.testing.expectEqualStrings("embedding", result.phrases[2]);
}
test "parsePhrasesResponse handles newline-separated" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    var e = try enhancer_mod.Enhancer.init(gpa.allocator(), .{
        .api_url = "http://localhost:11434/v1/chat/completions",
        .model = "test",
    });
    defer e.deinit();

    const tagged = "cosine similarity\nvector search\nembedding";
    const result = try e.parsePhrasesResponse(tagged);
    defer result.deinit(gpa.allocator());

    try std.testing.expectEqual(@as(usize, 3), result.phrases.len);
}
test "parsePhrasesResponse limits to 5 phrases" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    var e = try enhancer_mod.Enhancer.init(gpa.allocator(), .{
        .api_url = "http://localhost:11434/v1/chat/completions",
        .model = "test",
    });
    defer e.deinit();

    const tagged = "one, two, three, four, five, six, seven";
    const result = try e.parsePhrasesResponse(tagged);
    defer result.deinit(gpa.allocator());

    try std.testing.expectEqual(@as(usize, 5), result.phrases.len);
}
test "parsePhrasesResponse skips generic words" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    var e = try enhancer_mod.Enhancer.init(gpa.allocator(), .{
        .api_url = "http://localhost:11434/v1/chat/completions",
        .model = "test",
    });
    defer e.deinit();

    const tagged = "function, module, cosine similarity";
    const result = try e.parsePhrasesResponse(tagged);
    defer result.deinit(gpa.allocator());

    // Only "cosine similarity" should survive
    try std.testing.expectEqual(@as(usize, 1), result.phrases.len);
    try std.testing.expectEqualStrings("cosine similarity", result.phrases[0]);
}
