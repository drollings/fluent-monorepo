/// string.zig — Generic string classification and inspection helpers
///
/// Functions here operate purely on `[]const u8` slices and have no
/// dependency on guidance-domain types.  Suitable for reuse in any Zig
/// tool that needs lightweight text analysis.
const std = @import("std");

/// Checks if a needle substring exists within the haystack, ignoring case sensitivity.
pub fn containsIgnoreCase(haystack: []const u8, needle: []const u8) bool {
    if (needle.len == 0) return true;
    if (needle.len > haystack.len) return false;
    var i: usize = 0;
    while (i + needle.len <= haystack.len) : (i += 1) {
        if (std.ascii.eqlIgnoreCase(haystack[i .. i + needle.len], needle)) return true;
    }
    return false;
}

/// Checks if a needle substring exists within the haystack array of bytes.
pub fn containsWord(haystack: []const u8, needle: []const u8) bool {
    if (needle.len > haystack.len) return false;
    var i: usize = 0;
    while (i + needle.len <= haystack.len) : (i += 1) {
        if (!std.ascii.eqlIgnoreCase(haystack[i .. i + needle.len], needle)) continue;
        const left_ok = i == 0 or !std.ascii.isAlphanumeric(haystack[i - 1]);
        const right_end = i + needle.len;
        const right_ok = right_end >= haystack.len or !std.ascii.isAlphanumeric(haystack[right_end]);
        if (left_ok and right_ok) return true;
    }
    return false;
}

/// Checks if any keywords exist within the source string slice.
pub fn containsAny(source: []const u8, keywords: []const []const u8) bool {
    for (keywords) |kw| {
        if (containsIgnoreCase(source, kw)) return true;
    }
    return false;
}

/// Checks if any word from keywords appears in the given source string.
pub fn containsAnyWord(source: []const u8, keywords: []const []const u8) bool {
    for (keywords) |kw| {
        if (containsWord(source, kw)) return true;
    }
    return false;
}

/// Checks if the input string contains any of the specified extensions.
pub fn hasExtension(s: []const u8, extensions: []const []const u8) bool {
    const dot = std.mem.lastIndexOfScalar(u8, s, '.') orelse return false;
    const ext = s[dot + 1 ..];
    for (extensions) |known| {
        if (std.mem.eql(u8, ext, known)) return true;
    }
    return false;
}

/// Checks if a given slice of bytes matches a specified extension pattern.
pub fn isPathToken(s: []const u8, extensions: []const []const u8) bool {
    if (s.len < 3) return false;
    return hasExtension(s, extensions) or std.mem.indexOf(u8, s, "/") != null;
}

test "containsIgnoreCase" {
    try std.testing.expect(containsIgnoreCase("Hello World", "hello"));
    try std.testing.expect(containsIgnoreCase("Hello World", "WORLD"));
    try std.testing.expect(containsIgnoreCase("Hello World", "lo wo"));
    try std.testing.expect(!containsIgnoreCase("Hello", "goodbye"));
    try std.testing.expect(containsIgnoreCase("", ""));
    try std.testing.expect(!containsIgnoreCase("a", "ab"));
}

test "containsWord" {
    try std.testing.expect(containsWord("Ring Buffer implementation", "ring"));
    try std.testing.expect(containsWord("Ring Buffer implementation", "buffer"));
    try std.testing.expect(!containsWord("dupeStrings", "ring"));
    try std.testing.expect(!containsWord("RingBuffer", "ring"));
    try std.testing.expect(containsWord("configure the ring", "ring"));
}

test "containsAny" {
    const keywords = [_][]const u8{ "delete", "remove", "breaking" };
    try std.testing.expect(containsAny("We should delete this file", &keywords));
    try std.testing.expect(containsAny("Please REMOVE the entry", &keywords));
    try std.testing.expect(!containsAny("Add a new feature", &keywords));
}

test "containsAnyWord" {
    const keywords = [_][]const u8{ "ring", "fifo", "deque" };
    try std.testing.expect(containsAnyWord("Implementation of Ring buffer", &keywords));
    try std.testing.expect(!containsAnyWord("dupeStrings function", &keywords));
}

test "hasExtension" {
    const exts = [_][]const u8{ "zig", "py", "md" };
    try std.testing.expect(hasExtension("main.zig", &exts));
    try std.testing.expect(hasExtension("README.md", &exts));
    try std.testing.expect(!hasExtension("Makefile", &exts));
    try std.testing.expect(!hasExtension("main.c", &exts));
}

test "isPathToken" {
    const exts = [_][]const u8{ "zig", "py" };
    try std.testing.expect(isPathToken("src/main.zig", &exts));
    try std.testing.expect(isPathToken("bin/script.py", &exts));
    try std.testing.expect(!isPathToken("ab", &exts));
    try std.testing.expect(isPathToken("path/to/file", &exts));
}

/// Checks if a token matches a pattern resembling an identifier in Zig.
pub fn looksLikeIdentifier(token: []const u8) bool {
    if (token.len < 2) return false;

    // Check for underscore (snake_case / SCREAMING_SNAKE_CASE)
    for (token) |ch| {
        if (ch == '_') return true;
    }

    // Check for uppercase after first character (camelCase, PascalCase)
    var i: usize = 1;
    while (i < token.len) : (i += 1) {
        if (std.ascii.isUpper(token[i])) return true;
    }

    return false;
}

/// Checks if a given relative path matches expected test patterns, returning true or false.
pub fn isTestPath(rel_path: []const u8) bool {
    const basename = std.fs.path.basename(rel_path);
    // Strip extension to get the stem.
    const ext = std.fs.path.extension(basename);
    const stem = if (ext.len > 0) basename[0 .. basename.len - ext.len] else basename;
    if (std.mem.endsWith(u8, stem, "_test")) return true;
    if (std.mem.startsWith(u8, stem, "test_")) return true;
    if (std.mem.eql(u8, stem, "tests")) return true;
    if (std.mem.indexOf(u8, rel_path, "/test/") != null) return true;
    if (std.mem.indexOf(u8, rel_path, "/tests/") != null) return true;
    if (std.mem.indexOf(u8, rel_path, "\\test\\") != null) return true;
    if (std.mem.indexOf(u8, rel_path, "\\tests\\") != null) return true;
    return false;
}

/// Converts a reference to a null-terminated C string into a Zig array slice.
pub fn skillNameFromRef(ref: []const u8) []const u8 {
    const base = std.fs.path.basename(ref);
    if (std.mem.eql(u8, base, "SKILL.md")) {
        const dir = std.fs.path.dirname(ref) orelse return base;
        return std.fs.path.basename(dir);
    }
    return base;
}

// =============================================================================
// String search helpers — from coral/src/common/string.zig
// =============================================================================

/// Checks if a needle substring exists within the haystack array of bytes.
pub fn containsWord(haystack: []const u8, needle: []const u8) bool {
    if (needle.len > haystack.len) return false;
    var i: usize = 0;
    while (i + needle.len <= haystack.len) : (i += 1) {
        if (!std.ascii.eqlIgnoreCase(haystack[i .. i + needle.len], needle)) continue;
        const left_ok = i == 0 or !std.ascii.isAlphanumeric(haystack[i - 1]);
        const right_end = i + needle.len;
        const right_ok = right_end >= haystack.len or !std.ascii.isAlphanumeric(haystack[right_end]);
        if (left_ok and right_ok) return true;
    }
    return false;
}

/// Checks if any keywords exist within the source string slice.
pub fn containsAny(source: []const u8, keywords: []const []const u8) bool {
    for (keywords) |kw| {
        if (containsIgnoreCase(source, kw)) return true;
    }
    return false;
}

/// Checks if any word from keywords appears in the source string.
pub fn containsAnyWord(source: []const u8, keywords: []const []const u8) bool {
    for (keywords) |kw| {
        if (containsWord(source, kw)) return true;
    }
    return false;
}

/// Checks if the input slice contains a specified extension, returning true if found.
pub fn hasExtension(s: []const u8, extensions: []const []const u8) bool {
    const dot = std.mem.lastIndexOfScalar(u8, s, '.') orelse return false;
    const ext = s[dot + 1 ..];
    for (extensions) |known| {
        if (std.mem.eql(u8, ext, known)) return true;
    }
    return false;
}

/// Checks if a given slice of bytes matches a specified extension pattern, returning true or false.
pub fn isPathToken(s: []const u8, extensions: []const []const u8) bool {
    if (s.len < 3) return false;
    return hasExtension(s, extensions) or std.mem.indexOf(u8, s, "/") != null;
}

/// Checks if a needle substring exists within the haystack, ignoring case, and returns true or false.
pub fn containsIgnoreCase(haystack: []const u8, needle: []const u8) bool {
    if (needle.len == 0) return true;
    if (needle.len > haystack.len) return false;
    var i: usize = 0;
    while (i + needle.len <= haystack.len) : (i += 1) {
        if (std.ascii.eqlIgnoreCase(haystack[i .. i + needle.len], needle)) return true;
    }
    return false;
}

/// Converts a null-terminated C string slice into a Zig array of characters.
pub fn langFromPath(path: []const u8) []const u8 {
    if (std.mem.endsWith(u8, path, ".zig")) return "zig";
    if (std.mem.endsWith(u8, path, ".py")) return "python";
    if (std.mem.endsWith(u8, path, ".rs")) return "rust";
    if (std.mem.endsWith(u8, path, ".ts") or std.mem.endsWith(u8, path, ".tsx")) return "typescript";
    if (std.mem.endsWith(u8, path, ".js")) return "javascript";
    return "text";
}

/// Converts a slice of byte slices into a new slice of byte slices, duplicating each string.
pub fn dupeStrings(allocator: std.mem.Allocator, strs: []const []const u8) ![][]const u8 {
    const result = try allocator.alloc([]const u8, strs.len);
    var n: usize = 0;
    errdefer {
        for (result[0..n]) |s| allocator.free(s);
        allocator.free(result);
    }
    for (strs) |s| {
        result[n] = try allocator.dupe(u8, s);
        n += 1;
    }
    return result;
}

/// Converts a null-terminated string into a Zig-safe slice, handling memory allocation internally.
pub fn dupeString(allocator: std.mem.Allocator, s: []const u8) ![]const u8 {
    if (s.len == 0) return null;
    return try allocator.dupe(u8, s);
}

/// Converts a null-terminated string into a Zig-safe slice, handling memory allocation and validation.
pub fn dupeStringOpt(allocator: std.mem.Allocator, opt: ?[]const u8) !?[]const u8 {
    const s = opt orelse return null;
    if (s.len == 0) return null;
    return try allocator.dupe(u8, s);
}

// =============================================================================
// Embedding text preprocessing — strip boilerplate from doc comments
// =============================================================================

/// Common boilerplate prefixes in auto-generated or template doc comments.
/// Stripping these before embedding improves discriminative signal: the
/// remaining domain nouns and action verbs cluster more tightly in the
/// embedding space than generic management/ownership language.
const STRIP_PREFIXES = [_][]const u8{
    "Returns the ",     "Return the ",     "Returns a ",   "Returns an ",
    "Initializes the ", "Initialize the ", "Initialize ",  "Creates a ",
    "Create a ",        "Creates an ",     "Create an ",   "Checks if ",
    "Check if ",        "Verifies that ",  "Verify that ", "Ensures that ",
    "Ensure that ",     "Manages ",        "Represents ",  "Defines ",
    "Caller must ",     "Caller should ",  "The caller ",  "Not thread-safe",
    "Thread-safe",
};

/// Removes unnecessary comment bytes from a Zig source file.
pub fn stripBoilerplate(comment: []const u8) []const u8 {
    const trimmed = std.mem.trimLeft(u8, comment, " \t");
    for (STRIP_PREFIXES) |pfx| {
        if (std.mem.startsWith(u8, trimmed, pfx)) {
            return trimmed[pfx.len..];
        }
    }
    return trimmed;
}

/// Checks if a comment slice contains invalid or unexpected characters, returning true for noise.
pub fn isNoisyComment(comment: []const u8) bool {
    if (comment.len < 10) return true; // too short to be meaningful
    var non_ws: usize = 0;
    var noisy: usize = 0;
    for (comment) |ch| {
        if (!std.ascii.isWhitespace(ch)) {
            non_ws += 1;
            if (std.ascii.isDigit(ch) or (!std.ascii.isAlphanumeric(ch) and !std.ascii.isWhitespace(ch))) noisy += 1;
        }
    }
    return non_ws > 0 and (noisy * 100 / non_ws) > 50;
}

// =============================================================================
// NL query normalization — strip interrogative prefixes and stop words
// =============================================================================

/// Common English filler words that add no semantic signal when embedding.
/// Stored as a comptime perfect hash — zero runtime overhead.
pub const STOP_WORDS = std.StaticStringMap(void).initComptime(.{
    .{ "the", {} },  .{ "a", {} },    .{ "an", {} },
    .{ "in", {} },   .{ "on", {} },   .{ "at", {} },
    .{ "to", {} },   .{ "of", {} },   .{ "for", {} },
    .{ "with", {} }, .{ "by", {} },   .{ "is", {} },
    .{ "are", {} },  .{ "was", {} },  .{ "be", {} },
    .{ "do", {} },   .{ "does", {} }, .{ "did", {} },
    .{ "done", {} }, .{ "work", {} }, .{ "works", {} },
    .{ "get", {} },  .{ "use", {} },
});

/// Natural-language interrogative prefixes to strip before embedding a query.
/// Stripping these improves cosine similarity against descriptor-format entries.
const NL_PREFIXES = [_][]const u8{
    "what is ",         "what are ",  "what does ",  "what's ",
    "where is ",        "where are ", "where does ", "where can i find ",
    "how does ",        "how do ",    "how can i ",  "how to ",
    "why does ",        "why is ",    "why are ",    "when does ",
    "when is ",         "which ",     "who ",        "whose ",
    "can you explain ", "explain ",   "describe ",   "tell me about ",
    "show me ",         "find ",      "search for ", "look for ",
    "i need ",          "i want ",    "help me ",
};

/// Removes leading null characters from a UTF-8 string slice.
pub fn stripNlPrefix(query: []const u8) []const u8 {
    // Lowercase a buffer large enough for the longest prefix we check.
    const MAX_PREFIX_LEN = 30;
    var buf: [MAX_PREFIX_LEN]u8 = undefined;
    const check_len = @min(query.len, MAX_PREFIX_LEN);
    const lower = std.ascii.lowerString(buf[0..check_len], query[0..check_len]);
    for (NL_PREFIXES) |prefix| {
        if (prefix.len <= check_len and std.mem.startsWith(u8, lower, prefix)) {
            return query[prefix.len..];
        }
    }
    return query;
}

// =============================================================================
// Tests
// =============================================================================

test "looksLikeIdentifier snake_case" {
    try std.testing.expect(looksLikeIdentifier("snake_case"));
    try std.testing.expect(looksLikeIdentifier("MY_CONST"));
}

test "looksLikeIdentifier camelCase and PascalCase" {
    try std.testing.expect(looksLikeIdentifier("camelCase"));
    try std.testing.expect(looksLikeIdentifier("PascalCase"));
    try std.testing.expect(looksLikeIdentifier("myFunction"));
}

test "containsWord whole-word match" {
    try std.testing.expect(containsWord("Ring Buffer implementation", "ring"));
    try std.testing.expect(containsWord("Ring Buffer implementation", "buffer"));
    try std.testing.expect(!containsWord("dupeStrings", "ring"));
    try std.testing.expect(!containsWord("RingBuffer", "ring")); // no boundary
    try std.testing.expect(containsWord("configure the ring", "ring"));
}

test "containsAny matches any keyword" {
    const kws = [_][]const u8{ "delete", "remove", "breaking" };
    try std.testing.expect(containsAny("We should delete this file", &kws));
    try std.testing.expect(containsAny("Please REMOVE the entry", &kws));
    try std.testing.expect(!containsAny("Add a new feature", &kws));
}

test "containsAnyWord whole-word multi-keyword" {
    const kws = [_][]const u8{ "ring", "fifo", "deque" };
    try std.testing.expect(containsAnyWord("Implementation of Ring buffer", &kws));
    try std.testing.expect(!containsAnyWord("dupeStrings function", &kws));
}

test "hasExtension matches without dot" {
    const exts = [_][]const u8{ "zig", "py", "md" };
    try std.testing.expect(hasExtension("main.zig", &exts));
    try std.testing.expect(hasExtension("README.md", &exts));
    try std.testing.expect(!hasExtension("Makefile", &exts));
    try std.testing.expect(!hasExtension("main.c", &exts));
}

test "isPathToken detects paths and extensions" {
    const exts = [_][]const u8{ "zig", "py" };
    try std.testing.expect(isPathToken("src/main.zig", &exts));
    try std.testing.expect(isPathToken("bin/script.py", &exts));
    try std.testing.expect(!isPathToken("ab", &exts)); // too short
    try std.testing.expect(isPathToken("path/to/file", &exts)); // has slash
}

test "containsIgnoreCase finds substring" {
    try std.testing.expect(containsIgnoreCase("Hello World", "hello"));
    try std.testing.expect(containsIgnoreCase("UPPER", "upper"));
    try std.testing.expect(!containsIgnoreCase("abc", "xyz"));
    try std.testing.expect(containsIgnoreCase("abc", ""));
}

test "langFromPath maps extensions" {
    try std.testing.expectEqualStrings("zig", langFromPath("src/foo.zig"));
    try std.testing.expectEqualStrings("python", langFromPath("foo.py"));
    try std.testing.expectEqualStrings("rust", langFromPath("lib.rs"));
    try std.testing.expectEqualStrings("typescript", langFromPath("app.tsx"));
    try std.testing.expectEqualStrings("text", langFromPath("README.md"));
}

test "dupeStrings produces independent copies" {
    const orig = [_][]const u8{ "hello", "world", "zig" };
    const copy = try dupeStrings(std.testing.allocator, &orig);
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
    const copy = try dupeStrings(std.testing.allocator, &[_][]const u8{});
    defer std.testing.allocator.free(copy);
    try std.testing.expectEqual(@as(usize, 0), copy.len);
}

test "looksLikeIdentifier rejects plain words" {
    try std.testing.expect(!looksLikeIdentifier("search"));
    try std.testing.expect(!looksLikeIdentifier("work"));
    try std.testing.expect(!looksLikeIdentifier("how"));
    try std.testing.expect(!looksLikeIdentifier("a")); // too short
}

test "stripNlPrefix removes interrogative prefix" {
    try std.testing.expectEqualStrings("vectorSearch work?", stripNlPrefix("how does vectorSearch work?"));
    try std.testing.expectEqualStrings("the threshold?", stripNlPrefix("what is the threshold?"));
    try std.testing.expectEqualStrings("cosine similarity", stripNlPrefix("explain cosine similarity"));
    try std.testing.expectEqualStrings("BFS traversal", stripNlPrefix("where is BFS traversal"));
    try std.testing.expectEqualStrings("the config", stripNlPrefix("find the config"));
}

test "stripNlPrefix leaves identifiers unchanged" {
    try std.testing.expectEqualStrings("looksLikeIdentifier", stripNlPrefix("looksLikeIdentifier"));
    try std.testing.expectEqualStrings("vectorSearch", stripNlPrefix("vectorSearch"));
    try std.testing.expectEqualStrings("cosine similarity score", stripNlPrefix("cosine similarity score"));
}

test "stripNlPrefix is case-insensitive on prefix" {
    try std.testing.expectEqualStrings("SQLite search", stripNlPrefix("How does SQLite search"));
    try std.testing.expectEqualStrings("the config", stripNlPrefix("WHERE IS the config"));
}

test "stripBoilerplate removes leading phrase" {
    try std.testing.expectEqualStrings("node if it exists", stripBoilerplate("Returns the node if it exists"));
    try std.testing.expectEqualStrings("X with fixed-size buffers", stripBoilerplate("Manages X with fixed-size buffers"));
    try std.testing.expectEqualStrings("the schema init", stripBoilerplate("Initializes the schema init"));
    try std.testing.expectEqualStrings("cosine similarity · top-k", stripBoilerplate("cosine similarity · top-k"));
}

test "stripBoilerplate trims leading whitespace" {
    try std.testing.expectEqualStrings("entry", stripBoilerplate("  Creates a entry"));
}

test "isNoisyComment: short comment is noisy" {
    try std.testing.expect(isNoisyComment("x"));
    try std.testing.expect(isNoisyComment("ab"));
}

test "isNoisyComment: hex dump is noisy" {
    try std.testing.expect(isNoisyComment("0xDEADBEEF, 0xCAFEBABE, 0x12345678, 0x87654321"));
}

test "isNoisyComment: prose is not noisy" {
    try std.testing.expect(!isNoisyComment("Cosine similarity search over stored embeddings"));
    try std.testing.expect(!isNoisyComment("BFS graph traversal for context packing"));
}

test "STOP_WORDS contains expected words" {
    try std.testing.expect(STOP_WORDS.has("the"));
    try std.testing.expect(STOP_WORDS.has("is"));
    try std.testing.expect(STOP_WORDS.has("does"));
    try std.testing.expect(!STOP_WORDS.has("cosine"));
    try std.testing.expect(!STOP_WORDS.has("search"));
}

test "dupeString produces independent copy" {
    const original = "hello world";
    const copy = try dupeString(std.testing.allocator, original);
    defer std.testing.allocator.free(copy);
    try std.testing.expectEqualStrings(original, copy);
    try std.testing.expect(copy.ptr != original.ptr);
}

test "dupeString returns null for empty string" {
    const result = try dupeString(std.testing.allocator, "");
    try std.testing.expect(result == null);
}

test "dupeStringOpt returns null for null input" {
    const result = try dupeStringOpt(std.testing.allocator, null);
    try std.testing.expect(result == null);
}

test "dupeStringOpt returns null for empty string" {
    const result = try dupeStringOpt(std.testing.allocator, "");
    try std.testing.expect(result == null);
}

test "dupeStringOpt returns copy for non-empty string" {
    const original = "test content";
    const result = try dupeStringOpt(std.testing.allocator, original);
    defer if (result) |s| std.testing.allocator.free(s);
    try std.testing.expect(result != null);
    try std.testing.expectEqualStrings(original, result.?);
    try std.testing.expect(result.?.ptr != original.ptr);
}

/// Converts a string into a slugified format by trimming, lowercasing, and replacing spaces with hyphens.
pub fn slugify(allocator: std.mem.Allocator, s: []const u8) ![]const u8 {
    var buf: std.ArrayList(u8) = .{};
    var prev_dash = true;
    for (s) |c| {
        if (std.ascii.isAlphanumeric(c)) {
            try buf.append(allocator, std.ascii.toLower(c));
            prev_dash = false;
        } else if (!prev_dash and buf.items.len > 0) {
            try buf.append(allocator, '-');
            prev_dash = true;
        }
    }
    while (buf.items.len > 0 and buf.items[buf.items.len - 1] == '-') {
        buf.items.len -= 1;
    }
    if (buf.items.len == 0) try buf.appendSlice(allocator, "work-item");
    if (buf.items.len > 40) buf.items.len = 40;
    return buf.toOwnedSlice(allocator);
}

test "slugify converts to lowercase and replaces spaces" {
    const result = try slugify(std.testing.allocator, "Hello World Test");
    defer std.testing.allocator.free(result);
    try std.testing.expectEqualStrings("hello-world-test", result);
}

test "slugify caps at 40 chars" {
    const long_input = "This is a very long description that exceeds forty characters";
    const result = try slugify(std.testing.allocator, long_input);
    defer std.testing.allocator.free(result);
    try std.testing.expect(@as(usize, 40) >= result.len);
}

test "slugify handles empty string" {
    const result = try slugify(std.testing.allocator, "");
    defer std.testing.allocator.free(result);
    try std.testing.expectEqualStrings("work-item", result);
}

















