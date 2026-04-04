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
