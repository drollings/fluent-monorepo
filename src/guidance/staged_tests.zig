//! Tests for staged.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const types = @import("types.zig");
const common = @import("common");
const staged_mod = @import("staged.zig");

test "formatStaged: empty stages output contains header" {
    const allocator = std.testing.allocator;
    const result = try staged_mod.formatStaged(allocator, "myquery", &.{}, null, "/workspace");
    defer allocator.free(result);
    try std.testing.expect(std.mem.indexOf(u8, result, "# Explain: myquery") != null);
}
test "formatStaged: code stage with line emits source path and line number" {
    const allocator = std.testing.allocator;
    const stages = [_]types.Stage{.{
        .kind = .code,
        .content = "pub fn foo() void {}\n",
        .source = "src/foo.zig",
        .line = 10,
    }};
    const result = try staged_mod.formatStaged(allocator, "q", &stages, null, "/workspace");
    defer allocator.free(result);
    try std.testing.expect(std.mem.indexOf(u8, result, "src/foo.zig:10") != null);
    try std.testing.expect(std.mem.indexOf(u8, result, "pub fn foo()") != null);
}
test "formatStaged: code stage without line still emits code block" {
    const allocator = std.testing.allocator;
    const stages = [_]types.Stage{.{
        .kind = .code,
        .content = "const x = 1;\n",
        .source = "src/bar.zig",
        .line = null,
    }};
    const result = try staged_mod.formatStaged(allocator, "q", &stages, null, "/workspace");
    defer allocator.free(result);
    try std.testing.expect(std.mem.indexOf(u8, result, "src/bar.zig") != null);
    try std.testing.expect(std.mem.indexOf(u8, result, "const x = 1;") != null);
}
test "formatStaged: summary appears before code sections" {
    const allocator = std.testing.allocator;
    const stages = [_]types.Stage{.{
        .kind = .code,
        .content = "pub fn foo() void {}\n",
        .source = "src/foo.zig",
        .line = 1,
    }};
    const result = try staged_mod.formatStaged(allocator, "q", &stages, "This is the summary.", "/workspace");
    defer allocator.free(result);
    const sum_pos = std.mem.indexOf(u8, result, "This is the summary.");
    const src_pos = std.mem.indexOf(u8, result, "## Source location:");
    try std.testing.expect(sum_pos != null);
    try std.testing.expect(src_pos != null);
    try std.testing.expect(sum_pos.? < src_pos.?);
}
test "formatStaged: skill_doc stage produces Knowledge Base section" {
    const allocator = std.testing.allocator;
    const stages = [_]types.Stage{.{
        .kind = .skill_doc,
        .content = "This skill teaches you X.",
        .source = "zig-current",
        .line = null,
    }};
    const result = try staged_mod.formatStaged(allocator, "q", &stages, null, "/workspace");
    defer allocator.free(result);
    try std.testing.expect(std.mem.indexOf(u8, result, "## Knowledge Base") != null);
    try std.testing.expect(std.mem.indexOf(u8, result, "zig-current") != null);
}
test "formatStaged: metadata stage with keywords prefix produces References section" {
    const allocator = std.testing.allocator;
    const stages = [_]types.Stage{.{
        .kind = .metadata,
        .content = "keywords: vtable, allocator, arena",
        .source = "src/types.zig",
        .line = null,
    }};
    const result = try staged_mod.formatStaged(allocator, "q", &stages, null, "/workspace");
    defer allocator.free(result);
    try std.testing.expect(std.mem.indexOf(u8, result, "## References") != null);
    try std.testing.expect(std.mem.indexOf(u8, result, "vtable") != null);
}
test "parseSkillDocContent: YAML front matter with description returns description" {
    const allocator = std.testing.allocator;
    const content =
        \\---
        \\title: My Skill
        \\description: Teaches you how to use vtables.
        \\---
        \\
        \\Body paragraph here.
    ;
    const result = try staged_mod.parseSkillDocContent(allocator, content);
    defer if (result) |r| allocator.free(r);
    try std.testing.expect(result != null);
    try std.testing.expectEqualStrings("Teaches you how to use vtables.", result.?);
}
test "parseSkillDocContent: YAML front matter without description returns first non-empty body line" {
    const allocator = std.testing.allocator;
    const content =
        \\---
        \\title: My Skill
        \\author: someone
        \\---
        \\
        \\First real paragraph.
    ;
    const result = try staged_mod.parseSkillDocContent(allocator, content);
    defer if (result) |r| allocator.free(r);
    try std.testing.expect(result != null);
    try std.testing.expectEqualStrings("First real paragraph.", result.?);
}
test "parseSkillDocContent: no front matter returns first paragraph up to blank line" {
    const allocator = std.testing.allocator;
    const content = "First paragraph text.\n\nSecond paragraph here.";
    const result = try staged_mod.parseSkillDocContent(allocator, content);
    defer if (result) |r| allocator.free(r);
    try std.testing.expect(result != null);
    try std.testing.expectEqualStrings("First paragraph text.", result.?);
}
test "formatStaged: capability_doc stage renders Capability section" {
    const allocator = std.testing.allocator;
    const stages = [_]types.Stage{.{
        .kind = .capability_doc,
        .content = "Pluggable embedding system.\n\n**Anchors**: EmbeddingProvider\n**Sources**: src/common/embeddings.zig (1.0)\n",
        .source = "embedding-providers",
        .line = null,
    }};
    const result = try staged_mod.formatStaged(allocator, "embed", &stages, null, "/workspace");
    defer allocator.free(result);
    try std.testing.expect(std.mem.indexOf(u8, result, "## Capability: embedding-providers") != null);
    try std.testing.expect(std.mem.indexOf(u8, result, "EmbeddingProvider") != null);
}
test "formatStaged: keywords are capped at 10 unique items" {
    const allocator = std.testing.allocator;
    const stages = [_]types.Stage{.{
        .kind = .metadata,
        .content = "keywords: a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13, a14, a15",
        .source = "src/foo.zig",
        .line = null,
    }};
    const result = try staged_mod.formatStaged(allocator, "q", &stages, null, "/workspace");
    defer allocator.free(result);
    const see_also_start = std.mem.indexOf(u8, result, "Other terms to search") orelse {
        try std.testing.expect(false); // must have Other terms section
        return;
    };
    const see_also_line_end = std.mem.indexOfScalar(u8, result[see_also_start..], '\n') orelse result.len - see_also_start;
    const see_also_line = result[see_also_start .. see_also_start + see_also_line_end];
    var comma_count: usize = 0;
    for (see_also_line) |ch| if (ch == ',') {
        comma_count += 1;
    };
    try std.testing.expect(comma_count <= 8); // 9 items in "Other terms" = 8 commas
}
