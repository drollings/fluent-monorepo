const std = @import("std");

/// Flags and values parsed from the command line.
/// All string slices point into the original `args` slice — no allocation.
pub const CommonArgs = struct {
    // Intelligence / LLM
    debug: bool = false,
    no_ai: bool = false,
    api_url: []const u8 = "http://localhost:11434/v1/chat/completions",
    model: []const u8 = "local:code:latest",
    /// True when --api-url was explicitly provided on the command line.
    api_url_set: bool = false,
    /// True when --model / -m was explicitly provided on the command line.
    model_set: bool = false,

    // Build-tool flags
    dry_run: bool = false,
    force: bool = false,
    verbose: bool = false,
    show_list: bool = false,
    show_graph: bool = false,
    enter_repl: bool = false,
    show_help: bool = false,
    show_version: bool = false,

    // Optional path to a JSON config file
    config_file: ?[]const u8 = null,

    // Optional LLM prompt
    llm_query: ?[]const u8 = null,

    // Positional arguments (target names, etc.)
    positional: []const []const u8 = &[_][]const u8{},
};

/// Parse a flat args slice (excluding argv[0]) into a `CommonArgs`.
/// Positional arguments are returned as a sub-slice of `args`; the caller
/// must keep `args` alive for the lifetime of the returned struct.
///
/// Unknown flags are skipped (permissive mode for command-specific flags).
/// A flag that requires a value but is the last arg produces `error.MissingValue`.
pub fn parseCommonArgs(
    args: []const []const u8,
    positional_buf: *std.ArrayListUnmanaged([]const u8),
    allocator: std.mem.Allocator,
) !CommonArgs {
    var result: CommonArgs = .{};
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];

        if (std.mem.eql(u8, arg, "-h") or std.mem.eql(u8, arg, "--help")) {
            result.show_help = true;
        } else if (std.mem.eql(u8, arg, "-v") or std.mem.eql(u8, arg, "--version")) {
            result.show_version = true;
        } else if (std.mem.eql(u8, arg, "--debug")) {
            result.debug = true;
        } else if (std.mem.eql(u8, arg, "--no-ai")) {
            result.no_ai = true;
        } else if (std.mem.eql(u8, arg, "-n") or std.mem.eql(u8, arg, "--dry-run")) {
            result.dry_run = true;
        } else if (std.mem.eql(u8, arg, "--force")) {
            result.force = true;
        } else if (std.mem.eql(u8, arg, "--verbose")) {
            result.verbose = true;
            result.debug = true; // verbose implies debug output
        } else if (std.mem.eql(u8, arg, "-l") or std.mem.eql(u8, arg, "--list")) {
            result.show_list = true;
        } else if (std.mem.eql(u8, arg, "--graph")) {
            result.show_graph = true;
        } else if (std.mem.eql(u8, arg, "--repl")) {
            result.enter_repl = true;
        } else if (std.mem.eql(u8, arg, "-f") or std.mem.eql(u8, arg, "--file")) {
            i += 1;
            if (i >= args.len) return error.MissingValue;
            result.config_file = args[i];
        } else if (std.mem.eql(u8, arg, "--llm-query")) {
            i += 1;
            if (i >= args.len) return error.MissingValue;
            result.llm_query = args[i];
        } else if (std.mem.eql(u8, arg, "--api-url")) {
            i += 1;
            if (i >= args.len) return error.MissingValue;
            result.api_url = args[i];
            result.api_url_set = true;
        } else if (std.mem.eql(u8, arg, "-m") or std.mem.eql(u8, arg, "--model")) {
            i += 1;
            if (i >= args.len) return error.MissingValue;
            result.model = args[i];
            result.model_set = true;
        } else if (std.mem.startsWith(u8, arg, "-")) {
            // Unknown flag - skip (permissive mode for command-specific flags)
        } else {
            try positional_buf.append(allocator, arg);
        }
    }
    result.positional = positional_buf.items;
    return result;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "parseCommonArgs: basic flags" {
    var pos: std.ArrayListUnmanaged([]const u8) = .{};
    defer pos.deinit(testing.allocator);

    const argv = [_][]const u8{ "--verbose", "--dry-run", "--force" };
    const args = try parseCommonArgs(&argv, &pos, testing.allocator);

    try testing.expect(args.verbose);
    try testing.expect(args.dry_run);
    try testing.expect(args.force);
    try testing.expect(!args.show_help);
}

test "parseCommonArgs: model and api-url with set flags" {
    var pos: std.ArrayListUnmanaged([]const u8) = .{};
    defer pos.deinit(testing.allocator);

    const argv = [_][]const u8{ "-m", "llama3", "--api-url", "http://host:11434/v1/chat/completions" };
    const args = try parseCommonArgs(&argv, &pos, testing.allocator);

    try testing.expectEqualStrings("llama3", args.model);
    try testing.expect(args.model_set);
    try testing.expectEqualStrings("http://host:11434/v1/chat/completions", args.api_url);
    try testing.expect(args.api_url_set);
}

test "parseCommonArgs: positional arguments collected" {
    var pos: std.ArrayListUnmanaged([]const u8) = .{};
    defer pos.deinit(testing.allocator);

    const argv = [_][]const u8{ "build", "clean", "--force" };
    const args = try parseCommonArgs(&argv, &pos, testing.allocator);

    try testing.expectEqual(@as(usize, 2), args.positional.len);
    try testing.expectEqualStrings("build", args.positional[0]);
    try testing.expectEqualStrings("clean", args.positional[1]);
    try testing.expect(args.force);
}

test "parseCommonArgs: --file and --llm-query" {
    var pos: std.ArrayListUnmanaged([]const u8) = .{};
    defer pos.deinit(testing.allocator);

    const argv = [_][]const u8{ "-f", "targets.json", "--llm-query", "how do I add a target?" };
    const args = try parseCommonArgs(&argv, &pos, testing.allocator);

    try testing.expectEqualStrings("targets.json", args.config_file.?);
    try testing.expectEqualStrings("how do I add a target?", args.llm_query.?);
}

test "parseCommonArgs: unknown flags are skipped (permissive mode)" {
    var pos: std.ArrayListUnmanaged([]const u8) = .{};
    defer pos.deinit(testing.allocator);

    const argv = [_][]const u8{ "--unknown-flag", "--verbose", "--guidance", "dir" };
    const args = try parseCommonArgs(&argv, &pos, testing.allocator);

    try testing.expect(args.verbose);
    try testing.expectEqualStrings("dir", args.positional[0]);
}

test "parseCommonArgs: missing value for -m returns error" {
    var pos: std.ArrayListUnmanaged([]const u8) = .{};
    defer pos.deinit(testing.allocator);

    const argv = [_][]const u8{"-m"};
    const result = parseCommonArgs(&argv, &pos, testing.allocator);
    try testing.expectError(error.MissingValue, result);
}

test "parseCommonArgs: defaults when no args given" {
    var pos: std.ArrayListUnmanaged([]const u8) = .{};
    defer pos.deinit(testing.allocator);

    const argv = [_][]const u8{};
    const args = try parseCommonArgs(&argv, &pos, testing.allocator);

    try testing.expect(!args.debug);
    try testing.expect(!args.dry_run);
    try testing.expect(!args.show_help);
    try testing.expect(!args.api_url_set);
    try testing.expectEqualStrings("local:code:latest", args.model);
    try testing.expectEqual(@as(usize, 0), args.positional.len);
}
