//! guidance — AST-guided LanceDB vector search database generator.
//!
//! Produces:
//!   .guidance/src/**/*.json  — Per-file structured metadata mirror
//!   .guidance.db              — LanceDB database consumed by NullClaw's explain tool
//!
//! Usage:
//!   guidance gen      [options]   Generate JSON + compile .guidance.db
//!   guidance status   [options]   Report generation status
//!   guidance clean    [options]   Remove .guidance/ and .guidance.db
//!   guidance structure [options]  Update STRUCTURE.md from guidance JSON
//!   guidance deps     [options]   Generate Makefile .depend file

const std = @import("std");
const types = @import("types.zig");
const ast_parser = @import("ast_parser.zig");
const sync_mod = @import("sync.zig");
const structure_mod = @import("structure.zig");
const deps_mod = @import("deps.zig");
const lance_db_mod = @import("lance_db.zig");
const vector_mod = @import("vector/root.zig");

/// Canonical search result type — LanceDB hybrid search (vector + keyword).
const GuidanceDb = lance_db_mod.GuidanceDb;
const SearchResult = GuidanceDb.SearchResult;
const freeSearchResult = GuidanceDb.freeSearchResult;
const llm = @import("common");
const enhancer_mod = @import("enhancer.zig");
const config_mod = @import("config.zig");
const plugin_mod = @import("plugin.zig");
const plugin_registry = @import("plugin_registry.zig");
const staged_mod = @import("staged.zig");
const llm_filter_mod = @import("llm_filter.zig");
const synthesize_mod = @import("synthesize.zig");
const marker_mod = @import("marker.zig");
const provider_mod = @import("provider_discovery.zig");

pub const version = "0.1.0";

const Command = enum {
    init,
    gen,
    status,
    clean,
    structure,
    deps,
    query,
    explain,
    commit,
    check,
    @"show-aliases",
    @"test",
};

pub fn main() !void {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    const args = try std.process.argsAlloc(allocator);
    defer std.process.argsFree(allocator, args);

    if (args.len < 2) {
        try printHelp();
        return;
    }

    if (std.mem.eql(u8, args[1], "-h") or std.mem.eql(u8, args[1], "--help")) {
        try printHelp();
        return;
    }
    if (std.mem.eql(u8, args[1], "-v") or std.mem.eql(u8, args[1], "--version")) {
        std.debug.print("guidance v{s}\n", .{version});
        return;
    }

    const subcmd = std.meta.stringToEnum(Command, args[1]) orelse {
        std.debug.print("Unknown subcommand: {s}\n\n", .{args[1]});
        try printHelp();
        return;
    };

    switch (subcmd) {
        .init => try cmdInit(allocator, args[2..]),
        .gen => try cmdGen(allocator, args[2..]),
        .status => try cmdStatus(allocator, args[2..]),
        .clean => try cmdClean(allocator, args[2..]),
        .structure => try cmdStructure(allocator, args[2..]),
        .deps => try cmdDeps(allocator, args[2..]),
        .query => try cmdQuery(allocator, args[2..]),
        .explain => try cmdExplain(allocator, args[2..]),
        .commit => try cmdCommit(allocator, args[2..]),
        .check => try cmdCheck(allocator, args[2..]),
        .@"show-aliases" => try cmdShowAliases(allocator, args[2..]),
        .@"test" => try cmdTest(allocator, args[2..]),
    }
}

fn printHelp() !void {
    var ws: llm.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();

    try stdout.writeAll(
        \\guidance v0.1.0 — AST-guided LanceDB vector search database generator
        \\
        \\Produces .guidiance/src/**/*.json and .guidance.db for NullClaw.
        \\
        \\Usage:
        \\  guidance <command> [options]
        \\  guidance --help | --version
        \\
        \\Commands:
        \\  init       Create default configuration (AGENTS.md integration)
        \\  gen        Generate .guidance/ JSON mirror and .guidance.db
        \\  status     Show generation status (synced, stale, missing)
        \\  clean      Remove .guidance/src and .guidance.db
        \\  structure  Regenerate STRUCTURE.md from guidance JSON
        \\  deps       Generate Makefile .depend file from Zig imports
        \\  query      Search .guidance.db with vector/keyword search (no LLM)
        \\  explain    Search with LLM-synthesized summary
        \\  check      Run full RALPH loop (test → lint → fmt → guidance → structure)
        \\  commit    Generate AI commit message from staged diff + guidance
        \\  show-aliases  Show semantic aliases from .guidance.db
        \\  test       Benchmark explain queries against module-level comments
        \\
        \\Produces .guidance/src/**/*.json and .guidance.db for NullClaw.
        \\
        \\Usage:
        \\  guidance <command> [options]
        \\  guidance --help | --version
        \\
        \\Commands:
        \\  init       Create default configuration (AGENTS.md integration)
        \\  gen        Generate .guidance/ JSON mirror and .guidance.db
        \\  status     Show generation status (synced, stale, missing)
        \\  clean      Remove .guidance/src and .guidance.db
        \\  structure  Regenerate STRUCTURE.md from guidance JSON
        \\  deps       Generate Makefile .depend file from Zig imports
        \\  query      Search .guidance.db with vector/keyword search (no LLM)
        \\  explain    Search with LLM-synthesized summary
        \\  check      Run full RALPH loop (test → lint → fmt → guidance → structure)
        \\  commit     Generate AI commit message from staged diff + guidance
        \\
        \\Init options:
        \\  -g, --guidance-dir DIR   Guidance directory (default: .guidance)
        \\  -o, --db PATH            Database path (default: .guidance.db)
        \\
        \\Gen options:
        \\  --file FILE           Process a single source file (incremental)
        \\  --scan DIR            Process all source files under DIR
        \\  -w, --workspace DIR   Source root directory (default: current directory)
        \\  --guidance-dir DIR    Guidance directory (default: .guidance)
        \\  -o, --db PATH         LanceDB database path (default: .guidance.db)
        \\  --no-db               Skip database compilation step
        \\  --regen               LLM-regenerate all comments
        \\  --dry-run             Show what would change without writing
        \\  --verbose             Print LLM prompts and raw responses
        \\  --api-url URL         LLM API endpoint (default: http://localhost:11434/v1/chat/completions)
        \\  -m, --model NAME      Model name (default: code:latest)
        \\
        \\Query/Explain options:
        \\  <query>              Search query (required)
        \\  -l, --limit N         Max results (default: 10)
        \\  --json                Output JSON (query only)
        \\  -o, --db PATH         Database path (default: .guidance.db)
        \\  -w, --workspace DIR   Workspace root (default: current directory)
        \\  --guidance-dir DIR    Guidance directory (default: .guidance)
        \\  --no-llm              Skip LLM synthesis (explain only)
        \\  --staged=false        Use legacy output format (rollback safety)
        \\  --filter=auto|force|skip  LLM relevance filter mode (default: auto)
        \\                          auto  = LLM filter only for long queries (5+ words)
        \\                          force = always apply LLM filter
        \\                          skip  = never apply LLM filter (fast path)
        \\  --api-url URL         LLM endpoint
        \\  -m, --model NAME      Model for synthesis
        \\
        \\Structure options:
        \\  --guidance-dir DIR    Guidance JSON directory (default: .guidance)
        \\  --no-ai               Skip AI infill pre-pass
        \\  --api-url URL         LLM endpoint
        \\  -m, --model NAME      Model for AI infill
        \\
        \\Deps options:
        \\  --src DIR             Source directory to scan (default: src)
        \\
        \\Examples:
        \\  guidance init
        \\  guidance gen
        \\  guidance gen --file src/main.zig
        \\  guidance gen --scan src
        \\  guidance gen -o /tmp/project.guidance.db
        \\  guidance query "hash function"
        \\  guidance explain "how does the sync processor work" --limit 5
        \\  guidance status
        \\  guidance clean
        \\  guidance structure
        \\  guidance deps --src src > zig.depend
        \\  guidance commit
        \\  guidance check
        \\
    );
    try stdout.flush();
}

// =============================================================================
// init — create default configuration
// =============================================================================

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

fn cmdInit(allocator: std.mem.Allocator, args: []const []const u8) !void {
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

    const file = std.fs.openFileAbsolute(json_path, .{}) catch return &.{};
    defer file.close();
    const content = file.readToEndAlloc(allocator, 256 * 1024) catch return &.{};
    defer allocator.free(content);

    var parsed = std.json.parseFromSlice(std.json.Value, allocator, content, .{}) catch return &.{};
    defer parsed.deinit();

    const root = parsed.value;
    if (root != .object) return &.{};
    const members_val = root.object.get("members") orelse return &.{};
    if (members_val != .array) return &.{};

    var result: std.ArrayList(CommitMemberInfo) = .{};
    errdefer {
        for (result.items) |m| m.deinit(allocator);
        result.deinit(allocator);
    }

    const CONTEXT_LINES: u32 = 15;

    for (members_val.array.items) |member| {
        if (member != .object) continue;
        const name_val = member.object.get("name") orelse continue;
        if (name_val != .string or name_val.string.len == 0) continue;

        const line_num: ?u32 = blk: {
            if (member.object.get("line")) |lv| {
                break :blk switch (lv) {
                    .integer => |n| if (n >= 0) @as(u32, @intCast(n)) else null,
                    .float => |f| if (f >= 0) @as(u32, @intFromFloat(f)) else null,
                    else => null,
                };
            }
            break :blk null;
        };

        const include = if (hunk_ranges.len == 0 or line_num == null)
            true
        else blk: {
            const ln = line_num.?;
            for (hunk_ranges) |range| {
                const lo = if (range[0] > CONTEXT_LINES) range[0] - CONTEXT_LINES else 0;
                const hi = range[1] + CONTEXT_LINES;
                if (ln >= lo and ln <= hi) break :blk true;
            }
            break :blk false;
        };

        if (!include) continue;

        const comment = if (member.object.get("comment")) |cv|
            if (cv == .string) cv.string else ""
        else
            "";
        const sig = if (member.object.get("signature")) |sv|
            if (sv == .string) sv.string else ""
        else
            "";

        try result.append(allocator, .{
            .name = try allocator.dupe(u8, name_val.string),
            .line = line_num,
            .comment = try allocator.dupe(u8, comment),
            .signature = try allocator.dupe(u8, sig),
        });

        // Recurse one level into nested members (struct methods).
        if (member.object.get("members")) |nested_val| {
            if (nested_val == .array) {
                for (nested_val.array.items) |nested| {
                    if (nested != .object) continue;
                    const nn = nested.object.get("name") orelse continue;
                    if (nn != .string or nn.string.len == 0) continue;
                    const nl_num: ?u32 = blk: {
                        if (nested.object.get("line")) |lv| {
                            break :blk switch (lv) {
                                .integer => |n| if (n >= 0) @as(u32, @intCast(n)) else null,
                                else => null,
                            };
                        }
                        break :blk null;
                    };
                    const n_include = if (hunk_ranges.len == 0 or nl_num == null)
                        true
                    else blk: {
                        const ln = nl_num.?;
                        for (hunk_ranges) |range| {
                            const lo = if (range[0] > CONTEXT_LINES) range[0] - CONTEXT_LINES else 0;
                            const hi = range[1] + CONTEXT_LINES;
                            if (ln >= lo and ln <= hi) break :blk true;
                        }
                        break :blk false;
                    };
                    if (!n_include) continue;
                    const nc = if (nested.object.get("comment")) |cv| if (cv == .string) cv.string else "" else "";
                    const ns = if (nested.object.get("signature")) |sv| if (sv == .string) sv.string else "" else "";
                    try result.append(allocator, .{
                        .name = try allocator.dupe(u8, nn.string),
                        .line = nl_num,
                        .comment = try allocator.dupe(u8, nc),
                        .signature = try allocator.dupe(u8, ns),
                    });
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

fn cmdCommit(allocator: std.mem.Allocator, args: []const []const u8) !void {
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

    var commit_child = std.process.Child.init(&.{ "git", "commit", "-m", final_msg }, allocator);
    commit_child.cwd = cwd;
    const commit_result = try commit_child.spawnAndWait();
    if (commit_result == .Exited and commit_result.Exited == 0) {
        std.debug.print("Committed successfully.\n", .{});
    } else {
        std.debug.print("git commit failed.\n", .{});
    }
}

/// Read `models.commit` or `models.default` from guidance-config.json.
/// Returns an owned slice; caller must free. Returns error when absent.
fn loadCommitModelFromConfig(allocator: std.mem.Allocator, cwd: []const u8) ![]const u8 {
    const path = try std.fs.path.join(allocator, &.{ cwd, config_mod.DEFAULT_GUIDANCE_DIR, config_mod.CONFIG_FILENAME });
    defer allocator.free(path);

    const file = std.fs.openFileAbsolute(path, .{}) catch return error.FileNotFound;
    defer file.close();
    const content = file.readToEndAlloc(allocator, 64 * 1024) catch return error.ReadError;
    defer allocator.free(content);

    var parsed = std.json.parseFromSlice(std.json.Value, allocator, content, .{}) catch return error.ParseError;
    defer parsed.deinit();

    const root = parsed.value;
    if (root != .object) return error.InvalidConfig;

    if (root.object.get("models")) |models| {
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
                // Accepted but ignored — LanceDB is always used.
            } else if (std.mem.eql(u8, arg, "--guidance-db")) {
                // Alias for -o when used with old scripts.
                i += 1;
                if (i >= args.len) return error.MissingValue;
                ga.db_path = args[i];
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
/// paths.  db_path is the LanceDB vector database; it defaults to the value
/// in guidance-config.json, or DEFAULT_GUIDANCE_DB_PATH if not set.
fn resolveGenPaths(allocator: std.mem.Allocator, ga: GenArgs, cwd: []const u8) !ResolvedGenPaths {
    const workspace = try resolveAbsOrJoin(allocator, cwd, ga.workspace orelse cwd);
    errdefer allocator.free(workspace);

    const json_dir = try resolveAbsOrJoin(allocator, workspace, ga.json_dir orelse config_mod.DEFAULT_GUIDANCE_DIR);
    errdefer allocator.free(json_dir);

    const db_path = try resolveAbsOrJoin(allocator, workspace, ga.db_path orelse config_mod.DEFAULT_GUIDANCE_DB_PATH);
    return .{ .workspace = workspace, .json_dir = json_dir, .db_path = db_path };
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
    const llm_config = resolveLlmConfigForThinking(
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
        const thinking_config = resolveLlmConfigForThinking(
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
        const full_path = try resolveAbsOrJoin(allocator, paths.workspace, file_arg);
        defer allocator.free(full_path);
        _ = try processor.processFile(full_path);
        if (ga.verbose) std.debug.print("gen: processed {s}\n", .{full_path});
        return 1;
    }

    if (ga.scan) |scan_arg| {
        const scan_abs = try resolveAbsOrJoin(allocator, paths.workspace, scan_arg);
        defer allocator.free(scan_abs);
        const count = try processor.processDirectory(scan_abs);
        std.debug.print("gen: {d} source files processed from {s}\n", .{ count, scan_abs });
        return count;
    }

    // Full workspace scan: read src_dirs from config, fail fast on any error.
    var cfg = try config_mod.loadConfig(allocator, paths.workspace);
    defer cfg.deinit();

    var total: usize = 0;
    for (cfg.src_dirs) |src_rel| {
        const src_abs = try resolveAbsOrJoin(allocator, paths.workspace, src_rel);
        defer allocator.free(src_abs);
        total += try processor.processDirectory(src_abs);
    }
    std.debug.print("gen: {d} source files processed\n", .{total});
    return total;
}

/// Sync .guidance.db (LanceDB-style vector database).
/// Creates an embedding provider from config and calls lance_db.syncDatabase.
/// Failures are logged as warnings but do not abort the gen pipeline.
fn syncGuidanceDb(
    allocator: std.mem.Allocator,
    json_dir: []const u8,
    guidance_db_path: []const u8,
    cfg: *const config_mod.ProjectConfig,
    verbose: bool,
) void {
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
        lance_db_mod.syncDatabase(allocator, json_dir, guidance_db_path, p, null, null) catch |se| {
            std.debug.print("guidance.db: sync failed: {s}\n", .{@errorName(se)});
        };
        p.deinit();
        return;
    };
    defer embedder.deinit();

    if (verbose) {
        std.debug.print("gen: syncing guidance.db to {s} (embedder={s})\n", .{ guidance_db_path, embedder.getName() });
    }

    // Resolve capabilities_dir relative to workspace (not json_dir)
    // We derive workspace from json_dir by stripping /.guidance suffix.
    const cap_dir_abs = blk: {
        const workspace = std.fs.path.dirname(json_dir) orelse json_dir;
        const abs = std.fs.path.join(allocator, &.{ workspace, cfg.capabilities_dir }) catch break :blk null;
        // Check it exists before passing it through
        std.fs.accessAbsolute(abs, .{}) catch {
            allocator.free(abs);
            break :blk null;
        };
        break :blk abs;
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

    lance_db_mod.syncDatabase(allocator, json_dir, guidance_db_path, embedder, cap_dir_abs, aliases) catch |err| {
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

fn cmdGen(allocator: std.mem.Allocator, args: []const []const u8) !void {
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
        const src_abs = try resolveAbsOrJoin(allocator, paths.workspace, file_arg);
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
        const scan_abs = try resolveAbsOrJoin(allocator, paths.workspace, scan_arg);
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
            const src_abs = try resolveAbsOrJoin(allocator, paths.workspace, src_rel);
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
            if (ga.verbose) std.debug.print("gen: {d}/{d} built-in file(s) stale\n", .{
                stale.items.len, all_builtin.items.len,
            });
            try runBuiltinLanguagePipeline(allocator, &cfg, &processor, "zig", stale.items, all_builtin.items, paths.json_dir, ga);
        } else {
            if (ga.verbose) std.debug.print("gen: all {d} built-in file(s) up to date\n", .{all_builtin.items.len});
        }
    }

    // External providers (e.g. guidance-py for .py files).
    if (ga.all_languages) {
        // Collect every distinct extension found in src_dirs that is NOT built-in.
        var foreign_exts: std.StringHashMapUnmanaged(void) = .{};
        defer foreign_exts.deinit(allocator);

        for (cfg.src_dirs) |src_rel| {
            const src_abs = try resolveAbsOrJoin(allocator, paths.workspace, src_rel);
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
                const src_abs = try resolveAbsOrJoin(allocator, paths.workspace, src_rel);
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

fn cmdStatus(allocator: std.mem.Allocator, args: []const []const u8) !void {
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

    const json_dir = if (json_dir_arg) |jd|
        if (std.fs.path.isAbsolute(jd)) try allocator.dupe(u8, jd) else try std.fs.path.join(allocator, &.{ cwd, jd })
    else
        try std.fs.path.join(allocator, &.{ cwd, config_mod.DEFAULT_GUIDANCE_DIR });
    defer allocator.free(json_dir);

    const db_path = if (db_path_arg) |dp|
        if (std.fs.path.isAbsolute(dp)) try allocator.dupe(u8, dp) else try std.fs.path.join(allocator, &.{ cwd, dp })
    else
        try std.fs.path.join(allocator, &.{ cwd, config_mod.DEFAULT_GUIDANCE_DB_PATH });
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
}

// =============================================================================
// clean
// =============================================================================

fn cmdClean(allocator: std.mem.Allocator, args: []const []const u8) !void {
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

    const json_dir = if (json_dir_arg) |jd|
        if (std.fs.path.isAbsolute(jd)) try allocator.dupe(u8, jd) else try std.fs.path.join(allocator, &.{ cwd, jd })
    else
        try std.fs.path.join(allocator, &.{ cwd, config_mod.DEFAULT_GUIDANCE_DIR });
    defer allocator.free(json_dir);

    const db_path = if (db_path_arg) |dp|
        if (std.fs.path.isAbsolute(dp)) try allocator.dupe(u8, dp) else try std.fs.path.join(allocator, &.{ cwd, dp })
    else
        try std.fs.path.join(allocator, &.{ cwd, config_mod.DEFAULT_GUIDANCE_DB_PATH });
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
// structure
// =============================================================================

fn cmdStructure(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var json_dir_arg: ?[]const u8 = null;
    var no_ai: bool = false;
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--json-dir") or std.mem.eql(u8, arg, "--guidance")) {
            i += 1;
            if (i >= args.len) return;
            json_dir_arg = args[i];
        } else if (std.mem.eql(u8, arg, "--no-ai")) {
            no_ai = true;
        }
    }
    const _no_ai = no_ai; // LLM infill for structure is a future enhancement
    _ = _no_ai;

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const json_dir = if (json_dir_arg) |jd|
        if (std.fs.path.isAbsolute(jd)) try allocator.dupe(u8, jd) else try std.fs.path.join(allocator, &.{ cwd, jd })
    else
        try std.fs.path.join(allocator, &.{ cwd, config_mod.DEFAULT_GUIDANCE_DIR });
    defer allocator.free(json_dir);

    var gen = structure_mod.StructureGenerator.init(allocator, cwd, json_dir, false);
    defer gen.deinit();
    try gen.generate();
}

// =============================================================================
// deps
// =============================================================================

fn cmdDeps(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var src_dir: []const u8 = "src";
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--src")) {
            i += 1;
            if (i >= args.len) return;
            src_dir = args[i];
        }
    }

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    var deps_gen = deps_mod.DepsGenerator.init(allocator, cwd);
    try deps_gen.generateDependencies(src_dir);
}

// =============================================================================
// query
// =============================================================================

const QueryArgs = struct {
    query_str: ?[]const u8 = null,
    limit: usize = 10,
    json_mode: bool = false,
    db_path: ?[]const u8 = null,
    workspace: ?[]const u8 = null,

    /// Parse query subcommand arguments. Returns error.MissingValue when a
    /// flag-with-value is the last argument (consistent with GenArgs.parse).
    fn parse(args: []const []const u8) error{MissingValue}!QueryArgs {
        var qa: QueryArgs = .{};
        var i: usize = 0;
        while (i < args.len) : (i += 1) {
            const arg = args[i];
            if (std.mem.eql(u8, arg, "-l") or std.mem.eql(u8, arg, "--limit")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                qa.limit = std.fmt.parseInt(usize, args[i], 10) catch 10;
            } else if (std.mem.eql(u8, arg, "--json")) {
                qa.json_mode = true;
            } else if (std.mem.eql(u8, arg, "-o") or std.mem.eql(u8, arg, "--db")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                qa.db_path = args[i];
            } else if (std.mem.eql(u8, arg, "-w") or std.mem.eql(u8, arg, "--workspace")) {
                i += 1;
                if (i >= args.len) return error.MissingValue;
                qa.workspace = args[i];
            } else if (!std.mem.startsWith(u8, arg, "-")) {
                qa.query_str = arg;
            }
        }
        return qa;
    }
};

fn cmdQuery(allocator: std.mem.Allocator, args: []const []const u8) !void {
    const qa = QueryArgs.parse(args) catch |err| {
        std.debug.print("error: query flag missing value ({s})\n", .{@errorName(err)});
        return err;
    };

    const query_text = qa.query_str orelse {
        std.debug.print("Error: query string required\n", .{});
        return;
    };

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const workspace = try resolveAbsOrJoin(allocator, cwd, qa.workspace orelse cwd);
    defer allocator.free(workspace);

    const db_path = try resolveAbsOrJoin(allocator, workspace, qa.db_path orelse config_mod.DEFAULT_GUIDANCE_DB_PATH);
    defer allocator.free(db_path);

    std.fs.accessAbsolute(db_path, .{}) catch {
        std.debug.print("Error: No .guidance.db found at {s}\n", .{db_path});
        std.debug.print("Run 'guidance gen' to generate it.\n", .{});
        return;
    };

    const cfg = config_mod.loadConfig(allocator, workspace) catch
        try config_mod.loadConfig(allocator, cwd);
    defer @constCast(&cfg).deinit();

    const embedder = vector_mod.createEmbeddingProvider(
        allocator,
        cfg.embedding_provider,
        null,
        cfg.embedding_model,
        cfg.embedding_dims,
    ) catch blk: {
        var noop = try allocator.create(vector_mod.NoopEmbedding);
        noop.* = .{ .allocator = allocator };
        break :blk noop.provider();
    };
    defer embedder.deinit();

    var db = GuidanceDb.init(allocator, db_path, embedder) catch |err| {
        std.debug.print("Error opening database: {s}\n", .{@errorName(err)});
        return;
    };
    defer db.deinit();

    const results = try db.search(allocator, query_text, qa.limit);
    defer {
        for (results) |r| freeSearchResult(allocator, r);
        allocator.free(results);
    }

    if (results.len == 0) {
        std.debug.print("No results found for: {s}\n", .{query_text});
        return;
    }

    if (qa.json_mode) {
        try printQueryJson(query_text, results);
    } else {
        try printQueryText(query_text, results);
    }
}

fn printQueryText(query_text: []const u8, results: []SearchResult) !void {
    var ws: llm.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();

    try stdout.print("# Query: {s}\n\n", .{query_text});

    for (results, 1..) |r, idx| {
        try stdout.print("{d}. **{s}** ({s}) — {s}", .{ idx, r.name, r.node_type, r.module });
        if (r.line) |l| try stdout.print(":{d}", .{l});
        try stdout.print("\n", .{});

        if (r.comment) |c| {
            const nl = std.mem.indexOfScalar(u8, c, '\n') orelse c.len;
            const snippet = c[0..@min(nl, 120)];
            try stdout.print("   {s}\n", .{snippet});
        }
        if (r.signature) |s| {
            try stdout.print("   `{s}`\n", .{s});
        }
        try stdout.print("\n", .{});
    }

    try stdout.flush();
}

/// Write a single JSON string value — the content between the surrounding
/// quotes — with all special characters properly escaped. Call this instead
/// of `{s}` format specifiers inside JSON literals to prevent malformed output
/// when field values contain `"`, `\`, or control characters.
fn writeJsonStr(writer: *std.Io.Writer, s: []const u8) !void {
    for (s) |c| {
        switch (c) {
            '"' => try writer.writeAll("\\\""),
            '\\' => try writer.writeAll("\\\\"),
            '\n' => try writer.writeAll("\\n"),
            '\r' => try writer.writeAll("\\r"),
            '\t' => try writer.writeAll("\\t"),
            else => try writer.writeByte(c),
        }
    }
}

fn printQueryJson(query_text: []const u8, results: []SearchResult) !void {
    var ws: llm.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();

    try stdout.writeAll("{\"query\":\"");
    try writeJsonStr(stdout, query_text);
    try stdout.writeAll("\",\"results\":[\n");

    for (results, 0..) |r, i| {
        try stdout.writeAll("  {\"module\":\"");
        try writeJsonStr(stdout, r.module);
        try stdout.writeAll("\",\"name\":\"");
        try writeJsonStr(stdout, r.name);
        try stdout.writeAll("\",\"type\":\"");
        try writeJsonStr(stdout, r.node_type);
        try stdout.writeByte('"');
        if (r.signature) |s| {
            try stdout.writeAll(",\"signature\":\"");
            try writeJsonStr(stdout, s);
            try stdout.writeByte('"');
        }
        if (r.comment) |c| {
            const nl = std.mem.indexOfScalar(u8, c, '\n') orelse c.len;
            try stdout.writeAll(",\"comment\":\"");
            try writeJsonStr(stdout, c[0..@min(nl, 200)]);
            try stdout.writeByte('"');
        }
        if (r.line) |l| try stdout.print(",\"line\":{d}", .{l});
        try stdout.writeAll(",\"language\":\"");
        try writeJsonStr(stdout, r.language);
        try stdout.print("\",\"score\":{d:.4}}}", .{r.score});
        if (i < results.len - 1) try stdout.writeAll(",\n");
    }

    try stdout.writeAll("]}\n");
    try stdout.flush();
}

// =============================================================================
// explain — shared types
// =============================================================================

const SkillExcerpt = struct { name: []const u8, excerpt: []const u8 };
const ExcerptEntry = struct {
    file_path: []const u8, // borrowed from SearchResult
    label: []const u8, // owned: "src/foo.zig:42"
    code: []const u8, // owned: pruned source block
    lang: []const u8, // borrowed constant
};
const FileMatchItem = struct { path: []const u8, count: usize, lines: []usize };

// =============================================================================
// explain — small path/config helpers
// =============================================================================

fn resolveAbsOrJoin(allocator: std.mem.Allocator, base: []const u8, path: []const u8) ![]const u8 {
    return llm.resolvePath(allocator, base, path);
}

/// Construct an LlmConfig from the parsed ExplainArgs.
fn makeLlmConfig(ea: ExplainArgs) llm.LlmConfig {
    return .{ .api_url = ea.api_url, .model = ea.model, .debug = ea.verbose };
}

/// Central control point for LLM configuration with thinking model support.
///
/// If model matches the thinking slot:
///   - Force Ollama /api/chat endpoint (required for think parameter)
///   - Set think = true
///
/// Returns owned memory in resolved_url (caller must free).
fn resolveLlmConfigForThinking(
    allocator: std.mem.Allocator,
    cfg: *const config_mod.ProjectConfig,
    model_ref: []const u8,
    explicit_api_url: ?[]const u8,
) !struct { api_url: []const u8, model: []const u8, think: ?bool, resolved_url: ?[]const u8 } {
    const is_thinking_slot = cfg.isThinkingModelRef(model_ref);

    // Use explicit API URL if provided
    if (explicit_api_url) |url| {
        return .{
            .api_url = url,
            .model = model_ref,
            .think = if (is_thinking_slot) true else null,
            .resolved_url = null,
        };
    }

    // Thinking model: must use Ollama /api/chat endpoint
    if (is_thinking_slot) {
        // Get ollama provider (uses /api/chat)
        if (cfg.getProvider("ollama")) |ollama| {
            const url = try std.fmt.allocPrint(allocator, "{s}{s}", .{ ollama.base_url, ollama.chat_endpoint });
            return .{
                .api_url = url,
                .model = model_ref,
                .think = true,
                .resolved_url = url,
            };
        }
        // No ollama provider: construct URL with /api/chat endpoint
        const parsed = config_mod.ProjectConfig.parseModelRef(model_ref) orelse {
            // Fallback to localhost
            const url = try allocator.dupe(u8, "http://localhost:11434/api/chat");
            return .{ .api_url = url, .model = model_ref, .think = true, .resolved_url = url };
        };
        if (cfg.getProvider(parsed.provider)) |provider| {
            // Use provider's base_url with /api/chat
            const scheme_end = std.mem.indexOf(u8, provider.base_url, "://") orelse 0;
            const host_start: usize = if (scheme_end > 0) scheme_end + 3 else 0;
            const path_start = std.mem.indexOfScalarPos(u8, provider.base_url, host_start, '/') orelse provider.base_url.len;
            const base = provider.base_url[0..path_start];
            const url = try std.fmt.allocPrint(allocator, "{s}/api/chat", .{base});
            return .{
                .api_url = url,
                .model = model_ref,
                .think = true,
                .resolved_url = url,
            };
        }
        // Final fallback
        const url = try allocator.dupe(u8, "http://localhost:11434/api/chat");
        return .{ .api_url = url, .model = model_ref, .think = true, .resolved_url = url };
    }

    // Non-thinking model: use configured provider
    const parsed = config_mod.ProjectConfig.parseModelRef(model_ref) orelse {
        return .{
            .api_url = config_mod.DEFAULT_API_URL,
            .model = model_ref,
            .think = null,
            .resolved_url = null,
        };
    };

    const provider = cfg.getProvider(parsed.provider) orelse {
        return .{
            .api_url = config_mod.DEFAULT_API_URL,
            .model = model_ref,
            .think = null,
            .resolved_url = null,
        };
    };

    const url = try std.fmt.allocPrint(allocator, "{s}{s}", .{ provider.base_url, provider.chat_endpoint });
    return .{
        .api_url = url,
        .model = model_ref,
        .think = null,
        .resolved_url = url,
    };
}

/// Dispatch to single-file, explicit-scan, or full-workspace processing.
/// Whether LLM relevance filtering should be applied on the staged path.
const FilterMode = enum {
    /// Auto-detect: apply LLM filter only for long queries (5+ words).
    auto,
    /// Always apply LLM filter (even for short queries).
    force,
    /// Never apply LLM filter (always fast path).
    skip,
};

const ExplainArgs = struct {
    query_str: ?[]const u8 = null,
    limit: usize = 10,
    /// Path to .guidance.db. Defaults to config guidance_db_path or DEFAULT_GUIDANCE_DB_PATH.
    db_path: ?[]const u8 = null,
    workspace: ?[]const u8 = null,
    guidance: ?[]const u8 = null,
    api_url: []const u8 = config_mod.DEFAULT_API_URL,
    model: []const u8 = config_mod.DEFAULT_MODEL,
    /// Skip LLM synthesis; emit structural output only.
    no_llm: bool = false,
    verbose: bool = false,
    /// Use new staged pipeline (default: true).  --staged=false → legacy path.
    staged: bool = true,
    /// LLM relevance filtering mode.
    filter: FilterMode = .auto,
};

/// Return true when the query is "short" (fast path, no LLM filter).
/// Short queries: 2 or fewer words, AND not ending with "?", AND not starting
/// with question words (if, how, where, when, does, why, what).
fn isShortQuery(query: []const u8) bool {
    const trimmed = std.mem.trim(u8, query, " \t\n\r");
    if (trimmed.len == 0) return true;

    // Question mark at end triggers LLM filter
    if (trimmed[trimmed.len - 1] == '?') return false;

    // Check for question word prefixes (case-insensitive, with trailing space)
    const question_prefixes = [_][]const u8{ "if ", "how ", "where ", "when ", "does ", "why ", "what " };
    for (question_prefixes) |prefix| {
        if (trimmed.len >= prefix.len) {
            const candidate = trimmed[0..prefix.len];
            var i: usize = 0;
            while (i < prefix.len) : (i += 1) {
                if (std.ascii.toLower(candidate[i]) != std.ascii.toLower(prefix[i])) break;
            }
            if (i == prefix.len) return false;
        }
    }

    // Word count: 2 or fewer = short (no LLM filter)
    var tok = std.mem.tokenizeAny(u8, trimmed, " \t\n\r");
    var count: usize = 0;
    while (tok.next()) |_| {
        count += 1;
        if (count > 2) return false;
    }
    return true;
}

// =============================================================================
// explain — phase helpers
// =============================================================================

/// Phase A: Load skill excerpts for the top search result's JSON guidance file.
/// Returns owned slice; caller must free each `.name` and `.excerpt`, then free the slice.
fn collectSkillExcerpts(
    allocator: std.mem.Allocator,
    top_json_path: []const u8,
    guidance_dir: []const u8,
    workspace: []const u8,
) ![]SkillExcerpt {
    var out: std.ArrayList(SkillExcerpt) = .{};
    errdefer {
        for (out.items) |se| {
            allocator.free(se.name);
            allocator.free(se.excerpt);
        }
        out.deinit(allocator);
    }

    const skills_str = loadSkillsFromJson(allocator, top_json_path) orelse return out.toOwnedSlice(allocator);
    defer allocator.free(skills_str);

    var seen: std.StringHashMapUnmanaged(void) = .{};
    defer seen.deinit(allocator);

    var sp = std.mem.splitScalar(u8, skills_str, '\n');
    while (sp.next()) |raw| {
        const name = std.mem.trim(u8, raw, " \t\r");
        if (name.len == 0 or seen.contains(name)) continue;
        try seen.put(allocator, name, {});
        if (loadSkillPara(allocator, guidance_dir, workspace, name)) |para| {
            try out.append(allocator, .{
                .name = try allocator.dupe(u8, name),
                .excerpt = para,
            });
        }
    }
    return out.toOwnedSlice(allocator);
}

/// Phase B: Collect up to 3 source excerpts, preferring exact-name matches.
/// Returns owned slice; caller must free each `.label` and `.code`, then free the slice.
fn collectSourceExcerpts(
    allocator: std.mem.Allocator,
    results: []const SearchResult,
    search_terms: []const []const u8,
    workspace: []const u8,
) ![]ExcerptEntry {
    var out: std.ArrayList(ExcerptEntry) = .{};
    errdefer {
        for (out.items) |e| {
            allocator.free(e.label);
            allocator.free(e.code);
        }
        out.deinit(allocator);
    }

    // Re-sort: exact-name-match first, then non-test, then score.
    var sorted: std.ArrayList(SearchResult) = .{};
    defer sorted.deinit(allocator);
    for (results) |r| try sorted.append(allocator, r);
    std.sort.insertion(SearchResult, sorted.items, search_terms, struct {
        fn lessThan(terms: []const []const u8, a: SearchResult, b: SearchResult) bool {
            const a_exact = isExactNameMatch(a.name, terms);
            const b_exact = isExactNameMatch(b.name, terms);
            if (a_exact != b_exact) return a_exact;
            const a_test = std.mem.eql(u8, a.node_type, "test_decl");
            const b_test = std.mem.eql(u8, b.node_type, "test_decl");
            if (a_test != b_test) return !a_test;
            return a.score > b.score;
        }
    }.lessThan);

    var seen_files: std.StringHashMapUnmanaged(void) = .{};
    defer seen_files.deinit(allocator);

    for (sorted.items) |r| {
        if (out.items.len >= 3) break;
        if (r.source.len == 0 or seen_files.contains(r.source)) continue;
        const start_line = r.line orelse continue;

        const src_abs = try std.fs.path.join(allocator, &.{ workspace, r.source });
        defer allocator.free(src_abs);

        const src_opt: ?[]const u8 = blk: {
            const f = std.fs.openFileAbsolute(src_abs, .{}) catch break :blk null;
            defer f.close();
            break :blk f.readToEndAlloc(allocator, 10 * 1024 * 1024) catch null;
        };
        const src = src_opt orelse continue;
        defer allocator.free(src);

        const code = try explainExtractExcerpt(allocator, src, start_line, r.node_type);
        if (code.len == 0) {
            allocator.free(code);
            continue;
        }
        const lang: []const u8 = if (std.mem.endsWith(u8, r.source, ".zig")) "zig" else if (std.mem.endsWith(u8, r.source, ".py")) "python" else "text";
        const label = try std.fmt.allocPrint(allocator, "{s}:{d}", .{ r.source, start_line });
        try out.append(allocator, .{ .file_path = r.source, .label = label, .code = code, .lang = lang });
        try seen_files.put(allocator, r.source, {});
    }
    return out.toOwnedSlice(allocator);
}

/// Phase C: Grep top result files for search-term matches.
/// Returns owned slice; caller must free each `.lines`, then free the slice.
fn grepTopFiles(
    allocator: std.mem.Allocator,
    results: []const SearchResult,
    search_terms: []const []const u8,
    workspace: []const u8,
) ![]FileMatchItem {
    var out: std.ArrayList(FileMatchItem) = .{};
    errdefer {
        for (out.items) |fm| allocator.free(fm.lines);
        out.deinit(allocator);
    }

    var seen: std.StringHashMapUnmanaged(void) = .{};
    defer seen.deinit(allocator);

    for (results[0..@min(5, results.len)]) |r| {
        if (r.source.len == 0 or seen.contains(r.source)) continue;
        try seen.put(allocator, r.source, {});

        const abs = try std.fs.path.join(allocator, &.{ workspace, r.source });
        defer allocator.free(abs);

        const matches = try explainGrepFile(allocator, abs, search_terms, 10);
        if (matches.len > 0) {
            try out.append(allocator, .{ .path = r.source, .count = matches.len, .lines = matches });
        } else {
            allocator.free(matches);
        }
    }

    std.sort.insertion(FileMatchItem, out.items, {}, struct {
        fn less(_: void, a: FileMatchItem, b: FileMatchItem) bool {
            return a.count > b.count;
        }
    }.less);

    return out.toOwnedSlice(allocator);
}

/// Phase E: Render the legacy explain output to stdout.
fn renderExplainOutput(
    allocator: std.mem.Allocator,
    query_text: []const u8,
    results: []const SearchResult,
    search_terms: []const []const u8,
    ai_summary: ?[]const u8,
    skill_excerpts: []const SkillExcerpt,
    excerpts: []const ExcerptEntry,
    file_matches: []const FileMatchItem,
) !void {
    var ws: llm.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();

    try stdout.print("# Explain: {s}\n\n", .{query_text});

    if (ai_summary) |s| {
        const trimmed_s = std.mem.trim(u8, s, " \t\n\r");
        if (trimmed_s.len > 0) try stdout.print("{s}\n\n", .{trimmed_s});
    }

    try stdout.print("---\n", .{});
    try stdout.print("**Source**: `{s}`\n", .{results[0].source});

    for (skill_excerpts[0..@min(2, skill_excerpts.len)]) |se| {
        const first_nl = std.mem.indexOfScalar(u8, se.excerpt, '\n') orelse se.excerpt.len;
        try stdout.print("**Pattern**: `{s}` — {s}\n", .{ se.name, se.excerpt[0..@min(first_nl, 120)] });
    }
    try stdout.print("\n", .{});

    for (excerpts) |e| {
        try stdout.print("```{s}\n// {s}\n{s}\n```\n\n", .{ e.lang, e.label, e.code });
    }

    // Keywords: public non-test members from primary source JSON, excluding search terms.
    {
        var kw_buf: std.ArrayList(u8) = .{};
        defer kw_buf.deinit(allocator);
        var kw_count: usize = 0;

        if (loadPublicMemberNames(allocator, results[0].file_path)) |names| {
            defer {
                for (names) |n| allocator.free(n);
                allocator.free(names);
            }
            for (names) |mname| {
                if (kw_count >= 8) break;
                const mname_lower = try std.ascii.allocLowerString(allocator, mname);
                defer allocator.free(mname_lower);
                const is_term = for (search_terms) |term| {
                    if (std.mem.eql(u8, mname_lower, term)) break true;
                } else false;
                if (is_term) continue;
                if (kw_count > 0) try kw_buf.appendSlice(allocator, ", ");
                try kw_buf.writer(allocator).print("`{s}`", .{mname});
                kw_count += 1;
            }
        }
        if (kw_count > 0) try stdout.print("**See Also**: {s}\n\n", .{kw_buf.items});
    }

    // See also: used_by from top result + secondary file paths.
    {
        var see_buf: std.ArrayList(u8) = .{};
        defer see_buf.deinit(allocator);
        var see_count: usize = 0;

        var ub_from_json: ?[][]const u8 = null;
        defer if (ub_from_json) |ub| {
            for (ub) |s| allocator.free(s);
            allocator.free(ub);
        };
        const top_used_by: [][]const u8 = if (results[0].used_by.len > 0)
            results[0].used_by
        else blk: {
            ub_from_json = loadUsedByFromJson(allocator, results[0].file_path);
            break :blk ub_from_json orelse &.{};
        };

        for (top_used_by[0..@min(4, top_used_by.len)]) |ub| {
            if (see_count > 0) try see_buf.appendSlice(allocator, ", ");
            try see_buf.writer(allocator).print("`{s}`", .{ub});
            see_count += 1;
        }
        for (results[1..@min(results.len, 6)]) |r| {
            if (see_count >= 6) break;
            if (r.source.len == 0 or std.mem.eql(u8, r.source, results[0].source)) continue;
            if (see_count > 0) try see_buf.appendSlice(allocator, ", ");
            try see_buf.writer(allocator).print("`{s}`", .{r.source});
            see_count += 1;
        }
        if (see_count > 0) try stdout.print("**See also**: {s}\n\n", .{see_buf.items});
    }

    if (file_matches.len > 0) {
        try stdout.print("### Files with most matches\n\n", .{});
        for (file_matches[0..@min(3, file_matches.len)]) |fm| {
            try stdout.print("- `{s}` ({d} matches): lines ", .{ fm.path, fm.count });
            for (fm.lines[0..@min(10, fm.lines.len)], 0..) |ln, li| {
                if (li > 0) try stdout.print(", ", .{});
                try stdout.print("{d}", .{ln});
            }
            try stdout.print("\n", .{});
        }
        try stdout.print("\n", .{});
    }

    try stdout.flush();
}

fn cmdExplain(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var ea: ExplainArgs = .{};
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "-l") or std.mem.eql(u8, arg, "--limit")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --limit requires a value\n", .{});
                return;
            }
            ea.limit = std.fmt.parseInt(usize, args[i], 10) catch 10;
        } else if (std.mem.eql(u8, arg, "-o") or std.mem.eql(u8, arg, "--db")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --db requires a value\n", .{});
                return;
            }
            ea.db_path = args[i];
        } else if (std.mem.eql(u8, arg, "-w") or std.mem.eql(u8, arg, "--workspace")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --workspace requires a value\n", .{});
                return;
            }
            ea.workspace = args[i];
        } else if (std.mem.eql(u8, arg, "--api-url")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --api-url requires a value\n", .{});
                return;
            }
            ea.api_url = args[i];
        } else if (std.mem.eql(u8, arg, "-m") or std.mem.eql(u8, arg, "--model")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --model requires a value\n", .{});
                return;
            }
            ea.model = args[i];
        } else if (std.mem.eql(u8, arg, "--no-llm")) {
            ea.no_llm = true;
        } else if (std.mem.eql(u8, arg, "--guidance")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --guidance requires a value\n", .{});
                return;
            }
            ea.guidance = args[i];
        } else if (std.mem.startsWith(u8, arg, "--staged=")) {
            ea.staged = !std.mem.eql(u8, arg["--staged=".len..], "false");
        } else if (std.mem.eql(u8, arg, "--staged")) {
            ea.staged = true;
        } else if (std.mem.startsWith(u8, arg, "--filter=")) {
            ea.filter = std.meta.stringToEnum(FilterMode, arg["--filter=".len..]) orelse .auto;
        } else if (std.mem.eql(u8, arg, "--guidance-db")) {
            // Alias for -o / --db for backward compatibility.
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --guidance-db requires a value\n", .{});
                return;
            }
            ea.db_path = args[i];
        } else if (std.mem.eql(u8, arg, "--db-type=lance") or
            std.mem.eql(u8, arg, "--lance") or
            std.mem.startsWith(u8, arg, "--db-type="))
        {
            // Accepted but ignored — LanceDB is always used.
        } else if (!std.mem.startsWith(u8, arg, "-")) {
            ea.query_str = arg;
        }
    }

    const query_text = ea.query_str orelse {
        std.debug.print("Error: query string required\n", .{});
        return;
    };

    // ── Resolve paths ─────────────────────────────────────────────────────────
    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const workspace = if (ea.workspace) |w|
        try resolveAbsOrJoin(allocator, cwd, w)
    else
        try allocator.dupe(u8, cwd);
    defer allocator.free(workspace);

    // Load config for embedding provider and db path defaults.
    const cfg = config_mod.loadConfig(allocator, workspace) catch
        try config_mod.loadConfig(allocator, cwd);
    defer @constCast(&cfg).deinit();

    const db_path = try resolveAbsOrJoin(
        allocator,
        workspace,
        ea.db_path orelse cfg.guidance_db_path,
    );
    defer allocator.free(db_path);

    const guidance_dir = try resolveAbsOrJoin(allocator, workspace, ea.guidance orelse config_mod.DEFAULT_GUIDANCE_DIR);
    defer allocator.free(guidance_dir);

    // ── Open .guidance.db ─────────────────────────────────────────────────────
    std.fs.accessAbsolute(db_path, .{}) catch {
        std.debug.print("Error: No .guidance.db found at {s}\n", .{db_path});
        std.debug.print("Run 'guidance gen' to generate it.\n", .{});
        return;
    };

    const embedder = vector_mod.createEmbeddingProvider(
        allocator,
        cfg.embedding_provider,
        null,
        cfg.embedding_model,
        cfg.embedding_dims,
    ) catch blk: {
        var noop = try allocator.create(vector_mod.NoopEmbedding);
        noop.* = .{ .allocator = allocator };
        break :blk noop.provider();
    };
    defer embedder.deinit();

    var db = GuidanceDb.init(allocator, db_path, embedder) catch |err| {
        std.debug.print("Error opening database: {s}\n", .{@errorName(err)});
        return;
    };
    defer db.deinit();

    // ── Staged pipeline (default) ──────────────────────────────────────────────
    if (ea.staged) {
        staged_path: {
            // Resolve LLM config with thinking model support
            var resolved_url_to_free: ?[]const u8 = null;
            defer if (resolved_url_to_free) |url| allocator.free(url);

            const model = if (std.mem.eql(u8, ea.model, config_mod.DEFAULT_MODEL))
                cfg.model_default
            else
                ea.model;

            const llm_config = blk: {
                const resolved = resolveLlmConfigForThinking(
                    allocator,
                    &cfg,
                    model,
                    if (std.mem.eql(u8, ea.api_url, config_mod.DEFAULT_API_URL)) null else ea.api_url,
                ) catch {
                    // Fallback to direct args
                    break :blk llm.LlmConfig{
                        .api_url = ea.api_url,
                        .model = ea.model,
                        .debug = ea.verbose,
                    };
                };
                resolved_url_to_free = resolved.resolved_url;
                break :blk llm.LlmConfig{
                    .api_url = resolved.api_url,
                    .model = resolved.model,
                    .think = resolved.think,
                    .debug = ea.verbose,
                };
            };

            cmdExplainStaged(allocator, &db, query_text, workspace, guidance_dir, llm_config, cfg.infillModel(), ea) catch |err| {
                if (ea.verbose) std.debug.print("staged explain failed ({s}), falling back to legacy\n", .{@errorName(err)});
                break :staged_path;
            };
            return;
        }
    }

    // ── Legacy path (--staged=false) ──────────────────────────────────────────
    const results = db.search(allocator, query_text, ea.limit) catch |err| {
        std.debug.print("Search failed: {s}\n", .{@errorName(err)});
        return;
    };
    defer {
        for (results) |r| freeSearchResult(allocator, r);
        allocator.free(results);
    }

    if (results.len == 0) {
        const lower_q = try std.ascii.allocLowerString(allocator, query_text);
        defer allocator.free(lower_q);
        std.debug.print("# Explain: {s}\n\nNot indexed for '{s}'. Search the source directly:\n\n", .{ query_text, query_text });
        std.debug.print("    grep -ri '{s}' src/ | head -n 20\n\n", .{lower_q});
        std.debug.print("Run 'guidance gen' after finding the file to index it.\n", .{});
        return;
    }

    // Build normalised search terms (lowercase tokens).
    var search_terms: std.ArrayList([]const u8) = .{};
    defer {
        for (search_terms.items) |t| allocator.free(t);
        search_terms.deinit(allocator);
    }
    {
        var tok = std.mem.tokenizeAny(u8, query_text, " \t_");
        while (tok.next()) |word| {
            try search_terms.append(allocator, try std.ascii.allocLowerString(allocator, word));
        }
        if (search_terms.items.len == 0)
            try search_terms.append(allocator, try std.ascii.allocLowerString(allocator, query_text));
    }

    // ── Phase A: Skill excerpts ───────────────────────────────────────────────
    const skill_excerpts = try collectSkillExcerpts(allocator, results[0].file_path, guidance_dir, workspace);
    defer {
        for (skill_excerpts) |se| {
            allocator.free(se.name);
            allocator.free(se.excerpt);
        }
        allocator.free(skill_excerpts);
    }

    // ── Phase B: Source excerpts ──────────────────────────────────────────────
    const excerpts = try collectSourceExcerpts(allocator, results, search_terms.items, workspace);
    defer {
        for (excerpts) |e| {
            allocator.free(e.label);
            allocator.free(e.code);
        }
        allocator.free(excerpts);
    }

    // ── Phase C: Grep top files ───────────────────────────────────────────────
    const file_matches = try grepTopFiles(allocator, results, search_terms.items, workspace);
    defer {
        for (file_matches) |fm| allocator.free(fm.lines);
        allocator.free(file_matches);
    }

    // ── Phase D: LLM synthesis ────────────────────────────────────────────────
    var ai_summary: ?[]const u8 = null;
    defer if (ai_summary) |s| allocator.free(s);

    if (!ea.no_llm) {
        var client_opt: ?llm.LlmClient = llm.LlmClient.init(allocator, makeLlmConfig(ea)) catch null;
        defer if (client_opt) |*c| c.deinit();
        if (client_opt) |*client| {
            ai_summary = buildLlmSummary(allocator, client, query_text, results, skill_excerpts, excerpts) catch null;
        }
    }

    // ── Phase E: Render output ────────────────────────────────────────────────
    try renderExplainOutput(allocator, query_text, results, search_terms.items, ai_summary, skill_excerpts, excerpts, file_matches);
}

// ---------------------------------------------------------------------------
// explain helpers
// ---------------------------------------------------------------------------

/// Load `used_by` array from a guidance JSON file.
/// Returns an owned slice of owned strings, or null on failure / empty.
fn loadUsedByFromJson(allocator: std.mem.Allocator, json_path: []const u8) ?[][]const u8 {
    const f = std.fs.openFileAbsolute(json_path, .{}) catch return null;
    defer f.close();
    const content = f.readToEndAlloc(allocator, 8 * 1024 * 1024) catch return null;
    defer allocator.free(content);

    var parsed = std.json.parseFromSlice(std.json.Value, allocator, content, .{ .ignore_unknown_fields = true }) catch return null;
    defer parsed.deinit();

    const ub_val = parsed.value.object.get("used_by") orelse return null;
    if (ub_val != .array) return null;

    var out: std.ArrayList([]const u8) = .{};
    for (ub_val.array.items) |item| {
        if (item != .string) continue;
        out.append(allocator, allocator.dupe(u8, item.string) catch continue) catch continue;
    }
    if (out.items.len == 0) {
        out.deinit(allocator);
        return null;
    }
    return out.toOwnedSlice(allocator) catch null;
}

/// Return true when `name` (case-insensitive) exactly equals any search term.
fn isExactNameMatch(name: []const u8, terms: []const []const u8) bool {
    // Fast path — avoid allocation for short names.
    var buf: [128]u8 = undefined;
    if (name.len > buf.len) return false;
    const lower = std.ascii.lowerString(buf[0..name.len], name);
    for (terms) |term| {
        if (std.mem.eql(u8, lower, term)) return true;
    }
    return false;
}

/// Load skills listed in a guidance JSON file as a newline-separated string.
/// Returns an owned allocation or null if the file is absent or has no skills.
fn loadSkillsFromJson(allocator: std.mem.Allocator, json_path: []const u8) ?[]const u8 {
    const f = std.fs.openFileAbsolute(json_path, .{}) catch return null;
    defer f.close();
    const content = f.readToEndAlloc(allocator, 8 * 1024 * 1024) catch return null;
    defer allocator.free(content);

    var parsed = std.json.parseFromSlice(std.json.Value, allocator, content, .{ .ignore_unknown_fields = true }) catch return null;
    defer parsed.deinit();

    const skills_val = parsed.value.object.get("skills") orelse return null;
    if (skills_val != .array) return null;

    var out: std.ArrayList(u8) = .{};
    for (skills_val.array.items) |item| {
        // skills[] entries may be strings or objects with a "ref" field.
        const ref: []const u8 = switch (item) {
            .string => |s| s,
            .object => blk: {
                const rv = item.object.get("ref") orelse break :blk "";
                if (rv != .string) break :blk "";
                break :blk rv.string;
            },
            else => "",
        };
        if (ref.len == 0) continue;
        // Derive skill name: last path component before SKILL.md.
        // e.g. ".skills/gof-patterns/SKILL.md" → "gof-patterns"
        const base = std.fs.path.basename(ref);
        const skill_name: []const u8 = if (std.mem.eql(u8, base, "SKILL.md")) blk: {
            const dir = std.fs.path.dirname(ref) orelse break :blk base;
            break :blk std.fs.path.basename(dir);
        } else base;
        if (skill_name.len == 0) continue;
        out.appendSlice(allocator, skill_name) catch continue;
        out.append(allocator, '\n') catch continue;
    }
    if (out.items.len == 0) {
        out.deinit(allocator);
        return null;
    }
    return out.toOwnedSlice(allocator) catch null;
}

/// Load public non-test member names from a guidance JSON file.
/// Returns an owned slice of owned strings, or null on failure.
fn loadPublicMemberNames(allocator: std.mem.Allocator, json_path: []const u8) ?[][]const u8 {
    const f = std.fs.openFileAbsolute(json_path, .{}) catch return null;
    defer f.close();
    const content = f.readToEndAlloc(allocator, 8 * 1024 * 1024) catch return null;
    defer allocator.free(content);

    var parsed = std.json.parseFromSlice(std.json.Value, allocator, content, .{ .ignore_unknown_fields = true }) catch return null;
    defer parsed.deinit();

    const members_val = parsed.value.object.get("members") orelse return null;
    if (members_val != .array) return null;

    var out: std.ArrayList([]const u8) = .{};
    for (members_val.array.items) |item| {
        if (item != .object) continue;
        // Skip non-public members.
        const is_pub: bool = blk: {
            const pv = item.object.get("is_pub") orelse break :blk false;
            if (pv != .bool) break :blk false;
            break :blk pv.bool;
        };
        if (!is_pub) continue;
        // Skip test declarations.
        const type_v = item.object.get("type") orelse continue;
        if (type_v != .string) continue;
        if (std.mem.eql(u8, type_v.string, "test_decl")) continue;
        // Get name.
        const name_v = item.object.get("name") orelse continue;
        if (name_v != .string) continue;
        if (name_v.string.len == 0) continue;
        out.append(allocator, allocator.dupe(u8, name_v.string) catch continue) catch continue;
    }
    if (out.items.len == 0) {
        out.deinit(allocator);
        return null;
    }
    return out.toOwnedSlice(allocator) catch null;
}

/// Load the first paragraph (or `description:` front-matter value) of a SKILL.md.
/// Searches `<guidance_dir>/.skills/<name>/SKILL.md` and `<cwd>/doc/skills/<name>/SKILL.md`.
/// Returns an owned allocation or null if not found.
fn loadSkillPara(
    allocator: std.mem.Allocator,
    guidance_dir: []const u8,
    cwd: []const u8,
    skill_name: []const u8,
) ?[]const u8 {
    const SearchPath = struct { base: []const u8, rel: []const u8 };
    const paths = [_]SearchPath{
        .{ .base = guidance_dir, .rel = ".skills" },
        .{ .base = cwd, .rel = "doc/skills" },
    };
    for (paths) |sp| {
        const path = std.fs.path.join(allocator, &.{ sp.base, sp.rel, skill_name, "SKILL.md" }) catch continue;
        defer allocator.free(path);
        const sf = std.fs.openFileAbsolute(path, .{}) catch continue;
        defer sf.close();
        const content = sf.readToEndAlloc(allocator, 512 * 1024) catch continue;
        defer allocator.free(content);

        if (std.mem.startsWith(u8, content, "---\n")) {
            // YAML front matter — look for `description:`.
            const fm_close = std.mem.indexOf(u8, content[4..], "\n---\n") orelse {
                var lines = std.mem.splitScalar(u8, content[4..], '\n');
                while (lines.next()) |line| {
                    const t = std.mem.trim(u8, line, " \t\r");
                    if (t.len > 0 and !std.mem.eql(u8, t, "---"))
                        return allocator.dupe(u8, t[0..@min(t.len, 200)]) catch null;
                }
                return null;
            };
            const fm_body = content[4 .. 4 + fm_close];
            var fm_lines = std.mem.splitScalar(u8, fm_body, '\n');
            while (fm_lines.next()) |fl| {
                if (std.mem.startsWith(u8, fl, "description:")) {
                    const val = std.mem.trim(u8, fl["description:".len..], " \t\r");
                    if (val.len > 0) return allocator.dupe(u8, val[0..@min(val.len, 200)]) catch null;
                }
            }
            // No description: — return first non-empty body line.
            const after_fm = content[4 + fm_close + 5 ..];
            var body = std.mem.splitScalar(u8, after_fm, '\n');
            while (body.next()) |bl| {
                const t = std.mem.trim(u8, bl, " \t\r");
                if (t.len > 0) return allocator.dupe(u8, t[0..@min(t.len, 200)]) catch null;
            }
            return null;
        }
        // No front matter — first paragraph (up to blank line), max 600 chars.
        const para_end = std.mem.indexOf(u8, content, "\n\n") orelse content.len;
        return allocator.dupe(u8, content[0..@min(para_end, 600)]) catch null;
    }
    return null;
}

/// Extract the source block starting at `start_line` (1-based).
/// Stops at the next col-0 top-level declaration or after MAX_LINES, whichever
/// comes first.  Then prunes trailing blank and comment-only lines.
/// Returns an owned allocation; caller must free.
fn explainExtractExcerpt(
    allocator: std.mem.Allocator,
    src: []const u8,
    start_line: u32,
    node_type: []const u8,
) ![]const u8 {
    const node_type_enum = llm.NodeType.fromString(node_type);
    return llm.extractExcerpt(allocator, src, start_line, node_type_enum, 80);
}

/// Grep a file for search terms (case-insensitive substring, skipping comment lines).
/// Returns owned slice of matching line numbers (caller frees).
fn explainGrepFile(
    allocator: std.mem.Allocator,
    file_path: []const u8,
    terms: []const []const u8,
    max_results: usize,
) ![]usize {
    const f = std.fs.openFileAbsolute(file_path, .{}) catch return &.{};
    defer f.close();
    const content = f.readToEndAlloc(allocator, 10 * 1024 * 1024) catch return &.{};
    defer allocator.free(content);

    var line_numbers: std.ArrayList(usize) = .{};
    errdefer line_numbers.deinit(allocator);
    var it = std.mem.splitScalar(u8, content, '\n');
    var line_no: usize = 0;
    while (it.next()) |line| {
        line_no += 1;
        if (line_numbers.items.len >= max_results) break;
        const trimmed = std.mem.trimLeft(u8, line, " \t");
        if (std.mem.startsWith(u8, trimmed, "//") or std.mem.startsWith(u8, trimmed, "#")) continue;
        const lower = try std.ascii.allocLowerString(allocator, line);
        defer allocator.free(lower);
        for (terms) |term| {
            if (std.mem.indexOf(u8, lower, term) != null) {
                try line_numbers.append(allocator, line_no);
                break;
            }
        }
    }
    return line_numbers.toOwnedSlice(allocator);
}

/// Build LLM synthesis: skill context + member index + excerpts → prompt → summary.
/// Strips "absence" sentences.  Returns owned string or null on failure.
fn buildLlmSummary(
    allocator: std.mem.Allocator,
    client: *llm.LlmClient,
    query_text: []const u8,
    results: []const SearchResult,
    skill_excerpts_in: []const SkillExcerpt,
    excerpts_in: []const ExcerptEntry,
) !?[]const u8 {
    var kb: std.ArrayList(u8) = .{};
    defer kb.deinit(allocator);
    const kbw = kb.writer(allocator);

    // 1. Skill context first.
    if (skill_excerpts_in.len > 0) {
        try kbw.writeAll("=== Skill patterns ===\n");
        for (skill_excerpts_in[0..@min(2, skill_excerpts_in.len)]) |se| {
            try kbw.print("{s}: {s}\n\n", .{ se.name, se.excerpt });
        }
    }

    // 2. Module sections.
    var seen_files: std.StringHashMapUnmanaged(void) = .{};
    defer seen_files.deinit(allocator);
    for (results[0..@min(5, results.len)]) |r| {
        const src_key = if (r.source.len > 0) r.source else r.file_path;
        if (seen_files.contains(src_key)) {
            // Just add the member line.
            try kbw.print("  {s} (line {?})", .{ r.name, r.line });
            if (r.signature) |sig| try kbw.print(": {s}", .{sig});
            if (r.comment) |cm| {
                const nl = std.mem.indexOfScalar(u8, cm, '\n') orelse cm.len;
                try kbw.print(" — {s}", .{cm[0..@min(nl, 120)]});
            }
            try kbw.print("\n", .{});
            continue;
        }
        try seen_files.put(allocator, src_key, {});
        try kbw.print("=== {s} ===\n", .{src_key});
        if (r.comment) |cm| {
            const nl = std.mem.indexOfScalar(u8, cm, '\n') orelse cm.len;
            try kbw.print("{s}\n", .{cm[0..nl]});
        }
        if (r.used_by.len > 0) {
            try kbw.writeAll("Used by: ");
            for (r.used_by, 0..) |ub, ui| {
                if (ui > 0) try kbw.writeAll(", ");
                try kbw.writeAll(ub);
            }
            try kbw.writeByte('\n');
        }
        try kbw.print("\nMember: {s} (line {?})", .{ r.name, r.line });
        if (r.signature) |sig| try kbw.print(": {s}", .{sig});
        try kbw.writeByte('\n');
    }

    // 3. Source excerpts.
    for (excerpts_in[0..@min(2, excerpts_in.len)]) |e| {
        try kbw.print("\nSource excerpt ({s}):\n{s}\n\n", .{ e.label, e.code });
    }

    // 4. Build skill-name string for the instruction.
    var skill_names_buf: std.ArrayList(u8) = .{};
    defer skill_names_buf.deinit(allocator);
    for (skill_excerpts_in, 0..) |se, si| {
        if (si > 0) try skill_names_buf.appendSlice(allocator, ", ");
        try skill_names_buf.appendSlice(allocator, se.name);
    }
    const skill_instruction: []const u8 = if (skill_names_buf.items.len > 0)
        try std.fmt.allocPrint(allocator, "SKILL PATTERNS APPLIED: {s}\nThe code implements these patterns — name them in your summary.\n", .{skill_names_buf.items})
    else
        try allocator.dupe(u8, "");
    defer allocator.free(skill_instruction);

    const prompt = try std.fmt.allocPrint(
        allocator,
        "You are a code navigation assistant for a Zig/Python codebase. Be precise and terse.\n{s}\nSummarise '{s}': what it is, what design pattern it implements (if any), key members/functions with line numbers, and who calls it. 3-5 sentences. Use only facts from KNOWLEDGE. STRICT RULE: Never write sentences about absence.\n\nKNOWLEDGE:\n{s}\n\nReturn only the summary.",
        .{ skill_instruction, query_text, kb.items },
    );
    defer allocator.free(prompt);

    const raw = (client.complete(prompt, 1500, 0.15, null) catch null) orelse return null;
    defer allocator.free(raw);

    const cleaned = try synthesize_mod.stripAbsenceSentences(allocator, llm.stripThinkBlock(raw));
    defer allocator.free(cleaned);

    const trimmed = std.mem.trim(u8, cleaned, " \t\n\r");
    if (trimmed.len == 0) return null;
    return try allocator.dupe(u8, trimmed);
}

// =============================================================================
// Staged explain implementation  (M3/M5-M9)
// =============================================================================

/// Load semantic aliases from the guidance directory.
fn loadAliases(allocator: std.mem.Allocator, guidance_dir: []const u8) ?lance_db_mod.SemanticAliases {
    const alias_path = std.fs.path.join(allocator, &.{ guidance_dir, "semantic-aliases.json" }) catch return null;
    defer allocator.free(alias_path);
    return lance_db_mod.loadSemanticAliases(allocator, alias_path) catch null;
}

/// Extract key technical terms from a long query using LLM.
/// Returns owned slice of owned strings. Caller must free.
fn llmExtractKeyTerms(allocator: std.mem.Allocator, client: *llm.LlmClient, query: []const u8) !?[][]const u8 {
    const prompt = try std.fmt.allocPrint(allocator,
        \\Extract 3-5 key technical terms from this query. Return only a comma-separated list, no other text.
        \\Query: {s}
        \\
    , .{query});
    defer allocator.free(prompt);

    const response_opt = client.complete(prompt, 50, 0.0, null) catch return null;
    const response = response_opt orelse return null;
    defer allocator.free(response);

    const stripped = llm.stripThinkBlock(response);
    const trimmed = std.mem.trim(u8, stripped, " \t\n\r");
    if (trimmed.len == 0) return null;

    var terms: std.ArrayList([]const u8) = .{};
    errdefer {
        for (terms.items) |t| allocator.free(t);
        terms.deinit(allocator);
    }

    var it = std.mem.splitAny(u8, trimmed, ",\n");
    var count: usize = 0;
    while (it.next()) |term| {
        if (count >= 4) break;
        const t = std.mem.trim(u8, term, " \t\n\r\"");
        if (t.len == 0) continue;
        try terms.append(allocator, try allocator.dupe(u8, t));
        count += 1;
    }

    if (terms.items.len == 0) return null;
    return try terms.toOwnedSlice(allocator);
}

/// Full staged explain pipeline.  Called when `--staged` is active (default).
///
/// Pipeline:
///   Short query (≤3 words) or --no-llm or --filter=skip:
///     executeStaged() → formatStaged() → output
///   Long query (4+ words) with LLM:
///     executeStaged() → llmFilter() → expandFollowUps() → synthesize() → formatStaged() → output
fn cmdExplainStaged(
    allocator: std.mem.Allocator,
    db: *GuidanceDb,
    query_text: []const u8,
    workspace: []const u8,
    guidance_dir: []const u8,
    llm_config: llm.LlmConfig,
    fast_model_ref: []const u8,
    ea: ExplainArgs,
) !void {
    const skills_dir = try std.fs.path.join(allocator, &.{ guidance_dir, ".skills" });
    defer allocator.free(skills_dir);

    var aliases_opt: ?lance_db_mod.SemanticAliases = loadAliases(allocator, guidance_dir);
    defer if (aliases_opt) |*a| a.deinit();

    // use_llm: always on unless --no-llm is specified
    // use_filter: depends on --filter mode (auto enables filter for long queries only)
    const use_llm = !ea.no_llm;
    const use_filter = !ea.no_llm and switch (ea.filter) {
        .skip => false,
        .force => true,
        .auto => !isShortQuery(query_text),
    };

    // Create the LLM client for filtering (default model)
    var client_opt: ?llm.LlmClient = if (use_llm) llm.LlmClient.init(allocator, llm_config) catch |err| blk: {
        if (ea.verbose) std.debug.print("DEBUG: LLM client init failed: {}\n", .{err});
        break :blk null;
    } else null;
    defer if (client_opt) |*c| c.deinit();

    // Create separate client for synthesis (fast model)
    var fast_client_opt: ?llm.LlmClient = null;
    defer if (fast_client_opt) |*c| c.deinit();

    if (use_llm and fast_model_ref.len > 0) {
        const fast_config = llm.LlmConfig{
            .api_url = llm_config.api_url,
            .model = fast_model_ref,
            .think = null, // fast model never uses thinking
            .debug = ea.verbose,
        };
        fast_client_opt = llm.LlmClient.init(allocator, fast_config) catch null;
    }

    if (ea.verbose) {
        if (client_opt) |_| {
            std.debug.print("DEBUG: LLM client initialized - api_url: {s}, model: {s}, think: {?}\n", .{ llm_config.api_url, llm_config.model, llm_config.think });
        } else {
            std.debug.print("DEBUG: LLM client is null, synthesis will be skipped\n", .{});
        }
        if (fast_client_opt) |_| {
            std.debug.print("DEBUG: Fast client initialized - model: {s}\n", .{fast_model_ref});
        }
    }

    // For long queries, extract key terms to improve search recall.
    var expanded_query: ?[]const u8 = null;
    defer if (expanded_query) |q| allocator.free(q);

    if (use_filter) {
        if (client_opt) |*client| {
            if (llmExtractKeyTerms(allocator, client, query_text) catch null) |terms| {
                defer {
                    for (terms) |t| allocator.free(t);
                    allocator.free(terms);
                }
                var buf: std.ArrayList(u8) = .{};
                defer buf.deinit(allocator);
                try buf.appendSlice(allocator, query_text);
                for (terms) |t| {
                    try buf.append(allocator, ' ');
                    try buf.appendSlice(allocator, t);
                }
                expanded_query = try buf.toOwnedSlice(allocator);
            }
        }
    }

    const effective_query = expanded_query orelse query_text;

    const stages_raw = try staged_mod.executeStagedWithAliases(allocator, db, effective_query, workspace, aliases_opt);
    defer {
        types.freeStages(allocator, stages_raw);
        allocator.free(stages_raw);
    }

    if (stages_raw.len == 0) {
        const lower_q = try std.ascii.allocLowerString(allocator, effective_query);
        defer allocator.free(lower_q);
        std.debug.print("# Explain: {s}\n\nNot indexed for '{s}'. Search the source directly:\n\n", .{ query_text, effective_query });
        std.debug.print("    grep -ri '{s}' src/ | head -n 20\n\n", .{lower_q});
        std.debug.print("Run 'guidance gen' after finding the file to index it.\n", .{});
        return;
    }

    // ── Fast path: no LLM ─────────────────────────────────────────────────────
    if (!use_llm or client_opt == null) {
        if (use_llm and ea.verbose) std.debug.print("LLM unavailable, using fast path\n", .{});
        return emitStagedOutput(allocator, query_text, stages_raw, null, null, workspace);
    }

    // ── LLM path ─────────────────────────────────────────────────────────────
    const client = &client_opt.?;

    // M6: LLM relevance filter (only when filter mode enables it).
    const stages_filtered: ?[]types.Stage = if (use_filter) llm_filter_mod.filterStages(allocator, client, query_text, stages_raw) catch blk: {
        if (ea.verbose) std.debug.print("llm_filter failed, using unfiltered stages\n", .{});
        break :blk null;
    } else null;
    defer if (stages_filtered) |sf| {
        types.freeStages(allocator, sf);
        allocator.free(sf);
    };

    const working_stages: []const types.Stage = stages_filtered orelse stages_raw;

    // M7: Follow-up expansion — re-search to gather used_by for expansion inputs.
    const expansion_results = db.searchWithAliases(allocator, effective_query, 5, aliases_opt) catch &.{};
    defer {
        for (expansion_results) |r| freeSearchResult(allocator, r);
        allocator.free(expansion_results);
    }

    var fp_list: std.ArrayList([]const u8) = .{};
    defer fp_list.deinit(allocator);
    var src_list: std.ArrayList([]const u8) = .{};
    defer src_list.deinit(allocator);
    var ub_list: std.ArrayList([]const []const u8) = .{};
    defer ub_list.deinit(allocator);
    for (expansion_results) |r| {
        try fp_list.append(allocator, r.file_path);
        try src_list.append(allocator, r.source);
        try ub_list.append(allocator, r.used_by);
    }

    var existing_srcs: std.ArrayList([]const u8) = .{};
    defer existing_srcs.deinit(allocator);
    for (working_stages) |s| {
        if (s.kind == .code or s.kind == .prose) try existing_srcs.append(allocator, s.source);
    }

    const extra_stages: ?[]types.Stage = staged_mod.expandFollowUps(
        allocator,
        fp_list.items,
        src_list.items,
        ub_list.items,
        workspace,
        guidance_dir,
        skills_dir,
        existing_srcs.items,
        6,
    ) catch null;
    defer if (extra_stages) |es| {
        types.freeStages(allocator, es);
        allocator.free(es);
    };

    // Combine working + extra stages (borrows — no new string copies).
    var combined: std.ArrayList(types.Stage) = .{};
    defer combined.deinit(allocator); // only frees the ArrayList spine; strings owned by above slices
    for (working_stages) |s| try combined.append(allocator, s);
    if (extra_stages) |es| for (es) |s| try combined.append(allocator, s);

    // M8: LLM synthesis (use fast model if available, else default).
    const synth_client = if (fast_client_opt) |*fc| fc else &client_opt.?;
    const synth_result = synthesize_mod.synthesize(allocator, synth_client, query_text, combined.items) catch {
        return emitStagedOutput(allocator, query_text, combined.items, null, null, workspace);
    };
    defer {
        if (synth_result.summary) |s| allocator.free(s);
        if (synth_result.followup_keywords) |kw| {
            for (kw) |k| allocator.free(k);
            allocator.free(kw);
        }
    }

    return emitStagedOutput(allocator, query_text, combined.items, synth_result.summary, synth_result.followup_keywords, workspace);
}

/// Write formatted staged output to stdout and flush.
fn emitStagedOutput(
    allocator: std.mem.Allocator,
    query_text: []const u8,
    stages: []const types.Stage,
    summary: ?[]const u8,
    followup_keywords: ?[]const []const u8,
    workspace: []const u8,
) !void {
    const output = try staged_mod.formatStaged(allocator, query_text, stages, summary, workspace, followup_keywords);
    defer allocator.free(output);
    var ws: llm.WriterState = .{};
    ws.initStdout();
    const stdout = ws.writer();
    try stdout.writeAll(output);
    try stdout.flush();
}

// =============================================================================
// Pipeline helpers — test → lint → fmt → guidance → touch
// =============================================================================

/// Derive the guidance JSON path for `src_abs` within a workspace.
///
/// Mirrors the path formula used by SyncProcessor.processFile exactly:
///   `{json_dir}/{rel_path}.json`
/// where `rel_path` is `src_abs` stripped of the leading `{workspace}/`.
///
/// Example:
///   workspace = "/project"
///   json_dir  = "/project/.guidance"
///   src_abs   = "/project/src/foo.zig"
///   → "/project/.guidance/src/foo.zig.json"
///
/// There is NO extra `/src/` injected here — the `src/` already present in
/// `rel_path` (because source lives under `src/`) provides the single prefix.
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

/// Substitute `{file}` tokens in `argv_template` with `file_path`, then run
/// the resulting command as a child process with inherited stdout/stderr.
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
        if (std.mem.eql(u8, tok, "{file}")) {
            try argv.append(allocator, file_path);
        } else {
            try argv.append(allocator, tok);
        }
    }
    var child = std.process.Child.init(argv.items, allocator);
    child.stdin_behavior = .Ignore;
    child.stdout_behavior = .Inherit;
    child.stderr_behavior = .Inherit;
    try child.spawn();
    const term = try child.wait();
    return term == .Exited and term.Exited == 0;
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

    // ── 1. Lint ───────────────────────────────────────────────────────────
    if (!ga.skip_lint) {
        if (cfg.lintCommandForExt(ext)) |lint_argv| {
            if (ga.verbose) std.debug.print("lint:     {s}\n", .{src_abs});
            const ok = try runPhaseCommand(allocator, lint_argv, src_abs);
            if (!ok) {
                std.debug.print("error: lint failed for {s}\n", .{src_abs});
                return false;
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
    _ = processor.processFile(src_abs) catch |err| {
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
                if (ga.verbose) std.debug.print("test:     {s} ({d} file(s) changed)\n", .{ language, stale_files.len });
                const ok = try runCommand(allocator, argv);
                if (!ok) {
                    std.debug.print("error: test suite failed for language '{s}'\n", .{language});
                    return error.TestFailed;
                }
                // Touch the marker to record successful test run
                marker_mod.touchTestMarker(marker_path) catch |err| {
                    std.debug.print("warning: could not create test_passed marker: {s}\n", .{@errorName(err)});
                };
                if (ga.verbose) std.debug.print("test:     passed\n", .{});
            }
        } else {
            if (ga.verbose) std.debug.print("test:     skipped (no test command for {s})\n", .{language});
        }
    }

    // ── Per-file phases ───────────────────────────────────────────────────
    for (stale_files) |src_abs| {
        const ok = try runBuiltinFilePipeline(allocator, cfg, processor, src_abs, ga);
        if (!ok) return error.LintFailed;
    }
}

// =============================================================================
// check — orchestrate the full RALPH loop
// =============================================================================

/// `guidance check` runs the complete RALPH loop:
///   test → lint → fmt → guidance (all languages) → structure → db
///
/// It is the recommended entry point for pre-commit hooks and CI.
/// Incremental detection is always active: only stale files are processed.
fn cmdCheck(allocator: std.mem.Allocator, args: []const []const u8) !void {
    // Forward-compatible flag parsing: honour --skip-* overrides.
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
        }
    }

    // Delegate to the enhanced gen implementation.
    try cmdGenImpl(allocator, ga);

    // Update STRUCTURE.md after guidance is complete.
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
        if (ga.verbose) std.debug.print("check:    STRUCTURE.md updated\n", .{});
    }

    if (ga.verbose) std.debug.print("check:    done\n", .{});
}

// =============================================================================
// Public wrappers for testing
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

pub fn isExactNameMatchPub(name: []const u8, terms: []const []const u8) bool {
    return isExactNameMatch(name, terms);
}

pub fn loadSkillsFromJsonPub(allocator: std.mem.Allocator, json_path: []const u8) ?[]const u8 {
    return loadSkillsFromJson(allocator, json_path);
}

pub fn loadUsedByFromJsonPub(allocator: std.mem.Allocator, json_path: []const u8) ?[][]const u8 {
    return loadUsedByFromJson(allocator, json_path);
}

pub fn loadPublicMemberNamesPub(allocator: std.mem.Allocator, json_path: []const u8) ?[][]const u8 {
    return loadPublicMemberNames(allocator, json_path);
}

pub fn loadSkillParaPub(allocator: std.mem.Allocator, guidance_dir: []const u8, cwd: []const u8, skill_name: []const u8) ?[]const u8 {
    return loadSkillPara(allocator, guidance_dir, cwd, skill_name);
}

pub fn explainExtractExcerptPub(allocator: std.mem.Allocator, src: []const u8, start_line: u32, node_type: []const u8) ![]const u8 {
    return explainExtractExcerpt(allocator, src, start_line, node_type);
}

pub fn explainGrepFilePub(allocator: std.mem.Allocator, file_path: []const u8, terms: []const []const u8, max_results: usize) ![]usize {
    return explainGrepFile(allocator, file_path, terms, max_results);
}

pub fn isShortQueryPub(query: []const u8) bool {
    return isShortQuery(query);
}

// =============================================================================
// show-aliases command
// =============================================================================

fn cmdShowAliases(allocator: std.mem.Allocator, args: []const []const u8) !void {
    _ = args;
    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const cfg = config_mod.loadConfig(allocator, cwd) catch
        try config_mod.loadConfig(allocator, cwd);
    defer @constCast(&cfg).deinit();

    const guidance_dir = try resolveAbsOrJoin(allocator, cwd, cfg.guidance_dir);
    defer allocator.free(guidance_dir);

    const aliases_path = try std.fs.path.join(allocator, &.{ guidance_dir, "semantic-aliases.json" });
    defer allocator.free(aliases_path);

    const aliases_opt = lance_db_mod.loadSemanticAliases(allocator, aliases_path) catch |err| {
        std.debug.print("Error loading aliases from {s}: {s}\n", .{ aliases_path, @errorName(err) });
        std.debug.print("Run 'guidance gen' to generate semantic-aliases.json\n", .{});
        return;
    };
    var aliases = aliases_opt orelse {
        std.debug.print("No semantic aliases found in {s}\n", .{aliases_path});
        std.debug.print("Run 'guidance gen' to generate semantic-aliases.json\n", .{});
        return;
    };
    defer aliases.deinit();

    std.debug.print("# Semantic Aliases ({d} entries)\n\n", .{aliases.aliases.len});

    // Sort aliases alphabetically by key
    var sorted: std.ArrayList(lance_db_mod.SemanticAlias) = .{};
    defer sorted.deinit(allocator);
    for (aliases.aliases) |a| try sorted.append(allocator, a);
    std.sort.block(lance_db_mod.SemanticAlias, sorted.items, {}, struct {
        fn lessThan(_: void, a: lance_db_mod.SemanticAlias, b: lance_db_mod.SemanticAlias) bool {
            return std.mem.order(u8, a.key, b.key) == .lt;
        }
    }.lessThan);

    for (sorted.items) |alias| {
        std.debug.print("- **{s}**:", .{alias.key});
        for (alias.values) |val| {
            std.debug.print(" `{s}`", .{val});
        }
        std.debug.print("\n", .{});
    }
}

// =============================================================================
// test command
// =============================================================================

const TestQuery = struct {
    query: []const u8,
    accuracy: u8 = 0,
    relevance: u8 = 0,
    completeness: u8 = 0,
    observations: []const u8 = "",
};

fn cmdTest(allocator: std.mem.Allocator, args: []const []const u8) !void {
    var no_llm = false;
    var db_path: ?[]const u8 = null;
    var workspace: ?[]const u8 = null;
    var guidance_dir: ?[]const u8 = null;
    var api_url: ?[]const u8 = null;
    var model: ?[]const u8 = null;
    var single_query: ?[]const u8 = null;

    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--no-llm")) {
            no_llm = true;
        } else if (std.mem.eql(u8, arg, "-o") or std.mem.eql(u8, arg, "--db")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --db requires a value\n", .{});
                return;
            }
            db_path = args[i];
        } else if (std.mem.eql(u8, arg, "-w") or std.mem.eql(u8, arg, "--workspace")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --workspace requires a value\n", .{});
                return;
            }
            workspace = args[i];
        } else if (std.mem.eql(u8, arg, "--guidance")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --guidance requires a value\n", .{});
                return;
            }
            guidance_dir = args[i];
        } else if (std.mem.eql(u8, arg, "--api-url")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --api-url requires a value\n", .{});
                return;
            }
            api_url = args[i];
        } else if (std.mem.eql(u8, arg, "-m") or std.mem.eql(u8, arg, "--model")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --model requires a value\n", .{});
                return;
            }
            model = args[i];
        } else if (!std.mem.startsWith(u8, arg, "-")) {
            // First non-flag argument is the query
            single_query = arg;
        }
    }

    const cwd = try std.process.getCwdAlloc(allocator);
    defer allocator.free(cwd);

    const ws = if (workspace) |w|
        try resolveAbsOrJoin(allocator, cwd, w)
    else
        try allocator.dupe(u8, cwd);
    defer allocator.free(ws);

    const cfg = config_mod.loadConfig(allocator, ws) catch
        try config_mod.loadConfig(allocator, cwd);
    defer @constCast(&cfg).deinit();

    const db = db_path orelse cfg.guidance_db_path;
    const db_abs = try resolveAbsOrJoin(allocator, ws, db);
    defer allocator.free(db_abs);

    const gdir = guidance_dir orelse cfg.guidance_dir;
    const gdir_abs = try resolveAbsOrJoin(allocator, ws, gdir);
    defer allocator.free(gdir_abs);

    // Initialize LLM client for evaluation (when not --no-llm)
    var llm_client_opt: ?llm.LlmClient = null;
    var resolved_url_buf: ?[]const u8 = null;
    defer if (resolved_url_buf) |buf| allocator.free(buf);

    if (!no_llm) {
        const model_ref = model orelse cfg.model_default;

        // Use centralized helper for thinking model support
        const resolved = resolveLlmConfigForThinking(
            allocator,
            &cfg,
            model_ref,
            api_url,
        ) catch {
            // Fallback to defaults
            const fallback_config: llm.LlmConfig = .{
                .api_url = api_url orelse config_mod.DEFAULT_API_URL,
                .model = model_ref,
                .think = null,
            };
            llm_client_opt = llm.LlmClient.init(allocator, fallback_config) catch null;
            return;
        };
        resolved_url_buf = resolved.resolved_url;
        const llm_config: llm.LlmConfig = .{
            .api_url = resolved.api_url,
            .model = resolved.model,
            .think = resolved.think,
        };
        llm_client_opt = llm.LlmClient.init(allocator, llm_config) catch null;
    }
    defer if (llm_client_opt) |*c| c.deinit();

    // Load module-level comments to generate hypothetical queries, or use single query
    const queries = if (single_query) |sq| blk: {
        var single: std.ArrayList(TestQuery) = .{};
        try single.append(allocator, .{ .query = try allocator.dupe(u8, sq) });
        break :blk try single.toOwnedSlice(allocator);
    } else try generateTestQueries(allocator, gdir_abs);
    defer {
        for (queries) |q| {
            allocator.free(q.query);
            if (q.observations.len > 0) allocator.free(q.observations);
        }
        allocator.free(queries);
    }

    std.debug.print("# Explain Benchmark Results\n\n", .{});
    std.debug.print("Testing {d} queries (LLM evaluation: {s})\n\n", .{ queries.len, if (llm_client_opt != null) "enabled" else "disabled" });

    var total_acc: u32 = 0;
    var total_rel: u32 = 0;
    var total_cmpl: u32 = 0;
    var excellent_count: usize = 0;
    var good_count: usize = 0;
    var weak_count: usize = 0;

    // Run each query
    for (queries) |tq| {
        std.debug.print("## Query: `{s}`\n\n", .{tq.query});

        // Run explain
        var ea: ExplainArgs = .{ .no_llm = no_llm, .limit = 10 };
        if (db_path) |p| ea.db_path = p;
        if (workspace) |w| ea.workspace = w;
        if (guidance_dir) |g| ea.guidance = g;

        const query_text = try allocator.dupe(u8, tq.query);
        defer allocator.free(query_text);

        // Capture results via internal search
        const results = results_blk: {
            const embedder = vector_mod.createEmbeddingProvider(
                allocator,
                cfg.embedding_provider,
                null,
                cfg.embedding_model,
                cfg.embedding_dims,
            ) catch embedder_blk: {
                var noop = try allocator.create(vector_mod.NoopEmbedding);
                noop.* = .{ .allocator = allocator };
                break :embedder_blk noop.provider();
            };
            defer embedder.deinit();

            var gdb = GuidanceDb.init(allocator, db_abs, embedder) catch |err| {
                std.debug.print("Error opening database: {s}\n", .{@errorName(err)});
                return;
            };
            defer gdb.deinit();

            const aliases_path = try std.fs.path.join(allocator, &.{ gdir_abs, "semantic-aliases.json" });
            defer allocator.free(aliases_path);

            var aliases_opt = lance_db_mod.loadSemanticAliases(allocator, aliases_path) catch null;
            defer if (aliases_opt) |*a| a.deinit();

            const search_aliases = if (aliases_opt) |a| a else null;
            break :results_blk try gdb.searchWithAliases(allocator, query_text, 10, search_aliases);
        };
        defer {
            for (results) |r| freeSearchResult(allocator, r);
            allocator.free(results);
        }

        // Score results using LLM evaluation when available
        var acc: ?u8 = null;
        var rel: ?u8 = null;
        var cmpl: ?u8 = null;
        var obs_buf: [512]u8 = undefined;
        var obs_len: usize = 0;
        var llm_evaluated = false;

        if (llm_client_opt) |*client| {
            // Build results summary for LLM evaluation
            var results_buf: std.ArrayList(u8) = .{};
            defer results_buf.deinit(allocator);
            const rw = results_buf.writer(allocator);
            try rw.print("Query: \"{s}\"\n\n", .{query_text});
            if (results.len > 0) {
                try rw.print("Found {d} results:\n\n", .{results.len});
                for (results[0..@min(5, results.len)]) |r| {
                    try rw.print("- {s} ({s})\n", .{ r.name, r.node_type });
                    if (r.comment) |c| {
                        const ctrimmed = std.mem.trim(u8, c, " \t\n\r");
                        if (ctrimmed.len > 0) {
                            const first_line = std.mem.indexOfScalar(u8, ctrimmed, '\n') orelse ctrimmed.len;
                            try rw.print("  Description: {s}\n", .{ctrimmed[0..@min(first_line, 100)]});
                        }
                    }
                }
            } else {
                try rw.print("No results found.\n", .{});
            }

            const eval_prompt = try std.fmt.allocPrint(allocator,
                \\You are a code search evaluation assistant. Evaluate search results for a codebase query.
                \\
                \\{s}
                \\
                \\Evaluate on these dimensions (0-10):
                \\- Accuracy: Do results match the query intent?
                \\- Relevance: Is the best result ranked first?
                \\- Completeness: Are key relevant items found?
                \\
                \\Be strict but fair. Good results should score 7-10. Poor matches should score 0-4.
                \\For no results, score 0 for all dimensions unless the query is for non-existent code.
                \\
                \\Respond EXACTLY in this format (no other text):
                \\Accuracy: <0-10>
                \\Relevance: <0-10>
                \\Completeness: <0-10>
                \\Observation: <one sentence>
            , .{results_buf.items});
            defer allocator.free(eval_prompt);

            const response_opt = client.complete(eval_prompt, 400, 0.1, null) catch |err| blk: {
                std.debug.print("Warning: LLM complete() failed: {s}\n", .{@errorName(err)});
                break :blk null;
            };
            if (response_opt) |response| {
                defer allocator.free(response);
                const stripped = llm.stripThinkBlock(response);

                // Parse scores from response
                var lines = std.mem.splitScalar(u8, stripped, '\n');
                while (lines.next()) |line| {
                    const trimmed = std.mem.trim(u8, line, "\t\r");

                    // Check for score lines like "### Accuracy: 8/10" or "Accuracy: 8" or "- **Accuracy:** 8/10"
                    if (std.mem.indexOf(u8, trimmed, "Accuracy")) |_| {
                        if (std.mem.indexOf(u8, trimmed, ":")) |colon| {
                            var val_start = colon + 1;
                            // Skip spaces and ** markdown
                            while (val_start < trimmed.len and (trimmed[val_start] == ' ' or trimmed[val_start] == '*' or trimmed[val_start] == '-')) {
                                val_start += 1;
                            }
                            // Find end of number (before / or space)
                            var val_end = val_start;
                            while (val_end < trimmed.len and trimmed[val_end] >= '0' and trimmed[val_end] <= '9') {
                                val_end += 1;
                            }
                            if (val_end > val_start) {
                                const val_str = trimmed[val_start..val_end];
                                if (std.fmt.parseInt(u8, val_str, 10)) |v| {
                                    acc = @min(10, v);
                                } else |_| {}
                            }
                        }
                    } else if (std.mem.indexOf(u8, trimmed, "Relevance")) |_| {
                        if (std.mem.indexOf(u8, trimmed, ":")) |colon| {
                            var val_start = colon + 1;
                            while (val_start < trimmed.len and (trimmed[val_start] == ' ' or trimmed[val_start] == '*' or trimmed[val_start] == '-')) {
                                val_start += 1;
                            }
                            var val_end = val_start;
                            while (val_end < trimmed.len and trimmed[val_end] >= '0' and trimmed[val_end] <= '9') {
                                val_end += 1;
                            }
                            if (val_end > val_start) {
                                const val_str = trimmed[val_start..val_end];
                                if (std.fmt.parseInt(u8, val_str, 10)) |v| {
                                    rel = @min(10, v);
                                } else |_| {}
                            }
                        }
                    } else if (std.mem.indexOf(u8, trimmed, "Completeness")) |_| {
                        if (std.mem.indexOf(u8, trimmed, ":")) |colon| {
                            var val_start = colon + 1;
                            while (val_start < trimmed.len and (trimmed[val_start] == ' ' or trimmed[val_start] == '*' or trimmed[val_start] == '-')) {
                                val_start += 1;
                            }
                            var val_end = val_start;
                            while (val_end < trimmed.len and trimmed[val_end] >= '0' and trimmed[val_end] <= '9') {
                                val_end += 1;
                            }
                            if (val_end > val_start) {
                                const val_str = trimmed[val_start..val_end];
                                if (std.fmt.parseInt(u8, val_str, 10)) |v| {
                                    cmpl = @min(10, v);
                                } else |_| {}
                            }
                        }
                    } else if (std.mem.indexOf(u8, trimmed, "Observation")) |_| {
                        if (std.mem.indexOf(u8, trimmed, ":")) |colon| {
                            var obs_start = colon + 1;
                            while (obs_start < trimmed.len and (trimmed[obs_start] == ' ' or trimmed[obs_start] == '*')) {
                                obs_start += 1;
                            }
                            const obs_text = trimmed[obs_start..];
                            obs_len = @min(obs_text.len, obs_buf.len - 1);
                            @memcpy(obs_buf[0..obs_len], obs_text[0..obs_len]);
                            obs_buf[obs_len] = 0;
                        }
                    }
                }
                llm_evaluated = (acc != null and rel != null and cmpl != null);
            }
        }

        // If LLM evaluation failed, use "-" for scores
        const eval_status = if (llm_evaluated) "LLM" else "FALLBACK";

        // Get actual values for display (use "-" if not evaluated)
        const acc_val = acc orelse 0;
        const rel_val = rel orelse 0;
        const cmpl_val = cmpl orelse 0;
        const acc_display = if (acc) |v| try std.fmt.allocPrint(allocator, "{d}", .{v}) else try std.fmt.allocPrint(allocator, "-", .{});
        defer allocator.free(acc_display);
        const rel_display = if (rel) |v| try std.fmt.allocPrint(allocator, "{d}", .{v}) else try std.fmt.allocPrint(allocator, "-", .{});
        defer allocator.free(rel_display);
        const cmpl_display = if (cmpl) |v| try std.fmt.allocPrint(allocator, "{d}", .{v}) else try std.fmt.allocPrint(allocator, "-", .{});
        defer allocator.free(cmpl_display);

        // Only count toward statistics if actually evaluated
        if (llm_evaluated) {
            total_acc += acc_val;
            total_rel += rel_val;
            total_cmpl += cmpl_val;

            if (acc_val >= 9) {
                excellent_count += 1;
            } else if (acc_val >= 7) {
                good_count += 1;
            } else {
                weak_count += 1;
            }
        }

        std.debug.print("| Metric | Score |\n", .{});
        std.debug.print("|--------|-------|\n", .{});
        std.debug.print("| Accuracy | {s}/10 |\n", .{acc_display});
        std.debug.print("| Relevance | {s}/10 |\n", .{rel_display});
        std.debug.print("| Completeness | {s}/10 |\n", .{cmpl_display});
        std.debug.print("| Results | {d} |\n", .{results.len});
        std.debug.print("| Evaluation | {s} |\n\n", .{eval_status});

        // Show top 3 results
        std.debug.print("**Top Results:**\n", .{});
        for (results[0..@min(3, results.len)]) |r| {
            std.debug.print("- `{s}` ({s}:{s})\n", .{ r.name, r.module, r.node_type });
        }
        if (obs_len > 0) {
            std.debug.print("\n**Observation:** {s}\n", .{obs_buf[0..obs_len]});
        }
        std.debug.print("\n---\n\n", .{});
    }

    // Summary
    const n = queries.len;
    const evaluated_count = excellent_count + good_count + weak_count;
    if (n > 0) {
        std.debug.print("# Summary Statistics\n\n", .{});
        std.debug.print("| Metric | Value |\n", .{});
        std.debug.print("|--------|-------|\n", .{});
        std.debug.print("| **Total Queries** | {d} |\n", .{n});
        std.debug.print("| **LLM Evaluated** | {d} |\n", .{evaluated_count});
        if (evaluated_count > 0) {
            std.debug.print("| **Average Accuracy** | {d:.1}/10 |\n", .{@as(f32, @floatFromInt(total_acc)) / @as(f32, @floatFromInt(evaluated_count))});
            std.debug.print("| **Average Relevance** | {d:.1}/10 |\n", .{@as(f32, @floatFromInt(total_rel)) / @as(f32, @floatFromInt(evaluated_count))});
            std.debug.print("| **Average Completeness** | {d:.1}/10 |\n", .{@as(f32, @floatFromInt(total_cmpl)) / @as(f32, @floatFromInt(evaluated_count))});
        } else {
            std.debug.print("| **Average Accuracy** | -/10 |\n", .{});
            std.debug.print("| **Average Relevance** | -/10 |\n", .{});
            std.debug.print("| **Average Completeness** | -/10 |\n", .{});
        }
        std.debug.print("| **Excellent (9-10)** | {d}/{d} ({d}%) |\n", .{ excellent_count, evaluated_count, if (evaluated_count > 0) @as(usize, @intFromFloat(@as(f32, @floatFromInt(excellent_count)) / @as(f32, @floatFromInt(evaluated_count)) * 100)) else 0 });
        std.debug.print("| **Good (7-8)** | {d}/{d} |\n", .{ good_count, evaluated_count });
        std.debug.print("| **Weak (<7)** | {d}/{d} |\n", .{ weak_count, evaluated_count });
    }
}

/// Generate test queries from module-level comments in guidance JSON files.
fn generateTestQueries(allocator: std.mem.Allocator, guidance_dir: []const u8) ![]TestQuery {
    var queries: std.ArrayList(TestQuery) = .{};
    errdefer {
        for (queries.items) |q| {
            allocator.free(q.query);
            if (q.observations.len > 0) allocator.free(q.observations);
        }
        queries.deinit(allocator);
    }

    // Scan .guidance/src/**/*.json for module-level comments
    const src_dir = try std.fs.path.join(allocator, &.{ guidance_dir, "src" });
    defer allocator.free(src_dir);

    var dir = std.fs.cwd().openDir(src_dir, .{ .iterate = true }) catch |err| {
        std.debug.print("Warning: cannot open guidance src dir: {s}\n", .{@errorName(err)});
        return queries.toOwnedSlice(allocator);
    };
    defer dir.close();

    var walker = try dir.walk(allocator);
    defer walker.deinit();

    while (try walker.next()) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.path, ".json")) continue;

        const json_path = try std.fs.path.join(allocator, &.{ src_dir, entry.path });
        defer allocator.free(json_path);

        const file = std.fs.cwd().openFile(json_path, .{}) catch continue;
        defer file.close();

        const content = file.readToEndAlloc(allocator, 1024 * 1024) catch continue;
        defer allocator.free(content);

        var parsed = std.json.parseFromSlice(std.json.Value, allocator, content, .{ .ignore_unknown_fields = true }) catch continue;
        defer parsed.deinit();

        if (parsed.value != .object) continue;
        const root = parsed.value.object;

        // Get module-level comment
        const comment_val = root.get("comment") orelse continue;
        if (comment_val != .string) continue;
        const comment = comment_val.string;
        if (comment.len < 20) continue;

        // Get module name
        const meta_val = root.get("meta") orelse continue;
        if (meta_val != .object) continue;
        const module_val = meta_val.object.get("module") orelse continue;
        if (module_val != .string) continue;
        const module = module_val.string;

        // Extract module basename (last component)
        const module_basename = std.mem.lastIndexOfScalar(u8, module, '.');
        const basename = if (module_basename) |idx| module[idx + 1 ..] else module;

        // Generate simple query from module name
        const query1 = try std.fmt.allocPrint(allocator, "{s}", .{basename});
        try queries.append(allocator, .{ .query = query1 });

        // Generate question-style query
        const query2 = try std.fmt.allocPrint(allocator, "How does {s} work?", .{basename});
        try queries.append(allocator, .{ .query = query2 });

        // Limit to 20 queries
        if (queries.items.len >= 20) break;
    }

    return queries.toOwnedSlice(allocator);
}
