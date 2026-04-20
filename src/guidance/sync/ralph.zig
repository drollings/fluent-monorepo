//! sync/ralph.zig — RALPH loop orchestration (check phase helpers).
//!
//! Extracted from sync_engine.zig (M2.1) to keep file sizes navigable.
//! Public API: cmdCheck, runPhaseCommand, collectFilesWithExts.
//!
//! ## Memory Ownership
//!
//!   - cmdCheck(): Accepts gen_fn and caps_sync_fn function pointers for decoupled
//!     dispatch; no circular imports. All allocations are arena-scoped per phase.
//!   - runPhaseCommand(): Runs an external command; output is allocator-owned.
//!   - collectFilesWithExts(): Returns allocator-owned ArrayList of file paths;
//!     caller owns and must deinit.

const std = @import("std");
const config_mod = @import("../config.zig");
const common = @import("common");
const structure_mod = @import("../structure.zig");
const types = @import("../types.zig");
const gen_files_mod = @import("gen_files.zig");
const stepPrint = types.stepPrint;

pub fn cmdCheck(
    allocator: std.mem.Allocator,
    args: []const []const u8,
    gen_fn: *const fn (std.mem.Allocator, gen_files_mod.GenArgs, ?gen_files_mod.CapabilitiesSyncFn) anyerror!void,
    caps_sync_fn: gen_files_mod.CapabilitiesSyncFn,
) !void {
    var ga: gen_files_mod.GenArgs = .{ .all_languages = true, .compile_db = true };
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
                ga.timeout_seconds = std.fmt.parseInt(u64, args[i], 2) catch ga.timeout_seconds;
            }
        } else if (std.mem.eql(u8, arg, "--dry-run")) {
            ga.dry_run = true;
            ga.skip_tests = true;
            run_structure = false;
        }
    }

    try gen_fn(allocator, ga, caps_sync_fn);

    if (run_structure and !ga.dry_run) {
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

pub fn runPhaseCommand(
    allocator: std.mem.Allocator,
    argv_template: []const []const u8,
    file_path: []const u8,
) !bool {
    var argv: std.ArrayList([]const u8) = .{};
    defer argv.deinit(allocator);
    for (argv_template) |tok| {
        try argv.append(allocator, if (std.mem.eql(u8, tok, "{file}")) file_path else tok);
    }
    return common.shell.runCommand(allocator, argv.items);
}

pub fn collectFilesWithExts(
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
        if (std.mem.endsWith(u8, entry.basename, "_tests.zig")) continue;
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
