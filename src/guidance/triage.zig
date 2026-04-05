/// Triage subcommand: generate TRIAGE.md from a TODO.md work item.
///
/// Mirrors Python's cmd_triage() in guidance.py.
/// Given .guidance/.todo/<item>/TODO.md, produces a TRIAGE.md with:
///   - Affected files (detected by regex + backtick paths)
///   - Risk assessment (deterministic)
///   - Recommended steps (LLM or fallback checklist)
///   - Lifecycle status
const std = @import("std");
const llm = @import("common");

/// Lifecycle states in order.
pub const LIFECYCLE = [_][]const u8{ "TODO", "TRIAGE", "WORK", "COMPLETE", "COMMITTED" };

pub const TriageResult = struct {
    triage_path: []const u8,
};

/// Retrieves the lifecycle state slice from the allocator and work directory.
pub fn getLifecycleState(allocator: std.mem.Allocator, work_dir: []const u8) ![]const u8 {
    var i: usize = LIFECYCLE.len;
    while (i > 0) {
        i -= 1;
        const state = LIFECYCLE[i];
        const state_path = try std.fmt.allocPrint(allocator, "{s}/{s}.md", .{ work_dir, state });
        defer allocator.free(state_path);
        if (std.fs.accessAbsolute(state_path, .{})) {
            return state;
        } else |_| {}
    }
    return "TODO";
}

/// Evaluates risk based on content and affected count, returning a processed slice.
pub fn assessRisk(content: []const u8, affected_count: usize) []const u8 {
    const has_high = llm.containsIgnoreCase(content, "delete") or
        llm.containsIgnoreCase(content, " remove ") or
        llm.containsIgnoreCase(content, "breaking");
    if (has_high) return "**High** — Destructive operations detected";

    const has_medium = llm.containsIgnoreCase(content, "refactor") or
        llm.containsIgnoreCase(content, "rename") or
        llm.containsIgnoreCase(content, "migration");
    if (has_medium) return "**Medium** — Structural changes detected";

    if (affected_count > 5) return "**Medium** — Wide scope: many affected files";

    return "**Low** — Targeted change";
}

/// Default 7-step checklist (mirrors Python's _default_steps).
pub const DEFAULT_STEPS =
    \\1. Read affected files and understand current implementation
    \\2. Write tests for the expected behavior (TDD)
    \\3. Implement the changes incrementally
    \\4. Run `make pre-commit` after each significant change
    \\5. Update STRUCTURE.md if new files/dirs are added
    \\6. Move work item to WORK.md when implementation begins
    \\7. Move to COMPLETE.md when tests pass
;

/// Identifies and returns affected file slices based on allocation data.
pub fn findAffectedFiles(
    allocator: std.mem.Allocator,
    content: []const u8,
    project_root: []const u8,
) ![][]const u8 {
    var found = std.ArrayList([]const u8){};
    errdefer {
        for (found.items) |s| allocator.free(s);
        found.deinit(allocator);
    }

    // Prefixes to look for.
    const prefixes = [_][]const u8{ "src/", "bin/", "test/", "tests/", "doc/", "guidance/" };

    var i: usize = 0;
    while (i < content.len) {
        // Check backtick-quoted path: `path.ext`
        if (content[i] == '`' and i + 2 < content.len) {
            const close = std.mem.indexOfScalarPos(u8, content, i + 1, '`') orelse {
                i += 1;
                continue;
            };
            const inner = content[i + 1 .. close];
            if (isPathToken(inner)) {
                if (try addUnique(allocator, &found, inner, project_root)) {
                    i = close + 1;
                    continue;
                }
            }
            i = close + 1;
            continue;
        }

        // Check for prefix-based path.
        for (prefixes) |prefix| {
            if (i + prefix.len > content.len) continue;
            if (!std.mem.eql(u8, content[i .. i + prefix.len], prefix)) continue;
            // Scan forward for valid path chars.
            var end = i + prefix.len;
            while (end < content.len) {
                const c = content[end];
                if (c == ' ' or c == '\n' or c == '\r' or c == '\t' or
                    c == ')' or c == ']' or c == ',' or c == ';' or c == '"' or c == '\'')
                {
                    break;
                }
                end += 1;
            }
            const token = content[i..end];
            if (token.len > prefix.len and llm.hasExtension(token, &TRIAGE_EXTS)) {
                _ = try addUnique(allocator, &found, token, project_root);
            }
            i = end;
            break;
        } else {
            i += 1;
        }
    }

    if (found.items.len == 0 and content.len > 0) {
        // Fallback: collect first 5 word-like tokens that look path-ish
        // (not implemented; return empty).
    }

    return found.toOwnedSlice(allocator);
}

/// Common source/doc extensions recognised by triage path detection.
const TRIAGE_EXTS = [_][]const u8{ "zig", "py", "md", "json", "toml", "yaml", "yml", "sh", "txt" };

/// Checks if a given byte slice represents a valid path token, returning true or false.
fn isPathToken(s: []const u8) bool {
    return llm.isPathToken(s, &TRIAGE_EXTS);
}

/// Add path to list if not a duplicate. Verifies existence in project_root.
/// Returns true if added.
const addUnique = llm.shell.addUniquePath;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "assessRisk high for delete" {
    const risk = assessRisk("We need to delete the old auth module", 2);
    try std.testing.expect(std.mem.startsWith(u8, risk, "**High**"));
}

test "assessRisk medium for refactor" {
    const risk = assessRisk("Refactor the sync logic for clarity", 3);
    try std.testing.expect(std.mem.startsWith(u8, risk, "**Medium**"));
}

test "assessRisk medium for wide scope" {
    const risk = assessRisk("Add logging", 8);
    try std.testing.expect(std.mem.startsWith(u8, risk, "**Medium**"));
}

test "assessRisk low for simple change" {
    const risk = assessRisk("Add a new unit test for the parser", 1);
    try std.testing.expect(std.mem.startsWith(u8, risk, "**Low**"));
}

test "findAffectedFiles detects backtick paths" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const tmp_path = try tmp.dir.realpathAlloc(allocator, ".");
    defer allocator.free(tmp_path);

    // Create src/foo.zig in temp dir.
    try tmp.dir.makePath("src");
    const sf = try tmp.dir.createFile("src/foo.zig", .{});
    sf.close();

    const content = "Update `src/foo.zig` to add logging support";
    const files = try findAffectedFiles(allocator, content, tmp_path);
    defer {
        for (files) |f| allocator.free(f);
        allocator.free(files);
    }
    try std.testing.expect(files.len >= 1);
    try std.testing.expectEqualStrings("src/foo.zig", files[0]);
}
