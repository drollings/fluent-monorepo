//! Tests for embeddings.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const embeddings_mod = @import("embeddings.zig");

test "NoopEmbedding returns empty vector" {
    var noop: embeddings_mod.NoopEmbedding = .{};
    const p = noop.provider();
    try std.testing.expectEqualStrings("none", p.getName());
    try std.testing.expectEqual(@as(u32, 0), p.getDimensions());
    const vec = try p.embed(std.testing.allocator, "hello");
    defer std.testing.allocator.free(vec);
    try std.testing.expectEqual(@as(usize, 0), vec.len);
    // stack-allocated NoopEmbedding: deinit is a no-op
}
test "OllamaEmbedding init and deinit" {
    var impl_ = try embeddings_mod.OllamaEmbedding.init(std.testing.allocator, null, null, null);
    const p = impl_.provider();
    try std.testing.expectEqualStrings("ollama", p.getName());
    try std.testing.expectEqual(@as(u32, 768), p.getDimensions());
    p.deinit();
}
test "OllamaEmbedding rejects insecure remote http" {
    const result = embeddings_mod.OllamaEmbedding.init(std.testing.allocator, null, "http://gpu-server:11434", null);
    try std.testing.expectError(error.InsecureApiUrl, result);
}
test "parseOllamaResponse valid" {
    const json = "{\"embeddings\":[[0.1,0.2,0.3]]}";
    const result = try embeddings_mod.parseOllamaResponse(std.testing.allocator, json);
    defer std.testing.allocator.free(result);
    try std.testing.expectEqual(@as(usize, 3), result.len);
    try std.testing.expect(@abs(result[0] - 0.1) < 0.001);
}
test "parseOpenAiResponse valid" {
    const json = "{\"data\":[{\"embedding\":[0.1,0.2,0.3]}]}";
    const result = try embeddings_mod.parseOpenAiResponse(std.testing.allocator, json);
    defer std.testing.allocator.free(result);
    try std.testing.expectEqual(@as(usize, 3), result.len);
}
test "createEmbeddingProvider noop" {
    const p = try embeddings_mod.createEmbeddingProvider(std.testing.allocator, "none", null, "", 0);
    defer p.deinit();
    try std.testing.expectEqualStrings("none", p.getName());
}
test "contentHashWithModel is deterministic and model-sensitive" {
    const h1 = embeddings_mod.contentHashWithModel("text", "model-a");
    const h2 = embeddings_mod.contentHashWithModel("text", "model-a");
    const h3 = embeddings_mod.contentHashWithModel("text", "model-b");
    try std.testing.expectEqualSlices(u8, &h1, &h2);
    try std.testing.expect(!std.mem.eql(u8, &h1, &h3));
}
test "EmbeddingProvider: same-thread embed succeeds (thread_id assertion not triggered)" {
    var noop: embeddings_mod.NoopEmbedding = .{};
    const p = noop.provider();
    // Calling from the same thread that created the handle — assertion passes.
    const vec = try p.embed(std.testing.allocator, "hello");
    defer std.testing.allocator.free(vec);
    try std.testing.expectEqual(@as(usize, 0), vec.len);
}
test "EmbeddingProvider: thread_safe=true disables thread assertion" {
    var noop: embeddings_mod.NoopEmbedding = .{};
    var p = noop.provider();
    p.thread_safe = true;
    // With thread_safe set, embed() skips the thread_id assertion.
    // We can't actually simulate a cross-thread call in a unit test without
    // spawning threads (which risks flakiness), so we just verify the flag works.
    const vec = try p.embed(std.testing.allocator, "safe");
    defer std.testing.allocator.free(vec);
    try std.testing.expectEqual(@as(usize, 0), vec.len);
}
