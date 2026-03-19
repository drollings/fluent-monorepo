/// str.zig — Generic string classification and inspection helpers
///
/// Functions here operate purely on `[]const u8` slices and have no
/// dependency on guidance-domain types.  Suitable for reuse in any Zig
/// tool that needs lightweight text analysis.
const std = @import("std");

/// Return true when `token` looks like a code identifier rather than a plain
/// English word.
///
/// Heuristics (any one is sufficient):
///   - Contains an underscore        → snake_case / SCREAMING_SNAKE
///   - Has an uppercase letter after the first character → camelCase / PascalCase
///
/// This filter prevents short, lowercase common words ("search", "work", "how")
/// from accidentally matching AST names during deterministic lookup phases.
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

/// Return true when `rel_path` matches common test file naming conventions.
///
/// Checks both Unix (`/test/`, `/tests/`) and Windows (`\test\`, `\tests\`)
/// directory separators.  Suitable for filtering test files out of `used_by`
/// lists and reverse-dependency scans.
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

/// Extract a short skill name from a JSON skill ref path.
///
/// Examples:
///   ".skills/gof-patterns/SKILL.md"  → "gof-patterns"
///   ".skills/zig-current/SKILL.md"   → "zig-current"
///   "gof-patterns"                   → "gof-patterns"  (bare name, returned as-is)
///
/// Returns a slice into the original `ref` — no allocation.
pub fn skillNameFromRef(ref: []const u8) []const u8 {
    const base = std.fs.path.basename(ref);
    if (std.mem.eql(u8, base, "SKILL.md")) {
        const dir = std.fs.path.dirname(ref) orelse return base;
        return std.fs.path.basename(dir);
    }
    return base;
}

/// Return true when `needle` appears anywhere in `haystack` (case-insensitive).
///
/// Uses a sliding-window comparison with `std.ascii.eqlIgnoreCase`.
/// No allocation — suitable for hot paths and large haystacks.
/// An empty needle always matches.
pub fn containsIgnoreCase(haystack: []const u8, needle: []const u8) bool {
    if (needle.len == 0) return true;
    if (needle.len > haystack.len) return false;
    var i: usize = 0;
    while (i + needle.len <= haystack.len) : (i += 1) {
        if (std.ascii.eqlIgnoreCase(haystack[i .. i + needle.len], needle)) return true;
    }
    return false;
}

/// Map a file path to a fenced-code-block language identifier.
///
/// Used for Markdown output (e.g. ` ```zig `) when embedding source excerpts.
/// Recognised extensions: .zig, .py, .rs, .ts, .tsx, .js.
/// Falls back to "text" for unrecognised extensions.
///
/// Returns a `[]const u8` into a static string literal — no allocation.
pub fn langFromPath(path: []const u8) []const u8 {
    if (std.mem.endsWith(u8, path, ".zig")) return "zig";
    if (std.mem.endsWith(u8, path, ".py")) return "python";
    if (std.mem.endsWith(u8, path, ".rs")) return "rust";
    if (std.mem.endsWith(u8, path, ".ts") or std.mem.endsWith(u8, path, ".tsx")) return "typescript";
    if (std.mem.endsWith(u8, path, ".js")) return "javascript";
    return "text";
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

test "looksLikeIdentifier rejects plain words" {
    try std.testing.expect(!looksLikeIdentifier("search"));
    try std.testing.expect(!looksLikeIdentifier("work"));
    try std.testing.expect(!looksLikeIdentifier("how"));
    try std.testing.expect(!looksLikeIdentifier("a")); // too short
}
