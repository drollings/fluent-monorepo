//! core/skill_loader.zig — Unified SKILL.md paragraph loading.
//!
//! Consolidates:
//!   - query_engine.zig:loadSkillPara()
//!   - staged.zig:loadSkillExcerpt()
//!   - staged.zig:parseSkillDocContent()
//!
//! parseSkillDocContent delegates to doc_parser.parseSkillDocContent
//! (single source of truth for SKILL.md front-matter stripping).

const std = @import("std");
const common = @import("common");
const doc_parser = @import("../doc_parser.zig");

/// Load a relevant excerpt from a SKILL.md file for the given skill name.
/// Searches:
///   1. `{guidance_dir}/.skills/{skill_name}/SKILL.md`
///   2. `{cwd}/doc/skills/{skill_name}/SKILL.md`
/// Returns null if no skill file can be read or parsed.
/// Result is allocator-owned; caller frees.
pub fn loadSkillExcerpt(
    allocator: std.mem.Allocator,
    guidance_dir: []const u8,
    cwd: []const u8,
    skill_name: []const u8,
) ?[]const u8 {
    const SearchPath = struct { base: []const u8, rel: []const u8 };
    const paths = [_]SearchPath{
        .{ .base = guidance_dir, .rel = "skills" },
        .{ .base = cwd, .rel = "doc/skills" },
    };
    for (paths) |sp| {
        const path = std.fs.path.join(allocator, &.{ sp.base, sp.rel, skill_name, "SKILL.md" }) catch continue;
        defer allocator.free(path);
        const sf = std.fs.openFileAbsolute(path, .{}) catch continue;
        defer sf.close();
        const content = sf.readToEndAlloc(allocator, 512 * 1024) catch continue;
        defer allocator.free(content);
        if (parseSkillDocContent(allocator, content) catch null) |doc| return doc;
    }
    return null;
}

/// Extract the first meaningful paragraph from SKILL.md content.
/// Delegates to doc_parser.parseSkillDocContent (single source of truth).
/// Returns null if no meaningful content found.
pub fn parseSkillDocContent(allocator: std.mem.Allocator, content: []const u8) !?[]const u8 {
    return doc_parser.parseSkillDocContent(allocator, content);
}
