const std = @import("std");
const Target = @import("target.zig").Target;
const TargetType = @import("target.zig").TargetType;
const StringInterner = @import("interner.zig").StringInterner;

pub const TargetRegistry = @This();

allocator: std.mem.Allocator,
interner: *StringInterner,
targets: std.StringHashMapUnmanaged(*Target),
by_bit_index: std.AutoHashMapUnmanaged(usize, *Target),
provider_map: std.AutoHashMapUnmanaged(usize, std.ArrayListUnmanaged(*Target)),

/// Initializes the registry with an allocator and interners, returning a target registry instance.
pub fn init(allocator: std.mem.Allocator, interner: *StringInterner) TargetRegistry {
    return .{
        .allocator = allocator,
        .interner = interner,
        .targets = .{},
        .by_bit_index = .{},
        .provider_map = .{},
    };
}

/// Cleans up resources by releasing the registry instance.
pub fn deinit(self: *TargetRegistry) void {
    var iter = self.targets.valueIterator();
    while (iter.next()) |t| {
        t.*.*.deinit(self.allocator);
        self.allocator.destroy(t.*);
    }
    self.targets.deinit(self.allocator);
    self.by_bit_index.deinit(self.allocator);

    var prov_iter = self.provider_map.valueIterator();
    while (prov_iter.next()) |list| {
        list.deinit(self.allocator);
    }
    self.provider_map.deinit(self.allocator);
}

/// Adds a target registry entry to the registry.
pub fn add(self: *TargetRegistry, tgt: *Target) !void {
    try self.targets.put(self.allocator, tgt.name, tgt);
    try self.by_bit_index.put(self.allocator, tgt.bit_index, tgt);
    self.updateProviderMap(tgt);
}

/// Retrieves a target registry entry by name, returning a pointer to its registry data.
pub fn get(self: *const TargetRegistry, name: []const u8) ?*Target {
    return self.targets.get(name);
}

/// Retrieves a target registry entry by its bit index, returning a pointer to the matching record.
pub fn getByBitIndex(self: *const TargetRegistry, idx: usize) ?*Target {
    return self.by_bit_index.get(idx);
}

/// Removes a registry entry by name, returning void and no error on success.
pub fn remove(self: *TargetRegistry, name: []const u8) void {
    if (self.targets.get(name)) |t| {
        _ = self.by_bit_index.remove(t.bit_index);
        _ = self.targets.remove(name);
    }
}

/// Converts a registry string into a slice of byte slices, returning an array of arrays of u8.
pub fn listNames(self: *const TargetRegistry, allocator: std.mem.Allocator) ![][]const u8 {
    var names: std.ArrayList([]const u8) = .{};
    errdefer names.deinit(allocator);

    var iter = self.targets.keyIterator();
    while (iter.next()) |name| {
        try names.append(allocator, name.*);
    }

    std.mem.sort([]const u8, names.items, {}, struct {
        fn lessThan(_: void, a: []const u8, b: []const u8) bool {
            return std.mem.lessThan(u8, a, b);
        }
    }.lessThan);

    return names.toOwnedSlice(allocator);
}

fn updateProviderMap(self: *TargetRegistry, tgt: *Target) void {
    var iter = tgt.provides.iterator(.{});
    while (iter.next()) |provides_idx| {
        const gop = self.provider_map.getOrPut(self.allocator, provides_idx) catch return;
        if (!gop.found_existing) {
            gop.value_ptr.* = .{};
        }
        gop.value_ptr.append(self.allocator, tgt) catch return;
    }
}

/// Retrieves provider addresses for a specified registry bit index.
pub fn getProviders(self: *const TargetRegistry, bit_index: usize) ?[]*Target {
    if (self.provider_map.get(bit_index)) |list| {
        return list.items;
    }
    return null;
}

/// Retrieves registry providers matching the given name, returning their addresses.
pub fn getProvidersForName(self: *const TargetRegistry, name: []const u8) ?[]*Target {
    const idx = self.interner.getIndex(name) orelse return null;
    return self.getProviders(idx);
}

/// Counts registry entries; returns the number of valid targets.
pub fn count(self: *const TargetRegistry) usize {
    return self.targets.count();
}

/// Retrieves essential target registry entries with allocator support.
pub fn essentialTargets(self: *const TargetRegistry, allocator: std.mem.Allocator) ![]*Target {
    var essentials: std.ArrayList(*Target) = .{};
    errdefer essentials.deinit(allocator);

    var iter = self.targets.valueIterator();
    while (iter.next()) |t| {
        if (t.*.*.essential) {
            try essentials.append(allocator, t.*);
        }
    }

    return essentials.toOwnedSlice(allocator);
}

/// Converts a TargetRegistry reference into a slice of Target objects.
pub fn abstractTargets(self: *const TargetRegistry, allocator: std.mem.Allocator) ![]*Target {
    var abstracts: std.ArrayList(*Target) = .{};
    errdefer abstracts.deinit(allocator);

    var iter = self.targets.valueIterator();
    while (iter.next()) |t| {
        if (t.*.*.isAbstract()) {
            try abstracts.append(allocator, t.*);
        }
    }

    return abstracts.toOwnedSlice(allocator);
}

// ---------------------------------------------------------------------------
// TargetBuilder — fluent DSL for registering targets
// ---------------------------------------------------------------------------
//
// Usage:
//   try registry.target("build", .file)
//       .depends(&.{"compile", "link"})
//       .provides(&.{"artifact"})
//       .command("gcc -o artifact a.o b.o")
//       .essential()
//       .register();
//
// Errors from .depends()/.provides()/.command() are accumulated and surfaced
// at .register().  If .register() is never called the heap-allocated Target
// leaks — always terminate the chain with .register().
//
// Ownership: on successful .register() the Target is owned by the registry.
// On error (surfaced at .register()) the Target is freed before returning.

/// Manages registry targets with ownership and invariants; ensures stable access patterns.
pub const TargetBuilder = struct {
    allocator: std.mem.Allocator,
    registry: *TargetRegistry,
    interner: *StringInterner,
    /// null when allocation failed in `TargetRegistry.target()`.
    target: ?*Target,
    /// First error encountered during chaining; surfaced by `register()`.
    err: ?anyerror,

    /// Set the dependency list (interned bit-set).
    pub fn depends(self: *TargetBuilder, names: []const []const u8) *TargetBuilder {
        if (self.err != null or self.target == null) return self;
        self.target.?.setDepends(self.allocator, self.interner, names) catch |e| {
            self.err = e;
        };
        return self;
    }

    /// Set the provides list (interned bit-set).
    pub fn provides(self: *TargetBuilder, names: []const []const u8) *TargetBuilder {
        if (self.err != null or self.target == null) return self;
        self.target.?.setProvides(self.allocator, self.interner, names) catch |e| {
            self.err = e;
        };
        return self;
    }

    /// Append a shell command string (allocator-owned copy).
    pub fn command(self: *TargetBuilder, cmd: []const u8) *TargetBuilder {
        if (self.err != null or self.target == null) return self;
        const owned = self.allocator.dupe(u8, cmd) catch |e| {
            self.err = e;
            return self;
        };
        self.target.?.commands.append(self.allocator, owned) catch |e| {
            self.allocator.free(owned);
            self.err = e;
        };
        return self;
    }

    /// Mark this target as essential (must succeed for the build to pass).
    pub fn essential(self: *TargetBuilder) *TargetBuilder {
        if (self.target) |t| t.essential = true;
        return self;
    }

    /// Set the `exists` path guard (target is skipped when the file exists).
    pub fn exists(self: *TargetBuilder, path: []const u8) *TargetBuilder {
        if (self.err != null or self.target == null) return self;
        const owned = self.allocator.dupe(u8, path) catch |e| {
            self.err = e;
            return self;
        };
        if (self.target.?.exists) |old| self.allocator.free(old);
        self.target.?.exists = owned;
        return self;
    }

    /// Set the LanceDB/node id (optional; 0 if not set).
    pub fn id(self: *TargetBuilder, node_id: i64) *TargetBuilder {
        if (self.target) |t| t.id = node_id;
        return self;
    }

    /// Commit the target to the registry.
    ///
    /// On success the registry owns the Target.
    /// On any accumulated error the Target is freed and the error is returned.
    pub fn register(self: *TargetBuilder) !void {
        if (self.err) |e| {
            if (self.target) |t| {
                t.deinit(self.allocator);
                self.allocator.destroy(t);
                self.target = null;
            }
            return e;
        }
        if (self.target) |t| {
            try self.registry.add(t);
            self.target = null; // registry now owns it
        }
    }
};

/// Return a fluent TargetBuilder for adding a new target to this registry.
///
/// Errors during Target allocation are deferred to `register()` so the entire
/// chain can be expressed as a single `try` expression:
///
///   try registry.target("build", .file)
///       .depends(&.{"compile"})
///       .register();
pub fn target(self: *TargetRegistry, name: []const u8, target_type: TargetType) TargetBuilder {
    const t = self.allocator.create(Target) catch |e| {
        return .{ .allocator = self.allocator, .registry = self, .interner = self.interner, .target = null, .err = e };
    };
    t.* = Target.init(self.allocator, self.interner, name, target_type) catch |e| {
        self.allocator.destroy(t);
        return .{ .allocator = self.allocator, .registry = self, .interner = self.interner, .target = null, .err = e };
    };
    return .{ .allocator = self.allocator, .registry = self, .interner = self.interner, .target = t, .err = null };
}

// ---------------------------------------------------------------------------

const testing = std.testing;

test "TargetRegistry basic operations" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const target1 = try testing.allocator.create(Target);
    target1.* = try Target.init(testing.allocator, &interner, "build", .phony);

    try registry.add(target1);

    try testing.expectEqual(@as(usize, 1), registry.count());
    try testing.expect(registry.get("build") != null);
}

test "TargetRegistry GPA no leaks" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    const allocator = gpa.allocator();

    {
        var interner = StringInterner.init(allocator);
        defer interner.deinit();

        var registry = TargetRegistry.init(allocator, &interner);
        defer registry.deinit();

        const t1 = try allocator.create(Target);
        t1.* = try Target.init(allocator, &interner, "build", .phony);
        try registry.add(t1);

        const t2 = try allocator.create(Target);
        t2.* = try Target.init(allocator, &interner, "compile", .command);
        try t2.setDepends(allocator, &interner, &[_][]const u8{"build"});
        try registry.add(t2);

        try testing.expectEqual(@as(usize, 2), registry.count());
        try testing.expect(registry.get("build") != null);
        try testing.expect(registry.get("compile") != null);
    }

    try testing.expectEqual(.ok, gpa.deinit());
}

test "TargetRegistry: getByBitIndex returns the correct target" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const t = try testing.allocator.create(Target);
    t.* = try Target.init(testing.allocator, &interner, "mytgt", .phony);
    const idx = t.bit_index;
    try registry.add(t);

    const found = registry.getByBitIndex(idx);
    try testing.expect(found != null);
    try testing.expectEqualStrings("mytgt", found.?.name);
}

test "TargetRegistry: getByBitIndex for unknown index returns null" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    try testing.expect(registry.getByBitIndex(9999) == null);
}

test "TargetRegistry: remove drops the target" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const t = try testing.allocator.create(Target);
    t.* = try Target.init(testing.allocator, &interner, "gone", .phony);
    // Save bit_index before ownership moves to registry.
    const idx = t.bit_index;
    try registry.add(t);

    try testing.expect(registry.get("gone") != null);

    // remove() only removes from the maps; it does NOT free the target.
    registry.remove("gone");

    try testing.expect(registry.get("gone") == null);
    try testing.expect(registry.getByBitIndex(idx) == null);

    // Manually free the target that was removed without being destroyed by registry.
    t.deinit(testing.allocator);
    testing.allocator.destroy(t);
}

test "TargetRegistry: listNames returns sorted names" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const tc = try testing.allocator.create(Target);
    tc.* = try Target.init(testing.allocator, &interner, "charlie", .phony);
    const ta = try testing.allocator.create(Target);
    ta.* = try Target.init(testing.allocator, &interner, "alpha", .phony);
    const tb = try testing.allocator.create(Target);
    tb.* = try Target.init(testing.allocator, &interner, "beta", .phony);

    try registry.add(tc);
    try registry.add(ta);
    try registry.add(tb);

    const names = try registry.listNames(testing.allocator);
    defer testing.allocator.free(names);

    try testing.expectEqual(@as(usize, 3), names.len);
    try testing.expectEqualStrings("alpha", names[0]);
    try testing.expectEqualStrings("beta", names[1]);
    try testing.expectEqualStrings("charlie", names[2]);
}

test "TargetRegistry: essentialTargets returns only essential ones" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const t1 = try testing.allocator.create(Target);
    t1.* = try Target.init(testing.allocator, &interner, "required", .file);
    t1.essential = true;
    try registry.add(t1);

    const t2 = try testing.allocator.create(Target);
    t2.* = try Target.init(testing.allocator, &interner, "optional", .phony);
    try registry.add(t2);

    const essentials = try registry.essentialTargets(testing.allocator);
    defer testing.allocator.free(essentials);

    try testing.expectEqual(@as(usize, 1), essentials.len);
    try testing.expectEqualStrings("required", essentials[0].name);
}

test "TargetRegistry: abstractTargets returns only abstract ones" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const ta = try testing.allocator.create(Target);
    ta.* = try Target.init(testing.allocator, &interner, "iface", .abstract);
    // No commands, no exists → isAbstract() = true
    try registry.add(ta);

    const tc = try testing.allocator.create(Target);
    tc.* = try Target.init(testing.allocator, &interner, "concrete", .command);
    try tc.commands.append(testing.allocator, try testing.allocator.dupe(u8, "echo hi"));
    try registry.add(tc);

    const abstracts = try registry.abstractTargets(testing.allocator);
    defer testing.allocator.free(abstracts);

    try testing.expectEqual(@as(usize, 1), abstracts.len);
    try testing.expectEqualStrings("iface", abstracts[0].name);
}

test "TargetRegistry: getProviders and getProvidersForName" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    // "impl_a" and "impl_b" both provide "capability".
    const cap_idx = try interner.intern("capability");

    const ia = try testing.allocator.create(Target);
    ia.* = try Target.init(testing.allocator, &interner, "impl_a", .phony);
    try ia.setProvides(testing.allocator, &interner, &[_][]const u8{"capability"});
    try registry.add(ia);

    const ib = try testing.allocator.create(Target);
    ib.* = try Target.init(testing.allocator, &interner, "impl_b", .phony);
    try ib.setProvides(testing.allocator, &interner, &[_][]const u8{"capability"});
    try registry.add(ib);

    const by_idx = registry.getProviders(cap_idx);
    try testing.expect(by_idx != null);
    try testing.expectEqual(@as(usize, 2), by_idx.?.len);

    const by_name = registry.getProvidersForName("capability");
    try testing.expect(by_name != null);
    try testing.expectEqual(@as(usize, 2), by_name.?.len);

    // Unknown capability → null.
    try testing.expect(registry.getProvidersForName("no_such_cap") == null);
}

test "TargetRegistry: get returns null for unknown name" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    try testing.expect(registry.get("nope") == null);
}

// ---------------------------------------------------------------------------
// TargetBuilder — fluent API integration tests
// ---------------------------------------------------------------------------

test "TargetBuilder: basic fluent registration" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    var b1 = registry.target("build", .file);
    try b1.command("gcc -o app main.c").essential().register();

    const t = registry.get("build");
    try testing.expect(t != null);
    try testing.expect(t.?.essential);
    try testing.expectEqual(@as(usize, 1), t.?.commands.items.len);
    try testing.expectEqualStrings("gcc -o app main.c", t.?.commands.items[0]);
}

test "TargetBuilder: depends and provides interned correctly" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    var b2 = registry.target("compile", .command);
    try b2.depends(&.{ "source.c", "header.h" }).provides(&.{"object.o"}).register();

    const t = registry.get("compile");
    try testing.expect(t != null);
    try testing.expect(t.?.depends.count() == 2);
    try testing.expect(t.?.provides.count() == 1);
}

test "TargetBuilder: feature parity with imperative style" {
    // Build the same DAG both ways and verify identical results.
    var interner_a = StringInterner.init(testing.allocator);
    defer interner_a.deinit();
    var registry_a = TargetRegistry.init(testing.allocator, &interner_a);
    defer registry_a.deinit();

    // Imperative style (existing API)
    const src_a = try testing.allocator.create(Target);
    src_a.* = try Target.init(testing.allocator, &interner_a, "src", .file);
    try registry_a.add(src_a);

    const obj_a = try testing.allocator.create(Target);
    obj_a.* = try Target.init(testing.allocator, &interner_a, "obj", .command);
    try obj_a.setDepends(testing.allocator, &interner_a, &.{"src"});
    try obj_a.setProvides(testing.allocator, &interner_a, &.{"obj.o"});
    try obj_a.addCommand(testing.allocator, try testing.allocator.dupe(u8, "cc -c src.c"));
    try registry_a.add(obj_a);

    // Fluent style (new API)
    var interner_b = StringInterner.init(testing.allocator);
    defer interner_b.deinit();
    var registry_b = TargetRegistry.init(testing.allocator, &interner_b);
    defer registry_b.deinit();

    var b_src = registry_b.target("src", .file);
    try b_src.register();
    var b_obj = registry_b.target("obj", .command);
    try b_obj.depends(&.{"src"}).provides(&.{"obj.o"}).command("cc -c src.c").register();

    // Same shape: 2 targets, "obj" has 1 dep, 1 provide, 1 command.
    try testing.expectEqual(registry_a.count(), registry_b.count());

    const obj_b = registry_b.get("obj").?;
    try testing.expectEqual(obj_a.depends.count(), obj_b.depends.count());
    try testing.expectEqual(obj_a.provides.count(), obj_b.provides.count());
    try testing.expectEqual(obj_a.commands.items.len, obj_b.commands.items.len);
    try testing.expectEqualStrings(obj_a.commands.items[0], obj_b.commands.items[0]);
}

test "TargetBuilder: multiple commands accumulate" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    var bm = registry.target("multi", .command);
    try bm.command("step1").command("step2").command("step3").register();

    const t = registry.get("multi").?;
    try testing.expectEqual(@as(usize, 3), t.commands.items.len);
    try testing.expectEqualStrings("step1", t.commands.items[0]);
    try testing.expectEqualStrings("step3", t.commands.items[2]);
}

test "TargetBuilder: id field set via fluent" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    var bn = registry.target("node", .abstract);
    try bn.id(42).register();

    try testing.expectEqual(@as(i64, 42), registry.get("node").?.id);
}

test "TargetBuilder: GPA no leaks — happy path" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var interner = StringInterner.init(allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(allocator, &interner);
    defer registry.deinit();

    var ba = registry.target("a", .phony);
    try ba.register();
    var bb = registry.target("b", .command);
    try bb.depends(&.{"a"}).command("echo b").register();
    var bc = registry.target("c", .file);
    try bc.depends(&.{"b"}).provides(&.{"artifact"}).essential().register();

    try testing.expectEqual(@as(usize, 3), registry.count());
}

test "TargetBuilder: integrates with DependencyResolver end-to-end" {
    // Register a diamond DAG with fluent API, then resolve it.
    const resolver_mod = @import("resolver.zig");
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    var r_base = registry.target("base", .phony);
    try r_base.register();
    var r_left = registry.target("left", .phony);
    try r_left.depends(&.{"base"}).register();
    var r_right = registry.target("right", .phony);
    try r_right.depends(&.{"base"}).register();
    var r_top = registry.target("top", .phony);
    try r_top.depends(&.{ "left", "right" }).register();

    var resolver = resolver_mod.DependencyResolver.init(testing.allocator, &registry, &interner);
    var resolved = try resolver.resolve(&.{"top"});
    defer resolved.deinit();

    try testing.expectEqual(@as(usize, 4), resolved.targets.len);

    // base must appear before top
    var base_pos: usize = 0;
    var top_pos: usize = 0;
    for (resolved.targets, 0..) |t, i| {
        if (std.mem.eql(u8, t.name, "base")) base_pos = i;
        if (std.mem.eql(u8, t.name, "top")) top_pos = i;
    }
    try testing.expect(base_pos < top_pos);
}













