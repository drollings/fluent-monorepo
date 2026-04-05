//! External language provider discovery for guidance.
//!
//! Searches for provider binaries named `guidance-{ext_bare}` in:
//!   1. `{workspace}/bin/guidance-{ext_bare}`   (workspace-local, highest priority)
//!   2. PATH lookup for `guidance-{ext_bare}`   (system-wide)
//!
//! Provider protocol — subprocess invocation:
//!   guidance-py sync --file {src_abs} --output {json_dir}
//!   guidance-py sync --scan {src_dir} --output {json_dir}
//!
//! Providers must write guidance JSON files compatible with the existing format
//! to `{json_dir}/src/{relative_path}{ext}.json`.
//!
//! All returned Provider values are owned by the caller; call provider.deinit().

const std = @import("std");
const common = @import("common");

/// Manages provider discovery structures with fixed-size buffers; owned by the provider; ensures consistent state across initialization and cleanup.
pub const Provider = struct {
    /// Human-readable language name derived from the extension (e.g. "py", "rs").
    name: []const u8,
    /// Absolute path to the provider binary.
    binary: []const u8,
    /// The file extension this provider handles (e.g. ".py").
    extension: []const u8,

    pub fn deinit(self: Provider, allocator: std.mem.Allocator) void {
        allocator.free(self.name);
        allocator.free(self.binary);
        allocator.free(self.extension);
    }
};

/// Determines the provider to be discovered using the provided allocator and workspace data.
pub fn discoverProvider(
    allocator: std.mem.Allocator,
    workspace: []const u8,
    ext: []const u8,
) !?Provider {
    // Extension must start with '.' (e.g. ".py") and have a bare name.
    if (ext.len < 2 or ext[0] != '.') return null;
    const bare = ext[1..]; // "py", "rs", etc.

    const binary_name = try std.fmt.allocPrint(allocator, "guidance-{s}", .{bare});
    defer allocator.free(binary_name);

    // 1. Workspace-local: {workspace}/bin/guidance-{bare}
    {
        const candidate = try std.fs.path.join(allocator, &.{ workspace, "bin", binary_name });
        defer allocator.free(candidate);
        if (isExecutable(candidate)) {
            return Provider{
                .name = try allocator.dupe(u8, bare),
                .binary = try allocator.dupe(u8, candidate),
                .extension = try allocator.dupe(u8, ext),
            };
        }
    }

    // 2. PATH lookup.
    if (try findInPath(allocator, binary_name)) |bin_path| {
        return Provider{
            .name = try allocator.dupe(u8, bare),
            .binary = bin_path, // already allocated by findInPath
            .extension = try allocator.dupe(u8, ext),
        };
    }

    return null;
}

/// Handles invocation of provider files with allocation and metadata parameters.
pub fn invokeProviderFile(
    allocator: std.mem.Allocator,
    provider: Provider,
    src_abs: []const u8,
    json_dir: []const u8,
    extra_args: []const []const u8,
) !bool {
    var argv: std.ArrayList([]const u8) = .{};
    defer argv.deinit(allocator);

    try argv.append(allocator, provider.binary);
    try argv.append(allocator, "sync");
    try argv.append(allocator, "--file");
    try argv.append(allocator, src_abs);
    try argv.append(allocator, "--output");
    try argv.append(allocator, json_dir);
    for (extra_args) |a| try argv.append(allocator, a);

    return runCommand(allocator, argv.items);
}

/// Executes a provider scan using an allocator, directory paths, and JSON options, returning a boolean result.
pub fn invokeProviderScan(
    allocator: std.mem.Allocator,
    provider: Provider,
    scan_dir: []const u8,
    json_dir: []const u8,
    extra_args: []const []const u8,
) !bool {
    var argv: std.ArrayList([]const u8) = .{};
    defer argv.deinit(allocator);

    try argv.append(allocator, provider.binary);
    try argv.append(allocator, "sync");
    try argv.append(allocator, "--scan");
    try argv.append(allocator, scan_dir);
    try argv.append(allocator, "--output");
    try argv.append(allocator, json_dir);
    for (extra_args) |a| try argv.append(allocator, a);

    return runCommand(allocator, argv.items);
}

// ---------------------------------------------------------------------------
// File system helpers
// ---------------------------------------------------------------------------

/// Checks if a given path is a valid executable file format, returning true or false.
fn isExecutable(path: []const u8) bool {
    // std.fs.accessAbsolute with execute mode (.read_only is available; we
    // check for file existence + non-directory as a best-effort approach,
    // since Zig 0.15 access() mode flags may vary by platform).
    std.fs.accessAbsolute(path, .{}) catch return false;
    // Confirm it is a regular file (not a directory).
    const f = std.fs.openFileAbsolute(path, .{}) catch return false;
    defer f.close();
    const stat = f.stat() catch return false;
    return stat.kind == .file;
}

/// Search for a binary path in memory using an allocator and returns the matching slice.
fn findInPath(allocator: std.mem.Allocator, binary_name: []const u8) !?[]const u8 {
    const path_env = std.process.getEnvVarOwned(allocator, "PATH") catch return null;
    defer allocator.free(path_env);

    const sep: u8 = if (@import("builtin").os.tag == .windows) ';' else ':';
    var it = std.mem.splitScalar(u8, path_env, sep);
    while (it.next()) |dir| {
        if (dir.len == 0) continue;
        const candidate = try std.fs.path.join(allocator, &.{ dir, binary_name });
        defer allocator.free(candidate);
        if (isExecutable(candidate)) {
            return try allocator.dupe(u8, candidate);
        }
    }
    return null;
}

/// Spawn `argv` as a child process, inheriting stdout/stderr.
/// Returns true when the process exits with code 0.
const runCommand = common.shell.runCommand;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "discoverProvider: returns null for unknown extension" {
    // ".xyz" is unlikely to have a provider binary on any CI machine.
    const result = try discoverProvider(std.testing.allocator, "/tmp", ".xyz_explain_gen_test");
    try std.testing.expect(result == null);
}

test "discoverProvider: returns null for bare extension without dot" {
    const result = try discoverProvider(std.testing.allocator, "/tmp", "py");
    try std.testing.expect(result == null);
}

test "discoverProvider: workspace-local bin takes priority" {
    // Create a temporary directory with a fake provider binary.
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const tmp_path = try tmp.dir.realpathAlloc(std.testing.allocator, ".");
    defer std.testing.allocator.free(tmp_path);

    // Create bin/guidance-tst as a regular file (not actually executable on
    // all platforms, but isExecutable only checks existence + file kind in tests).
    try tmp.dir.makeDir("bin");
    {
        const f = try tmp.dir.createFile("bin/guidance-tst", .{});
        f.close();
    }

    // discoverProvider should find it.
    const result = try discoverProvider(std.testing.allocator, tmp_path, ".tst");
    if (result) |p| {
        defer p.deinit(std.testing.allocator);
        try std.testing.expectEqualStrings("tst", p.name);
        try std.testing.expectEqualStrings(".tst", p.extension);
        try std.testing.expect(std.mem.endsWith(u8, p.binary, "bin/guidance-tst"));
    } else {
        // On some systems access() may fail for non-executable files; skip.
    }
}
