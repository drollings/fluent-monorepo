/// explain-gen project configuration loader.
///
/// Resolves paths for the explain-gen system using a two-level fallback chain:
///   1. {cwd}/.explain-gen/explain-gen-config.json  (project-local)
///   2. ~/.config/explain-gen/explain-gen-config.json  (user global)
///   3. Built-in defaults
///
/// All path fields in ProjectConfig are pre-computed absolute paths so callers
/// do not need to allocate to derive them.
const std = @import("std");
const builtin = @import("builtin");

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

pub const DEFAULT_GUIDANCE_DIR = ".explain-gen";
pub const DEFAULT_SRC_DIR = "src";
pub const DEFAULT_DB_PATH = ".explain.db";
pub const DEFAULT_MODEL = "code:latest";
pub const DEFAULT_API_URL = "http://localhost:11434/api/chat";
pub const CONFIG_FILENAME = "explain-gen-config.json";

/// Default test command tokens.  Kept as a comptime slice so callers can
/// reference it without allocating when the config carries no override.
pub const DEFAULT_TEST_COMMAND = [_][]const u8{ "zig", "build", "test", "--summary", "all" };

/// A single lint-command entry: one file extension → shell-style argv template.
/// The template may contain the literal substring `{file}` which is replaced
/// with the source file path at invocation time.
pub const LintCommand = struct {
    /// File extension including the leading dot (e.g. ".zig").
    extension: []const u8,
    /// argv tokens; `{file}` is substituted with the source path.
    argv: []const []const u8,

    pub fn deinit(self: LintCommand, allocator: std.mem.Allocator) void {
        allocator.free(self.extension);
        for (self.argv) |a| allocator.free(a);
        allocator.free(self.argv);
    }
};

// ---------------------------------------------------------------------------
// ProjectConfig
// ---------------------------------------------------------------------------

/// Resolved, absolute paths for a single explain-gen project instance.
/// All strings are owned by this struct; call deinit() to free them.
pub const ProjectConfig = struct {
    allocator: std.mem.Allocator,

    /// Absolute path to the explain-gen root (e.g. /project/.explain-gen).
    guidance_root: []const u8,

    /// Absolute path to the guidance JSON source tree
    /// ({guidance_root}/src/<path>.json).
    /// This field stores guidance_root; callers append the rel path.
    json_base: []const u8,

    /// Absolute path to the skills directory ({guidance_root}/.skills).
    skills_dir: []const u8,

    /// Absolute path to the inbox directory ({guidance_root}/.doc/inbox).
    inbox_dir: []const u8,

    /// Source directories to search, relative to the project root.
    /// Defaults to ["src"]; read from config JSON "src_dirs" array.
    src_dirs: []const []const u8,

    /// LLM API endpoint for infill/regen (from config or default).
    api_url: []const u8,

    /// Model name for infill/regen (from config or default).
    model: []const u8,

    /// Command to run the project test suite.
    /// Tokens are joined by spaces; {file} is NOT substituted (test runs the
    /// whole suite).  Defaults to DEFAULT_TEST_COMMAND.
    /// Owned slice of owned strings.
    test_command: []const []const u8,

    /// Per-extension lint commands.  Run before formatting.  May be empty.
    /// Owned slice of owned LintCommand values.
    lint_commands: []const LintCommand,

    /// Per-extension format commands.  Run after lint, before guidance.
    /// Formatting may shift line numbers, so it must precede AST parsing.
    /// Owned slice of owned LintCommand values (same structure).
    fmt_commands: []const LintCommand,

    pub fn deinit(self: *ProjectConfig) void {
        self.allocator.free(self.guidance_root);
        self.allocator.free(self.json_base);
        self.allocator.free(self.skills_dir);
        self.allocator.free(self.inbox_dir);
        for (self.src_dirs) |d| self.allocator.free(d);
        self.allocator.free(self.src_dirs);
        self.allocator.free(self.api_url);
        self.allocator.free(self.model);
        for (self.test_command) |t| self.allocator.free(t);
        self.allocator.free(self.test_command);
        for (self.lint_commands) |lc| lc.deinit(self.allocator);
        self.allocator.free(self.lint_commands);
        for (self.fmt_commands) |fc| fc.deinit(self.allocator);
        self.allocator.free(self.fmt_commands);
    }

    /// Return the lint argv template for `ext`, or null if none is configured.
    pub fn lintCommandForExt(self: *const ProjectConfig, ext: []const u8) ?[]const []const u8 {
        for (self.lint_commands) |lc| {
            if (std.mem.eql(u8, lc.extension, ext)) return lc.argv;
        }
        return null;
    }

    /// Return the format argv template for `ext`, or null if none is configured.
    pub fn fmtCommandForExt(self: *const ProjectConfig, ext: []const u8) ?[]const []const u8 {
        for (self.fmt_commands) |fc| {
            if (std.mem.eql(u8, fc.extension, ext)) return fc.argv;
        }
        return null;
    }
};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Load project configuration with the two-level fallback chain.
/// On success the caller owns the returned ProjectConfig and must call deinit().
pub fn loadConfig(allocator: std.mem.Allocator, cwd: []const u8) !ProjectConfig {
    // 1. Project-local config.
    {
        const path = try std.fs.path.join(allocator, &.{ cwd, DEFAULT_GUIDANCE_DIR, CONFIG_FILENAME });
        defer allocator.free(path);
        if (tryLoadFile(allocator, cwd, path)) |cfg| return cfg else |err| {
            if (err != error.FileNotFound and !builtin.is_test) {
                std.debug.print("warning: config file {s} is invalid ({}) — using defaults\n", .{ path, err });
            }
        }
    }

    // 2. User-global config (~/.config/explain-gen/explain-gen-config.json).
    if (std.process.getEnvVarOwned(allocator, "HOME") catch null) |home| {
        defer allocator.free(home);
        const path = try std.fs.path.join(allocator, &.{ home, ".config", "explain-gen", CONFIG_FILENAME });
        defer allocator.free(path);
        if (tryLoadFile(allocator, cwd, path)) |cfg| return cfg else |err| {
            if (err != error.FileNotFound and !builtin.is_test) {
                std.debug.print("warning: config file {s} is invalid ({}) — using defaults\n", .{ path, err });
            }
        }
    }

    // 3. Built-in defaults.
    return buildDefault(allocator, cwd);
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Attempt to load and parse a config file.  Returns error on any failure so
/// the caller can silently fall through to the next source.
fn tryLoadFile(allocator: std.mem.Allocator, cwd: []const u8, path: []const u8) !ProjectConfig {
    const file = try std.fs.openFileAbsolute(path, .{});
    defer file.close();

    const content = try file.readToEndAlloc(allocator, 64 * 1024);
    defer allocator.free(content);

    var parsed = try std.json.parseFromSlice(std.json.Value, allocator, content, .{});
    defer parsed.deinit();

    const root = parsed.value;
    if (root != .object) return error.InvalidConfig;

    // --- guidance_dir (relative string, optional) ---
    const guidance_dir_rel: []const u8 = if (root.object.get("guidance_dir")) |gd|
        if (gd == .string) gd.string else DEFAULT_GUIDANCE_DIR
    else
        DEFAULT_GUIDANCE_DIR;

    // --- src_dirs (array of strings, optional) ---
    var src_dirs: std.ArrayList([]const u8) = .{};
    errdefer {
        for (src_dirs.items) |d| allocator.free(d);
        src_dirs.deinit(allocator);
    }
    if (root.object.get("src_dirs")) |sd| {
        if (sd == .array) {
            for (sd.array.items) |item| {
                if (item == .string) {
                    try src_dirs.append(allocator, try allocator.dupe(u8, item.string));
                }
            }
        }
    }
    if (src_dirs.items.len == 0) {
        try src_dirs.append(allocator, try allocator.dupe(u8, DEFAULT_SRC_DIR));
    }

    // --- LLM settings ---
    const model = blk: {
        if (root.object.get("models")) |models| {
            if (models == .object) {
                // Prefer "infill" model; fall back to "default".
                if (models.object.get("infill")) |m| if (m == .string) break :blk m.string;
                if (models.object.get("default")) |m| if (m == .string) break :blk m.string;
            }
        }
        break :blk DEFAULT_MODEL;
    };

    const api_url = blk: {
        if (root.object.get("ollama")) |ollama| {
            if (ollama == .object) {
                const base = if (ollama.object.get("base_url")) |u| if (u == .string) u.string else "" else "";
                const ep = if (ollama.object.get("chat_endpoint")) |e| if (e == .string) e.string else DEFAULT_API_URL else DEFAULT_API_URL;
                if (base.len > 0 and ep.len > 0) {
                    break :blk try std.fmt.allocPrint(allocator, "{s}{s}", .{ base, ep });
                }
            }
        }
        break :blk try allocator.dupe(u8, DEFAULT_API_URL);
    };
    errdefer allocator.free(api_url);

    // --- test_command (array of strings, optional) ---
    var test_command: std.ArrayList([]const u8) = .{};
    errdefer {
        for (test_command.items) |t| allocator.free(t);
        test_command.deinit(allocator);
    }
    if (root.object.get("test_command")) |tc| {
        if (tc == .array) {
            for (tc.array.items) |item| {
                if (item == .string) {
                    try test_command.append(allocator, try allocator.dupe(u8, item.string));
                }
            }
        }
    }
    if (test_command.items.len == 0) {
        // Default: zig build test --summary all
        for (&DEFAULT_TEST_COMMAND) |tok| {
            try test_command.append(allocator, try allocator.dupe(u8, tok));
        }
    }

    // --- lint_commands (object: ext → [argv...], optional) ---
    var lint_commands: std.ArrayList(LintCommand) = .{};
    errdefer {
        for (lint_commands.items) |lc| lc.deinit(allocator);
        lint_commands.deinit(allocator);
    }
    if (root.object.get("lint_commands")) |lc_val| {
        if (lc_val == .object) {
            var it = lc_val.object.iterator();
            while (it.next()) |entry| {
                const ext_str = entry.key_ptr.*;
                const argv_val = entry.value_ptr.*;
                if (argv_val != .array) continue;
                var argv: std.ArrayList([]const u8) = .{};
                errdefer {
                    for (argv.items) |a| allocator.free(a);
                    argv.deinit(allocator);
                }
                for (argv_val.array.items) |tok| {
                    if (tok == .string) {
                        try argv.append(allocator, try allocator.dupe(u8, tok.string));
                    }
                }
                if (argv.items.len == 0) {
                    argv.deinit(allocator);
                    continue;
                }
                try lint_commands.append(allocator, LintCommand{
                    .extension = try allocator.dupe(u8, ext_str),
                    .argv = try argv.toOwnedSlice(allocator),
                });
            }
        }
    }

    // --- fmt_commands (object: ext → [argv...], optional) ---
    var fmt_commands: std.ArrayList(LintCommand) = .{};
    errdefer {
        for (fmt_commands.items) |fc| fc.deinit(allocator);
        fmt_commands.deinit(allocator);
    }
    if (root.object.get("fmt_commands")) |fc_val| {
        if (fc_val == .object) {
            var it = fc_val.object.iterator();
            while (it.next()) |entry| {
                const ext_str = entry.key_ptr.*;
                const argv_val = entry.value_ptr.*;
                if (argv_val != .array) continue;
                var argv: std.ArrayList([]const u8) = .{};
                errdefer {
                    for (argv.items) |a| allocator.free(a);
                    argv.deinit(allocator);
                }
                for (argv_val.array.items) |tok| {
                    if (tok == .string) {
                        try argv.append(allocator, try allocator.dupe(u8, tok.string));
                    }
                }
                if (argv.items.len == 0) {
                    argv.deinit(allocator);
                    continue;
                }
                try fmt_commands.append(allocator, LintCommand{
                    .extension = try allocator.dupe(u8, ext_str),
                    .argv = try argv.toOwnedSlice(allocator),
                });
            }
        }
    }

    return buildFromParts(
        allocator,
        cwd,
        guidance_dir_rel,
        try src_dirs.toOwnedSlice(allocator),
        try allocator.dupe(u8, model),
        api_url,
        try test_command.toOwnedSlice(allocator),
        try lint_commands.toOwnedSlice(allocator),
        try fmt_commands.toOwnedSlice(allocator),
    );
}

fn buildDefault(allocator: std.mem.Allocator, cwd: []const u8) !ProjectConfig {
    var src_dirs = try allocator.alloc([]const u8, 1);
    src_dirs[0] = try allocator.dupe(u8, DEFAULT_SRC_DIR);
    return buildFromParts(
        allocator,
        cwd,
        DEFAULT_GUIDANCE_DIR,
        src_dirs,
        try allocator.dupe(u8, DEFAULT_MODEL),
        try allocator.dupe(u8, DEFAULT_API_URL),
        try dupeDefaultTestCommand(allocator),
        &.{}, // no lint commands in built-in defaults
        &.{}, // no fmt commands in built-in defaults
    );
}

/// Duplicate the DEFAULT_TEST_COMMAND comptime slice into owned heap memory.
fn dupeDefaultTestCommand(allocator: std.mem.Allocator) ![]const []const u8 {
    const cmds = &DEFAULT_TEST_COMMAND;
    const out = try allocator.alloc([]const u8, cmds.len);
    for (cmds, 0..) |tok, i| {
        out[i] = try allocator.dupe(u8, tok);
    }
    return out;
}

fn buildFromParts(
    allocator: std.mem.Allocator,
    cwd: []const u8,
    guidance_dir_rel: []const u8,
    src_dirs: []const []const u8,
    model: []const u8,
    api_url: []const u8,
    test_command: []const []const u8,
    lint_commands: []const LintCommand,
    fmt_commands: []const LintCommand,
) !ProjectConfig {
    const guidance_root = if (std.fs.path.isAbsolute(guidance_dir_rel))
        try allocator.dupe(u8, guidance_dir_rel)
    else
        try std.fs.path.join(allocator, &.{ cwd, guidance_dir_rel });
    errdefer allocator.free(guidance_root);

    // json_base == guidance_root: callers compute {guidance_root}/{rel}.json
    const json_base = try allocator.dupe(u8, guidance_root);
    errdefer allocator.free(json_base);

    const skills_dir = try std.fs.path.join(allocator, &.{ guidance_root, ".skills" });
    errdefer allocator.free(skills_dir);

    const inbox_dir = try std.fs.path.join(allocator, &.{ guidance_root, ".doc", "inbox" });
    errdefer allocator.free(inbox_dir);

    return .{
        .allocator = allocator,
        .guidance_root = guidance_root,
        .json_base = json_base,
        .skills_dir = skills_dir,
        .inbox_dir = inbox_dir,
        .src_dirs = src_dirs,
        .api_url = api_url,
        .model = model,
        .test_command = test_command,
        .lint_commands = lint_commands,
        .fmt_commands = fmt_commands,
    };
}
