//! sync/commit.zig — Git commit message generation from staged diff + guidance JSON context.
//!
//! Extracted from sync_engine.zig (M2.1) to keep file sizes navigable.
//! Public API: cmdCommit, CommitMemberInfo, and test wrappers.
//!
//! ## Memory Ownership
//!
//!   - cmdCommit(): Creates an ephemeral LlmClient for commit message generation;
//!     init/deinit within function scope. All output written to stdout.
//!   - CommitMemberInfo: Holds borrowed string slices (file_path, member_name,
//!     old_comment, new_comment) — valid only as long as the parent arena/diff data lives.
//!   - Internal helpers (gitDiff, splitDiffByFile, etc.): Return allocator-owned
//!     strings; caller must free.

const std = @import("std");
const common = @import("common");
const config_mod = @import("../config.zig");
const llm = @import("llm");
const todo_mod = @import("../todo.zig");

// =============================================================================
// Git diff parsing helpers
// =============================================================================

/// Compares two directories, returning differences in a Zig slice.
fn gitDiff(allocator: std.mem.Allocator, cwd: []const u8, staged: bool) ![]u8 {
    const io = common.io.singleIo();
    const argv: []const []const u8 = if (staged)
        &.{ "git", "diff", "--staged" }
    else
        &.{ "git", "diff" };

    const result = try std.process.run(allocator, io, .{
        .argv = argv,
        .cwd = .{ .path = cwd },
    });
    defer allocator.free(result.stderr);
    if (result.term != .exited or result.term.exited != 0) {
        allocator.free(result.stdout);
        return error.GitDiffFailed;
    }
    return result.stdout;
}

/// Splits a diff array into output slices using a specified allocator.
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

/// Converts a byte slice into a Zig-safe file path string.
fn chunkFilePath(chunk: []const u8) []const u8 {
    const prefix = "diff --git a/";
    const first_nl = std.mem.indexOfScalar(u8, chunk, '\n') orelse chunk.len;
    const first_line = chunk[0..first_nl];
    if (!std.mem.startsWith(u8, first_line, prefix)) return "";
    const after = first_line[prefix.len..];
    const sp = std.mem.indexOfScalar(u8, after, ' ') orelse after.len;
    return after[0..sp];
}

/// Checks if a chunk is valid JSON format for guidance processing.
fn chunkIsExplainGenJson(chunk: []const u8, guidance_dir: []const u8) bool {
    const path = chunkFilePath(chunk);
    var buf: [std.fs.max_path_bytes + 2]u8 = undefined;
    const prefix = std.fmt.bufPrint(&buf, "{s}/", .{guidance_dir}) catch return false;
    return std.mem.startsWith(u8, path, prefix) and std.mem.endsWith(u8, path, ".json");
}

/// Converts a chunk of bytes into an array of 2-byte ranges for synchronization.
fn parseHunkRanges(allocator: std.mem.Allocator, chunk: []const u8) ![][2]u32 {
    var ranges: std.ArrayList([2]u32) = .empty;
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

/// Holds name/line/comment/signature for a changed member.
///
/// String fields (`name`, `comment`, `signature`) are owned by the allocator
/// that was passed to `loadChangedMembers`.  Pass an arena allocator so the
/// entire result (slice + strings) is freed by a single `arena.deinit()` call.
pub const CommitMemberInfo = struct {
    name: []const u8,
    line: ?u32,
    comment: []const u8,
    signature: []const u8,
};

/// Determines the line number of a JSON object, returning a u32 value.
fn memberLineNum(obj: std.json.ObjectMap) ?u32 {
    const lv = obj.get("line") orelse return null;
    return switch (lv) {
        .integer => |n| if (n >= 0) @as(u32, @intCast(n)) else null,
        .float => |f| if (f >= 0) @as(u32, @intFromFloat(f)) else null,
        else => null,
    };
}

/// Checks if a line falls within any of the provided hunk ranges and returns true or false.
fn lineInRanges(line: u32, hunk_ranges: []const [2]u32, context: u32) bool {
    for (hunk_ranges) |range| {
        const lo = if (range[0] > context) range[0] - context else 0;
        const hi = range[1] + context;
        if (line >= lo and line <= hi) return true;
    }
    return false;
}

/// Checks if a member value falls within specified hunk ranges and appends it to the output list.
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

/// Loads changed member information from a Zig source file using allocator and path details.
///
/// All string fields in the returned `[]CommitMemberInfo` are allocated on
/// `allocator`.  Pass an arena allocator to collapse the entire result (slice +
/// strings) into a single `arena.deinit()` call with no per-member cleanup.
fn loadChangedMembers(
    allocator: std.mem.Allocator,
    guidance_root: []const u8,
    rel_path: []const u8,
    hunk_ranges: []const [2]u32,
) ![]CommitMemberInfo {
    // Arena for internal temporaries (json_path, parsed JSON).
    // Does NOT own the returned slice or its string fields — those go to `allocator`.
    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    const a = arena.allocator();

    const json_path = try std.fmt.allocPrint(a, "{s}/src/{s}.json", .{ guidance_root, rel_path });
    var parsed = common.parseJsonFile(a, json_path, 256 * 1024) orelse return &.{};
    // parsed lives in arena — no separate defer needed

    const members_val = parsed.value.object.get("members") orelse return &.{};
    if (members_val != .array) return &.{};

    var result: std.ArrayList(CommitMemberInfo) = .empty;
    errdefer {
        for (result.items) |m| {
            allocator.free(m.name);
            allocator.free(m.comment);
            allocator.free(m.signature);
        }
        result.deinit(allocator);
    }

    const CONTEXT_LINES: u32 = 15;

    for (members_val.array.items) |member| {
        try appendMemberIfInRange(allocator, member, hunk_ranges, CONTEXT_LINES, &result);
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

/// Generates a commit message string using provided file diffs and metadata for version control.
///
/// Returns a caller-owned `[]u8` on `allocator`. All intermediate allocations
/// (parsed diff chunks, member lists, prompt, LLM response) use a function-scoped
/// arena and are freed before this function returns.
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
    // All intermediates live here; only the returned []u8 escapes to `allocator`.
    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    const a = arena.allocator();

    var buf: [std.fs.max_path_bytes + 2]u8 = undefined;
    const guidance_prefix = std.fmt.bufPrint(&buf, "{s}/", .{guidance_dir}) catch return error.OutOfMemory;
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
            var all_chunks: std.ArrayList([]const u8) = .empty;
            try splitDiffByFile(diff, &all_chunks, a);

            var code_chunks: std.ArrayList([]const u8) = .empty;
            for (all_chunks.items) |chunk| {
                if (!chunkIsExplainGenJson(chunk, guidance_dir)) try code_chunks.append(a, chunk);
            }

            if (debug) {
                std.debug.print("[commit] {d} total chunk(s), {d} code, {d} guidance JSON\n", .{
                    all_chunks.items.len, code_chunks.items.len, guidance_json_count,
                });
            }

            if (code_chunks.items.len > 0 or guidance_json_count > 0) {
                const TOTAL_CAP: usize = 12_000;
                const n_code = @max(1, code_chunks.items.len);
                const per_file_cap: usize = @max(800, TOTAL_CAP / n_code);

                var combined: std.ArrayList(u8) = .empty;

                for (code_chunks.items) |chunk| {
                    if (combined.items.len >= TOTAL_CAP) break;

                    const rel_path = chunkFilePath(chunk);

                    if (guidance_root.len > 0 and rel_path.len > 0) {
                        // hunk_ranges and member strings both land on `a`
                        const hunk_ranges = parseHunkRanges(a, chunk) catch &.{};
                        const members = loadChangedMembers(a, guidance_root, rel_path, hunk_ranges) catch &.{};

                        if (members.len > 0) {
                            try combined.appendSlice(a, "### Functions in ");
                            try combined.appendSlice(a, rel_path);
                            try combined.appendSlice(a, ":\n");
                            for (members) |m| {
                                if (m.line) |ln| {
                                    try combined.appendSlice(a, "- ");
                                    try combined.appendSlice(a, m.name);
                                    try combined.appendSlice(a, " (line ");
                                    try combined.append(a, @as(u8, @intCast(@divTrunc(ln, 10) % 10 + '0')));
                                    if (ln >= 10) {
                                        try combined.append(a, @as(u8, @intCast(@divTrunc(ln, 100) % 10 + '0')));
                                        if (ln >= 100) {
                                            try combined.append(a, @as(u8, @intCast(@divTrunc(ln, 1000) % 10 + '0')));
                                        }
                                    }
                                    try combined.appendSlice(a, ")\n");
                                } else {
                                    try combined.appendSlice(a, "- ");
                                    try combined.appendSlice(a, m.name);
                                    try combined.append(a, '\n');
                                }
                                if (m.comment.len > 0) {
                                    const end = std.mem.indexOfScalar(u8, m.comment, '.') orelse m.comment.len;
                                    const snippet = m.comment[0..@min(end + 1, @min(m.comment.len, 120))];
                                    try combined.appendSlice(a, ": ");
                                    try combined.appendSlice(a, snippet);
                                    try combined.append(a, '\n');
                                } else if (m.signature.len > 0) {
                                    const snippet = m.signature[0..@min(m.signature.len, 80)];
                                    try combined.appendSlice(a, ": `");
                                    try combined.appendSlice(a, snippet);
                                    try combined.appendSlice(a, "`\n");
                                }
                            }
                            try combined.append(a, '\n');
                        }
                    }

                    const budget = @min(chunk.len, per_file_cap);
                    try combined.appendSlice(a, chunk[0..budget]);
                    try combined.append(a, '\n');
                }

                const prompt = try std.fmt.allocPrint(
                    a,
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

                if (debug) std.debug.print("[commit] prompt ({d} chars):\n{s}\n---\n", .{ prompt.len, prompt });

                const result = client.completeOrNull(prompt, 8192, 0.1, null);

                if (result) |raw| {
                    defer client.allocator.free(raw); // raw comes from LlmClient
                    if (debug) std.debug.print("[commit] response:\n{s}\n---\n", .{raw});

                    var bullets: std.ArrayList([]const u8) = .empty;
                    var resp_lines = std.mem.splitScalar(u8, raw, '\n');
                    while (resp_lines.next()) |line| {
                        const trimmed = std.mem.trim(u8, line, " \t\r");
                        const is_bullet = std.mem.startsWith(u8, trimmed, "* ") or
                            std.mem.startsWith(u8, trimmed, "- ");
                        if (is_bullet) {
                            const text = std.mem.trim(u8, trimmed[2..], " \t");
                            if (text.len > 0) try bullets.append(a, try a.dupe(u8, text));
                        }
                    }

                    if (bullets.items.len > 0) {
                        var out: std.ArrayList(u8) = .empty;
                        for (bullets.items) |b| {
                            try out.appendSlice(a, "* ");
                            try out.appendSlice(a, b);
                            try out.append(a, '\n');
                        }
                        if (guidance_json_count > 0) {
                            const line = try std.fmt.allocPrint(a, "* guidance: updated {d} JSON file(s) in {s}/src/\n", .{ guidance_json_count, guidance_dir });
                            try out.appendSlice(a, line);
                        }
                        if (out.items.len > 0 and out.items[out.items.len - 1] == '\n')
                            out.items.len -= 1;
                        // Escape to caller-owned memory before arena deinits
                        return try allocator.dupe(u8, out.items);
                    }
                }
            }
        }
    } else |_| {}

    std.debug.print("warning: LLM unavailable or returned no bullets; using filename fallback\n", .{});

    var fallback: std.ArrayList(u8) = .empty;
    var any = false;
    for (changed_files) |f| {
        if (std.mem.startsWith(u8, f, guidance_prefix) and std.mem.endsWith(u8, f, ".json")) continue;
        try fallback.appendSlice(a, "* Update ");
        try fallback.appendSlice(a, f);
        try fallback.append(a, '\n');
        any = true;
    }
    if (guidance_json_count > 0) {
        const line = try std.fmt.allocPrint(a, "* guidance: updated {d} JSON file(s) in {s}/src/\n", .{ guidance_json_count, guidance_dir });
        try fallback.appendSlice(a, line);
        any = true;
    }
    if (any) {
        if (fallback.items.len > 0 and fallback.items[fallback.items.len - 1] == '\n')
            fallback.items.len -= 1;
        return try allocator.dupe(u8, fallback.items);
    }
    return try allocator.dupe(u8, "* Update codebase");
}

/// Writes a Zig message to a temporary buffer using the provided allocator.
fn writeTmpCommitMsg(allocator: std.mem.Allocator, msg: []const u8) ![]u8 {
    const path = try std.fmt.allocPrint(allocator, "/tmp/explain_gen_commit_{d}.txt", .{@divTrunc(std.Io.Timestamp.now(std.Io.Threaded.global_single_threaded.io(), .real).nanoseconds, std.time.ns_per_s)});
    const io = std.Io.Threaded.global_single_threaded.io();
    const file = try std.Io.Dir.createFileAbsolute(io, path, .{});
    defer file.close(io);
    var wbuf: [4096]u8 = undefined;
    var fw = file.writer(io, &wbuf);
    try fw.interface.writeAll(msg);
    try fw.interface.writeAll("\n\n# Lines starting with '#' will be ignored.\n");
    try fw.interface.writeAll("# Edit the commit message above. Save and close to commit.\n");
    try fw.interface.flush();
    return path;
}

/// Loads a commit model configuration from memory allocator and returns its slice.
fn loadCommitModelFromConfig(allocator: std.mem.Allocator, cwd: []const u8) ![]const u8 {
    const path = try std.fs.path.join(allocator, &.{ cwd, config_mod.DEFAULT_GUIDANCE_DIR, config_mod.CONFIG_FILENAME });
    defer allocator.free(path);

    var parsed = common.parseJsonFile(allocator, path, 64 * 1024) orelse return error.ParseError;
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

/// Handles command execution with allocator and arguments, returning void.
pub fn cmdCommit(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var dry_run = false;
    var debug = false;
    var force = false;
    var api_url: []const u8 = config_mod.DEFAULT_API_URL;
    var model: []const u8 = config_mod.DEFAULT_MODEL;
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--dry-run")) {
            dry_run = true;
        } else if (std.mem.eql(u8, arg, "--debug") or std.mem.eql(u8, arg, "--verbose")) {
            debug = true;
        } else if (std.mem.eql(u8, arg, "--force") or std.mem.eql(u8, arg, "-f")) {
            force = true;
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

    const cwd = try std.process.currentPathAlloc(common.io.singleIo(), allocator);
    defer allocator.free(cwd);

    const default_guidance_dir: []const u8 = config_mod.DEFAULT_GUIDANCE_DIR;
    const default_guidance_root = try std.fs.path.join(allocator, &.{ cwd, config_mod.DEFAULT_GUIDANCE_DIR });
    defer allocator.free(default_guidance_root);

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
        model_owned = loadCommitModelFromConfig(allocator, cwd) catch
            try allocator.dupe(u8, cfg.model_default);
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

    const diff = try gitDiff(allocator, cwd, true);
    defer allocator.free(diff);

    if (diff.len == 0) {
        std.debug.print("No staged changes to commit. Use 'git add' to stage files first.\n", .{});
        return;
    }

    var changed_files: std.ArrayList([]const u8) = .empty;
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

    const tmp_path = try writeTmpCommitMsg(allocator, commit_msg);
    defer {
        std.Io.Dir.deleteFileAbsolute(std.Io.Threaded.global_single_threaded.io(), tmp_path) catch {};
        allocator.free(tmp_path);
    }

    const io = common.io.singleIo();
    const mtime_before: i128 = blk: {
        const f = std.Io.Dir.openFileAbsolute(io, tmp_path, .{}) catch break :blk 0;
        defer f.close(io);
        const stat = f.stat(io) catch break :blk 0;
        break :blk @as(i128, stat.mtime.nanoseconds);
    };

    const editor = blk: {
        if (std.c.getenv("EDITOR")) |e| break :blk try allocator.dupe(u8, std.mem.span(e));
        if (std.c.getenv("VISUAL")) |e| break :blk try allocator.dupe(u8, std.mem.span(e));
        break :blk try allocator.dupe(u8, "vi");
    };
    defer allocator.free(editor);

    const editor_result = try std.process.run(allocator, io, .{ .argv = &.{ editor, tmp_path }, .cwd = .{ .path = cwd } });
    allocator.free(editor_result.stdout);
    allocator.free(editor_result.stderr);

    const mtime_after: i128 = blk: {
        const f = std.Io.Dir.openFileAbsolute(io, tmp_path, .{}) catch break :blk 0;
        defer f.close(io);
        const stat = f.stat(io) catch break :blk 0;
        break :blk @as(i128, stat.mtime.nanoseconds);
    };

    if (mtime_after == mtime_before) {
        std.debug.print("Commit message not saved. Aborting.\n", .{});
        return;
    }

    const raw_msg = std.Io.Dir.cwd().readFileAlloc(io, tmp_path, allocator, .limited(64 * 1024)) catch {
        std.debug.print("Commit message is empty. Aborting.\n", .{});
        return;
    };
    defer allocator.free(raw_msg);

    var final_parts: std.ArrayList([]const u8) = .empty;
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

    const todo_dir = try std.fs.path.join(allocator, &.{ cwd, config_mod.DEFAULT_GUIDANCE_DIR, "todo" });
    defer allocator.free(todo_dir);

    const cl_status = try todo_mod.queryChecklistStatus(allocator, todo_dir);
    defer if (cl_status.item_dir) |d| allocator.free(d);
    if (cl_status.incomplete > 0 and !force) {
        std.debug.print(
            "Error: {d}/{d} checklist items incomplete in current work item. Use --force to commit anyway.\n",
            .{ cl_status.incomplete, cl_status.total },
        );
        return;
    }
    if (cl_status.incomplete > 0 and force) {
        std.debug.print(
            "Warning: {d}/{d} checklist items incomplete (forced).\n",
            .{ cl_status.incomplete, cl_status.total },
        );
    }

    const commit_result = try std.process.run(allocator, io, .{
        .argv = &.{ "git", "commit", "-m", final_msg },
        .cwd = .{ .path = cwd },
    });
    defer {
        allocator.free(commit_result.stdout);
        allocator.free(commit_result.stderr);
    }
    if (commit_result.term == .exited and commit_result.term.exited == 0) {
        std.debug.print("Committed successfully.\n", .{});

        if (cl_status.item_dir) |item_dir| {
            const hash_result = try std.process.run(allocator, io, .{
                .argv = &.{ "git", "rev-parse", "HEAD" },
                .cwd = .{ .path = cwd },
            });
            defer {
                allocator.free(hash_result.stdout);
                allocator.free(hash_result.stderr);
            }
            const hash = if (hash_result.term == .exited and hash_result.term.exited == 0)
                std.mem.trim(u8, hash_result.stdout, " \t\r\n")
            else
                "";

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
                std.debug.print("Warning: could not write COMMITTED.md: {any}\n", .{err});
            };
        }
    } else {
        std.debug.print("git commit failed.\n", .{});
    }
}

// =============================================================================
// Public test wrappers
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

/// Test wrapper — exposes generateCommitMessage for integration testing.
/// Returns caller-owned []u8; caller must free with the same allocator.
pub fn generateCommitMessagePub(
    allocator: std.mem.Allocator,
    diff: []const u8,
    changed_files: []const []const u8,
    guidance_dir: []const u8,
    guidance_root: []const u8,
    api_url: []const u8,
    model: []const u8,
    debug: bool,
) ![]u8 {
    return generateCommitMessage(allocator, diff, changed_files, guidance_dir, guidance_root, api_url, model, debug);
}
