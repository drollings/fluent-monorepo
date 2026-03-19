/// guidance project configuration loader.
///
/// Resolves paths for the guidance system using a two-level fallback chain:
///   1. {cwd}/{guidance_dir}/{config_filename}  (project-local)
///   2. ~/.config/guidance/{config_filename}  (user global)
///   3. Built-in defaults
///
/// All path fields in ProjectConfig are pre-computed absolute paths so callers
/// do not need to allocate to derive them.
const std = @import("std");
const builtin = @import("builtin");

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

pub const DEFAULT_GUIDANCE_DIR = ".guidance";
pub const DEFAULT_SRC_DIR = "src";
pub const DEFAULT_DB_PATH = ".guidance.db";
pub const DEFAULT_GUIDANCE_DB_PATH = ".guidance.db";
pub const DEFAULT_CAPABILITIES_DIR = "doc/capabilities";
pub const DEFAULT_MODEL = "local:code:latest";
pub const DEFAULT_EMBEDDING_PROVIDER = "ollama";
pub const DEFAULT_EMBEDDING_MODEL = "nomic-embed-text";
pub const DEFAULT_EMBEDDING_DIMS: u32 = 768;
pub const DEFAULT_FAST_MODEL = "";
pub const DEFAULT_THINKING_MODEL = "";
pub const DEFAULT_BASE_URL = "http://localhost:11434";
pub const DEFAULT_CHAT_ENDPOINT = "/v1/chat/completions";
pub const DEFAULT_API_URL = DEFAULT_BASE_URL ++ DEFAULT_CHAT_ENDPOINT;
pub const DEFAULT_EMBEDDING_CACHE_LIMIT: u32 = 400;
pub const CONFIG_FILENAME = "guidance-config.json";

pub const Provider = struct {
    name: []const u8,
    base_url: []const u8,
    chat_endpoint: []const u8,

    pub fn deinit(self: *Provider, allocator: std.mem.Allocator) void {
        allocator.free(self.name);
        allocator.free(self.base_url);
        allocator.free(self.chat_endpoint);
    }
};

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

/// Resolved, absolute paths for a single guidance project instance.
/// All strings are owned by this struct; call deinit() to free them.
pub const ProjectConfig = struct {
    allocator: std.mem.Allocator,

    /// Relative path to the guidance root (e.g. .guidance).
    guidance_dir: []const u8,

    /// Absolute path to the guidance root (e.g. /project/.guidance).
    guidance_root: []const u8,

    /// Absolute path to the guidance JSON source tree
    /// ({guidance_root}/src/<path>.json).
    json_base: []const u8,

    /// Absolute path to the skills directory ({guidance_root}/.skills).
    skills_dir: []const u8,

    /// Absolute path to the inbox directory ({guidance_root}/.doc/inbox).
    inbox_dir: []const u8,

    /// Relative path to the guidance database file from workspace root.
    /// (Legacy field, kept for config compatibility; guidance_db_path is preferred.)
    db_path: []const u8,

    /// Relative path to the LanceDB vector search database.
    guidance_db_path: []const u8,

    /// Enable .guidance.db (LanceDB vector search). Always true; kept for
    /// backward-compatible config parsing.
    enable_guidance_db: bool,

    /// Embedding provider name: "ollama", "openai", "none", "custom:<url>".
    embedding_provider: []const u8,

    /// Embedding model name (provider-specific).
    embedding_model: []const u8,

    /// Embedding vector dimensions (0 = use provider default).
    embedding_dims: u32,

    /// Path to the capabilities tree (e.g. "doc/capabilities").
    /// Each subdirectory may contain a CAPABILITY.md file that is indexed
    /// into the LanceDB capabilities table for semantic search.
    capabilities_dir: []const u8,

    /// Source directories to search, relative to the project root.
    src_dirs: []const []const u8,

    /// Available providers (from config or defaults).
    providers: []const Provider,

    /// General-purpose model (models.default), format: "provider:modelname".
    model_default: []const u8,

    /// Fast model for comment infill (models.fast). Empty string means unset;
    /// callers fall back to model_default when this is empty.
    model_fast: []const u8,

    /// Thinking/reasoning model slot (models.thinking). Empty string means unset.
    /// is_thinking=true should be set in LlmConfig when using this slot.
    model_thinking: []const u8,

    /// Per-extension test commands. `{file}` is NOT substituted (whole-suite).
    /// Extension "*" is the fallback for languages without a specific command.
    test_commands: []const LintCommand,

    /// Per-extension lint commands. Run before formatting. May be empty.
    lint_commands: []const LintCommand,

    /// Per-extension format commands. Run after lint, before guidance.
    fmt_commands: []const LintCommand,

    /// Maximum number of entries in the embedding cache. 0 means unlimited.
    embedding_cache_limit: u32,

    pub fn deinit(self: *ProjectConfig) void {
        self.allocator.free(self.guidance_dir);
        self.allocator.free(self.guidance_root);
        self.allocator.free(self.json_base);
        self.allocator.free(self.skills_dir);
        self.allocator.free(self.inbox_dir);
        self.allocator.free(self.db_path);
        self.allocator.free(self.guidance_db_path);
        self.allocator.free(self.capabilities_dir);
        self.allocator.free(self.embedding_provider);
        self.allocator.free(self.embedding_model);
        for (self.src_dirs) |d| self.allocator.free(d);
        self.allocator.free(self.src_dirs);
        for (self.providers) |p| {
            var provider = p;
            provider.deinit(self.allocator);
        }
        self.allocator.free(self.providers);
        self.allocator.free(self.model_default);
        self.allocator.free(self.model_fast);
        self.allocator.free(self.model_thinking);
        for (self.test_commands) |tc| tc.deinit(self.allocator);
        self.allocator.free(self.test_commands);
        for (self.lint_commands) |lc| lc.deinit(self.allocator);
        self.allocator.free(self.lint_commands);
        for (self.fmt_commands) |fc| fc.deinit(self.allocator);
        self.allocator.free(self.fmt_commands);
    }

    /// Return the model to use for comment infill: fast slot if set, else default.
    pub fn infillModel(self: *const ProjectConfig) []const u8 {
        if (self.model_fast.len > 0) return self.model_fast;
        return self.model_default;
    }

    /// Return the model to use for thinking/reasoning tasks (module detail generation).
    /// Returns empty string if not configured.
    pub fn thinkingModel(self: *const ProjectConfig) []const u8 {
        return self.model_thinking;
    }

    /// Find a provider by name. Returns null if not found.
    pub fn getProvider(self: *const ProjectConfig, name: []const u8) ?Provider {
        for (self.providers) |p| {
            if (std.mem.eql(u8, p.name, name)) return p;
        }
        return null;
    }

    /// Parse a model reference like "local:code:latest" into (provider_name, model_name).
    /// Returns null if the format is invalid.
    pub fn parseModelRef(model_ref: []const u8) ?struct { provider: []const u8, model: []const u8 } {
        const colon1 = std.mem.indexOfScalar(u8, model_ref, ':') orelse return null;
        const provider = model_ref[0..colon1];
        const rest = model_ref[colon1 + 1 ..];
        if (provider.len == 0 or rest.len == 0) return null;
        return .{ .provider = provider, .model = rest };
    }

    /// Resolve a model reference to its provider and full API URL.
    /// Returns the resolved URL and whether this is a thinking model endpoint.
    /// Caller owns the returned URL string.
    pub fn resolveModelUrl(self: *const ProjectConfig, allocator: std.mem.Allocator, model_ref: []const u8) !?struct { url: []const u8, provider: Provider, is_thinking_endpoint: bool } {
        const parsed = parseModelRef(model_ref) orelse return null;
        const provider = self.getProvider(parsed.provider) orelse return null;

        const url = try std.fmt.allocPrint(allocator, "{s}{s}", .{ provider.base_url, provider.chat_endpoint });

        return .{
            .url = url,
            .provider = provider,
            .is_thinking_endpoint = std.mem.eql(u8, provider.chat_endpoint, "/api/chat"),
        };
    }

    /// Check if a model reference matches the thinking model slot.
    pub fn isThinkingModelRef(self: *const ProjectConfig, model_ref: []const u8) bool {
        if (self.model_thinking.len == 0) return false;
        return std.mem.eql(u8, model_ref, self.model_thinking);
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

    // 2. User-global config (~/.config/guidance/guidance-config.json).
    if (std.process.getEnvVarOwned(allocator, "HOME") catch null) |home| {
        defer allocator.free(home);
        const path = try std.fs.path.join(allocator, &.{ home, ".config", "guidance", CONFIG_FILENAME });
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
            \\  "providers": {{
            \\    "local": {{
            \\      "base_url": "http://localhost:11434",
            \\      "chat_endpoint": "/v1/chat/completions"
            \\    }},
            \\    "ollama": {{
            \\      "base_url": "http://localhost:11434",
            \\      "chat_endpoint": "/api/chat"
            \\    }}
            \\  }},
            \\  "models": {{
            \\    "default": "local:code:latest",
            \\    "fast": "local:code:latest",
            \\    "thinking": "ollama:code:latest",
            \\    "embed": "{s}"
            \\  }},
            \\  "embed": {{
            \\    "dims": {d},
            \\    "cache_limit": {d}
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
        , .{ guidance_dir, db_path, DEFAULT_EMBEDDING_PROVIDER ++ ":" ++ DEFAULT_EMBEDDING_MODEL, DEFAULT_EMBEDDING_DIMS, DEFAULT_EMBEDDING_CACHE_LIMIT });

        try file.writeAll(fbs.getWritten());
        std.debug.print("Created {s}\n", .{config_path});
        return true;
    };

    std.debug.print("Config already exists at {s}\n", .{config_path});
    return false;
}

/// Generate AGENTS.md content for guidance integration.
/// Returns an owned slice that the caller must free.
pub fn generateAgentsMdContent(allocator: std.mem.Allocator, guidance_dir: []const u8) ![]const u8 {
    var buf: std.ArrayList(u8) = .{};
    errdefer buf.deinit(allocator);
    const w = buf.writer(allocator);

    try w.writeAll(
        \\# guidance Integration
        \\
        \\This project uses guidance for AST-guided code navigation and documentation.
        \\
        \\## Quick Start
        \\
        \\```
        \\# Initialize guidance configuration
        \\guidance init
        \\
        \\# Run the full RALPH loop (build → test → lint → fmt → guidance)
        \\guidance check
        \\
        \\# Query the codebase
        \\guidance explain "how does X work?"
        \\```
        \\
        \\## Key Files
        \\
    );
    try w.print("- `{s}/guidance-config.json` — Model and provider configuration\n", .{guidance_dir});
    try w.print("- `{s}/src/` — Generated guidance JSON files\n", .{guidance_dir});
    try w.writeAll(
        \\- `.guidance.db` — LanceDB vector search database
        \\- `STRUCTURE.md` — Project structure documentation (auto-generated)
        \\
        \\## RALPH Loop
        \\
        \\The recommended workflow is:
        \\
        \\1. **DISCOVER**: `guidance explain "query"` — Search codebase
        \\2. **UNDERSTAND**: Read source files identified by the query
        \\3. **DECIDE**: Apply relevant skills/patterns
        \\4. **IMPLEMENT**: Make changes
        \\5. **VERIFY**: `guidance check` — Run tests, lint, format, guidance
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
    \\## guidance Integration
    \\
    \\This project uses guidance for AST-guided code navigation.
    \\
    \\```
    \\# Initialize and run
    \\guidance init
    \\guidance check
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

    const model_default = blk: {
        if (root.object.get("models")) |models| {
            if (models == .object) {
                if (models.object.get("default")) |m| if (m == .string) break :blk m.string;
            }
        }
        break :blk DEFAULT_MODEL;
    };
    const model_fast = blk: {
        if (root.object.get("models")) |models| {
            if (models == .object) {
                if (models.object.get("fast")) |m| if (m == .string) break :blk m.string;
            }
        }
        break :blk DEFAULT_FAST_MODEL;
    };
    const model_thinking = blk: {
        if (root.object.get("models")) |models| {
            if (models == .object) {
                if (models.object.get("thinking")) |m| if (m == .string) break :blk m.string;
            }
        }
        break :blk DEFAULT_THINKING_MODEL;
    };

    var providers: std.ArrayList(Provider) = .{};
    errdefer {
        for (providers.items) |*p| p.deinit(allocator);
        providers.deinit(allocator);
    }

    if (root.object.get("providers")) |providers_val| {
        if (providers_val == .object) {
            var it = providers_val.object.iterator();
            while (it.next()) |entry| {
                const name = entry.key_ptr.*;
                const val = entry.value_ptr.*;
                if (val != .object) continue;

                const base_url = if (val.object.get("base_url")) |u| if (u == .string) u.string else "" else "";
                const chat_endpoint = if (val.object.get("chat_endpoint")) |e| if (e == .string) e.string else "" else "";

                if (base_url.len > 0 and chat_endpoint.len > 0) {
                    try providers.append(allocator, Provider{
                        .name = try allocator.dupe(u8, name),
                        .base_url = try allocator.dupe(u8, base_url),
                        .chat_endpoint = try allocator.dupe(u8, chat_endpoint),
                    });
                }
            }
        }
    }

    if (providers.items.len == 0) {
        if (root.object.get("openai")) |openai| {
            if (openai == .object) {
                const base = if (openai.object.get("base_url")) |u| if (u == .string) u.string else "" else "";
                const ep = if (openai.object.get("chat_endpoint")) |e| if (e == .string) e.string else "" else "";
                if (base.len > 0 and ep.len > 0) {
                    try providers.append(allocator, Provider{
                        .name = try allocator.dupe(u8, "local"),
                        .base_url = try allocator.dupe(u8, base),
                        .chat_endpoint = try allocator.dupe(u8, ep),
                    });
                }
            }
        }
        if (providers.items.len == 0) {
            try providers.append(allocator, Provider{
                .name = try allocator.dupe(u8, "local"),
                .base_url = try allocator.dupe(u8, DEFAULT_BASE_URL),
                .chat_endpoint = try allocator.dupe(u8, DEFAULT_CHAT_ENDPOINT),
            });
        }
    }

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

    var fmt_commands: []LintCommand = &.{};
    if (root.object.get("fmt_commands")) |fc_val| {
        if (fc_val == .object) {
            fmt_commands = try parseCommandsObject(allocator, fc_val.object);
        }
    }

    // Capabilities dir
    const capabilities_dir_rel: []const u8 = if (root.object.get("capabilities_dir")) |cd|
        if (cd == .string) cd.string else DEFAULT_CAPABILITIES_DIR
    else
        DEFAULT_CAPABILITIES_DIR;

    // Embedding / guidance.db config
    const guidance_db_path_rel: []const u8 = if (root.object.get("guidance_db_path")) |gp|
        if (gp == .string) gp.string else DEFAULT_GUIDANCE_DB_PATH
    else
        DEFAULT_GUIDANCE_DB_PATH;

    const enable_guidance_db: bool = if (root.object.get("enable_guidance_db")) |egp|
        if (egp == .bool) egp.bool else false
    else
        false;

    // Parse embed object (dims, cache_limit) with fallback to flat fields (backward compat)
    const embed_obj = if (root.object.get("embed")) |eo| if (eo == .object) eo.object else null else null;

    // Parse models object for embed model (new structure: models.embed)
    const models_obj = if (root.object.get("models")) |mo| if (mo == .object) mo.object else null else null;

    const embedding_provider: []const u8 = blk: {
        // New structure: models.embed (e.g. "ollama:nomic-embed-text")
        if (models_obj) |obj| {
            if (obj.get("embed")) |em| {
                if (em == .string) {
                    const full = em.string;
                    if (std.mem.indexOfScalar(u8, full, ':')) |colon| {
                        break :blk full[0..colon];
                    }
                }
            }
        }
        // Old structure: embed.provider
        if (embed_obj) |obj| {
            if (obj.get("provider")) |ep| if (ep == .string) break :blk ep.string;
        }
        // Old structure: flat embedding_provider
        if (root.object.get("embedding_provider")) |ep| if (ep == .string) break :blk ep.string;
        break :blk DEFAULT_EMBEDDING_PROVIDER;
    };

    const embedding_model: []const u8 = blk: {
        // New structure: models.embed (e.g. "ollama:nomic-embed-text")
        if (models_obj) |obj| {
            if (obj.get("embed")) |em| {
                if (em == .string) {
                    const full = em.string;
                    if (std.mem.indexOfScalar(u8, full, ':')) |colon| {
                        break :blk full[colon + 1 ..];
                    }
                }
            }
        }
        // Old structure: embed.model
        if (embed_obj) |obj| {
            if (obj.get("model")) |em| if (em == .string) break :blk em.string;
        }
        // Old structure: flat embedding_model
        if (root.object.get("embedding_model")) |em| if (em == .string) break :blk em.string;
        break :blk DEFAULT_EMBEDDING_MODEL;
    };

    const embedding_dims: u32 = blk: {
        if (embed_obj) |obj| {
            if (obj.get("dims")) |ed| {
                switch (ed) {
                    .integer => |n| if (n > 0) break :blk @intCast(n),
                    else => {},
                }
            }
        }
        if (root.object.get("embedding_dims")) |ed| {
            switch (ed) {
                .integer => |n| break :blk if (n > 0) @intCast(n) else DEFAULT_EMBEDDING_DIMS,
                else => {},
            }
        }
        break :blk DEFAULT_EMBEDDING_DIMS;
    };

    const embedding_cache_limit: u32 = blk: {
        if (embed_obj) |obj| {
            if (obj.get("cache_limit")) |cl| {
                switch (cl) {
                    .integer => |n| if (n >= 0) break :blk @intCast(n),
                    else => {},
                }
            }
        }
        if (root.object.get("embedding_cache_limit")) |cl| {
            switch (cl) {
                .integer => |n| break :blk if (n >= 0) @intCast(n) else DEFAULT_EMBEDDING_CACHE_LIMIT,
                else => {},
            }
        }
        break :blk DEFAULT_EMBEDDING_CACHE_LIMIT;
    };

    return buildFromParts(
        allocator,
        cwd,
        guidance_dir_rel,
        db_path_rel,
        guidance_db_path_rel,
        enable_guidance_db,
        try allocator.dupe(u8, capabilities_dir_rel),
        try allocator.dupe(u8, embedding_provider),
        try allocator.dupe(u8, embedding_model),
        embedding_dims,
        try src_dirs.toOwnedSlice(allocator),
        try providers.toOwnedSlice(allocator),
        try allocator.dupe(u8, model_default),
        try allocator.dupe(u8, model_fast),
        try allocator.dupe(u8, model_thinking),
        test_commands,
        lint_commands,
        fmt_commands,
        embedding_cache_limit,
    );
}

fn buildDefault(allocator: std.mem.Allocator, cwd: []const u8) !ProjectConfig {
    var src_dirs = try allocator.alloc([]const u8, 1);
    src_dirs[0] = try allocator.dupe(u8, DEFAULT_SRC_DIR);

    var providers = try allocator.alloc(Provider, 1);
    providers[0] = Provider{
        .name = try allocator.dupe(u8, "local"),
        .base_url = try allocator.dupe(u8, DEFAULT_BASE_URL),
        .chat_endpoint = try allocator.dupe(u8, DEFAULT_CHAT_ENDPOINT),
    };

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
        DEFAULT_GUIDANCE_DB_PATH,
        true, // enable_guidance_db — always on by default
        try allocator.dupe(u8, DEFAULT_CAPABILITIES_DIR),
        try allocator.dupe(u8, DEFAULT_EMBEDDING_PROVIDER),
        try allocator.dupe(u8, DEFAULT_EMBEDDING_MODEL),
        DEFAULT_EMBEDDING_DIMS,
        src_dirs,
        providers,
        try allocator.dupe(u8, DEFAULT_MODEL),
        try allocator.dupe(u8, DEFAULT_FAST_MODEL),
        try allocator.dupe(u8, DEFAULT_THINKING_MODEL),
        test_commands,
        &.{},
        &.{},
        DEFAULT_EMBEDDING_CACHE_LIMIT,
    );
}

fn buildFromParts(
    allocator: std.mem.Allocator,
    cwd: []const u8,
    guidance_dir_rel: []const u8,
    db_path_rel: []const u8,
    guidance_db_path_rel: []const u8,
    enable_guidance_db: bool,
    capabilities_dir: []const u8,
    embedding_provider: []const u8,
    embedding_model: []const u8,
    embedding_dims: u32,
    src_dirs: []const []const u8,
    providers: []const Provider,
    model_default: []const u8,
    model_fast: []const u8,
    model_thinking: []const u8,
    test_commands: []const LintCommand,
    lint_commands: []const LintCommand,
    fmt_commands: []const LintCommand,
    embedding_cache_limit: u32,
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

    const guidance_db_path = try allocator.dupe(u8, guidance_db_path_rel);
    errdefer allocator.free(guidance_db_path);

    return .{
        .allocator = allocator,
        .guidance_dir = guidance_dir,
        .guidance_root = guidance_root,
        .json_base = json_base,
        .skills_dir = skills_dir,
        .inbox_dir = inbox_dir,
        .db_path = db_path,
        .guidance_db_path = guidance_db_path,
        .enable_guidance_db = enable_guidance_db,
        .capabilities_dir = capabilities_dir,
        .embedding_provider = embedding_provider,
        .embedding_model = embedding_model,
        .embedding_dims = embedding_dims,
        .src_dirs = src_dirs,
        .providers = providers,
        .model_default = model_default,
        .model_fast = model_fast,
        .model_thinking = model_thinking,
        .test_commands = test_commands,
        .lint_commands = lint_commands,
        .fmt_commands = fmt_commands,
        .embedding_cache_limit = embedding_cache_limit,
    };
}
