const std = @import("std");
const Target = @import("target.zig").Target;
const TargetRegistry = @import("registry.zig").TargetRegistry;
const DependencyResolver = @import("resolver.zig").DependencyResolver;
const common = @import("common");
const StringInterner = common.interner.StringInterner;
const shell_parser = common.shell_parser;

pub const BuildError = error{
    ExecutionFailed,
    TargetNotFound,
    OutOfMemory,
    CircularDependency,
};

pub const BuildResult = struct {
    success: bool,
    targets_built: usize,
    targets_failed: usize,
    failed_names: std.ArrayList([]const u8),
    duration_ns: u64,

    pub fn deinit(self: *BuildResult, allocator: std.mem.Allocator) void {
        for (self.failed_names.items) |name| {
            allocator.free(name);
        }
        self.failed_names.deinit(allocator);
    }
};

pub const BuildContext = @This();

allocator: std.mem.Allocator,
registry: *TargetRegistry,
interner: *StringInterner,
resolver: DependencyResolver,
dry_run: bool = false,
force: bool = false,
verbose: bool = false,

/// Initializes a BuildContext using an allocator, registry, and string interners.
pub fn init(
    allocator: std.mem.Allocator,
    registry: *TargetRegistry,
    interner: *StringInterner,
) BuildContext {
    return .{
        .allocator = allocator,
        .registry = registry,
        .interner = interner,
        .resolver = DependencyResolver.init(allocator, registry, interner),
    };
}

/// Converts a Zig source code string into a BuildResult indicating success or error.
pub fn build(self: *BuildContext, target_names: []const []const u8) !BuildResult {
    const io = std.Io.Threaded.global_single_threaded.io();
    const start: i96 = std.Io.Timestamp.now(io, .real).nanoseconds;

    var result = BuildResult{
        .success = true,
        .targets_built = 0,
        .targets_failed = 0,
        .failed_names = .empty,
        .duration_ns = 0,
    };
    errdefer result.deinit(self.allocator);

    if (target_names.len == 0) {
        if (self.registry.get("default")) |default| {
            var names: std.ArrayList([]const u8) = .empty;
            defer names.deinit(self.allocator);

            var iter = default.depends.iterator(.{});
            while (iter.next()) |dep_idx| {
                if (self.interner.getString(dep_idx)) |name| {
                    try names.append(self.allocator, name);
                }
            }

            if (names.items.len > 0) {
                return self.build(names.items);
            }
        }

        std.log.info("No targets specified and no default target found", .{});
        result.duration_ns = @intCast(std.Io.Timestamp.now(io, .real).nanoseconds - start);
        return result;
    }

    var provided = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(self.allocator, self.interner.count());
    defer provided.deinit(self.allocator);

    var resolved = self.resolver.resolveAbstractDependencies(target_names, &provided) catch |err| {
        if (err == error.CircularDependency) {
            std.log.debug("Circular dependency detected", .{});
        }
        result.success = false;
        result.duration_ns = @intCast(std.Io.Timestamp.now(io, .real).nanoseconds - start);
        return result;
    };
    defer resolved.deinit();

    if (self.verbose) {
        std.log.info("Resolved {d} targets:", .{resolved.targets.len});
        for (resolved.targets) |target| {
            std.log.info("  {s}", .{target.name});
        }
    }

    for (resolved.targets) |target| {
        if (target.isAbstract()) {
            if (self.verbose) {
                std.log.info("Skipping abstract target: {s}", .{target.name});
            }
            continue;
        }

        if (!self.force and self.isUpToDate(target)) {
            if (self.verbose) {
                std.log.info("Up to date: {s}", .{target.name});
            }
            continue;
        }

        if (self.dry_run) {
            std.log.info("[DRY-RUN] {s}", .{target.name});
            result.targets_built += 1;
            continue;
        }

        const success = self.executeTarget(target) catch false;
        if (success) {
            result.targets_built += 1;
            if (self.verbose) {
                std.log.info("[OK] {s}", .{target.name});
            }
        } else {
            result.targets_failed += 1;
            try result.failed_names.append(self.allocator, try self.allocator.dupe(u8, target.name));

            if (target.essential) {
                std.log.err("[FAIL] Essential target failed: {s}", .{target.name});
                result.success = false;
                break;
            } else {
                std.log.warn("[FAIL] Target failed: {s}", .{target.name});
            }
        }
    }

    result.duration_ns = @intCast(std.Io.Timestamp.now(io, .real).nanoseconds - start);
    return result;
}

/// Checks if a BuildContext's target is up-to-date with Zig's version constraints.
fn isUpToDate(self: *BuildContext, target: *const Target) bool {
    const io = std.Io.Threaded.global_single_threaded.io();
    const exists_path = target.exists orelse return false;

    std.Io.Dir.cwd().access(io, exists_path, .{}) catch return false;

    if (!target.check_mtime) return true;

    const output_stat = std.Io.Dir.cwd().statFile(io, exists_path, .{}) catch return false;
    const output_mtime = output_stat.mtime.nanoseconds;

    var iter = target.depends.iterator(.{});
    while (iter.next()) |dep_idx| {
        if (self.registry.getByBitIndex(dep_idx)) |dep_target| {
            if (dep_target.exists) |dep_path| {
                const dep_stat = std.Io.Dir.cwd().statFile(io, dep_path, .{}) catch continue;
                if (dep_stat.mtime.nanoseconds > output_mtime) {
                    return false;
                }
            }
        }
    }

    return true;
}

/// Checks if a target is reachable and returns a boolean indicating success.
fn executeTarget(self: *BuildContext, target: *const Target) !bool {
    if (target.commands.items.len == 0) {
        return true;
    }

    std.log.info("[EXEC] {s}", .{target.name});

    for (target.commands.items) |cmd| {
        if (self.verbose) {
            std.log.info("  > {s}", .{cmd});
        }

        const argv = shell_parser.parseCommand(self.allocator, cmd) catch |err| {
            std.log.err("Failed to parse command '{s}': {s}", .{ cmd, @errorName(err) });
            return false;
        };
        defer {
            for (argv) |arg| self.allocator.free(arg);
            self.allocator.free(argv);
        }

        const io = std.Io.Threaded.global_single_threaded.io();
        var child = std.process.spawn(io, .{ .argv = @as([]const []const u8, argv) }) catch |err| {
            std.log.err("Failed to spawn command: {}", .{err});
            return false;
        };

        const term = child.wait(io) catch |err| {
            std.log.err("Failed to wait for command: {}", .{err});
            return false;
        };

        switch (term) {
            .exited => |code| {
                if (code != 0) {
                    std.log.err("Command exited with code {d}: {s}", .{ code, cmd });
                    return false;
                }
            },
            .signal => {
                std.log.err("Command killed by signal: {s}", .{cmd});
                return false;
            },
            else => {
                std.log.err("Command terminated abnormally: {s}", .{cmd});
                return false;
            },
        }
    }

    return true;
}

/// Converts a BuildContext string into a Zig slice for target listing.
pub fn listTargets(self: *const BuildContext, writer: *std.Io.Writer) !void {
    const names = try self.registry.listNames(self.allocator);
    defer self.allocator.free(names);

    try writer.print("Available targets ({d}):\n", .{names.len});
    try writer.writeAll("=" ** 60 ++ "\n");

    for (names) |name| {
        const target = self.registry.get(name).?;
        try writer.print("  {s:<20} [{s}]", .{ name, @tagName(target.target_type) });

        if (target.essential) {
            try writer.writeAll(" [essential]");
        }

        if (target.depends.count() > 0) {
            try writer.writeAll(" deps:");
            var iter = target.depends.iterator(.{});
            while (iter.next()) |dep_idx| {
                if (self.interner.getString(dep_idx)) |dep_name| {
                    try writer.print(" {s}", .{dep_name});
                }
            }
        }

        if (target.provides.count() > 0) {
            try writer.writeAll(" provides:");
            var iter = target.provides.iterator(.{});
            while (iter.next()) |prov_idx| {
                if (self.interner.getString(prov_idx)) |prov_name| {
                    try writer.print(" {s}", .{prov_name});
                }
            }
        }

        try writer.writeAll("\n");
    }
}

/// Displays a graphical representation using provided context and data.
pub fn showGraph(self: *BuildContext, target_names: []const []const u8, writer: *std.Io.Writer) !void {
    const graph_str = try self.resolver.visualizeGraph(target_names, self.allocator);
    defer self.allocator.free(graph_str);

    try writer.writeAll(graph_str);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const testing = std.testing;

test "BuildContext: empty target list with no default returns success" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    var ctx = BuildContext.init(testing.allocator, &registry, &interner);
    var result = try ctx.build(&[_][]const u8{});
    defer result.deinit(testing.allocator);

    try testing.expect(result.success);
    try testing.expectEqual(@as(usize, 0), result.targets_built);
    try testing.expectEqual(@as(usize, 0), result.targets_failed);
}

test "BuildContext: empty target list dispatches to default target" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    // "real" is a phony target with no commands — counts as built.
    const real = try testing.allocator.create(Target);
    real.* = try Target.init(testing.allocator, &interner, "real", .phony);
    try registry.add(real);

    // "default" depends on "real".
    const def = try testing.allocator.create(Target);
    def.* = try Target.init(testing.allocator, &interner, "default", .phony);
    try def.setDepends(testing.allocator, &interner, &[_][]const u8{"real"});
    try registry.add(def);

    var ctx = BuildContext.init(testing.allocator, &registry, &interner);
    var result = try ctx.build(&[_][]const u8{});
    defer result.deinit(testing.allocator);

    try testing.expect(result.success);
}

test "BuildContext: dry_run counts targets without executing" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    // A command target that would fail if actually executed.
    const t = try testing.allocator.create(Target);
    t.* = try Target.init(testing.allocator, &interner, "bad", .command);
    try t.commands.append(testing.allocator, try testing.allocator.dupe(u8, "false"));
    try registry.add(t);

    var ctx = BuildContext.init(testing.allocator, &registry, &interner);
    ctx.dry_run = true;

    var result = try ctx.build(&[_][]const u8{"bad"});
    defer result.deinit(testing.allocator);

    try testing.expect(result.success);
    try testing.expectEqual(@as(usize, 1), result.targets_built);
    try testing.expectEqual(@as(usize, 0), result.targets_failed);
}

test "BuildContext: abstract target is skipped (no commands, no exists)" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const t = try testing.allocator.create(Target);
    t.* = try Target.init(testing.allocator, &interner, "iface", .abstract);
    // No commands, no exists → isAbstract() returns true.
    try registry.add(t);

    var ctx = BuildContext.init(testing.allocator, &registry, &interner);
    var result = try ctx.build(&[_][]const u8{"iface"});
    defer result.deinit(testing.allocator);

    // Abstract targets are skipped, so nothing is built and nothing fails.
    try testing.expect(result.success);
    try testing.expectEqual(@as(usize, 0), result.targets_built);
    try testing.expectEqual(@as(usize, 0), result.targets_failed);
}

test "BuildContext: successful command target is counted" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const t = try testing.allocator.create(Target);
    t.* = try Target.init(testing.allocator, &interner, "ok", .command);
    try t.commands.append(testing.allocator, try testing.allocator.dupe(u8, "true"));
    try registry.add(t);

    var ctx = BuildContext.init(testing.allocator, &registry, &interner);
    var result = try ctx.build(&[_][]const u8{"ok"});
    defer result.deinit(testing.allocator);

    try testing.expect(result.success);
    try testing.expectEqual(@as(usize, 1), result.targets_built);
    try testing.expectEqual(@as(usize, 0), result.targets_failed);
}

test "BuildContext: dry_run non-essential failing command is counted without executing" {
    // Prove that a command that would fail at runtime is counted in dry_run mode.
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const t = try testing.allocator.create(Target);
    t.* = try Target.init(testing.allocator, &interner, "would_fail", .command);
    try t.commands.append(testing.allocator, try testing.allocator.dupe(u8, "false"));
    try registry.add(t);

    var ctx = BuildContext.init(testing.allocator, &registry, &interner);
    ctx.dry_run = true;

    var result = try ctx.build(&[_][]const u8{"would_fail"});
    defer result.deinit(testing.allocator);

    // In dry_run every target is "built" (not executed), none fail.
    try testing.expect(result.success);
    try testing.expectEqual(@as(usize, 1), result.targets_built);
    try testing.expectEqual(@as(usize, 0), result.targets_failed);
}

test "BuildContext: essential target — dry_run does not fail" {
    // Verify that an essential target in dry_run mode does not abort the build.
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const essential = try testing.allocator.create(Target);
    essential.* = try Target.init(testing.allocator, &interner, "gate", .command);
    essential.essential = true;
    try essential.commands.append(testing.allocator, try testing.allocator.dupe(u8, "true"));
    try registry.add(essential);

    const after = try testing.allocator.create(Target);
    after.* = try Target.init(testing.allocator, &interner, "step2", .command);
    try after.setDepends(testing.allocator, &interner, &[_][]const u8{"gate"});
    try after.commands.append(testing.allocator, try testing.allocator.dupe(u8, "true"));
    try registry.add(after);

    var ctx = BuildContext.init(testing.allocator, &registry, &interner);
    ctx.dry_run = true;

    var result = try ctx.build(&[_][]const u8{"step2"});
    defer result.deinit(testing.allocator);

    try testing.expect(result.success);
    try testing.expectEqual(@as(usize, 2), result.targets_built);
}

test "BuildContext: phony target with no commands succeeds and counts as built" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const t = try testing.allocator.create(Target);
    t.* = try Target.init(testing.allocator, &interner, "phonytgt", .phony);
    try registry.add(t);

    var ctx = BuildContext.init(testing.allocator, &registry, &interner);
    var result = try ctx.build(&[_][]const u8{"phonytgt"});
    defer result.deinit(testing.allocator);

    try testing.expect(result.success);
    // phony with no commands: executeTarget returns true → counted as built.
    try testing.expectEqual(@as(usize, 1), result.targets_built);
}

test "BuildContext: circular dependency is detected and build returns not-success" {
    // The resolver detects the cycle; BuildContext must surface it as success=false.
    // We keep this test but note that the resolver itself is tested for
    // CircularDependency in resolver.zig; here we validate the BuildContext contract.
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const ta = try testing.allocator.create(Target);
    ta.* = try Target.init(testing.allocator, &interner, "cx", .phony);
    const tb = try testing.allocator.create(Target);
    tb.* = try Target.init(testing.allocator, &interner, "cy", .phony);
    try ta.setDepends(testing.allocator, &interner, &[_][]const u8{"cy"});
    try tb.setDepends(testing.allocator, &interner, &[_][]const u8{"cx"});
    try registry.add(ta);
    try registry.add(tb);

    var ctx = BuildContext.init(testing.allocator, &registry, &interner);
    var result = try ctx.build(&[_][]const u8{"cx"});
    defer result.deinit(testing.allocator);

    try testing.expect(!result.success);
}

test "BuildContext: duration_ns is non-zero after a build" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const t = try testing.allocator.create(Target);
    t.* = try Target.init(testing.allocator, &interner, "timed", .phony);
    try registry.add(t);

    var ctx = BuildContext.init(testing.allocator, &registry, &interner);
    var result = try ctx.build(&[_][]const u8{"timed"});
    defer result.deinit(testing.allocator);

    try testing.expect(result.duration_ns > 0);
}

test "BuildContext: GPA no leaks across a multi-target build" {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    const allocator = gpa.allocator();

    {
        var interner = StringInterner.init(allocator);
        defer interner.deinit();
        var registry = TargetRegistry.init(allocator, &interner);
        defer registry.deinit();

        const ta = try allocator.create(Target);
        ta.* = try Target.init(allocator, &interner, "src", .phony);
        try registry.add(ta);

        const tb = try allocator.create(Target);
        tb.* = try Target.init(allocator, &interner, "build", .command);
        try tb.setDepends(allocator, &interner, &[_][]const u8{"src"});
        try tb.commands.append(allocator, try allocator.dupe(u8, "true"));
        try registry.add(tb);

        var ctx = BuildContext.init(allocator, &registry, &interner);
        var result = try ctx.build(&[_][]const u8{"build"});
        defer result.deinit(allocator);

        try testing.expect(result.success);
    }

    try testing.expectEqual(.ok, gpa.deinit());
}
