//! query/args.zig — Argument parsing for explain and related query commands.
//!
//! Extracted from query_engine.zig (M2.2) to keep argument types separate from
//! command dispatch logic.
//!
//! ## Memory Ownership
//!
//!   - ExplainArgs: Holds borrowed string slices from CLI argv; no deinit needed.
//!   - QueryContext: Owns workspace, guidance_dir, db_path strings, and the ProjectConfig;
//!     call deinit() to release all owned memory.
//!   - FilterMode: Value type enum; no allocation.
//!   - parseExplainArgs(): Returns ExplainArgs by value; no allocation.

const std = @import("std");
const config_mod = @import("../config.zig");

/// LLM filter mode for query results (auto, force, skip).
pub const FilterMode = enum {
    /// Auto-detect: apply LLM filter only for long queries (5+ words).
    auto,
    /// Always apply LLM filter (even for short queries).
    force,
    /// Never apply LLM filter (always fast path).
    skip,
};

/// Command-line arguments for the explain command.
pub const ExplainArgs = struct {
    query_str: ?[]const u8 = null,
    limit: usize = 10,
    /// Path to .guidance.db. Defaults to config db_path or DEFAULT_GUIDANCE_DB_PATH.
    db_path: ?[]const u8 = null,
    workspace: ?[]const u8 = null,
    guidance: ?[]const u8 = null,
    api_url: []const u8 = config_mod.DEFAULT_API_URL,
    model: []const u8 = config_mod.DEFAULT_MODEL,
    /// Skip LLM synthesis; emit structural output only.
    no_llm: bool = false,
    verbose: bool = false,
    debug: bool = false,
    /// Use new staged pipeline (default: true).  --staged=false → legacy path.
    staged: bool = true,
    /// LLM relevance filtering mode.
    filter: FilterMode = .auto,
    /// Disable deterministic DRIFT follow-up generation.
    no_drift: bool = false,
    /// Absolute path to the capabilities tree; sourced from cfg.capabilities_dir.
    capabilities_dir: []const u8 = "",
};

/// Resolved context for a query: workspace paths and loaded config.
pub const QueryContext = struct {
    workspace: []const u8,
    guidance_dir: []const u8,
    db_path: []const u8,
    cfg: config_mod.ProjectConfig,

    pub fn deinit(self: *QueryContext, allocator: std.mem.Allocator) void {
        allocator.free(self.workspace);
        allocator.free(self.guidance_dir);
        allocator.free(self.db_path);
        self.cfg.deinit();
    }
};

/// Parses explain command-line args. Returns error.MissingArg (after printing) on malformed input.
pub fn parseExplainArgs(args: []const []const u8) error{MissingArg}!ExplainArgs {
    var ea: ExplainArgs = .{};
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "-l") or std.mem.eql(u8, arg, "--limit")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --limit requires a value\n", .{});
                return error.MissingArg;
            }
            ea.limit = std.fmt.parseInt(usize, args[i], 10) catch 10;
        } else if (std.mem.eql(u8, arg, "-o") or std.mem.eql(u8, arg, "--db")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --db requires a value\n", .{});
                return error.MissingArg;
            }
            ea.db_path = args[i];
        } else if (std.mem.eql(u8, arg, "-w") or std.mem.eql(u8, arg, "--workspace")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --workspace requires a value\n", .{});
                return error.MissingArg;
            }
            ea.workspace = args[i];
        } else if (std.mem.eql(u8, arg, "--api-url")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --api-url requires a value\n", .{});
                return error.MissingArg;
            }
            ea.api_url = args[i];
        } else if (std.mem.eql(u8, arg, "-m") or std.mem.eql(u8, arg, "--model")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --model requires a value\n", .{});
                return error.MissingArg;
            }
            ea.model = args[i];
        } else if (std.mem.eql(u8, arg, "--debug")) {
            ea.debug = true;
        } else if (std.mem.eql(u8, arg, "--no-llm")) {
            ea.no_llm = true;
        } else if (std.mem.eql(u8, arg, "--no-drift")) {
            ea.no_drift = true;
        } else if (std.mem.eql(u8, arg, "--guidance")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --guidance requires a value\n", .{});
                return error.MissingArg;
            }
            ea.guidance = args[i];
        } else if (std.mem.startsWith(u8, arg, "--staged=")) {
            ea.staged = !std.mem.eql(u8, arg["--staged=".len..], "false");
        } else if (std.mem.eql(u8, arg, "--staged")) {
            ea.staged = true;
        } else if (std.mem.startsWith(u8, arg, "--filter=")) {
            ea.filter = std.meta.stringToEnum(FilterMode, arg["--filter=".len..]) orelse .auto;
        } else if (std.mem.eql(u8, arg, "--guidance-db")) {
            i += 1;
            if (i >= args.len) {
                std.debug.print("Error: --guidance-db requires a value\n", .{});
                return error.MissingArg;
            }
            ea.db_path = args[i];
        } else if (std.mem.eql(u8, arg, "--db-type=lance") or
            std.mem.eql(u8, arg, "--lance") or
            std.mem.startsWith(u8, arg, "--db-type="))
        {
            // Accepted but ignored — SQLite is always used.
        } else if (!std.mem.startsWith(u8, arg, "-")) {
            ea.query_str = arg;
        }
    }
    return ea;
}
