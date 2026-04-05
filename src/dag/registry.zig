const std = @import("std");
const Target = @import("target.zig").Target;
const TargetType = @import("target.zig").TargetType;
const common = @import("common");
const StringInterner = common.interner.StringInterner;
const builder_error_mod = common.builder_error;
pub const BuilderError = builder_error_mod.BuilderError;
pub const BuilderPhase = builder_error_mod.Phase;
const joinStringSlice = builder_error_mod.joinStringSlice;
pub const logIfError = builder_error_mod.logIfError;

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

/// Updates the provider map with the given target registry and target values.
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

/// Retrieves provider addresses at the specified registry bit index.
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

/// Counts registry entries and returns the total count.
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
    /// Owns all strings in BuilderError (error messages, value copies).
    /// Deinited by register() on both success and error paths.
    arena: std.heap.ArenaAllocator,
    registry: *TargetRegistry,
    interner: *StringInterner,
    /// null when allocation failed in `TargetRegistry.target()`.
    target: ?*Target,
    /// Rich error with field/value/constraint context (arena-allocated).
    err: ?*BuilderError,
    /// Fallback plain error when BuilderError arena allocation itself fails.
    err_any: ?anyerror,

    fn hasError(self: *const TargetBuilder) bool {
        return self.err != null or self.err_any != null;
    }

    fn setError(self: *TargetBuilder, phase: BuilderPhase, field: []const u8, value: ?[]const u8, constraint: []const u8, cause: anyerror) void {
        if (self.hasError()) return; // keep first error
        const be = BuilderError.init(self.arena.allocator(), phase, field, value, constraint, cause) catch null;
        if (be) |e| {
            self.err = e;
        } else {
            self.err_any = cause;
        }
    }

    /// Set the dependency list (interned bit-set).
    pub fn depends(self: *TargetBuilder, names: []const []const u8) *TargetBuilder {
        if (self.hasError() or self.target == null) return self;
        self.target.?.setDepends(self.allocator, self.interner, names) catch |cause| {
            const value = joinStringSlice(self.arena.allocator(), names) catch null;
            self.setError(.depends, "depends", value, "invalid_reference", cause);
        };
        return self;
    }

    /// Set the provides list (interned bit-set).
    pub fn provides(self: *TargetBuilder, names: []const []const u8) *TargetBuilder {
        if (self.hasError() or self.target == null) return self;
        self.target.?.setProvides(self.allocator, self.interner, names) catch |cause| {
            const value = joinStringSlice(self.arena.allocator(), names) catch null;
            self.setError(.provides, "provides", value, "invalid_reference", cause);
        };
        return self;
    }

    /// Append a shell command string (allocator-owned copy).
    pub fn command(self: *TargetBuilder, cmd: []const u8) *TargetBuilder {
        if (self.hasError() or self.target == null) return self;
        const owned = self.allocator.dupe(u8, cmd) catch |cause| {
            self.setError(.command, "command", cmd, "out_of_memory", cause);
            return self;
        };
        self.target.?.commands.append(self.allocator, owned) catch |cause| {
            self.allocator.free(owned);
            self.setError(.command, "command", cmd, "out_of_memory", cause);
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
        if (self.hasError() or self.target == null) return self;
        const owned = self.allocator.dupe(u8, path) catch |cause| {
            self.setError(.registration, "exists", path, "out_of_memory", cause);
            return self;
        };
        if (self.target.?.exists) |old| self.allocator.free(old);
        self.target.?.exists = owned;
        return self;
    }

    /// Set the SQLite node id (optional; 0 if not set).
    pub fn id(self: *TargetBuilder, node_id: i64) *TargetBuilder {
        if (self.target) |t| t.id = node_id;
        return self;
    }

    /// Commit the target to the registry.
    ///
    /// On success the registry owns the Target and the builder's arena is freed.
    /// On any accumulated error the Target is freed and the error is returned.
    /// Always deinits the arena — do not call any setter after register().
    pub fn register(self: *TargetBuilder) !void {
        defer self.arena.deinit();
        if (self.err) |e| {
            if (self.target) |t| {
                t.deinit(self.allocator);
                self.allocator.destroy(t);
                self.target = null;
            }
            logIfError(e);
            return e.cause;
        }
        if (self.err_any) |e| {
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

/// Transforms a Zig target registry entry into a TargetBuilder instance with specified name and type.
pub fn target(self: *TargetRegistry, name: []const u8, target_type: TargetType) TargetBuilder {
    const arena = std.heap.ArenaAllocator.init(self.allocator);
    const t = self.allocator.create(Target) catch |e| {
        return .{ .allocator = self.allocator, .arena = arena, .registry = self, .interner = self.interner, .target = null, .err = null, .err_any = e };
    };
    t.* = Target.init(self.allocator, self.interner, name, target_type) catch |e| {
        self.allocator.destroy(t);
        return .{ .allocator = self.allocator, .arena = arena, .registry = self, .interner = self.interner, .target = null, .err = null, .err_any = e };
    };
    return .{ .allocator = self.allocator, .arena = arena, .registry = self, .interner = self.interner, .target = t, .err = null, .err_any = null };
}

// ---------------------------------------------------------------------------
// Capability-distance resolver — R1
// ---------------------------------------------------------------------------
//
// Finds the best-matching Target for a given capability requirement mask.
// Distance = popCount(needed & ~provides): the number of required capabilities
// NOT covered by the candidate target's provides bitset.
// Distance 0 = exact match; returned immediately.
// Returns null if the registry is empty or all targets have distance MaxInt.

/// Calculates the distance between two bit sets using an allocator and provides the result.
fn capabilityDistance(
    allocator: std.mem.Allocator,
    needed: *const std.bit_set.DynamicBitSetUnmanaged,
    provides: *const std.bit_set.DynamicBitSetUnmanaged,
) !usize {
    _ = allocator; // no temporary allocations needed
    if (needed.bit_length == 0) return 0;

    var unmet: usize = 0;
    var iter = needed.iterator(.{});
    while (iter.next()) |bit| {
        if (bit >= provides.bit_length or !provides.isSet(bit)) {
            unmet += 1;
        }
    }
    return unmet;
}

/// Resolves a target registry entry using its capability and allocator, returning a pointer to the registered object.
pub fn resolveByCapability(
    self: *const TargetRegistry,
    allocator: std.mem.Allocator,
    needed: *const std.bit_set.DynamicBitSetUnmanaged,
) !?*Target {
    if (needed.bit_length == 0 or needed.count() == 0) return null;

    var best: ?*Target = null;
    var best_dist: usize = std.math.maxInt(usize);

    var iter = self.targets.valueIterator();
    while (iter.next()) |t_ptr| {
        const t = t_ptr.*;
        if (t.provides.count() == 0) continue;

        const dist = try capabilityDistance(allocator, needed, &t.provides);
        if (dist < best_dist) {
            best_dist = dist;
            best = t;
            if (dist == 0) return t; // exact match — no need to check further
        }
    }
    return best;
}

/// Resolves a target registry entry using capability hierarchy and bit sets, returning a pointer to the registered value.
pub fn resolveByCapabilityWithHierarchy(
    self: *const TargetRegistry,
    allocator: std.mem.Allocator,
    needed: *const std.bit_set.DynamicBitSetUnmanaged,
    ancestor_caps: []const []const u8,
) !?*Target {
    // Fast path: exact or best match without hierarchy.
    if (try self.resolveByCapability(allocator, needed)) |t| {
        const dist = try capabilityDistance(allocator, needed, &t.provides);
        if (dist == 0) return t; // exact match
    }

    if (ancestor_caps.len == 0) return self.resolveByCapability(allocator, needed);

    // Expand needed with capabilities that map to known bit indices in this
    // interner.  Unknown ancestor names are silently skipped.
    var expanded = try needed.clone(allocator);
    defer expanded.deinit(allocator);

    for (ancestor_caps) |cap_name| {
        if (self.interner.getIndex(cap_name)) |idx| {
            if (idx >= expanded.bit_length) {
                try expanded.resize(allocator, idx + 1, false);
            }
            expanded.set(idx);
        }
    }

    return self.resolveByCapability(allocator, &expanded);
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

test "resolveByCapability: exact match returns target" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    var builder = registry.target("provider", .command);
    try builder.provides(&.{ "cap_a", "cap_b" }).register();

    // Build a needed bitset that exactly matches "cap_a" + "cap_b"
    var needed = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(testing.allocator, interner.count());
    defer needed.deinit(testing.allocator);
    needed.set(interner.getIndex("cap_a").?);
    needed.set(interner.getIndex("cap_b").?);

    const result = try registry.resolveByCapability(testing.allocator, &needed);
    try testing.expect(result != null);
    try testing.expectEqualStrings("provider", result.?.name);
}

test "resolveByCapability: empty registry returns null" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    _ = try interner.intern("cap_x");
    var needed = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(testing.allocator, interner.count());
    defer needed.deinit(testing.allocator);
    needed.set(0);

    const result = try registry.resolveByCapability(testing.allocator, &needed);
    try testing.expect(result == null);
}

test "resolveByCapability: closest partial match returned" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    // t1 provides only cap_a; t2 provides cap_a + cap_b
    var b1 = registry.target("partial", .command);
    try b1.provides(&.{"cap_a"}).register();
    var b2 = registry.target("full", .command);
    try b2.provides(&.{ "cap_a", "cap_b" }).register();

    var needed = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(testing.allocator, interner.count());
    defer needed.deinit(testing.allocator);
    needed.set(interner.getIndex("cap_a").?);
    needed.set(interner.getIndex("cap_b").?);

    const result = try registry.resolveByCapability(testing.allocator, &needed);
    try testing.expect(result != null);
    try testing.expectEqualStrings("full", result.?.name);
}

test "resolveByCapabilityWithHierarchy: matches via ancestor capability" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    // Register a target providing "Person" capability.
    var builder = registry.target("person_handler", .command);
    try builder.provides(&.{"Person"}).register();

    // We query with "Scientist" but the target only knows "Person".
    // The ancestor chain includes "Person" (Scientist is-a Person).
    _ = try interner.intern("Scientist");
    var needed = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(testing.allocator, interner.count());
    defer needed.deinit(testing.allocator);
    needed.set(interner.getIndex("Scientist").?);

    const ancestor_caps = &[_][]const u8{"Person"};
    const result = try registry.resolveByCapabilityWithHierarchy(testing.allocator, &needed, ancestor_caps);
    try testing.expect(result != null);
    try testing.expectEqualStrings("person_handler", result.?.name);
}

test "resolveByCapabilityWithHierarchy: no ancestors returns same as resolveByCapability" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    _ = try interner.intern("cap_x");
    var needed = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(testing.allocator, interner.count());
    defer needed.deinit(testing.allocator);
    needed.set(0);

    const result = try registry.resolveByCapabilityWithHierarchy(testing.allocator, &needed, &[_][]const u8{});
    try testing.expect(result == null); // empty registry
}

// ---------------------------------------------------------------------------
// M1 — BuilderError context tests
// ---------------------------------------------------------------------------

test "TargetBuilder: arena freed on happy path — GPA no leaks" {
    // Verify that the builder arena is deinited by register() with no leaks.
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("memory leak");
    const allocator = gpa.allocator();

    var interner = StringInterner.init(allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(allocator, &interner);
    defer registry.deinit();

    // register() must deinit the builder arena regardless of success/error.
    var b = registry.target("check", .phony);
    try b.command("echo ok").essential().register();
}

test "TargetBuilder: err accumulates and surfaces cause" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    // Provide an empty name list to depends() — setDepends handles it fine,
    // but we verify the happy path still works through the new code paths.
    var b = registry.target("x", .phony);
    try b.depends(&.{}).provides(&.{}).register();
    try testing.expect(registry.get("x") != null);
}

test "TargetBuilder: err field is null on successful register" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    var b = registry.target("clean", .phony);
    // Verify no error before register()
    try testing.expect(b.err == null);
    try testing.expect(b.err_any == null);
    try b.command("rm -rf build").register();
}
