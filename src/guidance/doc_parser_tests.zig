//! Tests for doc_parser.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const doc_parser_mod = @import("doc_parser.zig");

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

    const excerpt = try doc_parser_mod.parseDocContent(allocator, content, false);
    defer doc_parser_mod.freeDocExcerpt(allocator, excerpt);

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

    const excerpt = try doc_parser_mod.parseDocContent(allocator, content, false);
    defer doc_parser_mod.freeDocExcerpt(allocator, excerpt);

    try std.testing.expectEqual(@as(usize, 3), excerpt.anchors.len);
    try std.testing.expectEqualStrings("EmbeddingProvider", excerpt.anchors[0]);
    try std.testing.expectEqualStrings("createEmbeddingProvider", excerpt.anchors[1]);
    try std.testing.expectEqualStrings("OllamaEmbedding", excerpt.anchors[2]);
}
test "parseDocContent: fallback to first paragraph when no frontmatter" {
    const allocator = std.testing.allocator;
    const content = "# Test\n\nThis is the first paragraph.\n\nMore content.";

    const excerpt = try doc_parser_mod.parseDocContent(allocator, content, false);
    defer doc_parser_mod.freeDocExcerpt(allocator, excerpt);

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

    const excerpt = try doc_parser_mod.parseDocContent(allocator, content, false);
    defer doc_parser_mod.freeDocExcerpt(allocator, excerpt);

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

    const result = try doc_parser_mod.parseSkillDocContent(allocator, content);
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

    const excerpt = try doc_parser_mod.parseDocContent(allocator, content, false);
    defer doc_parser_mod.freeDocExcerpt(allocator, excerpt);

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

    const excerpt = try doc_parser_mod.parseDocContent(allocator, content, false);
    defer doc_parser_mod.freeDocExcerpt(allocator, excerpt);

    try std.testing.expect(excerpt.description != null);
    try std.testing.expectEqual(@as(usize, 300), excerpt.description.?.len);
}
