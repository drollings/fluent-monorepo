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

fn isAlphanumericBoundary(c: u8) bool {
    return !std.ascii.isAlphanumeric(c);
}

fn isIdentBoundary(c: u8) bool {
    return !std.ascii.isAlphanumeric(c) and c != '_';
}

fn containsWordWithBoundary(haystack: []const u8, needle: []const u8, boundary_fn: *const fn (u8) bool) bool {
    if (needle.len > haystack.len) return false;
    var i: usize = 0;
    while (i + needle.len <= haystack.len) : (i += 1) {
        if (!std.ascii.eqlIgnoreCase(haystack[i .. i + needle.len], needle)) continue;
        const left_ok = i == 0 or boundary_fn(haystack[i - 1]);
        const right_end = i + needle.len;
        const right_ok = right_end >= haystack.len or boundary_fn(haystack[right_end]);
        if (left_ok and right_ok) return true;
    }
    return false;
}

/// Checks if a needle substring exists within the haystack array of bytes.
pub fn containsWord(haystack: []const u8, needle: []const u8) bool {
    return containsWordWithBoundary(haystack, needle, isAlphanumericBoundary);
}

/// Checks if a needle substring exists within the haystack as an identifier (underscore-aware).
pub fn containsIdentWord(haystack: []const u8, needle: []const u8) bool {
    return containsWordWithBoundary(haystack, needle, isIdentBoundary);
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
pub fn dupeString(allocator: std.mem.Allocator, s: []const u8) !?[]const u8 {
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

// =============================================================================
// Hot-path lowercase helpers — zero-allocation for identifiers ≤ 256 bytes
// =============================================================================

/// Copy `src` into `dst`, lowercasing every byte. Returns the written slice.
/// `dst.len` must be >= `src.len`; the result is clamped to `@min(src.len, dst.len)`.
/// Does NOT allocate. Safe to call in tight loops.
pub fn lowerInto(dst: []u8, src: []const u8) []u8 {
    const len = @min(src.len, dst.len);
    return std.ascii.lowerString(dst[0..len], src[0..len]);
}

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

// =============================================================================
// Sentence truncation — for LOD text pyramids
// =============================================================================

/// Truncates text at a sentence boundary within max_chars.
/// Searches backwards for `. `, `! `, `? `, `.\n` within the budget.
/// Falls back to word boundary, then hard cut if no boundary found.
pub fn truncateAtSentence(allocator: std.mem.Allocator, text: []const u8, max_chars: usize) ![]const u8 {
    if (text.len <= max_chars) return allocator.dupe(u8, text);

    const window = text[0..max_chars];
    // Search backwards for a sentence-end token.
    var i: usize = max_chars;
    while (i > max_chars / 2) : (i -= 1) {
        const ch = window[i - 1];
        const next = if (i < max_chars) window[i] else ' ';
        if ((ch == '.' or ch == '!' or ch == '?') and (next == ' ' or next == '\n')) {
            return allocator.dupe(u8, text[0..i]);
        }
    }
    // Fallback: word boundary
    i = max_chars;
    while (i > max_chars / 2) : (i -= 1) {
        if (window[i - 1] == ' ') {
            return allocator.dupe(u8, std.mem.trimRight(u8, text[0 .. i - 1], " \t"));
        }
    }
    // Hard cut if no boundary found.
    return allocator.dupe(u8, text[0..max_chars]);
}

/// Extracts the first line of a doc comment (for skeleton summaries).
/// For `/// Function description` returns `Function description`.
/// For `//! Module description` returns `Module description`.
/// Strips leading `/// ` or `//! ` prefix and trailing newlines.
pub fn firstCommentLine(comment: []const u8) []const u8 {
    if (comment.len == 0) return "";

    // Skip leading whitespace
    var start: usize = 0;
    while (start < comment.len and (comment[start] == ' ' or comment[start] == '\t')) {
        start += 1;
    }

    // Strip /// or //! prefix
    if (start + 3 <= comment.len) {
        if ((comment[start] == '/' and comment[start + 1] == '/' and comment[start + 2] == '/') or
            (comment[start] == '/' and comment[start + 1] == '!' and comment[start + 2] == '/'))
        {
            start += 3;
            // Skip space after ///
            while (start < comment.len and comment[start] == ' ') {
                start += 1;
            }
        }
    }

    // Find end of first line
    var end = start;
    while (end < comment.len and comment[end] != '\n' and comment[end] != '\r') {
        end += 1;
    }

    // Trim trailing whitespace
    while (end > start and (comment[end - 1] == ' ' or comment[end - 1] == '\t')) {
        end -= 1;
    }

    return comment[start..end];
}
