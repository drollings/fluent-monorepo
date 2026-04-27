//! Gitignore-aware file filtering for guidance scanner.
const std = @import("std");
const common = @import("common");

pub const GitignoreFilter = struct {
    allocator: std.mem.Allocator,
    patterns: [][]const u8,
    negations: [][]const u8,
    project_root: []const u8,
    always_exclude: []const []const u8,
    tracked_files: std.StringHashMapUnmanaged(void),

    pub fn init(allocator: std.mem.Allocator, project_root: []const u8) GitignoreFilter {
        return .{
            .allocator = allocator,
            .patterns = &.{},
            .negations = &.{},
            .project_root = project_root,
            .always_exclude = &.{ ".git", ".zig-cache", "zig-out" },
            .tracked_files = .{},
        };
    }

    pub fn deinit(self: *GitignoreFilter) void {
        for (self.patterns) |p| self.allocator.free(p);
        for (self.negations) |p| self.allocator.free(p);
        self.allocator.free(self.patterns);
        self.allocator.free(self.negations);
        var it = self.tracked_files.iterator();
        while (it.next()) |entry| {
            self.allocator.free(entry.key_ptr.*);
        }
        self.tracked_files.deinit(self.allocator);
    }

    pub fn loadFromFile(self: *GitignoreFilter, path: []const u8) !void {
        const content = common.readFileAlloc(self.allocator, path, 512 * 1024) orelse return;
        defer self.allocator.free(content);

        var patterns_list: std.ArrayList([]const u8) = .empty;
        var negations_list: std.ArrayList([]const u8) = .empty;
        errdefer {
            for (patterns_list.items) |p| self.allocator.free(p);
            patterns_list.deinit(self.allocator);
            for (negations_list.items) |p| self.allocator.free(p);
            negations_list.deinit(self.allocator);
        }

        var line_it = std.mem.splitScalar(u8, content, '\n');
        while (line_it.next()) |line| {
            const trimmed = std.mem.trim(u8, line, " \t\r");
            if (trimmed.len == 0 or trimmed[0] == '#') continue;

            if (trimmed[0] == '!') {
                try negations_list.append(self.allocator, try self.allocator.dupe(u8, trimmed[1..]));
            } else {
                try patterns_list.append(self.allocator, try self.allocator.dupe(u8, trimmed));
            }
        }

        // Free any previously loaded patterns before replacing.
        for (self.patterns) |p| self.allocator.free(p);
        self.allocator.free(self.patterns);
        for (self.negations) |p| self.allocator.free(p);
        self.allocator.free(self.negations);

        self.patterns = try patterns_list.toOwnedSlice(self.allocator);
        self.negations = try negations_list.toOwnedSlice(self.allocator);
    }

    /// Load tracked files from git index by running `git ls-files`.
    /// Must be called before isTracked() can return meaningful results.
    /// Returns true on success, false if git command fails or not a git repo.
    pub fn loadTrackedFiles(self: *GitignoreFilter) !void {
        const io = std.Io.Threaded.global_single_threaded.io();
        const result = std.process.run(self.allocator, io, .{
            .argv = &.{ "git", "ls-files", "--cached" },
        }) catch return error.GitCommandFailed;
        defer {
            self.allocator.free(result.stdout);
            self.allocator.free(result.stderr);
        }
        switch (result.term) {
            .exited => |code| if (code != 0) return error.GitCommandFailed,
            else => return error.GitCommandFailed,
        }

        var line_it = std.mem.splitScalar(u8, result.stdout, '\n');
        while (line_it.next()) |line| {
            const trimmed = std.mem.trim(u8, line, " \t\r");
            if (trimmed.len == 0) continue;

            const gop = try self.tracked_files.getOrPut(self.allocator, trimmed);
            if (!gop.found_existing) {
                gop.key_ptr.* = try self.allocator.dupe(u8, trimmed);
            }
        }
    }

    /// Returns true if the file is tracked in git (i.e., committed to the repo).
    /// Must call loadTrackedFiles() first.
    pub fn isTracked(self: *const GitignoreFilter, abs_path: []const u8) bool {
        const rel_path = common.stripPathPrefix(abs_path, self.project_root);
        // Handle leading slash in rel_path
        const path = if (rel_path.len > 0 and rel_path[0] == '/') rel_path[1..] else rel_path;
        return self.tracked_files.contains(path);
    }

    pub fn shouldIgnore(self: *const GitignoreFilter, filepath: []const u8) bool {
        const rel_path = common.stripPathPrefix(filepath, self.project_root);

        for (self.always_exclude) |exclude| {
            if (std.mem.indexOf(u8, rel_path, exclude) != null) {
                return true;
            }
        }

        var is_ignored = false;
        for (self.patterns) |pattern| {
            if (matchesPattern(rel_path, pattern)) {
                is_ignored = true;
                break;
            }
        }

        if (is_ignored) {
            for (self.negations) |pattern| {
                if (matchesPattern(rel_path, pattern)) {
                    is_ignored = false;
                    break;
                }
            }
        }

        return is_ignored;
    }
};

/// Checks if a given byte slice matches a specified pattern, returning true or false.
fn matchesPattern(path: []const u8, pattern: []const u8) bool {
    if (pattern.len == 0) return false;

    const is_dir_pattern = pattern[pattern.len - 1] == '/';
    const pat = if (is_dir_pattern) pattern[0 .. pattern.len - 1] else pattern;

    if (std.mem.indexOf(u8, pat, "**")) |doublestar| {
        const prefix = pat[0..doublestar];
        const suffix = pat[doublestar + 2 ..];

        if (prefix.len > 0 and !std.mem.startsWith(u8, path, prefix)) return false;
        if (suffix.len > 0 and !std.mem.endsWith(u8, path, suffix)) return false;
        return true;
    }

    if (std.mem.startsWith(u8, pat, "/") or std.mem.indexOf(u8, pat, "/") != null) {
        const clean_pat = if (pat[0] == '/') pat[1..] else pat;
        return globMatch(path, clean_pat);
    }

    if (std.mem.indexOf(u8, path, "/")) |slash| {
        const filename = path[slash + 1 ..];
        if (globMatch(filename, pat)) return true;
    }

    return globMatch(path, pat);
}

/// Checks if a pattern exists within a given text slice, returning true or false.
fn globMatch(text: []const u8, pattern: []const u8) bool {
    var ti: usize = 0;
    var pi: usize = 0;
    var star_idx: ?usize = null;
    var match_idx: usize = 0;

    while (ti < text.len) {
        if (pi < pattern.len and (pattern[pi] == text[ti] or pattern[pi] == '?')) {
            ti += 1;
            pi += 1;
        } else if (pi < pattern.len and pattern[pi] == '*') {
            star_idx = pi;
            match_idx = ti;
            pi += 1;
        } else if (star_idx) |si| {
            pi = si + 1;
            match_idx += 1;
            ti = match_idx;
        } else {
            return false;
        }
    }

    while (pi < pattern.len and pattern[pi] == '*') {
        pi += 1;
    }

    return pi == pattern.len;
}

test "globMatch basic patterns" {
    try std.testing.expect(globMatch("foo.zig", "*.zig"));
    try std.testing.expect(globMatch("src/foo.zig", "*.zig"));
    try std.testing.expect(!globMatch("foo.zig", "*.py"));
    try std.testing.expect(globMatch("test_foo.zig", "test_*.zig"));
    try std.testing.expect(globMatch("foo", "foo"));
}
