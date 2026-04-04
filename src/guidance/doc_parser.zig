//! doc_parser.zig — Unified parser for SKILL.md and CAPABILITY.md frontmatter.
//!
//! Extracts YAML frontmatter (name, description, anchors) and the first paragraph
//! from Markdown documentation files. Shared by:
//!   - staged.zig (skill excerpts)
//!   - query_engine.zig (skill loading)
//!   - sync_engine.zig (capability index generation)

const std = @import("std");

/// Represents a structured document excerpt with ownership and invariants; managed via a single lifecycle.
pub const DocExcerpt = struct {
    /// From frontmatter "name:" field (optional for SKILL.md)
    name: ?[]const u8 = null,
    /// From frontmatter "description:" field
    description: ?[]const u8 = null,
    /// From frontmatter "anchors:" list (CAPABILITY.md only)
    anchors: []const []const u8 = &.{},
    /// Fallback: first body paragraph after frontmatter
    first_para: ?[]const u8 = null,
};

/// Releases memory for a DocExcerpt by freeing the underlying allocation.
pub fn freeDocExcerpt(allocator: std.mem.Allocator, excerpt: DocExcerpt) void {
    if (excerpt.name) |n| allocator.free(n);
    if (excerpt.description) |d| allocator.free(d);
    for (excerpt.anchors) |a| allocator.free(a);
    allocator.free(excerpt.anchors);
    if (excerpt.first_para) |fp| allocator.free(fp);
}

/// Converts a C string into a Zig document excerpt, handling memory allocation and parsing.
pub fn parseDocContent(allocator: std.mem.Allocator, content: []const u8, verbose: bool) !DocExcerpt {
    var result: DocExcerpt = .{};
    errdefer freeDocExcerpt(allocator, result);

    // Check for YAML frontmatter (starts with ---)
    if (std.mem.startsWith(u8, content, "---\n")) {
        const fm_close = std.mem.indexOf(u8, content[4..], "\n---\n");
        if (fm_close) |fmc| {
            const fm_body = content[4 .. 4 + fmc];
            const after_fm = content[4 + fmc + 5 ..];

            // Parse frontmatter fields
            var name: ?[]const u8 = null;
            var description: ?[]const u8 = null;
            var anchors_list: std.ArrayList([]const u8) = .{};
            defer anchors_list.deinit(allocator);

            var in_anchors = false;
            var fm_lines = std.mem.splitScalar(u8, fm_body, '\n');
            while (fm_lines.next()) |line| {
                const trimmed = std.mem.trim(u8, line, " \t\r");

                // Check for anchors: list start
                if (std.mem.startsWith(u8, trimmed, "anchors:")) {
                    in_anchors = true;
                    continue;
                }

                // If we're parsing anchors list
                if (in_anchors) {
                    // List items start with "- "
                    if (std.mem.startsWith(u8, trimmed, "- ")) {
                        const anchor_val = std.mem.trim(u8, trimmed[2..], " \t");
                        if (anchor_val.len > 0) {
                            try anchors_list.append(allocator, try allocator.dupe(u8, anchor_val));
                        }
                        continue;
                    }
                    // End of anchors list when we hit a non-list line
                    if (trimmed.len > 0 and !std.mem.startsWith(u8, trimmed, "- ")) {
                        in_anchors = false;
                    }
                }

                // Parse name: field
                if (std.mem.startsWith(u8, trimmed, "name:")) {
                    const val = std.mem.trim(u8, trimmed["name:".len..], " \t\r");
                    if (val.len > 0) {
                        name = try allocator.dupe(u8, val);
                    }
                    continue;
                }

                // Parse description: field
                if (std.mem.startsWith(u8, trimmed, "description:")) {
                    const val = std.mem.trim(u8, trimmed["description:".len..], " \t\r");
                    if (val.len > 0) {
                        description = try allocator.dupe(u8, val[0..@min(val.len, 300)]);
                    }
                    continue;
                }
            }

            result.name = name;
            result.description = description;
            result.anchors = try anchors_list.toOwnedSlice(allocator);

            if (verbose) {
                if (name) |n| {
                    std.debug.print("[doc_parser] parsed name=\"{s}\"", .{n});
                }
                if (description) |d| {
                    if (name != null) {
                        std.debug.print(", description=\"{s}...\"", .{d[0..@min(d.len, 50)]});
                    } else {
                        std.debug.print("[doc_parser] parsed description=\"{s}...\"\n", .{d[0..@min(d.len, 50)]});
                    }
                }
                if (name != null) std.debug.print("\n", .{});
                if (anchors_list.items.len > 0) {
                    std.debug.print("[doc_parser] parsed {d} anchors:", .{anchors_list.items.len});
                    for (anchors_list.items) |a| {
                        std.debug.print(" {s}", .{a});
                    }
                    std.debug.print("\n", .{});
                }
            }

            // If no description, try first non-empty body line
            if (description == null) {
                var body = std.mem.splitScalar(u8, after_fm, '\n');
                while (body.next()) |bl| {
                    const t = std.mem.trim(u8, bl, " \t\r");
                    if (t.len > 0 and !std.mem.startsWith(u8, t, "#")) {
                        result.first_para = try allocator.dupe(u8, t[0..@min(t.len, 300)]);
                        break;
                    }
                }
            }

            return result;
        }
    }

    // No frontmatter — first paragraph (up to blank line), max 600 chars
    const para_end = std.mem.indexOf(u8, content, "\n\n") orelse content.len;
    result.first_para = try allocator.dupe(u8, content[0..@min(para_end, 600)]);
    return result;
}

/// Converts a Zig source snippet into a Zig array, handling allocator and content input.
pub fn parseSkillDocContent(allocator: std.mem.Allocator, content: []const u8) !?[]const u8 {
    const excerpt = try parseDocContent(allocator, content, false);
    defer freeDocExcerpt(allocator, excerpt);

    if (excerpt.description) |d| return try allocator.dupe(u8, d);
    if (excerpt.first_para) |fp| return try allocator.dupe(u8, fp);
    return null;
}

/// Converts a Zig source snippet into a DocExcerpt object for parsing.
pub fn parseCapabilityDocContent(allocator: std.mem.Allocator, content: []const u8, verbose: bool) !DocExcerpt {
    return parseDocContent(allocator, content, verbose);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "parseDocContent: extract name and description from frontmatter" {
    const allocator = std.testing.allocator;
    const content =
        \\---
        \\name: test-skill
        \\description: This is a test skill description.
        \\---
        \\# Test Skill
        \\
        \\Some body content.
    ;

    const excerpt = try parseDocContent(allocator, content, false);
    defer freeDocExcerpt(allocator, excerpt);

    try std.testing.expect(excerpt.name != null);
    try std.testing.expectEqualStrings("test-skill", excerpt.name.?);
    try std.testing.expect(excerpt.description != null);
    try std.testing.expect(std.mem.startsWith(u8, excerpt.description.?, "This is a test"));
    try std.testing.expectEqual(@as(usize, 0), excerpt.anchors.len);
}

test "parseDocContent: extract anchors list from CAPABILITY frontmatter" {
    const allocator = std.testing.allocator;
    const content =
        \\---
        \\name: embedding-providers
        \\description: Pluggable embedding system.
        \\anchors:
        \\  - EmbeddingProvider
        \\  - createEmbeddingProvider
        \\  - OllamaEmbedding
        \\---
        \\# Embedding Providers
        \\
        \\Body content here.
    ;

    const excerpt = try parseDocContent(allocator, content, false);
    defer freeDocExcerpt(allocator, excerpt);

    try std.testing.expectEqual(@as(usize, 3), excerpt.anchors.len);
    try std.testing.expectEqualStrings("EmbeddingProvider", excerpt.anchors[0]);
    try std.testing.expectEqualStrings("createEmbeddingProvider", excerpt.anchors[1]);
    try std.testing.expectEqualStrings("OllamaEmbedding", excerpt.anchors[2]);
}

test "parseDocContent: fallback to first paragraph when no frontmatter" {
    const allocator = std.testing.allocator;
    const content = "# Test\n\nThis is the first paragraph.\n\nMore content.";

    const excerpt = try parseDocContent(allocator, content, false);
    defer freeDocExcerpt(allocator, excerpt);

    try std.testing.expect(excerpt.name == null);
    try std.testing.expect(excerpt.description == null);
    try std.testing.expect(excerpt.first_para != null);
    try std.testing.expectEqualStrings("# Test", excerpt.first_para.?);
}

test "parseDocContent: fallback to first body line when no description in frontmatter" {
    const allocator = std.testing.allocator;
    const content =
        \\---
        \\name: skill-only
        \\---
        \\# Heading
        \\
        \\First body line after frontmatter.
        \\
        \\More content.
    ;

    const excerpt = try parseDocContent(allocator, content, false);
    defer freeDocExcerpt(allocator, excerpt);

    try std.testing.expectEqualStrings("skill-only", excerpt.name.?);
    try std.testing.expect(excerpt.description == null);
    try std.testing.expect(excerpt.first_para != null);
    try std.testing.expectEqualStrings("First body line after frontmatter.", excerpt.first_para.?);
}

test "parseSkillDocContent: backward compatible wrapper" {
    const allocator = std.testing.allocator;
    const content =
        \\---
        \\description: Skill description here.
        \\---
        \\# Skill
        \\
        \\Body.
    ;

    const result = try parseSkillDocContent(allocator, content);
    defer if (result) |r| allocator.free(r);

    try std.testing.expect(result != null);
    try std.testing.expectEqualStrings("Skill description here.", result.?);
}

test "parseDocContent: handle empty anchors list" {
    const allocator = std.testing.allocator;
    const content =
        \\---
        \\name: no-anchors
        \\anchors:
        \\---
        \\# No Anchors
    ;

    const excerpt = try parseDocContent(allocator, content, false);
    defer freeDocExcerpt(allocator, excerpt);

    try std.testing.expectEqual(@as(usize, 0), excerpt.anchors.len);
}

test "parseDocContent: multiline description truncated to 300 chars" {
    const allocator = std.testing.allocator;
    var long_desc: [400]u8 = undefined;
    @memset(&long_desc, 'a');
    long_desc[399] = 0;

    const content = try std.fmt.allocPrint(allocator,
        \\---
        \\description: {s}
        \\---
        \\# Test
    , .{long_desc[0..]});
    defer allocator.free(content);

    const excerpt = try parseDocContent(allocator, content, false);
    defer freeDocExcerpt(allocator, excerpt);

    try std.testing.expect(excerpt.description != null);
    try std.testing.expectEqual(@as(usize, 300), excerpt.description.?.len);
}





