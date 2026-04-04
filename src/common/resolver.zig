const std = @import("std");
const Target = @import("target.zig").Target;
const TargetRegistry = @import("registry.zig").TargetRegistry;
const StringInterner = @import("interner.zig").StringInterner;

pub const ResolverError = error{ CircularDependency, TargetNotFound };

/// Options controlling resolver behaviour.
pub const ResolverOptions = struct {
    /// When `true`, an unknown dependency causes `collectDependencies` to return
    /// `ResolverError.TargetNotFound` instead of logging a warning and continuing.
    strict: bool = false,
};

pub const DependencyResolver = @This();

allocator: std.mem.Allocator,
registry: *TargetRegistry,
interner: *StringInterner,
options: ResolverOptions,

// comment-above-init
/// Initializes the dependency resolver with allocator, registry, and interners, returning a resolved dependency.
pub fn init(allocator: std.mem.Allocator, registry: *TargetRegistry, interner: *StringInterner) DependencyResolver {
    return .{
        .allocator = allocator,
        .registry = registry,
        .interner = interner,
        .options = .{},
    };
}

/// Like `init`, but accepts explicit `ResolverOptions`.
pub fn initWithOptions(
    allocator: std.mem.Allocator,
    registry: *TargetRegistry,
    interner: *StringInterner,
    options: ResolverOptions,
) DependencyResolver {
    return .{
        .allocator = allocator,
        .registry = registry,
        .interner = interner,
        .options = options,
    };
}

/// Manages resolved build components, tracks ownership, ensures static invariants.
pub const ResolvedBuild = struct {
    targets: []*Target,
    allocator: std.mem.Allocator,

    pub fn deinit(self: *ResolvedBuild) void {
        self.allocator.free(self.targets);
    }
};

/// Simple topological sort over all transitive concrete dependencies.
pub fn resolve(self: *DependencyResolver, target_names: []const []const u8) !ResolvedBuild {
    var all_needed = std.AutoHashMap(usize, *Target).init(self.allocator);
    defer all_needed.deinit();

    for (target_names) |name| {
        const target = self.registry.get(name) orelse return ResolverError.TargetNotFound;
        try self.collectDependencies(target, &all_needed);
    }

    return self.topoSort(&all_needed);
}

/// Abstract-aware resolution: resolves abstract dependencies via provider selection.
pub fn resolveAbstractDependencies(
    self: *DependencyResolver,
    target_names: []const []const u8,
    provided: *std.bit_set.DynamicBitSetUnmanaged,
) !ResolvedBuild {
    var all_needed = std.AutoHashMap(usize, *Target).init(self.allocator);
    defer all_needed.deinit();

    var provided_local = try provided.clone(self.allocator);
    defer provided_local.deinit(self.allocator);

    for (target_names) |name| {
        const target = self.registry.get(name) orelse return ResolverError.TargetNotFound;
        try self.collectDependenciesWithAbstracts(target, &all_needed, &provided_local);
    }

    return self.topoSort(&all_needed);
}

/// Kahn's topological sort on the collected target set.
fn topoSort(self: *DependencyResolver, all_needed: *std.AutoHashMap(usize, *Target)) !ResolvedBuild {
    const total = all_needed.count();

    var in_degree = std.AutoHashMap(usize, usize).init(self.allocator);
    defer in_degree.deinit();

    var iter = all_needed.valueIterator();
    while (iter.next()) |target| {
        _ = try in_degree.getOrPutValue(target.*.bit_index, 0);
    }

    // adjacency: dep_idx -> list of targets that depend on it
    var graph = std.AutoHashMap(usize, std.ArrayListUnmanaged(usize)).init(self.allocator);
    defer {
        var giter = graph.valueIterator();
        while (giter.next()) |list| {
            list.deinit(self.allocator);
        }
        graph.deinit();
    }

    iter = all_needed.valueIterator();
    while (iter.next()) |target| {
        var dep_iter = target.*.depends.iterator(.{});
        while (dep_iter.next()) |dep_idx| {
            if (all_needed.get(dep_idx)) |_| {
                const gop = try graph.getOrPutValue(dep_idx, .{});
                try gop.value_ptr.append(self.allocator, target.*.bit_index);
                const current = in_degree.get(target.*.bit_index).?;
                try in_degree.put(target.*.bit_index, current + 1);
            }
        }
    }

    // Use an ArrayList as a head-pointer queue to avoid O(n) orderedRemove(0).
    var queue: std.ArrayListUnmanaged(usize) = .{};
    defer queue.deinit(self.allocator);

    var degree_iter = in_degree.iterator();
    while (degree_iter.next()) |entry| {
        if (entry.value_ptr.* == 0) {
            try queue.append(self.allocator, entry.key_ptr.*);
        }
    }

    std.mem.sort(usize, queue.items, {}, std.sort.asc(usize));

    var result: std.ArrayListUnmanaged(*Target) = .{};
    errdefer result.deinit(self.allocator);

    var head: usize = 0;
    while (head < queue.items.len) : (head += 1) {
        const current_idx = queue.items[head];

        const current_target = all_needed.get(current_idx).?;
        try result.append(self.allocator, current_target);

        if (graph.get(current_idx)) |dependents| {
            for (dependents.items) |dep_idx| {
                const current_degree = in_degree.get(dep_idx).?;
                const new_degree = current_degree - 1;
                try in_degree.put(dep_idx, new_degree);
                if (new_degree == 0) {
                    try queue.append(self.allocator, dep_idx);
                    std.mem.sort(usize, queue.items[head + 1 ..], {}, std.sort.asc(usize));
                }
            }
        }
    }

    if (result.items.len != total) {
        return ResolverError.CircularDependency;
    }

    return .{
        .targets = try result.toOwnedSlice(self.allocator),
        .allocator = self.allocator,
    };
}

fn collectDependencies(
    self: *DependencyResolver,
    target: *Target,
    collected: *std.AutoHashMap(usize, *Target),
) !void {
    const gop = try collected.getOrPut(target.bit_index);
    if (gop.found_existing) return;

    gop.value_ptr.* = target;

    var iter = target.depends.iterator(.{});
    while (iter.next()) |dep_idx| {
        if (self.registry.getByBitIndex(dep_idx)) |dep_target| {
            try self.collectDependencies(dep_target, collected);
        } else {
            if (self.options.strict) {
                return ResolverError.TargetNotFound;
            }
            if (self.interner.getString(dep_idx)) |name| {
                std.log.debug("Dependency '{s}' of target '{s}' not found", .{ name, target.name });
            }
        }
    }
}

fn collectDependenciesWithAbstracts(
    self: *DependencyResolver,
    target: *Target,
    collected: *std.AutoHashMap(usize, *Target),
    provided: *std.bit_set.DynamicBitSetUnmanaged,
) !void {
    const gop = try collected.getOrPut(target.bit_index);
    if (gop.found_existing) return;

    gop.value_ptr.* = target;

    // Mark everything this target provides as available.
    // Use bit-by-bit iteration to avoid setUnion size assertion when bitsets differ.
    var prov_iter = target.provides.iterator(.{});
    while (prov_iter.next()) |bit_idx| {
        if (bit_idx >= provided.bit_length) {
            try provided.resize(self.allocator, bit_idx + 1, false);
        }
        provided.set(bit_idx);
    }

    var iter = target.depends.iterator(.{});
    while (iter.next()) |dep_idx| {
        if (dep_idx < provided.bit_length and provided.isSet(dep_idx)) continue;

        if (self.registry.getByBitIndex(dep_idx)) |dep_target| {
            if (dep_target.isAbstract()) {
                // Abstract dep: pick the best concrete provider by popcount distance
                if (self.registry.getProviders(dep_idx)) |providers| {
                    if (providers.len > 0) {
                        var best_provider: ?*Target = null;
                        var best_score: usize = std.math.maxInt(usize);

                        for (providers) |provider| {
                            const score = provider.distanceFrom(provided);
                            if (score < best_score or
                                (score == best_score and best_provider != null and
                                    std.mem.lessThan(u8, provider.name, best_provider.?.name)))
                            {
                                best_score = score;
                                best_provider = provider;
                            }
                        }

                        if (best_provider) |provider| {
                            try self.collectDependenciesWithAbstracts(provider, collected, provided);
                        }
                    }
                }
            } else {
                try self.collectDependenciesWithAbstracts(dep_target, collected, provided);
            }
        }
    }
}

/// Manages resolved level configurations with a fixed-size buffer pool; ensures ownership and invariants are preserved.
pub const ResolvedLevels = struct {
    levels: []const []*Target,
    allocator: std.mem.Allocator,

    pub fn deinit(self: *ResolvedLevels) void {
        for (self.levels) |lvl| self.allocator.free(lvl);
        self.allocator.free(self.levels);
    }
};

/// Retrieves resolved levels from a dependency resolver for Zig targets.
pub fn getLevels(
    self: *const DependencyResolver,
    targets: []*Target,
) !ResolvedLevels {
    if (targets.len == 0) {
        return .{ .levels = &[_][]*Target{}, .allocator = self.allocator };
    }

    // Assign a level to each target: max(dep level) + 1.
    var level_map = std.AutoHashMap(usize, usize).init(self.allocator);
    defer level_map.deinit();

    // Targets arrive in topological order; dependencies are always processed
    // before dependents.
    for (targets) |t| {
        var max_dep_level: usize = 0;
        var dep_iter = t.depends.iterator(.{});
        while (dep_iter.next()) |dep_idx| {
            if (level_map.get(dep_idx)) |dep_level| {
                if (dep_level + 1 > max_dep_level) {
                    max_dep_level = dep_level + 1;
                }
            }
        }
        try level_map.put(t.bit_index, max_dep_level);
    }

    // Find the maximum level.
    var max_level: usize = 0;
    {
        var it = level_map.valueIterator();
        while (it.next()) |lv| {
            if (lv.* > max_level) max_level = lv.*;
        }
    }

    // Bucket targets by level.
    const num_levels = max_level + 1;
    var buckets = try self.allocator.alloc(std.ArrayListUnmanaged(*Target), num_levels);
    defer {
        for (buckets) |*b| b.deinit(self.allocator);
        self.allocator.free(buckets);
    }
    for (buckets) |*b| b.* = .{};

    for (targets) |t| {
        const lv = level_map.get(t.bit_index).?;
        try buckets[lv].append(self.allocator, t);
    }

    // Convert buckets to owned slices.
    const levels = try self.allocator.alloc([]*Target, num_levels);
    errdefer self.allocator.free(levels);
    for (buckets, 0..) |*b, i| {
        levels[i] = try b.toOwnedSlice(self.allocator);
    }

    return .{ .levels = levels, .allocator = self.allocator };
}

/// Generates a Zig array representation of a graph visualization based on dependency data.
pub fn visualizeGraph(
    self: *DependencyResolver,
    target_names: []const []const u8,
    allocator: std.mem.Allocator,
) ![]u8 {
    var output: std.ArrayListUnmanaged(u8) = .{};
    errdefer output.deinit(allocator);
    const writer = output.writer(allocator);

    try writer.writeAll("Dependency Graph:\n");
    try writer.writeAll("=" ** 50 ++ "\n");

    var visited = std.AutoHashMap(usize, void).init(allocator);
    defer visited.deinit();

    for (target_names) |name| {
        if (self.registry.get(name)) |target| {
            try self.printTree(target, &visited, "", true, writer);
            try writer.writeByte('\n');
        }
    }

    return output.toOwnedSlice(allocator);
}

fn printTree(
    self: *DependencyResolver,
    target: *Target,
    visited: *std.AutoHashMap(usize, void),
    prefix: []const u8,
    is_last: bool,
    writer: anytype,
) !void {
    if (visited.contains(target.bit_index)) {
        try writer.print("{s}{s}{s} (already shown)\n", .{
            prefix,
            if (is_last) "└── " else "├── ",
            target.name,
        });
        return;
    }

    try visited.put(target.bit_index, {});

    const connector = if (is_last) "└── " else "├── ";
    try writer.print("{s}{s}{s} [{s}]\n", .{
        prefix,
        connector,
        target.name,
        @tagName(target.target_type),
    });

    const new_prefix = try std.fmt.allocPrint(self.allocator, "{s}{s}", .{
        prefix,
        if (is_last) "    " else "│   ",
    });
    defer self.allocator.free(new_prefix);

    // Collect resolved dep targets in a single pass so we know the count up-front.
    var deps: std.ArrayListUnmanaged(*Target) = .{};
    defer deps.deinit(self.allocator);

    var iter = target.depends.iterator(.{});
    while (iter.next()) |dep_idx| {
        if (self.registry.getByBitIndex(dep_idx)) |dep_target| {
            try deps.append(self.allocator, dep_target);
        }
    }

    for (deps.items, 0..) |dep_target, idx| {
        const is_last_dep = idx == deps.items.len - 1;
        try self.printTree(dep_target, visited, new_prefix, is_last_dep, writer);
    }
}

const testing = std.testing;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn makeTarget(
    allocator: std.mem.Allocator,
    interner: *StringInterner,
    name: []const u8,
    target_type: @import("target.zig").TargetType,
) !*Target {
    const t = try allocator.create(Target);
    t.* = try Target.init(allocator, interner, name, target_type);
    return t;
}

// ---------------------------------------------------------------------------
// resolve() — basic topology
// ---------------------------------------------------------------------------

test "DependencyResolver basic resolution" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const target_a = try testing.allocator.create(Target);
    target_a.* = try Target.init(testing.allocator, &interner, "a", .phony);

    const target_b = try testing.allocator.create(Target);
    target_b.* = try Target.init(testing.allocator, &interner, "b", .phony);
    try target_b.setDepends(testing.allocator, &interner, &[_][]const u8{"a"});

    try registry.add(target_a);
    try registry.add(target_b);

    var resolver = DependencyResolver.init(testing.allocator, &registry, &interner);

    var resolved = try resolver.resolve(&[_][]const u8{"b"});
    defer resolved.deinit();

    try testing.expectEqual(@as(usize, 2), resolved.targets.len);
    try testing.expectEqualStrings("a", resolved.targets[0].name);
    try testing.expectEqualStrings("b", resolved.targets[1].name);
}

test "DependencyResolver GPA no leaks" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    const allocator = gpa.allocator();

    {
        var interner = StringInterner.init(allocator);
        defer interner.deinit();

        var registry = TargetRegistry.init(allocator, &interner);
        defer registry.deinit();

        const ta = try allocator.create(Target);
        ta.* = try Target.init(allocator, &interner, "src", .file);

        const tb = try allocator.create(Target);
        tb.* = try Target.init(allocator, &interner, "obj", .command);
        try tb.setDepends(allocator, &interner, &[_][]const u8{"src"});
        try tb.commands.append(allocator, try allocator.dupe(u8, "cc -c src.c"));

        const tc = try allocator.create(Target);
        tc.* = try Target.init(allocator, &interner, "bin", .command);
        try tc.setDepends(allocator, &interner, &[_][]const u8{"obj"});
        try tc.commands.append(allocator, try allocator.dupe(u8, "cc -o bin obj.o"));

        try registry.add(ta);
        try registry.add(tb);
        try registry.add(tc);

        var resolver = DependencyResolver.init(allocator, &registry, &interner);
        var resolved = try resolver.resolve(&[_][]const u8{"bin"});
        defer resolved.deinit();

        try testing.expectEqual(@as(usize, 3), resolved.targets.len);
        try testing.expectEqualStrings("src", resolved.targets[0].name);
        try testing.expectEqualStrings("obj", resolved.targets[1].name);
        try testing.expectEqualStrings("bin", resolved.targets[2].name);
    }

    try testing.expectEqual(.ok, gpa.deinit());
}

test "DependencyResolver: resolve unknown target returns TargetNotFound" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    var resolver = DependencyResolver.init(testing.allocator, &registry, &interner);
    const result = resolver.resolve(&[_][]const u8{"ghost"});
    try testing.expectError(ResolverError.TargetNotFound, result);
}

test "DependencyResolver: strict mode returns TargetNotFound for missing dep" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    // "b" depends on "a", but "a" is never registered.
    const b = try makeTarget(testing.allocator, &interner, "b", .phony);
    try b.setDepends(testing.allocator, &interner, &[_][]const u8{"a"});
    try registry.add(b);

    var resolver = DependencyResolver.initWithOptions(testing.allocator, &registry, &interner, .{ .strict = true });
    const result = resolver.resolve(&[_][]const u8{"b"});
    try testing.expectError(ResolverError.TargetNotFound, result);
}

test "DependencyResolver: non-strict mode silently skips missing dep" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    // "b" depends on "a", but "a" is never registered — non-strict ignores it.
    const b = try makeTarget(testing.allocator, &interner, "b", .phony);
    try b.setDepends(testing.allocator, &interner, &[_][]const u8{"a"});
    try registry.add(b);

    var resolver = DependencyResolver.init(testing.allocator, &registry, &interner);
    var resolved = try resolver.resolve(&[_][]const u8{"b"});
    defer resolved.deinit();

    try testing.expectEqual(@as(usize, 1), resolved.targets.len);
    try testing.expectEqualStrings("b", resolved.targets[0].name);
}

test "DependencyResolver: single target with no deps resolves to itself" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const t = try makeTarget(testing.allocator, &interner, "alone", .phony);
    try registry.add(t);

    var resolver = DependencyResolver.init(testing.allocator, &registry, &interner);
    var resolved = try resolver.resolve(&[_][]const u8{"alone"});
    defer resolved.deinit();

    try testing.expectEqual(@as(usize, 1), resolved.targets.len);
    try testing.expectEqualStrings("alone", resolved.targets[0].name);
}

test "DependencyResolver: diamond dependency resolves all nodes once" {
    // Graph:  top → left → base
    //              → right → base
    // base must appear exactly once and before left/right.
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const base = try makeTarget(testing.allocator, &interner, "base", .phony);
    const left = try makeTarget(testing.allocator, &interner, "left", .phony);
    const right = try makeTarget(testing.allocator, &interner, "right", .phony);
    const top = try makeTarget(testing.allocator, &interner, "top", .phony);

    try left.setDepends(testing.allocator, &interner, &[_][]const u8{"base"});
    try right.setDepends(testing.allocator, &interner, &[_][]const u8{"base"});
    try top.setDepends(testing.allocator, &interner, &[_][]const u8{ "left", "right" });

    try registry.add(base);
    try registry.add(left);
    try registry.add(right);
    try registry.add(top);

    var resolver = DependencyResolver.init(testing.allocator, &registry, &interner);
    var resolved = try resolver.resolve(&[_][]const u8{"top"});
    defer resolved.deinit();

    // Four distinct targets, each appearing once.
    try testing.expectEqual(@as(usize, 4), resolved.targets.len);
    // "base" must come before "left" and "right", which must come before "top".
    var base_pos: usize = 0;
    var top_pos: usize = 0;
    for (resolved.targets, 0..) |tgt, i| {
        if (std.mem.eql(u8, tgt.name, "base")) base_pos = i;
        if (std.mem.eql(u8, tgt.name, "top")) top_pos = i;
    }
    try testing.expect(base_pos < top_pos);
}

test "DependencyResolver: multi-root resolve includes all roots" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const ta = try makeTarget(testing.allocator, &interner, "aa", .phony);
    const tb = try makeTarget(testing.allocator, &interner, "bb", .phony);
    try registry.add(ta);
    try registry.add(tb);

    var resolver = DependencyResolver.init(testing.allocator, &registry, &interner);
    var resolved = try resolver.resolve(&[_][]const u8{ "aa", "bb" });
    defer resolved.deinit();

    try testing.expectEqual(@as(usize, 2), resolved.targets.len);
}

test "DependencyResolver: circular dependency returns CircularDependency" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const ta = try makeTarget(testing.allocator, &interner, "p", .phony);
    const tb = try makeTarget(testing.allocator, &interner, "q", .phony);
    try ta.setDepends(testing.allocator, &interner, &[_][]const u8{"q"});
    try tb.setDepends(testing.allocator, &interner, &[_][]const u8{"p"});
    try registry.add(ta);
    try registry.add(tb);

    var resolver = DependencyResolver.init(testing.allocator, &registry, &interner);
    const result = resolver.resolve(&[_][]const u8{"p"});
    try testing.expectError(ResolverError.CircularDependency, result);
}

// ---------------------------------------------------------------------------
// resolveAbstractDependencies()
// ---------------------------------------------------------------------------

test "DependencyResolver: abstract dep resolved via provider" {
    // "iface" is abstract; "impl" provides "iface"; "consumer" depends on "iface".
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const iface = try makeTarget(testing.allocator, &interner, "iface", .abstract);
    const impl = try makeTarget(testing.allocator, &interner, "impl", .phony);
    const consumer = try makeTarget(testing.allocator, &interner, "consumer", .phony);

    try impl.setProvides(testing.allocator, &interner, &[_][]const u8{"iface"});
    try consumer.setDepends(testing.allocator, &interner, &[_][]const u8{"iface"});

    try registry.add(iface);
    try registry.add(impl);
    try registry.add(consumer);

    var provided = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(testing.allocator, interner.count());
    defer provided.deinit(testing.allocator);

    var resolver = DependencyResolver.init(testing.allocator, &registry, &interner);
    var resolved = try resolver.resolveAbstractDependencies(&[_][]const u8{"consumer"}, &provided);
    defer resolved.deinit();

    // We expect "impl" to be in the resolved set (selected as provider of "iface").
    var found_impl = false;
    for (resolved.targets) |tgt| {
        if (std.mem.eql(u8, tgt.name, "impl")) found_impl = true;
    }
    try testing.expect(found_impl);
}

test "DependencyResolver: resolveAbstractDependencies unknown target returns error" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    var provided = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(testing.allocator, 1);
    defer provided.deinit(testing.allocator);

    var resolver = DependencyResolver.init(testing.allocator, &registry, &interner);
    const result = resolver.resolveAbstractDependencies(&[_][]const u8{"nope"}, &provided);
    try testing.expectError(ResolverError.TargetNotFound, result);
}

// ---------------------------------------------------------------------------
// visualizeGraph()
// ---------------------------------------------------------------------------

test "DependencyResolver: visualizeGraph contains target names" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const ta = try makeTarget(testing.allocator, &interner, "root", .phony);
    const tb = try makeTarget(testing.allocator, &interner, "leaf", .phony);
    try ta.setDepends(testing.allocator, &interner, &[_][]const u8{"leaf"});
    try registry.add(ta);
    try registry.add(tb);

    var resolver = DependencyResolver.init(testing.allocator, &registry, &interner);
    const graph = try resolver.visualizeGraph(&[_][]const u8{"root"}, testing.allocator);
    defer testing.allocator.free(graph);

    try testing.expect(std.mem.indexOf(u8, graph, "root") != null);
    try testing.expect(std.mem.indexOf(u8, graph, "leaf") != null);
}

test "DependencyResolver: visualizeGraph revisited node shows already shown" {
    // diamond: top → left → base, top → right → base
    // "base" will be visited twice in the tree traversal.
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const base = try makeTarget(testing.allocator, &interner, "gbase", .phony);
    const left = try makeTarget(testing.allocator, &interner, "gleft", .phony);
    const right = try makeTarget(testing.allocator, &interner, "gright", .phony);
    const top = try makeTarget(testing.allocator, &interner, "gtop", .phony);
    try left.setDepends(testing.allocator, &interner, &[_][]const u8{"gbase"});
    try right.setDepends(testing.allocator, &interner, &[_][]const u8{"gbase"});
    try top.setDepends(testing.allocator, &interner, &[_][]const u8{ "gleft", "gright" });
    try registry.add(base);
    try registry.add(left);
    try registry.add(right);
    try registry.add(top);

    var resolver = DependencyResolver.init(testing.allocator, &registry, &interner);
    const graph = try resolver.visualizeGraph(&[_][]const u8{"gtop"}, testing.allocator);
    defer testing.allocator.free(graph);

    try testing.expect(std.mem.indexOf(u8, graph, "already shown") != null);
}

test "DependencyResolver: GPA no leaks" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    const allocator = gpa.allocator();

    {
        var interner = StringInterner.init(allocator);
        defer interner.deinit();

        var registry = TargetRegistry.init(allocator, &interner);
        defer registry.deinit();

        const ta = try allocator.create(Target);
        ta.* = try Target.init(allocator, &interner, "src", .file);

        const tb = try allocator.create(Target);
        tb.* = try Target.init(allocator, &interner, "obj", .command);
        try tb.setDepends(allocator, &interner, &[_][]const u8{"src"});
        try tb.commands.append(allocator, try allocator.dupe(u8, "cc -c src.c"));

        const tc = try allocator.create(Target);
        tc.* = try Target.init(allocator, &interner, "bin", .command);
        try tc.setDepends(allocator, &interner, &[_][]const u8{"obj"});
        try tc.commands.append(allocator, try allocator.dupe(u8, "cc -o bin obj.o"));

        try registry.add(ta);
        try registry.add(tb);
        try registry.add(tc);

        var resolver = DependencyResolver.init(allocator, &registry, &interner);
        var resolved = try resolver.resolve(&[_][]const u8{"bin"});
        defer resolved.deinit();

        try testing.expectEqual(@as(usize, 3), resolved.targets.len);
        try testing.expectEqualStrings("src", resolved.targets[0].name);
        try testing.expectEqualStrings("obj", resolved.targets[1].name);
        try testing.expectEqualStrings("bin", resolved.targets[2].name);
    }

    try testing.expectEqual(.ok, gpa.deinit());
}

test "getLevels: diamond DAG produces two independent levels for middle nodes" {
    // Diamond: src <- obj_a, src <- obj_b, obj_a <- bin, obj_b <- bin
    // Expected levels: [src], [obj_a, obj_b], [bin]
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
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
        tb.* = try Target.init(allocator, &interner, "obj_a", .command);
        try tb.setDepends(allocator, &interner, &[_][]const u8{"src"});
        try registry.add(tb);

        const tc = try allocator.create(Target);
        tc.* = try Target.init(allocator, &interner, "obj_b", .command);
        try tc.setDepends(allocator, &interner, &[_][]const u8{"src"});
        try registry.add(tc);

        const td = try allocator.create(Target);
        td.* = try Target.init(allocator, &interner, "bin", .command);
        try td.setDepends(allocator, &interner, &[_][]const u8{ "obj_a", "obj_b" });
        try registry.add(td);

        var resolver = DependencyResolver.init(allocator, &registry, &interner);
        var resolved = try resolver.resolve(&[_][]const u8{"bin"});
        defer resolved.deinit();

        var lvls = try resolver.getLevels(resolved.targets);
        defer lvls.deinit();

        try testing.expectEqual(@as(usize, 3), lvls.levels.len);
        try testing.expectEqual(@as(usize, 1), lvls.levels[0].len); // src
        try testing.expectEqual(@as(usize, 2), lvls.levels[1].len); // obj_a, obj_b
        try testing.expectEqual(@as(usize, 1), lvls.levels[2].len); // bin
    }
    try testing.expectEqual(.ok, gpa.deinit());
}
