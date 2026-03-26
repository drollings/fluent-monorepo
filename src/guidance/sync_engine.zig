//! sync_engine.zig — init, commit, gen, status, clean, pipeline, and utility commands.
//!
//! Extracted from main.zig (M1.6) to keep individual file sizes navigable.
//! All public functions are called from main.zig's command dispatch switch.

const std = @import("std");
const types = @import("types.zig");
const ast_parser = @import("ast_parser.zig");
const lance_db_mod = @import("vector");
const vector_mod = @import("vector");
const common = @import("common");
const enhancer_mod = @import("enhancer.zig");
const config_mod = @import("config.zig");
const plugin_mod = @import("plugin.zig");
const plugin_registry = @import("plugin_registry.zig");
const llm_filter_mod = @import("llm_filter.zig");
const synthesize_mod = @import("synthesize.zig");
const marker_mod = @import("marker.zig");
const provider_mod = @import("provider_discovery.zig");
const comment_sync_mod = @import("comment_sync.zig");
const json_store_mod = @import("json_store.zig");
const comment_inserter_mod = @import("comment_inserter.zig");
const scrub_mod = @import("scrub.zig");
const todo_mod = @import("todo.zig");
const sync_mod = @import("sync.zig");
const structure_mod = @import("structure.zig");
const query_engine_mod = @import("query_engine.zig");
const deps_mod = @import("deps.zig");
const llm = @import("common");
const GuidanceDb = lance_db_mod.GuidanceDb;
const SearchResult = GuidanceDb.SearchResult;
const stepPrint = types.stepPrint;

// =============================================================================
// init — create default configuration
// =============================================================================

/// Manages initialization arguments for Zig's guidance system, owns state, ensures invariant correctness.
const InitArgs = struct {
    guidance_dir: ?[]const u8 = null,
    db_path: ?[]const u8 = null,

    fn parse(args: []const []const u8) error{MissingValue}!InitArgs {
        var ia: InitArgs = .{};
        var i: usize = 0;
        while (i < args.len) : (i += 1) {
            const arg = args[i];
            if (std.mem.eql(u8, arg, "--guidance-dir") or std.mem.eql(u8, arg, "-g")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ia.guidance_dir = args[i];
            } else if (std.mem.eql(u8, arg, "--db") or std.mem.eql(u8, arg, "-o")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ia.db_path = args[i];
            }
        }
        return ia;
    }
};

pub fn cmdInit(allocator: std.mem.Allocator, args: []const []const u8) !void {
    const ia = InitArgs.parse(args) catch |err| {
        std.debug.print("error: init flag missing value ({s})\n", .{@errorName(err)});
        return err;
    };

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const options: config_mod.InitOptions = .{
        .guidance_dir = ia.guidance_dir,
        .db_path = ia.db_path,
    };

    const created = try config_mod.initConfig(allocator, cwd, options);

    // Handle AGENTS.md
    const guidance_dir = ia.guidance_dir orelse config_mod.DEFAULT_GUIDANCE_DIR;
    const agents_path = try std.fs.path.join(allocator, &.{ cwd, "AGENTS.md" });
    defer allocator.free(agents_path);

    const agents_exists = blk: {
        std.fs.accessAbsolute(agents_path, .{}) catch break :blk false;
        break :blk true;
    };

    if (agents_exists) {
        // Read existing content
        const existing = blk: {
            const file = std.fs.openFileAbsolute(agents_path, .{}) catch break :blk null;
            defer file.close();
            break :blk file.readToEndAlloc(allocator, 1024 * 1024) catch null;
        };

        // Check if already has guidance integration
        if (existing) |e| {
            if (std.mem.indexOf(u8, e, "guidance Integration") != null) {
                std.debug.print("AGENTS.md already contains guidance integration.\n", .{});
                allocator.free(e);
            } else {
                // Prepend insertion
                const insertion = try std.fmt.allocPrint(allocator,
                    \\---
                    \\
                    \\## guidance Integration
                    \\
                    \\This project uses guidance for AST-guided code navigation.
                    \\
                    \\```
                    \\guidance init
                    \\guidance check
                    \\```
                    \\
                    \\Config: `{s}/guidance-config.json`
                    \\
                    \\
                , .{guidance_dir});
                defer allocator.free(insertion);

                const new_content = try std.fmt.allocPrint(allocator, "{s}{s}", .{ insertion, e });
                defer allocator.free(new_content);

                // Write new file
                const file = std.fs.createFileAbsolute(agents_path, .{}) catch |err| {
                    std.debug.print("Warning: could not update AGENTS.md: {}\n", .{err});
                    allocator.free(e);
                    return;
                };
                defer file.close();
                try file.writeAll(new_content);
                allocator.free(e);

                std.debug.print("AGENTS.md updated with guidance integration.\n", .{});

                // Offer to open $EDITOR
                if (std.process.getEnvVarOwned(allocator, "EDITOR")) |editor| {
                    defer allocator.free(editor);
                    std.debug.print("Review changes with: {s} AGENTS.md\n", .{editor});
                } else |_| {}
            }
        }
    } else {
        // Create new AGENTS.md
        const agents_content = try config_mod.generateAgentsMdContent(allocator, guidance_dir);
        defer allocator.free(agents_content);

        const file = std.fs.createFileAbsolute(agents_path, .{}) catch |err| {
            std.debug.print("Warning: could not create AGENTS.md: {}\n", .{err});
            return;
        };
        defer file.close();
        try file.writeAll(agents_content);
        std.debug.print("Created AGENTS.md\n", .{});
    }

    if (created) {
        std.debug.print("\nConfiguration created at {s}/{s}/guidance-config.json\n", .{ cwd, guidance_dir });
    }
}

// =============================================================================
// commit — AI git commit message from staged diff + guidance JSON context
// =============================================================================

fn gitDiff(allocator: std.mem.Allocator, cwd: []const u8, staged: bool) ![]u8 {
    const argv: []const []const u8 = if (staged)
        &.{ "git", "diff", "--staged" }
    else
        &.{ "git", "diff" };

    var child = std.process.Child.init(argv, allocator);
    child.cwd = cwd;
    child.stdout_behavior = .Pipe;
    child.stderr_behavior = .Ignore;
    try child.spawn();
    const output = try child.stdout.?.readToEndAlloc(allocator, 10 * 1024 * 1024);
    _ = try child.wait();
    return output;
}

/// Split a unified diff into per-file chunks starting at each "diff --git" header.
fn splitDiffByFile(diff: []const u8, out: *std.ArrayList([]const u8), allocator: std.mem.Allocator) !void {
    var start: usize = 0;
    var pos: usize = 0;
    while (pos < diff.len) {
        const nl = std.mem.indexOfScalarPos(u8, diff, pos, '\n') orelse diff.len;
        const line = diff[pos..nl];
        if (std.mem.startsWith(u8, line, "diff --git") and pos > start) {
            try out.append(allocator, diff[start..pos]);
            start = pos;
        }
        pos = nl + 1;
    }
    if (start < diff.len) try out.append(allocator, diff[start..]);
}

/// Extract the file path from "diff --git a/<path> b/<path>".
fn chunkFilePath(chunk: []const u8) []const u8 {
    const prefix = "diff --git a/";
    const first_nl = std.mem.indexOfScalar(u8, chunk, '\n') orelse chunk.len;
    const first_line = chunk[0..first_nl];
    if (!std.mem.startsWith(u8, first_line, prefix)) return "";
    const after = first_line[prefix.len..];
    const sp = std.mem.indexOfScalar(u8, after, ' ') orelse after.len;
    return after[0..sp];
}

/// Return true for auto-generated guidance JSON files (not real code changes).
/// Uses guidance_dir (e.g. ".guidance") to identify which paths to ignore.
fn chunkIsExplainGenJson(chunk: []const u8, guidance_dir: []const u8) bool {
    const path = chunkFilePath(chunk);
    const prefix = std.fmt.allocPrint(std.heap.page_allocator, "{s}/", .{guidance_dir}) catch return false;
    defer std.heap.page_allocator.free(prefix);
    return std.mem.startsWith(u8, path, prefix) and std.mem.endsWith(u8, path, ".json");
}

/// Parse `@@ -X,Y +A,B @@` hunk headers; returns owned [start, end) pairs in new-file coords.
fn parseHunkRanges(allocator: std.mem.Allocator, chunk: []const u8) ![][2]u32 {
    var ranges: std.ArrayList([2]u32) = .{};
    errdefer ranges.deinit(allocator);

    var lines = std.mem.splitScalar(u8, chunk, '\n');
    while (lines.next()) |line| {
        if (!std.mem.startsWith(u8, line, "@@ ")) continue;
        const plus_pos = std.mem.indexOf(u8, line, " +") orelse continue;
        const after_plus = line[plus_pos + 2 ..];
        const space_pos = std.mem.indexOfScalar(u8, after_plus, ' ') orelse after_plus.len;
        const range_part = after_plus[0..space_pos];
        const comma = std.mem.indexOfScalar(u8, range_part, ',');
        const start_str = if (comma) |c| range_part[0..c] else range_part;
        const count_str = if (comma) |c| range_part[c + 1 ..] else "1";
        const start = std.fmt.parseInt(u32, start_str, 10) catch continue;
        const count = std.fmt.parseInt(u32, count_str, 10) catch 1;
        try ranges.append(allocator, .{ start, start + count });
    }
    return ranges.toOwnedSlice(allocator);
}

/// Snapshot of a member's name, line, comment, and signature at the point a commit is prepared.
const CommitMemberInfo = struct {
    name: []const u8, // owned
    line: ?u32,
    comment: []const u8, // owned (may be empty)
    signature: []const u8, // owned (may be empty)

    pub fn deinit(self: CommitMemberInfo, allocator: std.mem.Allocator) void {
        allocator.free(self.name);
        allocator.free(self.comment);
        allocator.free(self.signature);
    }
};

/// Extract a ?u32 line number from a JSON member object value (.line field).
fn memberLineNum(obj: std.json.ObjectMap) ?u32 {
    const lv = obj.get("line") orelse return null;
    return switch (lv) {
        .integer => |n| if (n >= 0) @as(u32, @intCast(n)) else null,
        .float => |f| if (f >= 0) @as(u32, @intFromFloat(f)) else null,
        else => null,
    };
}

/// Return true when `line` falls within any hunk range expanded by `context`.
fn lineInRanges(line: u32, hunk_ranges: []const [2]u32, context: u32) bool {
    for (hunk_ranges) |range| {
        const lo = if (range[0] > context) range[0] - context else 0;
        const hi = range[1] + context;
        if (line >= lo and line <= hi) return true;
    }
    return false;
}

/// Append a CommitMemberInfo to `out` when the JSON member object is valid and
/// its line falls within `hunk_ranges` (or when hunk_ranges is empty).
fn appendMemberIfInRange(
    allocator: std.mem.Allocator,
    member: std.json.Value,
    hunk_ranges: []const [2]u32,
    context: u32,
    out: *std.ArrayList(CommitMemberInfo),
) !void {
    if (member != .object) return;
    const name_val = member.object.get("name") orelse return;
    if (name_val != .string or name_val.string.len == 0) return;

    const line_num = memberLineNum(member.object);
    const include = hunk_ranges.len == 0 or line_num == null or
        lineInRanges(line_num.?, hunk_ranges, context);
    if (!include) return;

    const comment = if (member.object.get("comment")) |cv| if (cv == .string) cv.string else "" else "";
    const sig = if (member.object.get("signature")) |sv| if (sv == .string) sv.string else "" else "";
    try out.append(allocator, .{
        .name = try allocator.dupe(u8, name_val.string),
        .line = line_num,
        .comment = try allocator.dupe(u8, comment),
        .signature = try allocator.dupe(u8, sig),
    });
}

/// Load guidance JSON for rel_path and return members whose lines overlap hunk_ranges.
fn loadChangedMembers(
    allocator: std.mem.Allocator,
    guidance_root: []const u8,
    rel_path: []const u8,
    hunk_ranges: []const [2]u32,
) ![]CommitMemberInfo {
    // JSON path: <guidance_root>/src/<rel_path>.json
    const json_path = try std.fmt.allocPrint(allocator, "{s}/src/{s}.json", .{ guidance_root, rel_path });
    defer allocator.free(json_path);

    var parsed = llm.parseJsonFile(allocator, json_path, 256 * 1024) orelse return &.{};
    defer parsed.deinit();

    const members_val = parsed.value.object.get("members") orelse return &.{};
    if (members_val != .array) return &.{};

    var result: std.ArrayList(CommitMemberInfo) = .{};
    errdefer {
        for (result.items) |m| m.deinit(allocator);
        result.deinit(allocator);
    }

    const CONTEXT_LINES: u32 = 15;

    for (members_val.array.items) |member| {
        try appendMemberIfInRange(allocator, member, hunk_ranges, CONTEXT_LINES, &result);
        // Recurse one level into nested members (struct methods).
        if (member.object.get("members")) |nested_val| {
            if (nested_val == .array) {
                for (nested_val.array.items) |nested| {
                    try appendMemberIfInRange(allocator, nested, hunk_ranges, CONTEXT_LINES, &result);
                }
            }
        }
    }

    return result.toOwnedSlice(allocator);
}

fn generateCommitMessage(
    allocator: std.mem.Allocator,
    diff: []const u8,
    changed_files: []const []const u8,
    guidance_dir: []const u8,
    guidance_root: []const u8,
    api_url: []const u8,
    model: []const u8,
    debug: bool,
) ![]u8 {
    // Count how many guidance JSON files changed (for the summary bullet).
    const guidance_prefix = std.fmt.allocPrint(allocator, "{s}/", .{guidance_dir}) catch return error.OutOfMemory;
    defer allocator.free(guidance_prefix);
    var guidance_json_count: usize = 0;
    for (changed_files) |f| {
        if (std.mem.startsWith(u8, f, guidance_prefix) and std.mem.endsWith(u8, f, ".json"))
            guidance_json_count += 1;
    }

    const config: llm.LlmConfig = .{ .api_url = api_url, .model = model, .debug = debug };
    if (llm.LlmClient.init(allocator, config)) |client_val| {
        var client = client_val;
        defer client.deinit();
        if (client.available()) {
            // Split diff; partition into code chunks vs. guidance JSON chunks.
            var all_chunks: std.ArrayList([]const u8) = .{};
            defer all_chunks.deinit(allocator);
            try splitDiffByFile(diff, &all_chunks, allocator);

            var code_chunks: std.ArrayList([]const u8) = .{};
            defer code_chunks.deinit(allocator);
            for (all_chunks.items) |chunk| {
                if (!chunkIsExplainGenJson(chunk, guidance_dir)) try code_chunks.append(allocator, chunk);
            }

            if (debug) {
                std.debug.print("[commit] {} total chunk(s), {} code, {} guidance JSON\n", .{
                    all_chunks.items.len, code_chunks.items.len, guidance_json_count,
                });
            }

            if (code_chunks.items.len > 0 or guidance_json_count > 0) {
                const TOTAL_CAP: usize = 12_000;
                const n_code = @max(1, code_chunks.items.len);
                const per_file_cap: usize = @max(800, TOTAL_CAP / n_code);

                var combined: std.ArrayList(u8) = .{};
                defer combined.deinit(allocator);
                const cw = combined.writer(allocator);

                // Build enriched context per code file.
                for (code_chunks.items) |chunk| {
                    if (combined.items.len >= TOTAL_CAP) break;

                    const rel_path = chunkFilePath(chunk);

                    if (guidance_root.len > 0 and rel_path.len > 0) {
                        const hunk_ranges = parseHunkRanges(allocator, chunk) catch &.{};
                        defer allocator.free(hunk_ranges);

                        const members = loadChangedMembers(allocator, guidance_root, rel_path, hunk_ranges) catch &.{};
                        defer {
                            for (members) |m| m.deinit(allocator);
                            allocator.free(members);
                        }

                        if (members.len > 0) {
                            try cw.print("### Functions in {s}:\n", .{rel_path});
                            for (members) |m| {
                                if (m.line) |ln| {
                                    try cw.print("- {s} (line {d})", .{ m.name, ln });
                                } else {
                                    try cw.print("- {s}", .{m.name});
                                }
                                if (m.comment.len > 0) {
                                    const end = std.mem.indexOfScalar(u8, m.comment, '.') orelse m.comment.len;
                                    const snippet = m.comment[0..@min(end + 1, @min(m.comment.len, 120))];
                                    try cw.print(": {s}", .{snippet});
                                } else if (m.signature.len > 0) {
                                    const snippet = m.signature[0..@min(m.signature.len, 80)];
                                    try cw.print(": `{s}`", .{snippet});
                                }
                                try cw.writeByte('\n');
                            }
                            try cw.writeByte('\n');
                        }
                    }

                    const budget = @min(chunk.len, per_file_cap);
                    try combined.appendSlice(allocator, chunk[0..budget]);
                    try combined.append(allocator, '\n');
                }

                const prompt = try std.fmt.allocPrint(
                    allocator,
                    \\TASK: Write a git commit message as a bullet list.
                    \\
                    \\Rules:
                    \\  - One bullet per distinct logical change.
                    \\  - Each bullet: "* <FunctionOrFileName>: <past-tense description of what changed and why>"
                    \\  - Be specific and concise. Use the function descriptions above when available.
                    \\  - Output ONLY the bullet list. No code. No explanations. No headings.
                    \\
                    \\Example:
                    \\* cmdExplain: added --staged flag to control LLM relevance filter mode
                    \\* searchWithAliases: expanded stop-word list to reduce noise in short queries
                    \\
                    \\CHANGED FILES:
                    \\{s}
                    \\END OF DIFF.
                    \\
                    \\Now write the bullet list (each line starts with "* "):
                ,
                    .{combined.items},
                );
                defer allocator.free(prompt);

                if (debug) std.debug.print("[commit] prompt ({d} chars):\n{s}\n---\n", .{ prompt.len, prompt });

                const result = client.complete(prompt, 8192, 0.1, null) catch null;

                if (result) |raw| {
                    defer allocator.free(raw);
                    if (debug) std.debug.print("[commit] response:\n{s}\n---\n", .{raw});

                    var bullets: std.ArrayList([]const u8) = .{};
                    defer {
                        for (bullets.items) |b| allocator.free(b);
                        bullets.deinit(allocator);
                    }
                    var resp_lines = std.mem.splitScalar(u8, raw, '\n');
                    while (resp_lines.next()) |line| {
                        const trimmed = std.mem.trim(u8, line, " \t\r");
                        const is_bullet = std.mem.startsWith(u8, trimmed, "* ") or
                            std.mem.startsWith(u8, trimmed, "- ");
                        if (is_bullet) {
                            const text = std.mem.trim(u8, trimmed[2..], " \t");
                            if (text.len > 0) try bullets.append(allocator, try allocator.dupe(u8, text));
                        }
                    }

                    if (bullets.items.len > 0) {
                        var out: std.ArrayList(u8) = .{};
                        for (bullets.items) |b| {
                            try out.appendSlice(allocator, "* ");
                            try out.appendSlice(allocator, b);
                            try out.append(allocator, '\n');
                        }
                        // Append guidance JSON summary bullet if any JSON files changed.
                        if (guidance_json_count > 0) {
                            try out.writer(allocator).print("* guidance: updated {d} JSON file(s) in {s}/src/\n", .{ guidance_json_count, guidance_dir });
                        }
                        if (out.items.len > 0 and out.items[out.items.len - 1] == '\n')
                            out.items.len -= 1;
                        return out.toOwnedSlice(allocator);
                    }
                }
            }
        }
    } else |_| {}

    // Deterministic fallback: list changed non-guidance files as bullets.
    std.debug.print("warning: LLM unavailable or returned no bullets; using filename fallback\n", .{});

    var fallback: std.ArrayList(u8) = .{};
    defer fallback.deinit(allocator);
    var any = false;
    for (changed_files) |f| {
        if (std.mem.startsWith(u8, f, guidance_prefix) and std.mem.endsWith(u8, f, ".json")) continue;
        try fallback.appendSlice(allocator, "* Update ");
        try fallback.appendSlice(allocator, f);
        try fallback.append(allocator, '\n');
        any = true;
    }
    if (guidance_json_count > 0) {
        try fallback.writer(allocator).print("* guidance: updated {d} JSON file(s) in {s}/src/\n", .{ guidance_json_count, guidance_dir });
        any = true;
    }
    if (any) {
        if (fallback.items.len > 0 and fallback.items[fallback.items.len - 1] == '\n')
            fallback.items.len -= 1;
        return fallback.toOwnedSlice(allocator);
    }
    return try allocator.dupe(u8, "* Update codebase");
}

fn writeTmpCommitMsg(allocator: std.mem.Allocator, msg: []const u8) ![]u8 {
    const path = try std.fmt.allocPrint(allocator, "/tmp/explain_gen_commit_{d}.txt", .{std.time.timestamp()});
    const file = try std.fs.createFileAbsolute(path, .{});
    defer file.close();
    try file.writeAll(msg);
    try file.writeAll("\n\n# Lines starting with '#' will be ignored.\n");
    try file.writeAll("# Edit the commit message above. Save and close to commit.\n");
    return path;
}

pub fn cmdCommit(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var dry_run = false;
    var debug = false;
    var api_url: []const u8 = config_mod.DEFAULT_API_URL;
    var model: []const u8 = config_mod.DEFAULT_MODEL;
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--dry-run")) {
            dry_run = true;
        } else if (std.mem.eql(u8, arg, "--debug") or std.mem.eql(u8, arg, "--verbose")) {
            debug = true;
        } else if (std.mem.eql(u8, arg, "--api-url")) {
            i += 1;
            if (i >= args.len) return;
            api_url = args[i];
        } else if (std.mem.eql(u8, arg, "-m") or std.mem.eql(u8, arg, "--model")) {
            i += 1;
            if (i >= args.len) return;
            model = args[i];
        }
    }

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    // Load config — override defaults from config file.
    const default_guidance_dir: []const u8 = config_mod.DEFAULT_GUIDANCE_DIR;
    const default_guidance_root = try std.fs.path.join(allocator, &.{ cwd, config_mod.DEFAULT_GUIDANCE_DIR });
    defer allocator.free(default_guidance_root);

    // api_url and model are owned slices valid for this function's lifetime.
    var api_url_owned: []const u8 = try allocator.dupe(u8, api_url);
    defer allocator.free(api_url_owned);
    var model_owned: []const u8 = undefined;
    var guidance_dir_owned: []const u8 = undefined;
    var guidance_root_owned: []const u8 = undefined;
    var has_config = false;

    if (config_mod.loadConfig(allocator, cwd)) |cfg_val| {
        var cfg = cfg_val;
        defer cfg.deinit();
        has_config = true;
        // Resolve API URL from providers based on model.
        // Fall back to default if model parsing or provider resolution fails.
        allocator.free(api_url_owned);
        const parsed = config_mod.ProjectConfig.parseModelRef(cfg.model_default);
        if (parsed) |p| {
            if (cfg.getProvider(p.provider)) |provider| {
                api_url_owned = try std.fmt.allocPrint(allocator, "{s}{s}", .{ provider.base_url, provider.chat_endpoint });
            } else {
                api_url_owned = try allocator.dupe(u8, api_url);
            }
        } else {
            api_url_owned = try allocator.dupe(u8, api_url);
        }
        // Prefer models.commit > models.default from raw config JSON.
        model_owned = loadCommitModelFromConfig(allocator, cwd) catch
            try allocator.dupe(u8, cfg.model_default);
        // guidance_root and guidance_dir: refresh from config.
        guidance_root_owned = try allocator.dupe(u8, cfg.guidance_root);
        guidance_dir_owned = try allocator.dupe(u8, cfg.guidance_dir);
    } else |_| {
        model_owned = try allocator.dupe(u8, model);
    }
    defer allocator.free(model_owned);

    const guidance_dir: []const u8 = if (has_config) guidance_dir_owned else default_guidance_dir;
    const guidance_root: []const u8 = if (has_config) guidance_root_owned else default_guidance_root;
    defer {
        if (has_config) {
            allocator.free(guidance_dir_owned);
            allocator.free(guidance_root_owned);
        }
    }
    model = model_owned;
    api_url = api_url_owned;

    // Get staged diff.
    const diff = try gitDiff(allocator, cwd, true);
    defer allocator.free(diff);

    if (diff.len == 0) {
        std.debug.print("No staged changes to commit. Use 'git add' to stage files first.\n", .{});
        return;
    }

    // Extract changed file paths from diff headers.
    var changed_files: std.ArrayList([]const u8) = .{};
    defer {
        for (changed_files.items) |f| allocator.free(f);
        changed_files.deinit(allocator);
    }
    {
        var lines = std.mem.splitScalar(u8, diff, '\n');
        while (lines.next()) |line| {
            if (std.mem.startsWith(u8, line, "diff --git a/")) {
                const after = line["diff --git a/".len..];
                const space = std.mem.indexOfScalar(u8, after, ' ') orelse after.len;
                try changed_files.append(allocator, try allocator.dupe(u8, after[0..space]));
            }
        }
    }

    const commit_msg = try generateCommitMessage(allocator, diff, changed_files.items, guidance_dir, guidance_root, api_url, model, debug);
    defer allocator.free(commit_msg);

    if (debug or dry_run) {
        std.debug.print("--- Generated commit message ---\n{s}\n-------------------------------\n", .{commit_msg});
    }

    if (dry_run) return;

    // Write to temp file and open $EDITOR.
    const tmp_path = try writeTmpCommitMsg(allocator, commit_msg);
    defer {
        std.fs.deleteFileAbsolute(tmp_path) catch {};
        allocator.free(tmp_path);
    }

    const mtime_before: i128 = blk: {
        const f = std.fs.openFileAbsolute(tmp_path, .{}) catch break :blk 0;
        defer f.close();
        const stat = f.stat() catch break :blk 0;
        break :blk stat.mtime;
    };

    const editor = std.process.getEnvVarOwned(allocator, "EDITOR") catch
        std.process.getEnvVarOwned(allocator, "VISUAL") catch
        try allocator.dupe(u8, "vi");
    defer allocator.free(editor);

    var editor_child = std.process.Child.init(&.{ editor, tmp_path }, allocator);
    editor_child.cwd = cwd;
    const editor_result = try editor_child.spawnAndWait();
    if (editor_result != .Exited or editor_result.Exited != 0) {
        std.debug.print("Editor exited with non-zero status. Aborting commit.\n", .{});
        return;
    }

    const mtime_after: i128 = blk: {
        const f = std.fs.openFileAbsolute(tmp_path, .{}) catch break :blk 0;
        defer f.close();
        const stat = f.stat() catch break :blk 0;
        break :blk stat.mtime;
    };

    if (mtime_after == mtime_before) {
        std.debug.print("Commit message not saved. Aborting.\n", .{});
        return;
    }

    // Read back the edited message, strip comment lines.
    const edited = try std.fs.openFileAbsolute(tmp_path, .{});
    defer edited.close();
    const raw_msg = try edited.readToEndAlloc(allocator, 64 * 1024);
    defer allocator.free(raw_msg);

    var final_parts: std.ArrayList([]const u8) = .{};
    defer final_parts.deinit(allocator);
    var msg_lines = std.mem.splitScalar(u8, raw_msg, '\n');
    while (msg_lines.next()) |line| {
        if (!std.mem.startsWith(u8, line, "#")) {
            try final_parts.append(allocator, line);
        }
    }
    const final_msg_raw = try std.mem.join(allocator, "\n", final_parts.items);
    defer allocator.free(final_msg_raw);
    const final_msg = std.mem.trim(u8, final_msg_raw, " \t\r\n");

    if (final_msg.len == 0) {
        std.debug.print("Commit message is empty. Aborting.\n", .{});
        return;
    }

    // Check CHECKLIST.md for incomplete items before committing.
    const todo_dir = try std.fs.path.join(allocator, &.{ cwd, config_mod.DEFAULT_GUIDANCE_DIR, "todo" });
    defer allocator.free(todo_dir);

    const cl_status = try todo_mod.queryChecklistStatus(allocator, todo_dir);
    defer if (cl_status.item_dir) |d| allocator.free(d);
    if (cl_status.incomplete > 0) {
        std.debug.print(
            "Warning: {d}/{d} checklist items incomplete in current work item.\n",
            .{ cl_status.incomplete, cl_status.total },
        );
    }

    var commit_child = std.process.Child.init(&.{ "git", "commit", "-m", final_msg }, allocator);
    commit_child.cwd = cwd;
    const commit_result = try commit_child.spawnAndWait();
    if (commit_result == .Exited and commit_result.Exited == 0) {
        std.debug.print("Committed successfully.\n", .{});

        // Write COMMITTED.md if there is an active work item.
        if (cl_status.item_dir) |item_dir| {
            // Get commit hash.
            const hash: []const u8 = blk: {
                var hash_child = std.process.Child.init(&.{ "git", "rev-parse", "HEAD" }, allocator);
                hash_child.stdout_behavior = .Pipe;
                hash_child.stderr_behavior = .Ignore;
                hash_child.cwd = cwd;
                hash_child.spawn() catch break :blk "";
                const out = hash_child.stdout.?.readToEndAlloc(allocator, 256) catch break :blk "";
                _ = hash_child.wait() catch {};
                break :blk std.mem.trim(u8, out, " \t\r\n");
            };
            defer if (hash.len > 0) allocator.free(hash);

            // First line of commit message is the summary.
            const summary = blk: {
                const nl = std.mem.indexOfScalar(u8, final_msg, '\n') orelse final_msg.len;
                break :blk final_msg[0..nl];
            };

            todo_mod.writeCommittedMd(
                allocator,
                item_dir,
                hash,
                summary,
                changed_files.items,
            ) catch |err| {
                std.debug.print("Warning: could not write COMMITTED.md: {}\n", .{err});
            };
        }
    } else {
        std.debug.print("git commit failed.\n", .{});
    }
}

/// Read `models.commit` or `models.default` from guidance-config.json.
/// Returns an owned slice; caller must free. Returns error when absent.
fn loadCommitModelFromConfig(allocator: std.mem.Allocator, cwd: []const u8) ![]const u8 {
    const path = try std.fs.path.join(allocator, &.{ cwd, config_mod.DEFAULT_GUIDANCE_DIR, config_mod.CONFIG_FILENAME });
    defer allocator.free(path);

    var parsed = llm.parseJsonFile(allocator, path, 64 * 1024) orelse return error.ParseError;
    defer parsed.deinit();

    if (parsed.value.object.get("models")) |models| {
        if (models == .object) {
            if (models.object.get("commit")) |m| if (m == .string and m.string.len > 0)
                return allocator.dupe(u8, m.string);
            if (models.object.get("default")) |m| if (m == .string and m.string.len > 0)
                return allocator.dupe(u8, m.string);
        }
    }
    return error.NoCommitModel;
}

// =============================================================================
// gen
// =============================================================================

/// Manages configuration parameters for Zig compilation; owns struct state; mutable during compilation.
const GenArgs = struct {
    file: ?[]const u8 = null, // single-file mode (--file)
    scan: ?[]const u8 = null, // directory scan mode (--scan)
    workspace: ?[]const u8 = null,
    json_dir: ?[]const u8 = null,
    /// Output path for the .guidance.db vector database.
    /// -o / --db sets this.  Defaults to config or DEFAULT_GUIDANCE_DB_PATH.
    db_path: ?[]const u8 = null,
    dry_run: bool = false,
    verbose: bool = false,
    api_url: []const u8 = config_mod.DEFAULT_API_URL,
    /// True when --api-url was explicitly passed on the CLI.
    api_url_set: bool = false,
    model: []const u8 = config_mod.DEFAULT_MODEL,
    /// True when -m was explicitly passed on the CLI, overriding config slots.
    model_override: bool = false,
    regen_comments: bool = false,
    /// Run CommentSyncProcessor before JSON generation to insert/update source comments.
    sync_comments: bool = false,
    /// Generate //! file headers for files that lack them (used with --sync-comments).
    sync_headers: bool = false,
    /// Disable all LLM calls (no_ai=true disables automatic comment sync).
    no_ai: bool = false,
    /// Set false via --no-db to skip database generation.
    compile_db: bool = true,
    /// Re-process all files even when guidance JSON is fresh.
    force: bool = false,
    /// Discover and invoke external providers for non-built-in extensions.
    all_languages: bool = false,
    /// Skip the test-suite phase (useful when tests were just run externally).
    skip_tests: bool = false,
    /// Skip the lint phase.
    skip_lint: bool = false,
    /// Skip the format phase.
    skip_fmt: bool = false,
    /// Sleep duration (in seconds) after processing each file. Default: 20.
    /// Set to 0 to disable.
    timeout_seconds: u64 = 20,

    /// Parse gen subcommand arguments. Returns error.MissingValue when a
    /// flag-with-value is the last argument (fail fast; do not silently drop).
    fn parse(args: []const []const u8) error{MissingValue}!GenArgs {
        var ga: GenArgs = .{};
        var i: usize = 0;
        while (i < args.len) : (i += 1) {
            const arg = args[i];
            if (std.mem.eql(u8, arg, "--file")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ga.file = args[i];
            } else if (std.mem.eql(u8, arg, "--scan")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ga.scan = args[i];
            } else if (std.mem.eql(u8, arg, "-w") or std.mem.eql(u8, arg, "--workspace")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ga.workspace = args[i];
            } else if (std.mem.eql(u8, arg, "--json-dir") or std.mem.eql(u8, arg, "--output")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ga.json_dir = args[i];
            } else if (std.mem.eql(u8, arg, "-o") or std.mem.eql(u8, arg, "--db")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ga.db_path = args[i];
            } else if (std.mem.eql(u8, arg, "--dry-run")) {
                ga.dry_run = true;
            } else if (std.mem.eql(u8, arg, "--verbose") or std.mem.eql(u8, arg, "--debug")) {
                ga.verbose = true;
            } else if (std.mem.eql(u8, arg, "--regen")) {
                ga.regen_comments = true;
            } else if (std.mem.eql(u8, arg, "--sync-comments")) {
                ga.sync_comments = true;
            } else if (std.mem.eql(u8, arg, "--sync-headers")) {
                ga.sync_headers = true;
            } else if (std.mem.eql(u8, arg, "--no-ai")) {
                ga.no_ai = true;
            } else if (std.mem.eql(u8, arg, "--no-db")) {
                ga.compile_db = false;
            } else if (std.mem.eql(u8, arg, "--force")) {
                ga.force = true;
            } else if (std.mem.eql(u8, arg, "--all-languages")) {
                ga.all_languages = true;
            } else if (std.mem.eql(u8, arg, "--skip-tests")) {
                ga.skip_tests = true;
            } else if (std.mem.eql(u8, arg, "--skip-lint")) {
                ga.skip_lint = true;
            } else if (std.mem.eql(u8, arg, "--skip-fmt")) {
                ga.skip_fmt = true;
            } else if (std.mem.eql(u8, arg, "--api-url")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ga.api_url = args[i];
                ga.api_url_set = true;
            } else if (std.mem.eql(u8, arg, "-m") or std.mem.eql(u8, arg, "--model")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ga.model = args[i];
                ga.model_override = true;
            } else if (std.mem.eql(u8, arg, "--db-type=lance") or
                std.mem.eql(u8, arg, "--lance") or
                std.mem.startsWith(u8, arg, "--db-type="))
            {
                // Accepted but ignored — SQLite is always used.
            } else if (std.mem.eql(u8, arg, "--guidance-db")) {
                // Alias for -o when used with old scripts.
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ga.db_path = args[i];
            } else if (std.mem.eql(u8, arg, "--timeout")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ga.timeout_seconds = std.fmt.parseInt(u64, args[i], 10) catch {
                    std.debug.print("error: --timeout requires a valid u64 value\n", .{});
                    return error.MissingValue;
                };
            }
        }
        return ga;
    }
};

/// Resolved absolute paths for a gen run. All fields are owned; call deinit().
const ResolvedGenPaths = struct {
    workspace: []const u8,
    json_dir: []const u8,
    db_path: []const u8,

    fn deinit(self: *@This(), allocator: std.mem.Allocator) void {
        allocator.free(self.workspace);
        allocator.free(self.json_dir);
        allocator.free(self.db_path);
    }
};

/// Resolve workspace, json_dir, and db_path (→ .guidance.db) to absolute
/// paths.  db_path is the SQLite vector database; it defaults to the value
/// in guidance-config.json, or DEFAULT_GUIDANCE_DB_PATH if not set.
fn resolveGenPaths(allocator: std.mem.Allocator, ga: GenArgs, cwd: []const u8) !ResolvedGenPaths {
    const workspace = try llm.resolvePath(allocator, cwd, ga.workspace orelse cwd);
    errdefer allocator.free(workspace);

    const json_dir = try llm.resolvePath(allocator, workspace, ga.json_dir orelse config_mod.DEFAULT_GUIDANCE_DIR);
    errdefer allocator.free(json_dir);

    const db_path = try llm.resolvePath(allocator, workspace, ga.db_path orelse config_mod.DEFAULT_GUIDANCE_DB_PATH);
    return .{ .workspace = workspace, .json_dir = json_dir, .db_path = db_path };
}

/// Wire up the LLM enhancer on a `CommentSyncProcessor`.
///
/// Allocates an `Enhancer` on the heap (required because `CommentSyncProcessor`
/// holds a `?*Enhancer` pointer).  The returned pointer must be freed with
/// `teardownCspEnhancer` after the processor is done.
fn setupCspEnhancer(
    allocator: std.mem.Allocator,
    ga: GenArgs,
    cfg: *const config_mod.ProjectConfig,
    csp: *comment_sync_mod.CommentSyncProcessor,
) void {
    const model = if (!std.mem.eql(u8, ga.model, config_mod.DEFAULT_MODEL) or ga.model_override)
        ga.model
    else
        cfg.infillModel();

    var resolved_url_to_free: ?[]const u8 = null;
    const llm_config = query_engine_mod.resolveLlmConfigForThinking(
        allocator,
        cfg,
        model,
        if (ga.api_url_set) ga.api_url else null,
    ) catch {
        const fallback_config: llm.LlmConfig = .{
            .api_url = ga.api_url,
            .model = model,
            .think = null,
            .debug = ga.verbose,
        };
        const enh_ptr = allocator.create(enhancer_mod.Enhancer) catch return;
        enh_ptr.* = enhancer_mod.Enhancer.init(allocator, fallback_config) catch {
            allocator.destroy(enh_ptr);
            return;
        };
        csp.enhancer = enh_ptr;
        return;
    };
    resolved_url_to_free = llm_config.resolved_url;

    const final_config: llm.LlmConfig = .{
        .api_url = llm_config.api_url,
        .model = llm_config.model,
        .think = llm_config.think,
        .debug = ga.verbose,
    };

    const enh_ptr = allocator.create(enhancer_mod.Enhancer) catch {
        if (resolved_url_to_free) |url| allocator.free(url);
        return;
    };
    enh_ptr.* = enhancer_mod.Enhancer.init(allocator, final_config) catch |err| {
        std.debug.print("warning: could not init LLM enhancer for comment sync: {}\n", .{err});
        allocator.destroy(enh_ptr);
        if (resolved_url_to_free) |url| allocator.free(url);
        return;
    };
    if (resolved_url_to_free) |url| allocator.free(url);
    csp.enhancer = enh_ptr;
}

/// Release the heap-allocated enhancer previously set by `setupCspEnhancer`.
fn teardownCspEnhancer(allocator: std.mem.Allocator, csp: *comment_sync_mod.CommentSyncProcessor) void {
    if (csp.enhancer) |enh_ptr| {
        enh_ptr.deinit();
        allocator.destroy(enh_ptr);
        csp.enhancer = null;
    }
}

/// Wire up the LLM enhancer for automatic comment generation.
/// Model selection: fast slot (if set) > default slot.
/// Uses resolveLlmConfigForThinking to ensure thinking models use /api/chat endpoint.
/// Logs a warning and leaves processor.enhancer null if initialisation fails.
fn setupEnhancer(
    allocator: std.mem.Allocator,
    ga: GenArgs,
    cfg: *const config_mod.ProjectConfig,
    processor: *sync_mod.SyncProcessor,
) void {
    // CLI -m flag overrides config; otherwise resolve from fast/default slots.
    const model = if (!std.mem.eql(u8, ga.model, config_mod.DEFAULT_MODEL) or ga.model_override)
        ga.model
    else
        cfg.infillModel();

    // Use centralized helper to resolve URL and thinking model settings
    var resolved_url_to_free: ?[]const u8 = null;
    const llm_config = query_engine_mod.resolveLlmConfigForThinking(
        allocator,
        cfg,
        model,
        if (ga.api_url_set) ga.api_url else null,
    ) catch {
        // Fallback to defaults
        const fallback_config: llm.LlmConfig = .{
            .api_url = ga.api_url,
            .model = model,
            .think = null,
            .debug = ga.verbose,
        };
        processor.enhancer = enhancer_mod.Enhancer.init(allocator, fallback_config) catch |init_err| {
            std.debug.print("warning: could not init LLM enhancer: {}\n", .{init_err});
            return;
        };
        processor.regen_comments = ga.regen_comments;
        return;
    };
    resolved_url_to_free = llm_config.resolved_url;

    // The returned api_url points to either resolved_url_to_free (if allocated) or a static string
    // Enhancer.init will dupe the api_url, so we can free resolved_url_to_free after init
    const api_url: []const u8 = llm_config.api_url;

    // Build final config with debug setting
    const final_config: llm.LlmConfig = .{
        .api_url = api_url,
        .model = llm_config.model,
        .think = llm_config.think,
        .debug = ga.verbose,
    };

    if (ga.verbose) std.debug.print("DEBUG: LLM config - api_url: {s}, model: {s}, think: {?}\n", .{ api_url, final_config.model, final_config.think });
    processor.enhancer = enhancer_mod.Enhancer.init(allocator, final_config) catch |err| {
        std.debug.print("warning: could not init LLM enhancer: {}\n", .{err});
        if (resolved_url_to_free) |url| allocator.free(url);
        processor.regen_comments = ga.regen_comments;
        return;
    };
    // Enhancer.init makes its own copy of api_url, so we can free our temp copy now.
    if (resolved_url_to_free) |url| allocator.free(url);
    processor.regen_comments = ga.regen_comments;

    // --- Set up thinking enhancer for module detail generation ---
    const thinking_model = cfg.thinkingModel();
    if (thinking_model.len > 0) {
        const thinking_config = query_engine_mod.resolveLlmConfigForThinking(
            allocator,
            cfg,
            thinking_model,
            if (ga.api_url_set) ga.api_url else null,
        ) catch {
            if (ga.verbose) std.debug.print("warning: could not resolve thinking model config\n", .{});
            return;
        };

        // Thinking model should use Ollama /api/chat endpoint with think=true
        const thinking_llm_config: llm.LlmConfig = .{
            .api_url = thinking_config.api_url,
            .model = thinking_config.model,
            .think = true, // Always enable thinking for detail generation
            .debug = ga.verbose,
        };

        if (ga.verbose) std.debug.print("DEBUG: Thinking model config - api_url: {s}, model: {s}\n", .{ thinking_llm_config.api_url, thinking_llm_config.model });

        processor.thinking_enhancer = enhancer_mod.Enhancer.init(allocator, thinking_llm_config) catch |err| {
            if (ga.verbose) std.debug.print("warning: could not init thinking enhancer: {}\n", .{err});
            return;
        };

        // Free resolved URL if allocated
        if (thinking_config.resolved_url) |url| allocator.free(url);
    }
}

/// Dispatch to single-file, explicit-scan, or full-workspace processing.
/// Fails fast and propagates the first error encountered.
/// Returns the count of source files processed.
fn processFiles(
    allocator: std.mem.Allocator,
    processor: *sync_mod.SyncProcessor,
    ga: GenArgs,
    paths: ResolvedGenPaths,
) !usize {
    if (ga.file) |file_arg| {
        const full_path = try llm.resolvePath(allocator, paths.workspace, file_arg);
        defer allocator.free(full_path);
        _ = try processor.processFile(full_path, ga.timeout_seconds);
        if (ga.verbose) std.debug.print("gen: processed {s}\n", .{full_path});
        return 1;
    }

    if (ga.scan) |scan_arg| {
        const scan_abs = try llm.resolvePath(allocator, paths.workspace, scan_arg);
        defer allocator.free(scan_abs);
        const count = try processor.processDirectory(scan_abs, ga.timeout_seconds);
        std.debug.print("gen: {d} source files processed from {s}\n", .{ count, scan_abs });
        return count;
    }

    // Full workspace scan: read src_dirs from config, fail fast on any error.
    var cfg = try config_mod.loadConfig(allocator, paths.workspace);
    defer cfg.deinit();

    var total: usize = 0;
    for (cfg.src_dirs) |src_rel| {
        const src_abs = try llm.resolvePath(allocator, paths.workspace, src_rel);
        defer allocator.free(src_abs);
        total += try processor.processDirectory(src_abs, ga.timeout_seconds);
    }
    std.debug.print("gen: {d} source files processed\n", .{total});
    return total;
}

/// Returns true when `db_path` is newer than every dependency that `syncGuidanceDb`
/// consumes:
///   • every .json file under `json_dir/src/`
///   • `json_dir/semantic-aliases.json`
///   • `json_dir/capability-mapping.json`
///   • `json_dir/guidance-config.json`
///   • every file under `capabilities_dir` (when non-null)
///
/// Returns false (→ sync needed) when the db is absent or any dep is newer.
fn guidanceDbIsUpToDate(
    allocator: std.mem.Allocator,
    db_path: []const u8,
    json_dir: []const u8,
    capabilities_dir: []const u8,
) bool {
    const db_mtime = marker_mod.fileMtime(db_path) orelse return false;

    // Top-level config and data files in json_dir.
    const top_level = [_][]const u8{
        "semantic-aliases.json",
        "capability-mapping.json",
        "guidance-config.json",
    };
    for (top_level) |name| {
        const p = std.fs.path.join(allocator, &.{ json_dir, name }) catch return false;
        defer allocator.free(p);
        const m = marker_mod.fileMtime(p) orelse continue; // absent → not a dep
        if (m > db_mtime) return false;
    }

    // Walk json_dir/src/ for newest JSON mtime.
    {
        const src_dir_path = std.fs.path.join(allocator, &.{ json_dir, "src" }) catch return false;
        defer allocator.free(src_dir_path);
        var src_dir = std.fs.openDirAbsolute(src_dir_path, .{ .iterate = true }) catch return false;
        defer src_dir.close();
        var walker = src_dir.walk(allocator) catch return false;
        defer walker.deinit();
        while (walker.next() catch return false) |entry| {
            if (entry.kind != .file) continue;
            if (!std.mem.endsWith(u8, entry.basename, ".json")) continue;
            const full = std.fs.path.join(allocator, &.{ src_dir_path, entry.path }) catch continue;
            defer allocator.free(full);
            const m = marker_mod.fileMtime(full) orelse continue;
            if (m > db_mtime) return false;
        }
    }

    // Walk capabilities_dir for newest mtime.
    {
        std.fs.accessAbsolute(capabilities_dir, .{}) catch return true; // absent → skip
        var cap_dir = std.fs.openDirAbsolute(capabilities_dir, .{ .iterate = true }) catch return true;
        defer cap_dir.close();
        var walker = cap_dir.walk(allocator) catch return true;
        defer walker.deinit();
        while (walker.next() catch return true) |entry| {
            if (entry.kind != .file) continue;
            const full = std.fs.path.join(allocator, &.{ capabilities_dir, entry.path }) catch continue;
            defer allocator.free(full);
            const m = marker_mod.fileMtime(full) orelse continue;
            if (m > db_mtime) return false;
        }
    }

    return true;
}

/// Sync .guidance.db (SQLite vector database with in-process cosine similarity).
/// Creates an embedding provider from config and calls lance_db.syncDatabase.
/// Failures are logged as warnings but do not abort the gen pipeline.
fn syncGuidanceDb(
    allocator: std.mem.Allocator,
    json_dir: []const u8,
    guidance_db_path: []const u8,
    cfg: *const config_mod.ProjectConfig,
    verbose: bool,
) void {
    if (guidanceDbIsUpToDate(allocator, guidance_db_path, json_dir, cfg.capabilities_dir)) {
        if (verbose) std.debug.print("gen: guidance.db is up to date, skipping\n", .{});
        return;
    }

    const embedder = vector_mod.createEmbeddingProvider(
        allocator,
        cfg.embedding_provider,
        null, // api_key — from environment, not config
        cfg.embedding_model,
        cfg.embedding_dims,
    ) catch |err| {
        std.debug.print("guidance.db: embedding provider init failed ({s}), using keyword-only\n", .{@errorName(err)});
        var noop = allocator.create(vector_mod.NoopEmbedding) catch return;
        noop.* = .{ .allocator = allocator };
        const p = noop.provider();
        lance_db_mod.syncDatabase(allocator, json_dir, guidance_db_path, p, null, null, cfg.embedding_cache_limit) catch |se| {
            std.debug.print("guidance.db: sync failed: {s}\n", .{@errorName(se)});
        };
        p.deinit();
        return;
    };
    defer embedder.deinit();

    stepPrint("gen: guidance.db ({s})\n", .{embedder.getName()});
    if (verbose) std.debug.print("gen: syncing guidance.db to {s}\n", .{guidance_db_path});

    // cfg.capabilities_dir is now an absolute path (resolved in config loader).
    const cap_dir_abs: ?[]const u8 = blk: {
        std.fs.accessAbsolute(cfg.capabilities_dir, .{}) catch break :blk null;
        break :blk allocator.dupe(u8, cfg.capabilities_dir) catch break :blk null;
    };
    defer if (cap_dir_abs) |p| allocator.free(p);

    // Load semantic aliases for embedding-based query steering
    const aliases_path = std.fs.path.join(allocator, &.{ json_dir, "semantic-aliases.json" }) catch null;
    defer if (aliases_path) |p| allocator.free(p);

    var aliases: ?lance_db_mod.SemanticAliases = if (aliases_path) |path|
        lance_db_mod.loadSemanticAliases(allocator, path) catch null
    else
        null;
    defer if (aliases) |*a| a.deinit();

    lance_db_mod.syncDatabase(allocator, json_dir, guidance_db_path, embedder, cap_dir_abs, aliases, cfg.embedding_cache_limit) catch |err| {
        std.debug.print("guidance.db: sync failed: {s}\n", .{@errorName(err)});
        return;
    };

    if (verbose) std.debug.print("gen: guidance.db written to {s}\n", .{guidance_db_path});
}

/// Extract keywords from all guidance JSON files, rank by frequency, and generate
/// optimized semantic aliases using LLM consolidation.
/// NOTE: This preserves existing semantic-aliases.json if it exists.
fn generateSemanticAliases(guidance_dir: []const u8, verbose: bool) !void {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    // Check if semantic-aliases.json already exists
    const aliases_path = try std.fs.path.join(allocator, &.{ guidance_dir, "semantic-aliases.json" });
    defer allocator.free(aliases_path);

    std.fs.cwd().access(aliases_path, .{}) catch |err| {
        if (err == error.FileNotFound) {
            if (verbose) std.debug.print("semantic-aliases: {s} not found, using default aliases\n", .{aliases_path});
            // Create a minimal default aliases file
            // Most projects should hand-curate their aliases
        }
        return;
    };

    if (verbose) std.debug.print("semantic-aliases: using existing {s}\n", .{aliases_path});
}

pub fn cmdGen(allocator: std.mem.Allocator, args: []const []const u8) !void {
    const ga = GenArgs.parse(args) catch |err| {
        std.debug.print("error: gen flag missing value ({s})\n", .{@errorName(err)});
        return err;
    };
    try cmdGenImpl(allocator, ga);
}

/// Core gen implementation shared by `gen` and `check`.
///
/// Pipeline (per source file):
///   incremental check → test (once/language) → lint → fmt → guidance → touch JSON
///
/// Incremental detection is always active: a file is skipped when its guidance
/// JSON is at least as new as the source.  Pass `ga.force = true` to override.
/// The guidance JSON is always touched after successful processing so its mtime
/// acts as the universal "all phases passed" marker across all languages.
fn cmdGenImpl(allocator: std.mem.Allocator, ga: GenArgs) !void {
    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    var paths = try resolveGenPaths(allocator, ga, cwd);
    defer paths.deinit(allocator);

    if (ga.verbose) {
        std.debug.print("guidance gen:\n  workspace: {s}\n  json_dir:  {s}\n  db_path:   {s}\n", .{
            paths.workspace, paths.json_dir, paths.db_path,
        });
    }

    // Load config for test/lint/fmt commands.
    var cfg = config_mod.loadConfig(allocator, paths.workspace) catch
        try config_mod.loadConfig(allocator, cwd);
    defer cfg.deinit();

    // ── Optional comment sync pre-pass ────────────────────────────────────────
    // Run CommentSyncProcessor over the target file(s) before JSON generation.
    // This inserts/updates /// doc comments in source files so they are
    // captured in the subsequent JSON sync pass.
    //
    // Condition: run when --sync-comments is explicitly passed, OR when AI is
    // not explicitly disabled (--no-ai).  When the LLM is unreachable,
    // generateMemberComment returns null and no changes are made (no-op).
    if (ga.sync_comments or !ga.no_ai) {
        var csp = comment_sync_mod.CommentSyncProcessor.init(
            allocator,
            paths.workspace,
            paths.json_dir,
            ga.verbose,
            ga.dry_run,
        );
        csp.generate_headers = ga.sync_headers;
        setupCspEnhancer(allocator, ga, &cfg, &csp);
        defer teardownCspEnhancer(allocator, &csp);

        if (ga.file) |file_arg| {
            const src_abs = try llm.resolvePath(allocator, paths.workspace, file_arg);
            defer allocator.free(src_abs);
            _ = csp.processFile(src_abs) catch |err| {
                if (ga.verbose) std.debug.print("[sync-comments] {s}: {s}\n", .{ src_abs, @errorName(err) });
            };
        } else {
            const src_scan_dir = try std.fs.path.join(allocator, &.{ paths.workspace, "src" });
            defer allocator.free(src_scan_dir);
            var dir = std.fs.openDirAbsolute(src_scan_dir, .{ .iterate = true }) catch null;
            if (dir) |*d| {
                defer d.close();
                var walker = try d.walk(allocator);
                defer walker.deinit();
                while (try walker.next()) |entry| {
                    if (entry.kind != .file) continue;
                    if (!std.mem.endsWith(u8, entry.path, ".zig")) continue;
                    const abs = try std.fs.path.join(allocator, &.{ src_scan_dir, entry.path });
                    defer allocator.free(abs);
                    _ = csp.processFile(abs) catch continue;
                }
            }
        }
    }

    var processor = sync_mod.SyncProcessor.init(
        allocator,
        paths.workspace,
        paths.json_dir,
        ga.dry_run,
        ga.verbose,
    );
    defer processor.deinit();
    setupEnhancer(allocator, ga, &cfg, &processor);

    // ── Single-file mode ──────────────────────────────────────────────────────
    if (ga.file) |file_arg| {
        const src_abs = try llm.resolvePath(allocator, paths.workspace, file_arg);
        defer allocator.free(src_abs);

        const json_path = try guidanceJsonPath(allocator, paths.workspace, paths.json_dir, src_abs);
        defer allocator.free(json_path);

        if (!ga.force and !marker_mod.fileNeedsProcessing(src_abs, json_path)) {
            if (ga.verbose) std.debug.print("gen: {s} is up to date\n", .{src_abs});
        } else {
            const ext = std.fs.path.extension(src_abs);
            // Only .zig files are handled by the built-in Zig AST pipeline.
            // .md and other files registered in the plugin registry go through
            // the provider/plugin path below, not through processFile (Zig-only).
            const is_zig_builtin = std.mem.eql(u8, ext, ".zig");
            if (is_zig_builtin) {
                const ok = try runBuiltinFilePipeline(
                    allocator,
                    &cfg,
                    &processor,
                    src_abs,
                    ga,
                );
                if (!ok) return error.LintFailed;
            } else if (ga.all_languages) {
                if (try provider_mod.discoverProvider(allocator, paths.workspace, ext)) |prov| {
                    defer prov.deinit(allocator);
                    _ = try provider_mod.invokeProviderFile(
                        allocator,
                        prov,
                        src_abs,
                        paths.json_dir,
                        &.{},
                    );
                }
            }
        }

        if (ga.dry_run) {
            std.debug.print("(dry-run — no files written)\n", .{});
            return;
        }
        if (ga.compile_db) {
            syncGuidanceDb(allocator, paths.json_dir, paths.db_path, &cfg, ga.verbose);
        }
        return;
    }

    // ── Explicit scan-dir mode  (--scan) ─────────────────────────────────────
    if (ga.scan) |scan_arg| {
        const scan_abs = try llm.resolvePath(allocator, paths.workspace, scan_arg);
        defer allocator.free(scan_abs);

        // Only collect .zig files for the built-in Zig AST pipeline.
        // .md files use the MarkdownPlugin path, not the Zig AST parser.
        const builtin_exts = [_][]const u8{".zig"};
        const zig_files = try collectFilesWithExts(allocator, scan_abs, &builtin_exts);
        defer {
            for (zig_files) |p| allocator.free(p);
            allocator.free(zig_files);
        }

        // Collect stale files only.
        var stale: std.ArrayList([]const u8) = .{};
        defer stale.deinit(allocator);
        for (zig_files) |src_abs| {
            const json_path = try guidanceJsonPath(allocator, paths.workspace, paths.json_dir, src_abs);
            defer allocator.free(json_path);
            if (ga.force or marker_mod.fileNeedsProcessing(src_abs, json_path))
                try stale.append(allocator, src_abs);
        }

        if (stale.items.len > 0) {
            try runBuiltinLanguagePipeline(allocator, &cfg, &processor, "zig", stale.items, zig_files, paths.json_dir, ga);
        } else {
            if (ga.verbose) std.debug.print("gen: all {d} built-in file(s) up to date\n", .{zig_files.len});
        }

        if (ga.verbose) std.debug.print("gen: {d}/{d} file(s) processed from {s}\n", .{
            stale.items.len, zig_files.len, scan_abs,
        });

        if (ga.dry_run) {
            std.debug.print("(dry-run — no files written)\n", .{});
            return;
        }
        if (ga.compile_db) {
            syncGuidanceDb(allocator, paths.json_dir, paths.db_path, &cfg, ga.verbose);
        }
        return;
    }

    // ── Full workspace scan (default) ─────────────────────────────────────────
    // Group source files by built-in vs. external so each language runs its
    // own test suite exactly once, before per-file lint/fmt/guidance.

    // Built-in language: Zig only. The Zig AST pipeline uses AstParser which
    // only understands Zig syntax. .md files are registered in the plugin
    // registry (MarkdownPlugin) and processed via the external-provider path.
    const builtin_exts = [_][]const u8{".zig"};
    {
        var all_builtin: std.ArrayList([]const u8) = .{};
        defer {
            for (all_builtin.items) |p| allocator.free(p);
            all_builtin.deinit(allocator);
        }
        for (cfg.src_dirs) |src_rel| {
            const src_abs = try llm.resolvePath(allocator, paths.workspace, src_rel);
            defer allocator.free(src_abs);
            const files = try collectFilesWithExts(allocator, src_abs, &builtin_exts);
            defer allocator.free(files);
            for (files) |p| try all_builtin.append(allocator, p);
            // Note: `p` is now owned by `all_builtin`; `files` slice freed above.
        }

        // Filter to stale only.
        var stale: std.ArrayList([]const u8) = .{};
        defer stale.deinit(allocator);
        for (all_builtin.items) |src_abs| {
            const json_path = try guidanceJsonPath(
                allocator,
                paths.workspace,
                paths.json_dir,
                src_abs,
            );
            defer allocator.free(json_path);
            if (ga.force or marker_mod.fileNeedsProcessing(src_abs, json_path))
                try stale.append(allocator, src_abs);
        }

        if (stale.items.len > 0) {
            stepPrint("gen: {d}/{d} zig files stale\n", .{ stale.items.len, all_builtin.items.len });
            try runBuiltinLanguagePipeline(allocator, &cfg, &processor, "zig", stale.items, all_builtin.items, paths.json_dir, ga);
        } else {
            stepPrint("gen: all {d} zig files up to date\n", .{all_builtin.items.len});
        }
    }

    // External providers (e.g. guidance-py for .py files).
    if (ga.all_languages) {
        // Collect every distinct extension found in src_dirs that is NOT built-in.
        var foreign_exts: std.StringHashMapUnmanaged(void) = .{};
        defer foreign_exts.deinit(allocator);

        for (cfg.src_dirs) |src_rel| {
            const src_abs = try llm.resolvePath(allocator, paths.workspace, src_rel);
            defer allocator.free(src_abs);

            var dir = std.fs.openDirAbsolute(src_abs, .{ .iterate = true }) catch continue;
            defer dir.close();
            var walker = try dir.walk(allocator);
            defer walker.deinit();

            while (try walker.next()) |entry| {
                if (entry.kind != .file) continue;
                const ext = std.fs.path.extension(entry.basename);
                if (ext.len == 0) continue;
                // Skip built-in extensions.
                const is_builtin = for (builtin_exts) |be| {
                    if (std.mem.eql(u8, ext, be)) break true;
                } else false;
                if (is_builtin) continue;
                // Check whether any file with this extension is stale before
                // recording the extension (avoids probing providers unnecessarily).
                const file_abs = try std.fs.path.join(allocator, &.{ src_abs, entry.path });
                defer allocator.free(file_abs);
                const json_path = try guidanceJsonPath(
                    allocator,
                    paths.workspace,
                    paths.json_dir,
                    file_abs,
                );
                defer allocator.free(json_path);
                if (ga.force or marker_mod.fileNeedsProcessing(file_abs, json_path)) {
                    if (!foreign_exts.contains(ext)) {
                        try foreign_exts.put(allocator, try allocator.dupe(u8, ext), {});
                    }
                }
            }
        }

        // Invoke one provider per stale extension group via --scan.
        var ext_it = foreign_exts.keyIterator();
        while (ext_it.next()) |ext_ptr| {
            const ext = ext_ptr.*;
            defer allocator.free(ext);
            const prov_opt = try provider_mod.discoverProvider(allocator, paths.workspace, ext);
            if (prov_opt == null) {
                if (ga.verbose) std.debug.print("gen: no provider found for {s} — skipping\n", .{ext});
                continue;
            }
            const prov = prov_opt.?;
            defer prov.deinit(allocator);

            // Invoke provider once per src_dir that contains stale files of this extension.
            for (cfg.src_dirs) |src_rel| {
                const src_abs = try llm.resolvePath(allocator, paths.workspace, src_rel);
                defer allocator.free(src_abs);
                if (ga.verbose) std.debug.print("gen: invoking {s} provider for {s} in {s}\n", .{
                    prov.name, ext, src_abs,
                });
                _ = try provider_mod.invokeProviderScan(
                    allocator,
                    prov,
                    src_abs,
                    paths.json_dir,
                    &.{},
                );
            }
        }
    }

    if (ga.dry_run) {
        std.debug.print("(dry-run — no files written)\n", .{});
        return;
    }

    if (ga.compile_db) {
        // Generate semantic aliases from keyword frequency analysis
        // json_dir is the .guidance directory
        generateSemanticAliases(paths.json_dir, ga.verbose) catch |err| {
            std.debug.print("warning: semantic alias generation failed: {s}\n", .{@errorName(err)});
        };
        syncGuidanceDb(allocator, paths.json_dir, paths.db_path, &cfg, ga.verbose);
    }
}

// =============================================================================
// status
// =============================================================================

pub fn cmdStatus(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var json_dir_arg: ?[]const u8 = null;
    var db_path_arg: ?[]const u8 = null;
    var verbose = false; // verbose defaults false; overridden by --verbose arg
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--json-dir")) {
            i += 1;
            if (i >= args.len) return;
            json_dir_arg = args[i];
        } else if (std.mem.eql(u8, arg, "-o") or std.mem.eql(u8, arg, "--db")) {
            i += 1;
            if (i >= args.len) return;
            db_path_arg = args[i];
        } else if (std.mem.eql(u8, arg, "--verbose") or std.mem.eql(u8, arg, "--debug")) {
            verbose = true;
        }
    }

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const json_dir = try llm.resolvePath(allocator, cwd, json_dir_arg orelse config_mod.DEFAULT_GUIDANCE_DIR);
    defer allocator.free(json_dir);

    const db_path = try llm.resolvePath(allocator, cwd, db_path_arg orelse config_mod.DEFAULT_GUIDANCE_DB_PATH);
    defer allocator.free(db_path);

    // Count JSON files in json_dir/src/.
    const json_src_dir = try std.fs.path.join(allocator, &.{ json_dir, "src" });
    defer allocator.free(json_src_dir);

    var json_count: usize = 0;
    if (std.fs.openDirAbsolute(json_src_dir, .{ .iterate = true })) |*jdir_ptr| {
        var jdir = jdir_ptr.*;
        defer jdir.close();
        var walker = try jdir.walk(allocator);
        defer walker.deinit();
        while (try walker.next()) |entry| {
            if (entry.kind == .file and std.mem.endsWith(u8, entry.path, ".json"))
                json_count += 1;
        }
    } else |_| {}

    const db_exists = if (std.fs.openFileAbsolute(db_path, .{})) |f| blk: {
        f.close();
        break :blk true;
    } else |_| false;

    std.debug.print("guidance status:\n", .{});
    std.debug.print("  json_dir:   {s}\n", .{json_dir});
    std.debug.print("  json files: {d}\n", .{json_count});
    std.debug.print("  db_path:    {s}\n", .{db_path});
    std.debug.print("  db_exists:  {}\n", .{db_exists});

    // Show embedding statistics if database exists and --verbose is set
    if (db_exists and verbose) {
        var noop: vector_mod.NoopEmbedding = .{};
        var db = GuidanceDb.init(allocator, db_path, noop.provider()) catch |err| {
            std.debug.print("  (could not open db: {})\n", .{err});
            return;
        };
        defer db.deinit();

        if (db.getEmbeddingStats()) |stats| {
            std.debug.print("  embeddings:\n", .{});
            std.debug.print("    ast_nodes:      {d}\n", .{stats.ast_nodes_with_embeddings});
            std.debug.print("    alias:          {d}\n", .{stats.alias_embeddings});
            std.debug.print("    keywords:       {d}\n", .{stats.keyword_embeddings});
            std.debug.print("    embedding_cache: {d}\n", .{stats.embedding_cache_entries});
        } else {
            std.debug.print("  (could not read embedding stats)\n", .{});
        }
    }
}

// =============================================================================
// clean
// =============================================================================

pub fn cmdClean(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var json_dir_arg: ?[]const u8 = null;
    var db_path_arg: ?[]const u8 = null;
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--json-dir")) {
            i += 1;
            if (i >= args.len) return;
            json_dir_arg = args[i];
        } else if (std.mem.eql(u8, arg, "-o") or std.mem.eql(u8, arg, "--db")) {
            i += 1;
            if (i >= args.len) return;
            db_path_arg = args[i];
        }
    }

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const json_dir = try llm.resolvePath(allocator, cwd, json_dir_arg orelse config_mod.DEFAULT_GUIDANCE_DIR);
    defer allocator.free(json_dir);

    const db_path = try llm.resolvePath(allocator, cwd, db_path_arg orelse config_mod.DEFAULT_GUIDANCE_DB_PATH);
    defer allocator.free(db_path);

    // Remove the database.
    std.fs.deleteFileAbsolute(db_path) catch |err| {
        if (err != error.FileNotFound)
            std.debug.print("warning: could not remove {s}: {}\n", .{ db_path, err });
    };
    std.debug.print("clean: removed {s}\n", .{db_path});

    // Remove the generated JSON src tree only (preserve config and skills).
    const json_src = try std.fs.path.join(allocator, &.{ json_dir, "src" });
    defer allocator.free(json_src);
    std.fs.deleteTreeAbsolute(json_src) catch |err| {
        if (err != error.FileNotFound)
            std.debug.print("warning: could not remove {s}: {}\n", .{ json_src, err });
    };
    std.debug.print("clean: removed {s}\n", .{json_src});
}

// =============================================================================
// Pipeline helpers — test → lint → fmt → guidance → touch
// =============================================================================

/// Extracts a JSON path from a Zig source file using an allocator and workspace parameters.
pub fn guidanceJsonPath(
    allocator: std.mem.Allocator,
    workspace: []const u8,
    json_dir: []const u8,
    src_abs: []const u8,
) ![]const u8 {
    const rel: []const u8 = if (std.mem.startsWith(u8, src_abs, workspace)) blk: {
        const stripped = src_abs[workspace.len..];
        break :blk if (stripped.len > 0 and stripped[0] == '/') stripped[1..] else stripped;
    } else src_abs;
    return std.fmt.allocPrint(allocator, "{s}/{s}.json", .{ json_dir, rel });
}

// ---------------------------------------------------------------------------
// map-capabilities — regenerate capability-mapping.json from CAPABILITY.md files
// ---------------------------------------------------------------------------

/// Map CAPABILITY.md files to source files by analysing AST JSON content.
/// Updates .guidance/capability-mapping.json with fresh keyword and source mappings.
/// Preserves the "mapping" (file→capability) section; regenerates "capability_keywords".
///
/// Usage: guidance map-capabilities [--guidance-dir DIR] [--workspace DIR] [--dry-run]
pub fn cmdMapCapabilities(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var guidance_dir_arg: []const u8 = config_mod.DEFAULT_GUIDANCE_DIR;
    var workspace: ?[]const u8 = null;
    var dry_run = false;
    var i: usize = 0;

    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--guidance-dir") or std.mem.eql(u8, arg, "-g")) {
            i += 1;
            if (i >= args.len) return error.MissingValue;
            guidance_dir_arg = args[i];
        } else if (std.mem.eql(u8, arg, "--workspace") or std.mem.eql(u8, arg, "-w")) {
            i += 1;
            if (i >= args.len) return error.MissingValue;
            workspace = args[i];
        } else if (std.mem.eql(u8, arg, "--dry-run")) {
            dry_run = true;
        }
    }

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const ws = workspace orelse cwd;
    const guidance_abs = try llm.resolvePath(allocator, ws, guidance_dir_arg);
    defer allocator.free(guidance_abs);

    const cap_dir = try std.fs.path.join(allocator, &.{ guidance_abs, "capabilities" });
    defer allocator.free(cap_dir);

    const mapping_path = try std.fs.path.join(allocator, &.{ guidance_abs, "capability-mapping.json" });
    defer allocator.free(mapping_path);

    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    const fa = arena.allocator();

    // ------------------------------------------------------------------
    // Step 1: Walk capabilities dir, extract keywords from each CAPABILITY.md.
    // ------------------------------------------------------------------
    var new_cap_keywords: std.StringHashMapUnmanaged([]const []const u8) = .{};
    defer new_cap_keywords.deinit(fa);

    var cap_d = std.fs.cwd().openDir(cap_dir, .{ .iterate = true }) catch |err| {
        std.debug.print("map-capabilities: cannot open {s}: {s}\n", .{ cap_dir, @errorName(err) });
        return;
    };
    defer cap_d.close();

    var walker = try cap_d.walk(fa);
    defer walker.deinit();

    var cap_count: usize = 0;

    while (try walker.next()) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.path, "CAPABILITY.md")) continue;

        const abs_path = try std.fmt.allocPrint(fa, "{s}/{s}", .{ cap_dir, entry.path });
        const content = std.fs.cwd().readFileAlloc(fa, abs_path, 512 * 1024) catch continue;

        // Extract capability name from frontmatter.
        var cap_name: []const u8 = std.fs.path.basename(std.fs.path.dirname(abs_path) orelse abs_path);
        var body: []const u8 = content;
        if (std.mem.startsWith(u8, content, "---")) {
            const end = std.mem.indexOf(u8, content[3..], "\n---") orelse 0;
            if (end > 0) {
                body = content[end + 7 ..];
                var fm_it = std.mem.splitScalar(u8, content[3 .. end + 3], '\n');
                while (fm_it.next()) |line| {
                    const trimmed = std.mem.trim(u8, line, " \r");
                    if (std.mem.startsWith(u8, trimmed, "name:")) {
                        cap_name = std.mem.trim(u8, trimmed["name:".len..], " ");
                        break;
                    }
                }
            }
        }

        // Extract code-fence identifiers: backtick-delimited tokens that look like
        // Zig/Python identifiers (camelCase, PascalCase, snake_case, no punctuation).
        var kw_set: std.StringHashMapUnmanaged(void) = .{};
        defer kw_set.deinit(fa);

        // Scan backtick spans and identifier-like tokens from the body.
        var tok_it = std.mem.tokenizeAny(u8, body, " \t\n\r`()[]{}:,;\"'");
        while (tok_it.next()) |tok| {
            if (tok.len < 2 or tok.len > 80) continue;
            if (!isCapabilityKeywordToken(tok)) continue;
            if (!kw_set.contains(tok)) {
                try kw_set.put(fa, try fa.dupe(u8, tok), {});
            }
        }

        var kw_list: std.ArrayList([]const u8) = .{};
        var kwit = kw_set.keyIterator();
        while (kwit.next()) |k| {
            try kw_list.append(fa, k.*);
        }

        try new_cap_keywords.put(fa, try fa.dupe(u8, cap_name), try kw_list.toOwnedSlice(fa));
        cap_count += 1;
    }

    // ------------------------------------------------------------------
    // Step 2: Load existing mapping to preserve "mapping" section.
    //         Merge: keep existing capability_keywords, add new capabilities.
    // ------------------------------------------------------------------
    var existing_mapping: std.StringHashMapUnmanaged(std.json.Value) = .{};
    defer existing_mapping.deinit(fa);

    var existing_cap_keywords: std.StringHashMapUnmanaged(std.json.Value) = .{};
    defer existing_cap_keywords.deinit(fa);

    const existing_content = std.fs.cwd().readFileAlloc(fa, mapping_path, 2 * 1024 * 1024) catch null;
    if (existing_content) |ec| {
        if (std.json.parseFromSlice(std.json.Value, fa, ec, .{ .ignore_unknown_fields = true })) |parsed| {
            defer parsed.deinit();
            if (parsed.value == .object) {
                // Preserve existing "mapping" section as-is.
                if (parsed.value.object.get("mapping")) |m| {
                    if (m == .object) {
                        var mit = m.object.iterator();
                        while (mit.next()) |mentry| {
                            try existing_mapping.put(fa, mentry.key_ptr.*, mentry.value_ptr.*);
                        }
                    }
                }
                // Preserve existing capability_keywords (hand-crafted).
                if (parsed.value.object.get("capability_keywords")) |ck| {
                    if (ck == .object) {
                        var ckit = ck.object.iterator();
                        while (ckit.next()) |ckentry| {
                            try existing_cap_keywords.put(fa, ckentry.key_ptr.*, ckentry.value_ptr.*);
                        }
                    }
                }
            }
        } else |_| {
            std.debug.print("map-capabilities: cannot parse existing {s}, preserving\n", .{mapping_path});
        }
    }

    // Merge: for capabilities without existing keywords, use extracted ones.
    var it = new_cap_keywords.iterator();
    while (it.next()) |entry| {
        const cap_name = entry.key_ptr.*;
        if (!existing_cap_keywords.contains(cap_name)) {
            // Build a json array from the extracted keyword list.
            var arr = std.json.Array.init(fa);
            for (entry.value_ptr.*) |kw| {
                try arr.append(.{ .string = kw });
            }
            try existing_cap_keywords.put(fa, cap_name, .{ .array = arr });
            std.debug.print("map-capabilities: added keywords for new capability '{s}'\n", .{cap_name});
        }
    }

    // ------------------------------------------------------------------
    // Step 3: Build output JSON object and write.
    // ------------------------------------------------------------------
    var mapping_obj_out = std.json.ObjectMap.init(fa);
    var file_to_caps_obj = std.json.ObjectMap.init(fa);
    {
        var mit = existing_mapping.iterator();
        while (mit.next()) |mentry| {
            try file_to_caps_obj.put(mentry.key_ptr.*, mentry.value_ptr.*);
        }
    }
    var cap_kw_obj = std.json.ObjectMap.init(fa);
    {
        var ckit = existing_cap_keywords.iterator();
        while (ckit.next()) |ckentry| {
            try cap_kw_obj.put(ckentry.key_ptr.*, ckentry.value_ptr.*);
        }
    }
    try mapping_obj_out.put("mapping", .{ .object = file_to_caps_obj });
    try mapping_obj_out.put("capability_keywords", .{ .object = cap_kw_obj });

    const json_out = try llm.jsonStringifyAlloc(fa, std.json.Value{ .object = mapping_obj_out });

    if (dry_run) {
        std.debug.print("map-capabilities (dry-run): would write {d} bytes to {s}\n", .{ json_out.len, mapping_path });
        return;
    }

    const file = try std.fs.cwd().createFile(mapping_path, .{});
    defer file.close();
    var wbuf: [8192]u8 = undefined;
    var fw = file.writer(&wbuf);
    try fw.interface.writeAll(json_out);
    try fw.interface.flush();

    std.debug.print("map-capabilities: wrote {s} ({d} capabilities)\n", .{ mapping_path, cap_count });
}

/// Return true if `tok` is a plausible Zig/Python identifier keyword.
/// Accepts: camelCase, PascalCase, snake_case, dotted.paths, but not
/// punctuation-heavy strings or all-lowercase common words.
fn isCapabilityKeywordToken(tok: []const u8) bool {
    // Must start with an ASCII letter or underscore.
    if (tok.len == 0) return false;
    if (!std.ascii.isAlphabetic(tok[0]) and tok[0] != '_') return false;

    var has_upper = false;
    var has_underscore = false;
    var has_dot = false;

    for (tok) |ch| {
        if (std.ascii.isUpper(ch)) has_upper = true;
        if (ch == '_') has_underscore = true;
        if (ch == '.') has_dot = true;
        // Reject if contains non-identifier characters (except _ and .).
        if (!std.ascii.isAlphanumeric(ch) and ch != '_' and ch != '.') return false;
    }

    // Accept: camelCase/PascalCase (has uppercase), snake_case (has underscore),
    // dotted paths (has dot), or short known identifiers (len >= 4).
    return has_upper or has_underscore or has_dot or tok.len >= 4;
}

/// Run an arbitrary command (full argv, no template substitution).
/// Returns true on exit code 0.
fn runCommand(allocator: std.mem.Allocator, argv: []const []const u8) !bool {
    var child = std.process.Child.init(argv, allocator);
    child.stdin_behavior = .Ignore;
    child.stdout_behavior = .Inherit;
    child.stderr_behavior = .Inherit;
    try child.spawn();
    const term = try child.wait();
    return term == .Exited and term.Exited == 0;
}

/// Substitute `{file}` tokens in `argv_template` with `file_path`, then run
/// the resulting command via `runCommand`.
///
/// Returns true on exit code 0, false otherwise.
fn runPhaseCommand(
    allocator: std.mem.Allocator,
    argv_template: []const []const u8,
    file_path: []const u8,
) !bool {
    var argv: std.ArrayList([]const u8) = .{};
    defer argv.deinit(allocator);
    for (argv_template) |tok| {
        try argv.append(allocator, if (std.mem.eql(u8, tok, "{file}")) file_path else tok);
    }
    return runCommand(allocator, argv.items);
}

/// Walk `dir_abs` recursively and collect all files whose extension matches
/// any member of `exts` (e.g. `.{".zig"}`).
/// Returns an owned slice of owned absolute paths; caller must free each and
/// then the slice.
fn collectFilesWithExts(
    allocator: std.mem.Allocator,
    dir_abs: []const u8,
    exts: []const []const u8,
) ![][]const u8 {
    var results: std.ArrayList([]const u8) = .{};
    errdefer {
        for (results.items) |p| allocator.free(p);
        results.deinit(allocator);
    }

    var dir = std.fs.openDirAbsolute(dir_abs, .{ .iterate = true }) catch return results.toOwnedSlice(allocator);
    defer dir.close();

    var walker = try dir.walk(allocator);
    defer walker.deinit();

    while (try walker.next()) |entry| {
        if (entry.kind != .file) continue;
        const ext = std.fs.path.extension(entry.basename);
        const matched = for (exts) |e| {
            if (std.mem.eql(u8, ext, e)) break true;
        } else false;
        if (!matched) continue;
        const full = try std.fs.path.join(allocator, &.{ dir_abs, entry.path });
        try results.append(allocator, full);
    }

    return results.toOwnedSlice(allocator);
}

/// Run the per-file pipeline for one built-in source file:
///   1. lint  (if configured and not skip_lint)
///   2. fmt   (if configured and not skip_fmt)
///   3. guidance  (AST → JSON via SyncProcessor)
///   4. touch JSON  (always — makes JSON mtime the universal "all phases passed" marker)
///
/// Formatting runs AFTER lint so that:
///   - Semantic lint errors are caught before any file modification.
///   - The formatter normalises whitespace before the AST is parsed, ensuring
///     line numbers in the JSON are stable and accurate.
///
/// Returns false when lint fails (caller should abort the batch).
fn runBuiltinFilePipeline(
    allocator: std.mem.Allocator,
    cfg: *const config_mod.ProjectConfig,
    processor: *sync_mod.SyncProcessor,
    src_abs: []const u8,
    ga: GenArgs,
) !bool {
    const ext = std.fs.path.extension(src_abs);

    // ── 1. Lint (with one auto-fix attempt via fmt) ───────────────────────
    if (!ga.skip_lint) {
        if (cfg.lintCommandForExt(ext)) |lint_argv| {
            if (ga.verbose) std.debug.print("lint:     {s}\n", .{src_abs});
            const ok = try runPhaseCommand(allocator, lint_argv, src_abs);
            if (!ok) {
                // One attempt to auto-fix via the fmt command.
                const fixed = if (!ga.skip_fmt)
                    if (cfg.fmtCommandForExt(ext)) |fmt_argv| blk: {
                        if (ga.verbose) std.debug.print("lint-fix: {s}\n", .{src_abs});
                        _ = try runPhaseCommand(allocator, fmt_argv, src_abs);
                        break :blk try runPhaseCommand(allocator, lint_argv, src_abs);
                    } else false
                else
                    false;
                if (!fixed) {
                    std.debug.print("error: lint failed for {s}\n", .{src_abs});
                    return false;
                }
                if (ga.verbose) std.debug.print("lint-fix: fixed {s}\n", .{src_abs});
            }
        }
    }

    // ── 2. Format ─────────────────────────────────────────────────────────
    if (!ga.skip_fmt) {
        if (cfg.fmtCommandForExt(ext)) |fmt_argv| {
            if (ga.verbose) std.debug.print("fmt:      {s}\n", .{src_abs});
            _ = try runPhaseCommand(allocator, fmt_argv, src_abs);
        }
    }

    // ── 3. Guidance ───────────────────────────────────────────────────────
    // processFile writes the JSON unconditionally (merge + save), which
    // advances the file's mtime naturally — no separate touch needed.
    // Touching would truncate the file we just wrote.
    _ = processor.processFile(src_abs, ga.timeout_seconds) catch |err| {
        std.debug.print("warning: guidance failed for {s}: {s}\n", .{ src_abs, @errorName(err) });
        // Leave JSON stale on failure so the next run retries this file.
        return true;
    };

    return true;
}

/// Run the full built-in pipeline over a set of source files that share the
/// same language group:
///   1. Test suite (once, if any file is stale relative to the test marker)
///   2. Per-file: lint → fmt → guidance → touch JSON
///
/// `language` is a short tag (e.g. "zig") used for the test marker path.
/// `stale_files` are the files that need processing (source newer than JSON).
/// `all_files` are ALL source files for this language (used for test marker check).
/// `guidance_root` is the absolute path to the guidance directory (e.g. .guidance).
fn runBuiltinLanguagePipeline(
    allocator: std.mem.Allocator,
    cfg: *const config_mod.ProjectConfig,
    processor: *sync_mod.SyncProcessor,
    language: []const u8,
    stale_files: []const []const u8,
    all_files: []const []const u8,
    guidance_root: []const u8,
    ga: GenArgs,
) !void {
    if (stale_files.len == 0) return;

    // ── Test suite (once per language group, before any file is modified) ─
    // Derive extension from language (e.g. "zig" -> ".zig")
    var ext_buf: [32]u8 = undefined;
    const ext = std.fmt.bufPrint(&ext_buf, ".{s}", .{language}) catch language;

    if (!ga.skip_tests) {
        const test_argv = cfg.testCommandForExt(ext);
        if (test_argv) |argv| {
            // Check if we can skip tests (marker newer than ALL source files)
            const marker_path = marker_mod.testMarkerPath(allocator, guidance_root) catch
                return error.OutOfMemory;
            defer allocator.free(marker_path);

            const can_skip = !ga.force and marker_mod.testsCanBeSkipped(marker_path, all_files);
            if (can_skip) {
                if (ga.verbose) std.debug.print("test:     {s} skipped (test_passed marker is fresh)\n", .{language});
            } else {
                stepPrint("test: {s} ({d} changed)\n", .{ language, stale_files.len });
                const ok = try runCommand(allocator, argv);
                if (!ok) {
                    std.debug.print("error: test suite failed for language '{s}'\n", .{language});
                    return error.TestFailed;
                }
                // Touch the marker to record successful test run
                marker_mod.touchTestMarker(marker_path) catch |err| {
                    std.debug.print("warning: could not create test_passed marker: {s}\n", .{@errorName(err)});
                };
                stepPrint("test: passed\n", .{});
            }
        } else {
            if (ga.verbose) std.debug.print("test:     skipped (no test command for {s})\n", .{language});
        }
    }

    // ── Per-file phases ───────────────────────────────────────────────────
    // Process every stale file so all lint failures are reported before exiting.
    var any_lint_failed = false;
    for (stale_files) |src_abs| {
        const ok = try runBuiltinFilePipeline(allocator, cfg, processor, src_abs, ga);
        if (!ok) any_lint_failed = true;
    }
    if (any_lint_failed) return error.LintFailed;
}

// =============================================================================
// sync-comments — insert/update /// doc comments in source files
// =============================================================================

pub fn cmdSyncComments(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var workspace: ?[]const u8 = null;
    var guidance_dir: ?[]const u8 = null;
    var single_file: ?[]const u8 = null;
    var dry_run = false;
    var debug_mode = false;
    var gen_headers = false;
    var no_ai = false;
    var api_url: []const u8 = config_mod.DEFAULT_API_URL;
    var api_url_set = false;
    var model: []const u8 = config_mod.DEFAULT_MODEL;
    var model_override = false;

    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "-w") or std.mem.eql(u8, arg, "--workspace")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("error: --workspace requires a value\n", .{});
                return;
            }
            workspace = args[i];
        } else if (std.mem.eql(u8, arg, "--guidance-dir")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("error: --guidance-dir requires a value\n", .{});
                return;
            }
            guidance_dir = args[i];
        } else if (std.mem.eql(u8, arg, "--file")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("error: --file requires a value\n", .{});
                return;
            }
            single_file = args[i];
        } else if (std.mem.eql(u8, arg, "--dry-run")) {
            dry_run = true;
        } else if (std.mem.eql(u8, arg, "--verbose") or std.mem.eql(u8, arg, "--debug")) {
            debug_mode = true;
        } else if (std.mem.eql(u8, arg, "--headers")) {
            gen_headers = true;
        } else if (std.mem.eql(u8, arg, "--no-ai")) {
            no_ai = true;
        } else if (std.mem.eql(u8, arg, "--api-url")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("error: --api-url requires a value\n", .{});
                return;
            }
            api_url = args[i];
            api_url_set = true;
        } else if (std.mem.eql(u8, arg, "-m") or std.mem.eql(u8, arg, "--model")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("error: --model requires a value\n", .{});
                return;
            }
            model = args[i];
            model_override = true;
        }
    }

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const ws = try llm.resolvePath(allocator, cwd, workspace orelse cwd);
    defer allocator.free(ws);

    const gdir = try llm.resolvePath(allocator, ws, guidance_dir orelse config_mod.DEFAULT_GUIDANCE_DIR);
    defer allocator.free(gdir);

    const src_json_dir = try std.fs.path.join(allocator, &.{ gdir, "src" });
    defer allocator.free(src_json_dir);

    var processor = comment_sync_mod.CommentSyncProcessor.init(allocator, ws, src_json_dir, debug_mode, dry_run);
    processor.generate_headers = gen_headers;

    // Wire up the LLM enhancer unless --no-ai was passed.
    if (!no_ai) {
        var cfg = config_mod.loadConfig(allocator, ws) catch
            try config_mod.loadConfig(allocator, cwd);
        defer cfg.deinit();
        const ga_for_enh: GenArgs = .{
            .api_url = api_url,
            .api_url_set = api_url_set,
            .model = model,
            .model_override = model_override,
            .verbose = debug_mode,
            .no_ai = false,
        };
        setupCspEnhancer(allocator, ga_for_enh, &cfg, &processor);
    }
    defer teardownCspEnhancer(allocator, &processor);

    var total_added: usize = 0;
    var total_regen: usize = 0;
    var total_files: usize = 0;

    if (single_file) |fp| {
        const abs_fp = try llm.resolvePath(allocator, cwd, fp);
        defer allocator.free(abs_fp);

        const r = processor.processFile(abs_fp) catch |err| {
            std.debug.print("error processing {s}: {s}\n", .{ fp, @errorName(err) });
            return;
        };
        total_added += r.comments_added;
        total_regen += r.comments_regenerated;
        total_files += 1;
        if (r.has_changes) {
            const action = if (dry_run) "[dry-run] " else "";
            std.debug.print("{s}{s}: +{} comments, {} regenerated{s}\n", .{
                action,
                fp,
                r.comments_added,
                r.comments_regenerated,
                if (r.header_added) ", header added" else "",
            });
        }
    } else {
        // Scan all .zig files under workspace/src.
        const src_dir_path = try std.fs.path.join(allocator, &.{ ws, "src" });
        defer allocator.free(src_dir_path);

        var src_dir = std.fs.openDirAbsolute(src_dir_path, .{ .iterate = true }) catch {
            std.debug.print("error: cannot open src dir: {s}\n", .{src_dir_path});
            return;
        };
        defer src_dir.close();

        var walker = try src_dir.walk(allocator);
        defer walker.deinit();

        while (try walker.next()) |entry| {
            if (entry.kind != .file) continue;
            if (!std.mem.endsWith(u8, entry.path, ".zig")) continue;

            const abs_path = try std.fs.path.join(allocator, &.{ src_dir_path, entry.path });
            defer allocator.free(abs_path);

            const r = processor.processFile(abs_path) catch continue;
            total_added += r.comments_added;
            total_regen += r.comments_regenerated;
            total_files += 1;

            if (r.has_changes and debug_mode) {
                std.debug.print("  {s}: +{} comments, {} regenerated\n", .{
                    entry.path,
                    r.comments_added,
                    r.comments_regenerated,
                });
            }
        }
    }

    const action = if (dry_run) "Would add" else "Added";
    std.debug.print("{s} {} comment(s) across {} file(s); {} regenerated.\n", .{
        action,
        total_added,
        total_files,
        total_regen,
    });
}

// =============================================================================
// migrate-comments — migrate JSON comment fields to source /// comments
// =============================================================================

pub fn cmdMigrateComments(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var workspace: ?[]const u8 = null;
    var guidance_dir: ?[]const u8 = null;
    var dry_run = false;

    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "-w") or std.mem.eql(u8, arg, "--workspace")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("error: --workspace requires a value\n", .{});
                return;
            }
            workspace = args[i];
        } else if (std.mem.eql(u8, arg, "--guidance-dir")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("error: --guidance-dir requires a value\n", .{});
                return;
            }
            guidance_dir = args[i];
        } else if (std.mem.eql(u8, arg, "--dry-run")) {
            dry_run = true;
        }
    }

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const ws = try llm.resolvePath(allocator, cwd, workspace orelse cwd);
    defer allocator.free(ws);

    const gdir = try llm.resolvePath(allocator, ws, guidance_dir orelse config_mod.DEFAULT_GUIDANCE_DIR);
    defer allocator.free(gdir);

    const src_json_dir = try std.fs.path.join(allocator, &.{ gdir, "src" });
    defer allocator.free(src_json_dir);

    var store = json_store_mod.JsonStore.init(allocator);

    var json_dir = std.fs.openDirAbsolute(src_json_dir, .{ .iterate = true }) catch {
        std.debug.print("error: cannot open guidance src dir: {s}\n", .{src_json_dir});
        return;
    };
    defer json_dir.close();

    var walker = try json_dir.walk(allocator);
    defer walker.deinit();

    var migrated_files: usize = 0;
    var migrated_members: usize = 0;

    while (try walker.next()) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.path, ".json")) continue;

        const json_path = try std.fs.path.join(allocator, &.{ src_json_dir, entry.path });
        defer allocator.free(json_path);

        const doc = (try store.loadGuidance(json_path)) orelse continue;
        defer store.freeGuidanceDoc(doc);

        // Only process Zig files.
        if (!std.mem.endsWith(u8, doc.meta.source, ".zig")) continue;

        const abs_source = try std.fs.path.join(allocator, &.{ ws, doc.meta.source });
        defer allocator.free(abs_source);

        // Check if source file exists.
        std.fs.accessAbsolute(abs_source, .{}) catch continue;

        // For each member that has a JSON comment but lacks a source comment,
        // insert the JSON comment into the source file.
        const processor = comment_sync_mod.CommentSyncProcessor.init(
            allocator,
            ws,
            src_json_dir,
            false,
            dry_run,
        );

        const source = llm.readFileAlloc(allocator, abs_source, 10 * 1024 * 1024) orelse continue;
        defer allocator.free(source);

        var source_changed = false;
        var current_source: []const u8 = try allocator.dupe(u8, source);
        defer allocator.free(current_source);

        // Process in reverse line order to preserve positions.
        const sorted_members = blk: {
            const m = try allocator.dupe(types.Member, doc.members);
            std.mem.sort(types.Member, m, {}, struct {
                fn gt(_: void, a: types.Member, b: types.Member) bool {
                    return (a.line orelse 0) > (b.line orelse 0);
                }
            }.gt);
            break :blk m;
        };
        defer allocator.free(sorted_members);

        for (sorted_members) |member| {
            const decl_line = member.line orelse continue;
            const json_comment = member.comment orelse continue;
            if (json_comment.len == 0) continue;

            // Check if source already has a comment at this line.
            const existing = try comment_inserter_mod.extractCommentAtLine(
                allocator,
                current_source,
                decl_line,
            );
            if (existing != null) {
                allocator.free(existing.?);
                continue; // Source already has a comment — don't overwrite.
            }

            if (!dry_run) {
                const ins = try comment_inserter_mod.insertComment(
                    allocator,
                    current_source,
                    decl_line,
                    json_comment,
                );
                if (ins.changed) {
                    allocator.free(current_source);
                    current_source = ins.new_source;
                    allocator.free(ins.line_adjustments);
                    source_changed = true;
                } else {
                    ins.deinit(allocator);
                }
            }
            migrated_members += 1;
        }

        _ = processor; // used for type context only

        if (source_changed) {
            const file = try std.fs.createFileAbsolute(abs_source, .{ .truncate = true });
            defer file.close();
            try file.writeAll(current_source);
            migrated_files += 1;
        } else if (dry_run and migrated_members > 0) {
            migrated_files += 1;
        }
    }

    const action = if (dry_run) "Would migrate" else "Migrated";
    std.debug.print("{s} {} comment(s) from JSON to source in {} file(s).\n", .{
        action,
        migrated_members,
        migrated_files,
    });
}

// ---------------------------------------------------------------------------
// scrub — blank synthetic LLM-generated comments in guidance JSON files
// ---------------------------------------------------------------------------

/// Walk .guidance/src/**/*.json and blank any synthetic comments in-place.
/// Usage: guidance scrub [--guidance-dir DIR] [--dry-run]
pub fn cmdScrub(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var dry_run = false;
    var guidance_dir_arg: []const u8 = config_mod.DEFAULT_GUIDANCE_DIR;
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--dry-run")) {
            dry_run = true;
        } else if (std.mem.eql(u8, arg, "--guidance-dir") or std.mem.eql(u8, arg, "-g")) {
            i += 1;
            if (i < args.len) guidance_dir_arg = args[i];
        }
    }

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const src_json_dir = try std.fs.path.join(allocator, &.{ cwd, guidance_dir_arg, "src" });
    defer allocator.free(src_json_dir);

    var json_dir = std.fs.openDirAbsolute(src_json_dir, .{ .iterate = true }) catch {
        std.debug.print("scrub: cannot open {s}\n", .{src_json_dir});
        return;
    };
    defer json_dir.close();

    var walker = try json_dir.walk(allocator);
    defer walker.deinit();

    var total_files: usize = 0;
    var scrubbed_files: usize = 0;
    var total_blanked: usize = 0;

    while (try walker.next()) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.path, ".json")) continue;

        const json_path = try std.fs.path.join(allocator, &.{ src_json_dir, entry.path });
        defer allocator.free(json_path);

        total_files += 1;

        // Load raw bytes.
        const file = std.fs.openFileAbsolute(json_path, .{}) catch continue;
        const raw = file.readToEndAlloc(allocator, 4 * 1024 * 1024) catch {
            file.close();
            continue;
        };
        file.close();
        defer allocator.free(raw);

        // Parse JSON.
        var parsed = std.json.parseFromSlice(std.json.Value, allocator, raw, .{}) catch continue;
        defer parsed.deinit();

        // Count comments before scrubbing for reporting.
        const blanked_before = total_blanked;
        _ = blanked_before;

        // Scrub synthetic comments.
        var file_blanked: usize = 0;
        if (parsed.value == .object) {
            file_blanked += scrubCount(&parsed.value);
        }

        if (file_blanked == 0) continue;
        total_blanked += file_blanked;
        scrubbed_files += 1;

        if (dry_run) {
            std.debug.print("scrub (dry-run): would blank {d} comment(s) in {s}\n", .{ file_blanked, entry.path });
            continue;
        }

        // Serialize and write back.
        var buf: std.io.Writer.Allocating = .init(allocator);
        defer buf.deinit();
        try std.json.Stringify.value(parsed.value, .{ .whitespace = .indent_2 }, &buf.writer);
        try buf.writer.writeByte('\n');

        const wfile = try std.fs.createFileAbsolute(json_path, .{ .truncate = true });
        defer wfile.close();
        try wfile.writeAll(buf.written());

        std.debug.print("scrub: blanked {d} comment(s) in {s}\n", .{ file_blanked, entry.path });
    }

    std.debug.print("scrub: {d} files checked, {d} modified, {d} comments blanked{s}\n", .{
        total_files,
        scrubbed_files,
        total_blanked,
        if (dry_run) " (dry-run)" else "",
    });
}

/// Count and scrub synthetic comments in a Value tree; returns count blanked.
fn scrubCount(value: *std.json.Value) usize {
    if (value.* != .object) return 0;
    var count: usize = 0;
    const obj = &value.object;

    if (obj.getPtr("comment")) |cv| {
        if (cv.* == .string and cv.string.len > 0 and scrub_mod.isSyntheticComment(cv.string)) {
            cv.* = .{ .string = "" };
            count += 1;
        }
    }
    if (obj.getPtr("members")) |mv| {
        if (mv.* == .array) {
            for (mv.array.items) |*member| {
                count += scrubCount(member);
            }
        }
    }
    return count;
}

// ---------------------------------------------------------------------------
// todo — work item lifecycle
// ---------------------------------------------------------------------------

/// Dispatch to todo subcommands.
/// Usage: guidance todo <new|triage|checklist|status|list|abandon> [args...]
pub fn cmdTodo(allocator: std.mem.Allocator, args: []const []const u8) !void {
    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const guidance_dir = config_mod.DEFAULT_GUIDANCE_DIR;
    const todo_dir = try std.fs.path.join(allocator, &.{ cwd, guidance_dir, "todo" });
    defer allocator.free(todo_dir);

    // Load config for LLM access.
    var api_url: []const u8 = config_mod.DEFAULT_API_URL;
    var model_thinking: []const u8 = "";
    var model_fast: []const u8 = "";

    if (config_mod.loadConfig(allocator, cwd)) |cfg_val| {
        var cfg = cfg_val;
        defer cfg.deinit();
        const parsed = config_mod.ProjectConfig.parseModelRef(cfg.model_thinking);
        if (parsed) |p| {
            if (cfg.getProvider(p.provider)) |provider| {
                api_url = try allocator.dupe(u8, try std.fmt.allocPrint(allocator, "{s}{s}", .{ provider.base_url, provider.chat_endpoint }));
            }
        }
        model_thinking = try allocator.dupe(u8, cfg.model_thinking);
        model_fast = try allocator.dupe(u8, cfg.model_fast);
    } else |_| {}
    defer if (model_thinking.len > 0) allocator.free(model_thinking);
    defer if (model_fast.len > 0) allocator.free(model_fast);
    defer if (!std.mem.eql(u8, api_url, config_mod.DEFAULT_API_URL)) allocator.free(api_url);

    if (args.len == 0) {
        std.debug.print("Usage: guidance todo <new|triage|checklist|status|list|abandon>\n", .{});
        return;
    }

    const subcmd = args[0];
    const sub_args = args[1..];

    if (std.mem.eql(u8, subcmd, "new")) {
        const description = if (sub_args.len > 0) sub_args[0] else "";
        return todo_mod.cmdTodoNew(allocator, description, todo_dir);
    } else if (std.mem.eql(u8, subcmd, "triage")) {
        const thinking_model = if (model_thinking.len > 0) model_thinking else model_fast;
        return todo_mod.cmdTodoTriage(allocator, todo_dir, api_url, thinking_model);
    } else if (std.mem.eql(u8, subcmd, "checklist")) {
        const fast_model = if (model_fast.len > 0) model_fast else model_thinking;
        return todo_mod.cmdTodoChecklist(allocator, todo_dir, api_url, fast_model);
    } else if (std.mem.eql(u8, subcmd, "status")) {
        return todo_mod.cmdTodoStatus(allocator, todo_dir);
    } else if (std.mem.eql(u8, subcmd, "list")) {
        return todo_mod.cmdTodoList(allocator, todo_dir);
    } else if (std.mem.eql(u8, subcmd, "abandon")) {
        return todo_mod.cmdTodoAbandon(allocator, todo_dir);
    } else {
        std.debug.print("Unknown todo subcommand: {s}\n", .{subcmd});
        std.debug.print("Usage: guidance todo <new|triage|checklist|status|list|abandon>\n", .{});
    }
}

// ---------------------------------------------------------------------------
// diary — append timestamped entry to current work item DIARY.md
// ---------------------------------------------------------------------------

/// Append a timestamped entry to the current work item's DIARY.md.
/// Usage: guidance diary "<message>"
pub fn cmdDiary(allocator: std.mem.Allocator, args: []const []const u8) !void {
    if (args.len == 0) {
        std.debug.print("Usage: guidance diary \"<message>\"\n", .{});
        return;
    }

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const todo_dir = try std.fs.path.join(allocator, &.{ cwd, config_mod.DEFAULT_GUIDANCE_DIR, "todo" });
    defer allocator.free(todo_dir);

    // Collect message from remaining args (join with space).
    var msg_buf: std.ArrayList(u8) = .{};
    defer msg_buf.deinit(allocator);
    for (args, 0..) |a, idx| {
        if (idx > 0) try msg_buf.append(allocator, ' ');
        try msg_buf.appendSlice(allocator, a);
    }

    // Get author from git config.
    var author: []const u8 = "unknown";
    const git_author = blk: {
        var child = std.process.Child.init(&.{ "git", "config", "user.name" }, allocator);
        child.stdout_behavior = .Pipe;
        child.stderr_behavior = .Ignore;
        child.cwd = cwd;
        child.spawn() catch break :blk null;
        const out = child.stdout.?.readToEndAlloc(allocator, 256) catch break :blk null;
        _ = child.wait() catch {};
        break :blk std.mem.trim(u8, out, " \t\r\n");
    };
    defer if (git_author) |a| allocator.free(a);
    if (git_author) |a| if (a.len > 0) {
        author = a;
    };

    return todo_mod.cmdDiaryEntry(allocator, msg_buf.items, todo_dir, author);
}

// =============================================================================
// check — orchestrate the full RALPH loop
// =============================================================================

/// `guidance check` runs the complete RALPH loop:
///   test → lint → fmt → guidance (all languages) → structure → db
///
/// It is the recommended entry point for pre-commit hooks and CI.
/// Incremental detection is always active: only stale files are processed.
pub fn cmdCheck(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var ga: GenArgs = .{ .all_languages = true, .compile_db = true };
    var run_structure = true;
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--skip-tests")) {
            ga.skip_tests = true;
        } else if (std.mem.eql(u8, arg, "--skip-lint")) {
            ga.skip_lint = true;
        } else if (std.mem.eql(u8, arg, "--skip-fmt")) {
            ga.skip_fmt = true;
        } else if (std.mem.eql(u8, arg, "--no-db")) {
            ga.compile_db = false;
        } else if (std.mem.eql(u8, arg, "--no-structure")) {
            run_structure = false;
        } else if (std.mem.eql(u8, arg, "--force")) {
            ga.force = true;
        } else if (std.mem.eql(u8, arg, "--verbose")) {
            ga.verbose = true;
        } else if (std.mem.eql(u8, arg, "--timeout")) {
            i += 1;
            if (i < args.len) {
                ga.timeout_seconds = std.fmt.parseInt(u64, args[i], 10) catch ga.timeout_seconds;
            }
        }
    }

    try cmdGenImpl(allocator, ga);

    if (run_structure) {
        const cwd = try std.process.getCwdAlloc(allocator);
        defer allocator.free(cwd);

        var cfg = config_mod.loadConfig(allocator, cwd) catch
            try config_mod.loadConfig(allocator, cwd);
        defer cfg.deinit();

        var gen = structure_mod.StructureGenerator.init(allocator, cwd, cfg.guidance_root, false);
        defer gen.deinit();
        gen.generate() catch |err| {
            std.debug.print("warning: structure update failed: {s}\n", .{@errorName(err)});
        };
        stepPrint("check: STRUCTURE.md\n", .{});
    }

    stepPrint("check: done\n", .{});
}

// =============================================================================
// Public wrappers for testing (commit helpers)
// =============================================================================

pub fn parseHunkRangesPub(allocator: std.mem.Allocator, chunk: []const u8) ![][2]u32 {
    return parseHunkRanges(allocator, chunk);
}

pub fn loadChangedMembersPub(allocator: std.mem.Allocator, guidance_root: []const u8, rel_path: []const u8, hunk_ranges: []const [2]u32) ![]CommitMemberInfo {
    return loadChangedMembers(allocator, guidance_root, rel_path, hunk_ranges);
}

pub fn chunkIsIgnoredPub(chunk: []const u8, guidance_dir: []const u8) bool {
    return chunkIsExplainGenJson(chunk, guidance_dir);
}

pub fn chunkFilePathPub(chunk: []const u8) []const u8 {
    return chunkFilePath(chunk);
}

pub fn splitDiffByFilePub(diff: []const u8, out: *std.ArrayList([]const u8), allocator: std.mem.Allocator) !void {
    return splitDiffByFile(diff, out, allocator);
}
