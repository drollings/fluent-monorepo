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

pub fn init(allocator: std.mem.Allocator, interner: *StringInterner) TargetRegistry {
    return .{
        .allocator = allocator,
        .interner = interner,
        .targets = .{},
        .by_bit_index = .{},
        .provider_map = .{},
    };
}

pub fn deinit(self: *TargetRegistry) void {
    var iter = self.targets.valueIterator();
    while (iter.next()) |target| {
        target.*.*.deinit(self.allocator);
        self.allocator.destroy(target.*);
    }
    self.targets.deinit(self.allocator);
    self.by_bit_index.deinit(self.allocator);

    var prov_iter = self.provider_map.valueIterator();
    while (prov_iter.next()) |list| {
        list.deinit(self.allocator);
    }
    self.provider_map.deinit(self.allocator);
}

pub fn add(self: *TargetRegistry, target: *Target) !void {
    try self.targets.put(self.allocator, target.name, target);
    try self.by_bit_index.put(self.allocator, target.bit_index, target);
    self.updateProviderMap(target);
}

pub fn get(self: *const TargetRegistry, name: []const u8) ?*Target {
    return self.targets.get(name);
}

pub fn getByBitIndex(self: *const TargetRegistry, idx: usize) ?*Target {
    return self.by_bit_index.get(idx);
}

pub fn remove(self: *TargetRegistry, name: []const u8) void {
    if (self.targets.get(name)) |target| {
        _ = self.by_bit_index.remove(target.bit_index);
        _ = self.targets.remove(name);
    }
}

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

fn updateProviderMap(self: *TargetRegistry, target: *Target) void {
    var iter = target.provides.iterator(.{});
    while (iter.next()) |provides_idx| {
        const gop = self.provider_map.getOrPut(self.allocator, provides_idx) catch return;
        if (!gop.found_existing) {
            gop.value_ptr.* = .{};
        }
        gop.value_ptr.append(self.allocator, target) catch return;
    }
}

pub fn getProviders(self: *const TargetRegistry, bit_index: usize) ?[]*Target {
    if (self.provider_map.get(bit_index)) |list| {
        return list.items;
    }
    return null;
}

pub fn getProvidersForName(self: *const TargetRegistry, name: []const u8) ?[]*Target {
    const idx = self.interner.getIndex(name) orelse return null;
    return self.getProviders(idx);
}

pub fn count(self: *const TargetRegistry) usize {
    return self.targets.count();
}

pub fn essentialTargets(self: *const TargetRegistry, allocator: std.mem.Allocator) ![]*Target {
    var essentials: std.ArrayList(*Target) = .{};
    errdefer essentials.deinit(allocator);

    var iter = self.targets.valueIterator();
    while (iter.next()) |target| {
        if (target.*.*.essential) {
            try essentials.append(allocator, target.*);
        }
    }

    return essentials.toOwnedSlice(allocator);
}

pub fn abstractTargets(self: *const TargetRegistry, allocator: std.mem.Allocator) ![]*Target {
    var abstracts: std.ArrayList(*Target) = .{};
    errdefer abstracts.deinit(allocator);

    var iter = self.targets.valueIterator();
    while (iter.next()) |target| {
        if (target.*.*.isAbstract()) {
            try abstracts.append(allocator, target.*);
        }
    }

    return abstracts.toOwnedSlice(allocator);
}

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
