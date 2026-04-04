/// frozen_snapshot.zig — Frozen State Snapshot for Session Prompt Stability
///
/// Captures memory, skills, and context file content at session start so that
/// the system prompt remains stable even if the underlying files change mid-session.
///
/// All owned strings are arena-allocated; callers must call deinit(allocator) to free.
const std = @import("std");

// ---------------------------------------------------------------------------
// FrozenSnapshot
// ---------------------------------------------------------------------------

/// Represents a snapshot of a frozen state, managing ownership and invariants for snapshot integrity.
pub const FrozenSnapshot = struct {
    /// Concatenated memory content (e.g., from memory index files).
    memory: []const u8,
    /// Concatenated skills content (e.g., from skill documents).
    skills: []const u8,
    /// Concatenated context file content (e.g., project docs, README).
    context_files: []const u8,

    const Self = @This();

    // ------------------------------------------------------------------
    // Lifecycle
    // ------------------------------------------------------------------

    /// Return an empty snapshot with no allocations.
    pub fn init() Self {
        return .{
            .memory = "",
            .skills = "",
            .context_files = "",
        };
    }

    /// Load and concatenate the content of each path in `paths`.
    /// All returned strings are owned by `allocator`; call deinit(allocator)
    /// to release them.
    pub fn load(allocator: std.mem.Allocator, paths: []const []const u8) !Self {
        const joined = try readAndJoin(allocator, paths);
        return Self{
            .memory = "",
            .skills = "",
            .context_files = joined,
        };
    }

    /// Load memory, skills, and context files from separate path lists.
    /// This is the full constructor; load() is a convenience wrapper that puts
    /// everything into context_files.
    pub fn loadSections(
        allocator: std.mem.Allocator,
        memory_paths: []const []const u8,
        skill_paths: []const []const u8,
        context_paths: []const []const u8,
    ) !Self {
        const memory = try readAndJoin(allocator, memory_paths);
        errdefer allocator.free(memory);

        const skills = try readAndJoin(allocator, skill_paths);
        errdefer allocator.free(skills);

        const context_files = try readAndJoin(allocator, context_paths);
        errdefer allocator.free(context_files);

        return Self{
            .memory = memory,
            .skills = skills,
            .context_files = context_files,
        };
    }

    /// Format the snapshot as a system prompt section.
    /// The returned slice is owned by `allocator`; caller must free it.
    pub fn formatForSystemPrompt(self: *const Self, allocator: std.mem.Allocator) ![]const u8 {
        var buf: std.ArrayList(u8) = .empty;
        defer buf.deinit(allocator);

        if (self.memory.len > 0) {
            try buf.appendSlice(allocator, "## Memory\n\n");
            try buf.appendSlice(allocator, self.memory);
            try buf.appendSlice(allocator, "\n\n");
        }

        if (self.skills.len > 0) {
            try buf.appendSlice(allocator, "## Skills\n\n");
            try buf.appendSlice(allocator, self.skills);
            try buf.appendSlice(allocator, "\n\n");
        }

        if (self.context_files.len > 0) {
            try buf.appendSlice(allocator, "## Context\n\n");
            try buf.appendSlice(allocator, self.context_files);
            try buf.appendSlice(allocator, "\n\n");
        }

        return buf.toOwnedSlice(allocator);
    }

    /// Free all owned strings.  Must be called with the same allocator used
    /// to create the snapshot.  Safe to call on a snapshot produced by init()
    /// (the empty strings are compile-time constants, not heap allocations).
    pub fn deinit(self: *Self, allocator: std.mem.Allocator) void {
        if (self.memory.len > 0) allocator.free(self.memory);
        if (self.skills.len > 0) allocator.free(self.skills);
        if (self.context_files.len > 0) allocator.free(self.context_files);
        self.* = init();
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    /// Read each path in `paths`, concatenate with a newline separator,
    /// and return the result as a single owned slice.
    fn readAndJoin(allocator: std.mem.Allocator, paths: []const []const u8) ![]const u8 {
        if (paths.len == 0) return allocator.dupe(u8, "");

        var buf: std.ArrayList(u8) = .empty;
        defer buf.deinit(allocator);

        for (paths, 0..) |path, i| {
            const content = std.fs.cwd().readFileAlloc(
                allocator,
                path,
                std.math.maxInt(usize),
            ) catch |err| switch (err) {
                error.FileNotFound => continue,
                else => return err,
            };
            defer allocator.free(content);

            if (i > 0 and buf.items.len > 0) {
                try buf.append(allocator, '\n');
            }
            try buf.appendSlice(allocator, content);
        }

        return buf.toOwnedSlice(allocator);
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "init produces empty snapshot" {
    var snap = FrozenSnapshot.init();
    defer snap.deinit(testing.allocator);

    try testing.expectEqualStrings("", snap.memory);
    try testing.expectEqualStrings("", snap.skills);
    try testing.expectEqualStrings("", snap.context_files);
}

test "formatForSystemPrompt empty snapshot produces empty string" {
    var snap = FrozenSnapshot.init();
    defer snap.deinit(testing.allocator);

    const prompt = try snap.formatForSystemPrompt(testing.allocator);
    defer testing.allocator.free(prompt);

    try testing.expectEqualStrings("", prompt);
}

test "load reads file content" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.writeFile(.{ .sub_path = "ctx.txt", .data = "hello context" });

    const dir_path = try tmp.dir.realpathAlloc(testing.allocator, ".");
    defer testing.allocator.free(dir_path);

    const file_path = try std.fs.path.join(testing.allocator, &.{ dir_path, "ctx.txt" });
    defer testing.allocator.free(file_path);

    const paths = [_][]const u8{file_path};
    var snap = try FrozenSnapshot.load(testing.allocator, &paths);
    defer snap.deinit(testing.allocator);

    try testing.expectEqualStrings("hello context", snap.context_files);
}

test "load skips missing files silently" {
    const paths = [_][]const u8{"/nonexistent/path/that/does/not/exist.txt"};
    var snap = try FrozenSnapshot.load(testing.allocator, &paths);
    defer snap.deinit(testing.allocator);

    try testing.expectEqualStrings("", snap.context_files);
}

test "load concatenates multiple files" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.writeFile(.{ .sub_path = "a.txt", .data = "file_a" });
    try tmp.dir.writeFile(.{ .sub_path = "b.txt", .data = "file_b" });

    const dir_path = try tmp.dir.realpathAlloc(testing.allocator, ".");
    defer testing.allocator.free(dir_path);

    const path_a = try std.fs.path.join(testing.allocator, &.{ dir_path, "a.txt" });
    defer testing.allocator.free(path_a);
    const path_b = try std.fs.path.join(testing.allocator, &.{ dir_path, "b.txt" });
    defer testing.allocator.free(path_b);

    const paths = [_][]const u8{ path_a, path_b };
    var snap = try FrozenSnapshot.load(testing.allocator, &paths);
    defer snap.deinit(testing.allocator);

    try testing.expect(std.mem.indexOf(u8, snap.context_files, "file_a") != null);
    try testing.expect(std.mem.indexOf(u8, snap.context_files, "file_b") != null);
}

test "formatForSystemPrompt includes section headers" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.writeFile(.{ .sub_path = "mem.txt", .data = "memory content" });
    try tmp.dir.writeFile(.{ .sub_path = "skill.txt", .data = "skill content" });
    try tmp.dir.writeFile(.{ .sub_path = "ctx.txt", .data = "context content" });

    const dir_path = try tmp.dir.realpathAlloc(testing.allocator, ".");
    defer testing.allocator.free(dir_path);

    const mem_path = try std.fs.path.join(testing.allocator, &.{ dir_path, "mem.txt" });
    defer testing.allocator.free(mem_path);
    const skill_path = try std.fs.path.join(testing.allocator, &.{ dir_path, "skill.txt" });
    defer testing.allocator.free(skill_path);
    const ctx_path = try std.fs.path.join(testing.allocator, &.{ dir_path, "ctx.txt" });
    defer testing.allocator.free(ctx_path);

    var snap = try FrozenSnapshot.loadSections(
        testing.allocator,
        &[_][]const u8{mem_path},
        &[_][]const u8{skill_path},
        &[_][]const u8{ctx_path},
    );
    defer snap.deinit(testing.allocator);

    const prompt = try snap.formatForSystemPrompt(testing.allocator);
    defer testing.allocator.free(prompt);

    try testing.expect(std.mem.indexOf(u8, prompt, "## Memory") != null);
    try testing.expect(std.mem.indexOf(u8, prompt, "memory content") != null);
    try testing.expect(std.mem.indexOf(u8, prompt, "## Skills") != null);
    try testing.expect(std.mem.indexOf(u8, prompt, "skill content") != null);
    try testing.expect(std.mem.indexOf(u8, prompt, "## Context") != null);
    try testing.expect(std.mem.indexOf(u8, prompt, "context content") != null);
}

test "loadSections with empty path lists" {
    var snap = try FrozenSnapshot.loadSections(
        testing.allocator,
        &[_][]const u8{},
        &[_][]const u8{},
        &[_][]const u8{},
    );
    defer snap.deinit(testing.allocator);

    const prompt = try snap.formatForSystemPrompt(testing.allocator);
    defer testing.allocator.free(prompt);

    try testing.expectEqualStrings("", prompt);
}

