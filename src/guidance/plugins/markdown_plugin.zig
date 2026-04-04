//! MarkdownPlugin — extracts sections and metadata from Markdown files.
//!
//! Each heading (# / ## / ###) becomes a "member" whose name is the heading
//! text and whose line number is the heading's line.  The first paragraph of
//! the document becomes the module-level comment.
//!
//! node_type values:
//!   "h1"  — top-level heading
//!   "h2"  — second-level heading
//!   "h3"  — third-level heading
//!   "section" — deeper / unrecognised headings

const std = @import("std");
const types = @import("../types.zig");
const plugin_mod = @import("../plugin.zig");

const LanguagePlugin = plugin_mod.LanguagePlugin;
const ParsedFile = plugin_mod.ParsedFile;

const EXTENSIONS = [_][]const u8{ ".md", ".markdown", ".mdx" };

/// Returns the singleton MarkdownPlugin descriptor.
pub fn plugin() LanguagePlugin {
    return .{
        .name = "markdown",
        .extensions = &EXTENSIONS,
        .parseFn = parseMarkdown,
        .extractImportsFn = extractMarkdownLinks,
    };
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

/// Converts a markdown file into a Zig source file by parsing its content.
fn parseMarkdown(
    arena: std.mem.Allocator,
    source: [:0]const u8,
    file_path: []const u8,
) anyerror!ParsedFile {
    const module = try deriveMarkdownModule(arena, file_path);
    const src_path = if (std.mem.startsWith(u8, file_path, "./")) file_path[2..] else file_path;

    var members: std.ArrayList(types.Member) = .{};

    var first_para_buf: std.ArrayList(u8) = .{};
    var first_para_done = false;
    var in_code_block = false;
    var line_no: u32 = 0;

    var lines = std.mem.splitScalar(u8, source, '\n');
    while (lines.next()) |raw_line| {
        line_no += 1;
        const line = std.mem.trimRight(u8, raw_line, "\r");

        // Track fenced code blocks so we don't parse their content.
        if (std.mem.startsWith(u8, line, "```") or std.mem.startsWith(u8, line, "~~~")) {
            in_code_block = !in_code_block;
            first_para_done = true;
            continue;
        }
        if (in_code_block) continue;

        // Heading detection.
        if (std.mem.startsWith(u8, line, "#")) {
            var depth: usize = 0;
            for (line) |ch| {
                if (ch == '#') depth += 1 else break;
            }
            // Require space after hashes.
            if (depth < line.len and line[depth] == ' ') {
                const heading_text = std.mem.trim(u8, line[depth + 1 ..], " \t");
                if (heading_text.len == 0) continue;

                const node_type: []const u8 = switch (depth) {
                    1 => "h1",
                    2 => "h2",
                    3 => "h3",
                    else => "section",
                };

                // Skip the first h1 — used as the module title, not a member.
                // Do NOT set first_para_done here so the paragraph below it is collected.
                if (depth == 1 and members.items.len == 0 and first_para_buf.items.len == 0) {
                    continue;
                }

                // Any non-skipped heading ends the first paragraph and starts a section.
                first_para_done = true;

                try members.append(arena, types.Member{
                    .type = .fn_decl, // reuse fn_decl as a generic "section" node type
                    .name = try arena.dupe(u8, heading_text),
                    .signature = try std.fmt.allocPrint(arena, "{s} {s}", .{ node_type, heading_text }),
                    .comment = null,
                    .is_pub = true,
                    .line = line_no,
                });
                continue;
            }
        }

        // Collect first paragraph (non-blank lines after the first heading).
        if (!first_para_done) {
            if (line.len == 0) {
                // A blank line after we've collected text ends the paragraph.
                if (first_para_buf.items.len > 0) first_para_done = true;
            } else if (!std.mem.startsWith(u8, line, "#")) {
                if (first_para_buf.items.len > 0)
                    try first_para_buf.append(arena, ' ');
                try first_para_buf.appendSlice(arena, line);
            }
        }
    }

    const module_comment: ?[]const u8 = if (first_para_buf.items.len > 0)
        first_para_buf.items
    else
        null;

    return ParsedFile{
        .module = module,
        .source = src_path,
        .language = "markdown",
        .module_comment = module_comment,
        .members = try members.toOwnedSlice(arena),
    };
}

/// Extract Markdown link targets as "imports" — useful for used_by analysis.
/// Only extracts `[text](path)` style links whose target does not start with
/// http/https (i.e. local file links).
fn extractMarkdownLinks(
    arena: std.mem.Allocator,
    source: [:0]const u8,
) anyerror![]const []const u8 {
    var links: std.ArrayList([]const u8) = .{};
    var in_code = false;
    var pos: usize = 0;
    const src = @as([]const u8, source);

    var line_iter = std.mem.splitScalar(u8, src, '\n');
    while (line_iter.next()) |line| {
        if (std.mem.startsWith(u8, line, "```") or std.mem.startsWith(u8, line, "~~~"))
            in_code = !in_code;
        if (in_code) continue;

        pos = 0;
        while (pos < line.len) {
            // Find `](`.
            const bracket = std.mem.indexOfPos(u8, line, pos, "](") orelse break;
            const close_paren = std.mem.indexOfScalarPos(u8, line, bracket + 2, ')') orelse break;
            const target = line[bracket + 2 .. close_paren];
            pos = close_paren + 1;
            if (target.len == 0) continue;
            if (std.mem.startsWith(u8, target, "http://") or
                std.mem.startsWith(u8, target, "https://") or
                std.mem.startsWith(u8, target, "#")) continue;
            try links.append(arena, target);
        }
    }

    return links.toOwnedSlice(arena);
}

/// Derive a dot-separated module name from a Markdown file path.
/// "docs/README.md" → "docs.README"
fn deriveMarkdownModule(arena: std.mem.Allocator, file_path: []const u8) ![]const u8 {
    var path = file_path;
    if (std.mem.startsWith(u8, path, "./")) path = path[2..];
    if (std.mem.lastIndexOfScalar(u8, path, '.')) |dot| path = path[0..dot];
    const out = try arena.dupe(u8, path);
    for (out) |*ch| if (ch.* == '/' or ch.* == '\\') {
        ch.* = '.';
    };
    return out;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "MarkdownPlugin handles .md extension" {
    const p = plugin();
    try std.testing.expectEqualStrings("markdown", p.name);
    var found = false;
    for (p.extensions) |ext| if (std.mem.eql(u8, ext, ".md")) {
        found = true;
    };
    try std.testing.expect(found);
}

test "MarkdownPlugin.parse extracts headings as members" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    // Build the source string explicitly to avoid multiline literal edge cases.
    const src_str = "# My Project\n\nA short description of the project.\n\n## Installation\n\nSteps to install.\n\n## Usage\n\nHow to use it.\n\n### Advanced\n\nAdvanced topics.\n";
    const src = try std.fmt.allocPrintSentinel(arena.allocator(), "{s}", .{src_str}, 0);

    const p = plugin();
    const result = try p.parse(arena.allocator(), src, "README.md");

    try std.testing.expectEqualStrings("markdown", result.language);
    try std.testing.expectEqualStrings("README", result.module);
    // First paragraph after h1 heading.
    try std.testing.expect(result.module_comment != null);
    try std.testing.expect(std.mem.indexOf(u8, result.module_comment.?, "description") != null);

    // Members: Installation, Usage, Advanced (h1 is skipped as module title).
    try std.testing.expect(result.members.len >= 3);
    try std.testing.expectEqualStrings("Installation", result.members[0].name);
    try std.testing.expectEqualStrings("Usage", result.members[1].name);
    try std.testing.expectEqualStrings("Advanced", result.members[2].name);
}

test "MarkdownPlugin.parse code blocks are skipped" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const src: [:0]const u8 =
        \\# Title
        \\
        \\```
        \\## Not a heading
        \\```
        \\
        \\## Real heading
    ;
    const p = plugin();
    const result = try p.parse(arena.allocator(), src, "doc.md");
    // Only "Real heading" should be a member.
    try std.testing.expectEqual(@as(usize, 1), result.members.len);
    try std.testing.expectEqualStrings("Real heading", result.members[0].name);
}

test "MarkdownPlugin.extractImports finds local links" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const src: [:0]const u8 =
        \\See [installation](docs/install.md) and [API](api/reference.md).
        \\Also [external](https://example.com) which should be skipped.
    ;
    const p = plugin();
    const links = try p.extractImports(arena.allocator(), src);

    try std.testing.expectEqual(@as(usize, 2), links.len);
    try std.testing.expectEqualStrings("docs/install.md", links[0]);
    try std.testing.expectEqualStrings("api/reference.md", links[1]);
}

test "FileType.fromExtension" {
    try std.testing.expectEqual(types.FileType.source, types.FileType.fromExtension(".zig"));
    try std.testing.expectEqual(types.FileType.source, types.FileType.fromExtension(".py"));
    try std.testing.expectEqual(types.FileType.markdown, types.FileType.fromExtension(".md"));
    try std.testing.expectEqual(types.FileType.config, types.FileType.fromExtension(".toml"));
    try std.testing.expectEqual(types.FileType.unknown, types.FileType.fromExtension(".xyz"));
}
