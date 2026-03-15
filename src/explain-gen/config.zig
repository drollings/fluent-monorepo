/// explain-gen project configuration loader.
///
/// Resolves paths for the explain-gen system using a two-level fallback chain:
///   1. {cwd}/{guidance_dir}/{config_filename}  (project-local)
///   2. ~/.config/explain-gen/{config_filename}  (user global)
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
pub const DEFAULT_BASE_URL = "http://localhost:11434";
pub const DEFAULT_CHAT_ENDPOINT = "/api/chat";
pub const DEFAULT_API_URL = DEFAULT_BASE_URL ++ DEFAULT_CHAT_ENDPOINT;
pub const CONFIG_FILENAME = "explain-gen-config.json";

/// Per-extension test command. `{file}` is substituted for the extension "*".
pub const LintCommand = struct {
    extension: []const u8,
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

    /// Relative path to the explain-gen root (e.g. .explain-gen).
    guidance_dir: []const u8,

    /// Absolute path to the explain-gen root (e.g. /project/.explain-gen).
    guidance_root: []const u8,

    /// Absolute path to the guidance JSON source tree
    /// ({guidance_root}/src/<path>.json).
    json_base: []const u8,

    /// Absolute path to the skills directory ({guidance_root}/.skills).
    skills_dir: []const u8,

    /// Absolute path to the inbox directory ({guidance_root}/.doc/inbox).
    inbox_dir: []const u8,

    /// Relative path to the database file from workspace root.
    db_path: []const u8,

    /// Source directories to search, relative to the project root.
    src_dirs: []const []const u8,

    /// LLM API endpoint for infill/regen (from config or default).
    api_url: []const u8,

    /// Model name for infill/regen (from config or default).
    model: []const u8,

    /// Per-extension test commands. `{file}` is NOT substituted (whole-suite).
    /// Extension "*" is the fallback for languages without a specific command.
    test_commands: []const LintCommand,

    /// Per-extension lint commands. Run before formatting. May be empty.
    lint_commands: []const LintCommand,

    /// Per-extension format commands. Run after lint, before guidance.
    fmt_commands: []const LintCommand,

    pub fn deinit(self: *ProjectConfig) void {
        self.allocator.free(self.guidance_dir);
        self.allocator.free(self.guidance_root);
        self.allocator.free(self.json_base);
        self.allocator.free(self.skills_dir);
        self.allocator.free(self.inbox_dir);
        self.allocator.free(self.db_path);
        for (self.src_dirs) |d| self.allocator.free(d);
        self.allocator.free(self.src_dirs);
        self.allocator.free(self.api_url);
        self.allocator.free(self.model);
        for (self.test_commands) |tc| tc.deinit(self.allocator);
        self.allocator.free(self.test_commands);
        for (self.lint_commands) |lc| lc.deinit(self.allocator);
        self.allocator.free(self.lint_commands);
        for (self.fmt_commands) |fc| fc.deinit(self.allocator);
        self.allocator.free(self.fmt_commands);
    }

    /// Return the test argv template for `ext`, or the fallback "*" command.
    pub fn testCommandForExt(self: *const ProjectConfig, ext: []const u8) ?[]const []const u8 {
        for (self.test_commands) |tc| {
            if (std.mem.eql(u8, tc.extension, ext)) return tc.argv;
        }
        for (self.test_commands) |tc| {
            if (std.mem.eql(u8, tc.extension, "*")) return tc.argv;
        }
        return null;
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

/// Options for initConfig.
pub const InitOptions = struct {
    guidance_dir: ?[]const u8 = null,
    db_path: ?[]const u8 = null,
};

/// Create a default configuration file at `{cwd}/{guidance_dir}/{CONFIG_FILENAME}`.
/// Returns true when a new file was created, false if one already exists.
pub fn initConfig(allocator: std.mem.Allocator, cwd: []const u8, options: InitOptions) !bool {
    const guidance_dir = options.guidance_dir orelse DEFAULT_GUIDANCE_DIR;
    const db_path = options.db_path orelse DEFAULT_DB_PATH;

    const dir_path = try std.fs.path.join(allocator, &.{ cwd, guidance_dir });
    defer allocator.free(dir_path);

    std.fs.makeDirAbsolute(dir_path) catch |err| {
        if (err != error.PathAlreadyExists) return err;
    };

    const config_path = try std.fs.path.join(allocator, &.{ dir_path, CONFIG_FILENAME });
    defer allocator.free(config_path);

    // Check if config already exists.
    std.fs.accessAbsolute(config_path, .{}) catch |err| {
        if (err != error.FileNotFound) return err;
        // File doesn't exist, create it.
        const file = try std.fs.createFileAbsolute(config_path, .{});
        defer file.close();

        var buf: [64 * 1024]u8 = undefined;
        var fbs = std.io.fixedBufferStream(&buf);
        const writer = fbs.writer();

        try writer.print(
            \\{{
            \\  "version": "1",
            \\  "guidance_dir": "{s}",
            \\  "db_path": "{s}",
            \\  "src_dirs": ["src"],
            \\  "ollama": {{
            \\    "base_url": "http://localhost:11434",
            \\    "chat_endpoint": "/api/chat"
            \\  }},
            \\  "models": {{
            \\    "default": "code:latest",
            \\    "infill": "code:latest"
            \\  }},
            \\  "test_commands": {{
            \\    ".zig": ["zig", "build", "test", "--summary", "all"],
            \\    ".py": ["python", "-m", "pytest", "-v"]
            \\  }},
            \\  "lint_commands": {{
            \\    ".zig": ["zig", "fmt", "--check", "{{file}}"],
            \\    ".py": ["ruff", "check", "{{file}}"]
            \\  }},
            \\  "fmt_commands": {{
            \\    ".zig": ["zig", "fmt", "{{file}}"],
            \\    ".py": ["ruff", "format", "{{file}}"]
            \\  }}
            \\}}
            \\
        , .{ guidance_dir, db_path });

        try file.writeAll(fbs.getWritten());
        std.debug.print("Created {s}\n", .{config_path});
        return true;
    };

    std.debug.print("Config already exists at {s}\n", .{config_path});
    return false;
}

/// Generate AGENTS.md content for explain-gen integration.
/// Returns an owned slice that the caller must free.
pub fn generateAgentsMdContent(allocator: std.mem.Allocator, guidance_dir: []const u8) ![]const u8 {
    var buf: std.ArrayList(u8) = .{};
    errdefer buf.deinit(allocator);
    const w = buf.writer(allocator);

    try w.writeAll(
        \\# explain-gen Integration
        \\
        \\This project uses explain-gen for AST-guided code navigation and documentation.
        \\
        \\## Quick Start
        \\
        \\```
        \\# Initialize explain-gen configuration
        \\explain-gen init
        \\
        \\# Run the full RALPH loop (build → test → lint → fmt → guidance)
        \\explain-gen check
        \\
        \\# Query the codebase
        \\explain-gen explain "how does X work?"
        \\```
        \\
        \\## Key Files
        \\
    );
    try w.print("- `{s}/explain-gen-config.json` — Model and provider configuration\n", .{guidance_dir});
    try w.print("- `{s}/src/` — Generated guidance JSON files\n", .{guidance_dir});
    try w.writeAll(
        \\- `.explain.db` — SQLite FTS5 database for fast searches
        \\- `STRUCTURE.md` — Project structure documentation (auto-generated)
        \\
        \\## RALPH Loop
        \\
        \\The recommended workflow is:
        \\
        \\1. **DISCOVER**: `explain-gen explain "query"` — Search codebase
        \\2. **UNDERSTAND**: Read source files identified by the query
        \\3. **DECIDE**: Apply relevant skills/patterns
        \\4. **IMPLEMENT**: Make changes
        \\5. **VERIFY**: `explain-gen check` — Run tests, lint, format, guidance
        \\
        \\## Skills
        \\
    );
    try w.print("Skills are stored in `{s}/.skills/<skill>/SKILL.md`. Reference them in source files:\n", .{guidance_dir});
    try w.writeAll(
        \\```zig
        \\// file.zig  # [skill-name, another-skill] Description
        \\```
        \\
    );

    return try buf.toOwnedSlice(allocator);
}

/// AGENTS.md insertion template for existing projects.
pub const AGENTS_INSERTION: []const u8 =
    \\---
    \\
    \\## explain-gen Integration
    \\
    \\This project uses explain-gen for AST-guided code navigation.
    \\
    \\```
    \\# Initialize and run
    \\explain-gen init
    \\explain-gen check
    \\```
    \\
;

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Parse a commands object (lint_commands, fmt_commands, test_commands) into LintCommand array.
fn parseCommandsObject(allocator: std.mem.Allocator, obj: std.json.ObjectMap) ![]LintCommand {
    var commands: std.ArrayList(LintCommand) = .{};
    errdefer {
        for (commands.items) |c| c.deinit(allocator);
        commands.deinit(allocator);
    }
    var it = obj.iterator();
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
        try commands.append(allocator, LintCommand{
            .extension = try allocator.dupe(u8, ext_str),
            .argv = try argv.toOwnedSlice(allocator),
        });
    }
    return try commands.toOwnedSlice(allocator);
}

fn tryLoadFile(allocator: std.mem.Allocator, cwd: []const u8, path: []const u8) !ProjectConfig {
    const file = try std.fs.openFileAbsolute(path, .{});
    defer file.close();

    const content = try file.readToEndAlloc(allocator, 64 * 1024);
    defer allocator.free(content);

    var parsed = try std.json.parseFromSlice(std.json.Value, allocator, content, .{});
    defer parsed.deinit();

    const root = parsed.value;
    if (root != .object) return error.InvalidConfig;

    const guidance_dir_rel: []const u8 = if (root.object.get("guidance_dir")) |gd|
        if (gd == .string) gd.string else DEFAULT_GUIDANCE_DIR
    else
        DEFAULT_GUIDANCE_DIR;

    const db_path_rel: []const u8 = if (root.object.get("db_path")) |dp|
        if (dp == .string) dp.string else DEFAULT_DB_PATH
    else
        DEFAULT_DB_PATH;

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

    const model = blk: {
        if (root.object.get("models")) |models| {
            if (models == .object) {
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
                const ep = if (ollama.object.get("chat_endpoint")) |e| if (e == .string) e.string else "" else "";
                if (base.len > 0 and ep.len > 0) {
                    break :blk try std.fmt.allocPrint(allocator, "{s}{s}", .{ base, ep });
                }
            }
        }
        break :blk try allocator.dupe(u8, DEFAULT_BASE_URL ++ DEFAULT_CHAT_ENDPOINT);
    };
    errdefer allocator.free(api_url);

    // test_commands (object: ext → [argv...], optional)
    var test_commands: []LintCommand = &.{};
    if (root.object.get("test_commands")) |tc_val| {
        if (tc_val == .object) {
            test_commands = try parseCommandsObject(allocator, tc_val.object);
        }
    }
    errdefer {
        for (test_commands) |tc| tc.deinit(allocator);
        allocator.free(test_commands);
    }

    // lint_commands (object: ext → [argv...], optional)
    var lint_commands: []LintCommand = &.{};
    if (root.object.get("lint_commands")) |lc_val| {
        if (lc_val == .object) {
            lint_commands = try parseCommandsObject(allocator, lc_val.object);
        }
    }
    errdefer {
        for (lint_commands) |lc| lc.deinit(allocator);
        allocator.free(lint_commands);
    }

    // fmt_commands (object: ext → [argv...], optional)
    var fmt_commands: []LintCommand = &.{};
    if (root.object.get("fmt_commands")) |fc_val| {
        if (fc_val == .object) {
            fmt_commands = try parseCommandsObject(allocator, fc_val.object);
        }
    }

    return buildFromParts(
        allocator,
        cwd,
        guidance_dir_rel,
        db_path_rel,
        try src_dirs.toOwnedSlice(allocator),
        try allocator.dupe(u8, model),
        api_url,
        test_commands,
        lint_commands,
        fmt_commands,
    );
}

fn buildDefault(allocator: std.mem.Allocator, cwd: []const u8) !ProjectConfig {
    var src_dirs = try allocator.alloc([]const u8, 1);
    src_dirs[0] = try allocator.dupe(u8, DEFAULT_SRC_DIR);

    // Build test_commands for .zig
    const zig_test_argv = [_][]const u8{ "zig", "build", "test", "--summary", "all" };
    var test_argv = try allocator.alloc([]const u8, zig_test_argv.len);
    for (zig_test_argv, 0..) |tok, i| {
        test_argv[i] = try allocator.dupe(u8, tok);
    }
    var test_commands = try allocator.alloc(LintCommand, 1);
    test_commands[0] = LintCommand{
        .extension = try allocator.dupe(u8, ".zig"),
        .argv = test_argv,
    };

    return buildFromParts(
        allocator,
        cwd,
        DEFAULT_GUIDANCE_DIR,
        DEFAULT_DB_PATH,
        src_dirs,
        try allocator.dupe(u8, DEFAULT_MODEL),
        try allocator.dupe(u8, DEFAULT_API_URL),
        test_commands,
        &.{},
        &.{},
    );
}

fn buildFromParts(
    allocator: std.mem.Allocator,
    cwd: []const u8,
    guidance_dir_rel: []const u8,
    db_path_rel: []const u8,
    src_dirs: []const []const u8,
    model: []const u8,
    api_url: []const u8,
    test_commands: []const LintCommand,
    lint_commands: []const LintCommand,
    fmt_commands: []const LintCommand,
) !ProjectConfig {
    const guidance_dir = try allocator.dupe(u8, guidance_dir_rel);
    errdefer allocator.free(guidance_dir);

    const guidance_root = if (std.fs.path.isAbsolute(guidance_dir_rel))
        try allocator.dupe(u8, guidance_dir_rel)
    else
        try std.fs.path.join(allocator, &.{ cwd, guidance_dir_rel });
    errdefer allocator.free(guidance_root);

    const json_base = try allocator.dupe(u8, guidance_root);
    errdefer allocator.free(json_base);

    const skills_dir = try std.fs.path.join(allocator, &.{ guidance_root, ".skills" });
    errdefer allocator.free(skills_dir);

    const inbox_dir = try std.fs.path.join(allocator, &.{ guidance_root, ".doc", "inbox" });
    errdefer allocator.free(inbox_dir);

    const db_path = try allocator.dupe(u8, db_path_rel);
    errdefer allocator.free(db_path);

    return .{
        .allocator = allocator,
        .guidance_dir = guidance_dir,
        .guidance_root = guidance_root,
        .json_base = json_base,
        .skills_dir = skills_dir,
        .inbox_dir = inbox_dir,
        .db_path = db_path,
        .src_dirs = src_dirs,
        .api_url = api_url,
        .model = model,
        .test_commands = test_commands,
        .lint_commands = lint_commands,
        .fmt_commands = fmt_commands,
    };
}
