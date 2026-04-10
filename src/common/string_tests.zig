//! Tests for string.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const string_mod = @import("string.zig");

test "containsIgnoreCase" {
    try std.testing.expect(string_mod.containsIgnoreCase("Hello World", "hello"));
    try std.testing.expect(string_mod.containsIgnoreCase("Hello World", "WORLD"));
    try std.testing.expect(string_mod.containsIgnoreCase("Hello World", "lo wo"));
    try std.testing.expect(!string_mod.containsIgnoreCase("Hello", "goodbye"));
    try std.testing.expect(string_mod.containsIgnoreCase("", ""));
    try std.testing.expect(!string_mod.containsIgnoreCase("a", "ab"));
}
test "containsWord" {
    try std.testing.expect(string_mod.containsWord("Ring Buffer implementation", "ring"));
    try std.testing.expect(string_mod.containsWord("Ring Buffer implementation", "buffer"));
    try std.testing.expect(!string_mod.containsWord("dupeStrings", "ring"));
    try std.testing.expect(!string_mod.containsWord("RingBuffer", "ring"));
    try std.testing.expect(string_mod.containsWord("configure the ring", "ring"));
}
test "containsAny" {
    const keywords = [_][]const u8{ "delete", "remove", "breaking" };
    try std.testing.expect(string_mod.containsAny("We should delete this file", &keywords));
    try std.testing.expect(string_mod.containsAny("Please REMOVE the entry", &keywords));
    try std.testing.expect(!string_mod.containsAny("Add a new feature", &keywords));
}
test "containsAnyWord" {
    const keywords = [_][]const u8{ "ring", "fifo", "deque" };
    try std.testing.expect(string_mod.containsAnyWord("Implementation of Ring buffer", &keywords));
    try std.testing.expect(!string_mod.containsAnyWord("dupeStrings function", &keywords));
}
test "hasExtension" {
    const exts = [_][]const u8{ "zig", "py", "md" };
    try std.testing.expect(string_mod.hasExtension("main.zig", &exts));
    try std.testing.expect(string_mod.hasExtension("README.md", &exts));
    try std.testing.expect(!string_mod.hasExtension("Makefile", &exts));
    try std.testing.expect(!string_mod.hasExtension("main.c", &exts));
}
test "isPathToken" {
    const exts = [_][]const u8{ "zig", "py" };
    try std.testing.expect(string_mod.isPathToken("src/main.zig", &exts));
    try std.testing.expect(string_mod.isPathToken("bin/script.py", &exts));
    try std.testing.expect(!string_mod.isPathToken("ab", &exts));
    try std.testing.expect(string_mod.isPathToken("path/to/file", &exts));
}
test "looksLikeIdentifier snake_case" {
    try std.testing.expect(string_mod.looksLikeIdentifier("snake_case"));
    try std.testing.expect(string_mod.looksLikeIdentifier("MY_CONST"));
}
test "looksLikeIdentifier camelCase and PascalCase" {
    try std.testing.expect(string_mod.looksLikeIdentifier("camelCase"));
    try std.testing.expect(string_mod.looksLikeIdentifier("PascalCase"));
    try std.testing.expect(string_mod.looksLikeIdentifier("myFunction"));
}
test "containsWord whole-word match" {
    try std.testing.expect(string_mod.containsWord("Ring Buffer implementation", "ring"));
    try std.testing.expect(string_mod.containsWord("Ring Buffer implementation", "buffer"));
    try std.testing.expect(!string_mod.containsWord("dupeStrings", "ring"));
    try std.testing.expect(!string_mod.containsWord("RingBuffer", "ring")); // no boundary
    try std.testing.expect(string_mod.containsWord("configure the ring", "ring"));
}
test "containsAny matches any keyword" {
    const kws = [_][]const u8{ "delete", "remove", "breaking" };
    try std.testing.expect(string_mod.containsAny("We should delete this file", &kws));
    try std.testing.expect(string_mod.containsAny("Please REMOVE the entry", &kws));
    try std.testing.expect(!string_mod.containsAny("Add a new feature", &kws));
}
test "containsAnyWord whole-word multi-keyword" {
    const kws = [_][]const u8{ "ring", "fifo", "deque" };
    try std.testing.expect(string_mod.containsAnyWord("Implementation of Ring buffer", &kws));
    try std.testing.expect(!string_mod.containsAnyWord("dupeStrings function", &kws));
}
test "hasExtension matches without dot" {
    const exts = [_][]const u8{ "zig", "py", "md" };
    try std.testing.expect(string_mod.hasExtension("main.zig", &exts));
    try std.testing.expect(string_mod.hasExtension("README.md", &exts));
    try std.testing.expect(!string_mod.hasExtension("Makefile", &exts));
    try std.testing.expect(!string_mod.hasExtension("main.c", &exts));
}
test "isPathToken detects paths and extensions" {
    const exts = [_][]const u8{ "zig", "py" };
    try std.testing.expect(string_mod.isPathToken("src/main.zig", &exts));
    try std.testing.expect(string_mod.isPathToken("bin/script.py", &exts));
    try std.testing.expect(!string_mod.isPathToken("ab", &exts)); // too short
    try std.testing.expect(string_mod.isPathToken("path/to/file", &exts)); // has slash
}
test "containsIgnoreCase finds substring" {
    try std.testing.expect(string_mod.containsIgnoreCase("Hello World", "hello"));
    try std.testing.expect(string_mod.containsIgnoreCase("UPPER", "upper"));
    try std.testing.expect(!string_mod.containsIgnoreCase("abc", "xyz"));
    try std.testing.expect(string_mod.containsIgnoreCase("abc", ""));
}
test "langFromPath maps extensions" {
    try std.testing.expectEqualStrings("zig", string_mod.langFromPath("src/foo.zig"));
    try std.testing.expectEqualStrings("python", string_mod.langFromPath("foo.py"));
    try std.testing.expectEqualStrings("rust", string_mod.langFromPath("lib.rs"));
    try std.testing.expectEqualStrings("typescript", string_mod.langFromPath("app.tsx"));
    try std.testing.expectEqualStrings("text", string_mod.langFromPath("README.md"));
}
test "dupeStrings produces independent copies" {
    const orig = [_][]const u8{ "hello", "world", "zig" };
    const copy = try string_mod.dupeStrings(std.testing.allocator, &orig);
    defer {
        for (copy) |s| std.testing.allocator.free(s);
        std.testing.allocator.free(copy);
    }
    try std.testing.expectEqual(@as(usize, 3), copy.len);
    try std.testing.expectEqualStrings("hello", copy[0]);
    try std.testing.expectEqualStrings("world", copy[1]);
    try std.testing.expectEqualStrings("zig", copy[2]);
    // Verify independence: pointers must differ from the originals.
    try std.testing.expect(copy[0].ptr != orig[0].ptr);
}
test "dupeStrings handles empty slice" {
    const copy = try string_mod.dupeStrings(std.testing.allocator, &[_][]const u8{});
    defer std.testing.allocator.free(copy);
    try std.testing.expectEqual(@as(usize, 0), copy.len);
}
test "looksLikeIdentifier rejects plain words" {
    try std.testing.expect(!string_mod.looksLikeIdentifier("search"));
    try std.testing.expect(!string_mod.looksLikeIdentifier("work"));
    try std.testing.expect(!string_mod.looksLikeIdentifier("how"));
    try std.testing.expect(!string_mod.looksLikeIdentifier("a")); // too short
}
test "stripNlPrefix removes interrogative prefix" {
    try std.testing.expectEqualStrings("vectorSearch work?", string_mod.stripNlPrefix("how does vectorSearch work?"));
    try std.testing.expectEqualStrings("the threshold?", string_mod.stripNlPrefix("what is the threshold?"));
    try std.testing.expectEqualStrings("cosine similarity", string_mod.stripNlPrefix("explain cosine similarity"));
    try std.testing.expectEqualStrings("BFS traversal", string_mod.stripNlPrefix("where is BFS traversal"));
    try std.testing.expectEqualStrings("the config", string_mod.stripNlPrefix("find the config"));
}
test "stripNlPrefix leaves identifiers unchanged" {
    try std.testing.expectEqualStrings("looksLikeIdentifier", string_mod.stripNlPrefix("looksLikeIdentifier"));
    try std.testing.expectEqualStrings("vectorSearch", string_mod.stripNlPrefix("vectorSearch"));
    try std.testing.expectEqualStrings("cosine similarity score", string_mod.stripNlPrefix("cosine similarity score"));
}
test "stripNlPrefix is case-insensitive on prefix" {
    try std.testing.expectEqualStrings("SQLite search", string_mod.stripNlPrefix("How does SQLite search"));
    try std.testing.expectEqualStrings("the config", string_mod.stripNlPrefix("WHERE IS the config"));
}
test "stripBoilerplate removes leading phrase" {
    try std.testing.expectEqualStrings("node if it exists", string_mod.stripBoilerplate("Returns the node if it exists"));
    try std.testing.expectEqualStrings("X with fixed-size buffers", string_mod.stripBoilerplate("Manages X with fixed-size buffers"));
    try std.testing.expectEqualStrings("the schema init", string_mod.stripBoilerplate("Initializes the schema init"));
    try std.testing.expectEqualStrings("cosine similarity · top-k", string_mod.stripBoilerplate("cosine similarity · top-k"));
}
test "stripBoilerplate trims leading whitespace" {
    try std.testing.expectEqualStrings("entry", string_mod.stripBoilerplate("  Creates a entry"));
}
test "isNoisyComment: short comment is noisy" {
    try std.testing.expect(string_mod.isNoisyComment("x"));
    try std.testing.expect(string_mod.isNoisyComment("ab"));
}
test "isNoisyComment: hex dump is noisy" {
    try std.testing.expect(string_mod.isNoisyComment("0xDEADBEEF, 0xCAFEBABE, 0x12345678, 0x87654321"));
}
test "isNoisyComment: prose is not noisy" {
    try std.testing.expect(!string_mod.isNoisyComment("Cosine similarity search over stored embeddings"));
    try std.testing.expect(!string_mod.isNoisyComment("BFS graph traversal for context packing"));
}
test "STOP_WORDS contains expected words" {
    try std.testing.expect(string_mod.STOP_WORDS.has("the"));
    try std.testing.expect(string_mod.STOP_WORDS.has("is"));
    try std.testing.expect(string_mod.STOP_WORDS.has("does"));
    try std.testing.expect(!string_mod.STOP_WORDS.has("cosine"));
    try std.testing.expect(!string_mod.STOP_WORDS.has("search"));
}
test "dupeString produces independent copy" {
    const original = "hello world";
    const copy = try string_mod.dupeString(std.testing.allocator, original);
    defer std.testing.allocator.free(copy);
    try std.testing.expectEqualStrings(original, copy);
    try std.testing.expect(copy.ptr != original.ptr);
}
test "dupeString returns null for empty string" {
    const result = try string_mod.dupeString(std.testing.allocator, "");
    try std.testing.expect(result == null);
}
test "dupeStringOpt returns null for null input" {
    const result = try string_mod.dupeStringOpt(std.testing.allocator, null);
    try std.testing.expect(result == null);
}
test "dupeStringOpt returns null for empty string" {
    const result = try string_mod.dupeStringOpt(std.testing.allocator, "");
    try std.testing.expect(result == null);
}
test "dupeStringOpt returns copy for non-empty string" {
    const original = "test content";
    const result = try string_mod.dupeStringOpt(std.testing.allocator, original);
    defer if (result) |s| std.testing.allocator.free(s);
    try std.testing.expect(result != null);
    try std.testing.expectEqualStrings(original, result.?);
    try std.testing.expect(result.?.ptr != original.ptr);
}
test "slugify converts to lowercase and replaces spaces" {
    const result = try string_mod.slugify(std.testing.allocator, "Hello World Test");
    defer std.testing.allocator.free(result);
    try std.testing.expectEqualStrings("hello-world-test", result);
}
test "slugify caps at 40 chars" {
    const long_input = "This is a very long description that exceeds forty characters";
    const result = try string_mod.slugify(std.testing.allocator, long_input);
    defer std.testing.allocator.free(result);
    try std.testing.expect(@as(usize, 40) >= result.len);
}
test "slugify handles empty string" {
    const result = try string_mod.slugify(std.testing.allocator, "");
    defer std.testing.allocator.free(result);
    try std.testing.expectEqualStrings("work-item", result);
}
test "truncateAtSentence returns input if within limit" {
    const result = try string_mod.truncateAtSentence(std.testing.allocator, "Short text.", 100);
    defer std.testing.allocator.free(result);
    try std.testing.expectEqualStrings("Short text.", result);
}
test "truncateAtSentence truncates at sentence boundary" {
    const result = try string_mod.truncateAtSentence(std.testing.allocator, "First sentence. Second sentence here.", 20);
    defer std.testing.allocator.free(result);
    try std.testing.expectEqualStrings("First sentence.", result);
}
test "truncateAtSentence truncates at word boundary when no sentence" {
    const result = try string_mod.truncateAtSentence(std.testing.allocator, "No punctuation here but we need to truncate", 20);
    defer std.testing.allocator.free(result);
    try std.testing.expectEqualStrings("No punctuation here", result);
}
test "firstCommentLine strips /// prefix" {
    try std.testing.expectEqualStrings("Function description", string_mod.firstCommentLine("/// Function description"));
    try std.testing.expectEqualStrings("Brief", string_mod.firstCommentLine("/// Brief"));
}
test "firstCommentLine strips //! prefix" {
    try std.testing.expectEqualStrings("Module description", string_mod.firstCommentLine("//! Module description"));
}
test "firstCommentLine handles leading whitespace" {
    try std.testing.expectEqualStrings("Text", string_mod.firstCommentLine("  /// Text"));
}
test "firstCommentLine stops at newline" {
    try std.testing.expectEqualStrings("First line", string_mod.firstCommentLine("/// First line\n/// Second line"));
}
test "firstCommentLine handles empty string" {
    try std.testing.expectEqualStrings("", string_mod.firstCommentLine(""));
}
test "firstCommentLine handles plain text (no prefix)" {
    try std.testing.expectEqualStrings("Just text", string_mod.firstCommentLine("Just text"));
}
