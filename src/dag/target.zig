const std = @import("std");
const common = @import("common");
const StringInterner = common.interner.StringInterner;
const reflection = @import("reflection");

/// Defines a fixed-size buffer pool with ownership and lifecycle management; ensures safe allocation/deallocation.
pub const TargetType = enum {
    abstract,
    file,
    command,
    phony,
};

// ---------------------------------------------------------------------------
// ExecutorKind — discriminated union replacing the separate WasmTarget type
// ---------------------------------------------------------------------------

/// How a Target is executed.  `native` targets run as host OS processes via
/// their `commands` list.  `wasm` targets are sandboxed via the Extism runtime.
/// Having the discriminant here means the registry and resolver never need to
/// know which executor is in use — they only see `Target`.
pub const ExecutorKind = union(enum) {
    /// Standard OS process execution: commands are shell strings.
    native,
    /// WASM sandbox execution via Extism.
    wasm: WasmExecutor,
};

/// Manages Wasm execution context, owns runtime state; ensures safe initialization and cleanup.
pub const WasmExecutor = struct {
    /// Raw WASM binary (allocator-owned).
    wasm_bytes: []const u8,
    /// Name of the exported WASM function to call (null-terminated, allocator-owned).
    entry_point: [:0]const u8,

    pub fn deinit(self: *WasmExecutor, allocator: std.mem.Allocator) void {
        allocator.free(self.wasm_bytes);
        allocator.free(self.entry_point);
    }
};

pub const Target = @This();

id: i64 = 0,
bit_index: usize,
name: []const u8,
target_type: TargetType,
essential: bool = false,
executor: ExecutorKind = .native,

depends: std.bit_set.DynamicBitSetUnmanaged,
provides: std.bit_set.DynamicBitSetUnmanaged,

exists: ?[]const u8 = null,
check_mtime: bool = false,
commands: std.ArrayListUnmanaged([]const u8),

/// Initializes a Zig allocation with provided allocator and interners, returning a target slice.
pub fn init(
    allocator: std.mem.Allocator,
    interner: *StringInterner,
    name: []const u8,
    target_type: TargetType,
) !Target {
    const bit_index = try interner.intern(name);
    const total_bits = interner.count();
    // Always dupe the name so deinit can always free it
    const owned_name = try allocator.dupe(u8, name);
    errdefer allocator.free(owned_name);

    return .{
        .bit_index = bit_index,
        .name = owned_name,
        .target_type = target_type,
        .depends = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(allocator, total_bits),
        .provides = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(allocator, total_bits),
        .commands = .{},
    };
}

/// Releases allocated memory by deallocating the Zig object.
pub fn deinit(self: *Target, allocator: std.mem.Allocator) void {
    self.depends.deinit(allocator);
    self.provides.deinit(allocator);
    for (self.commands.items) |cmd| {
        allocator.free(cmd);
    }
    self.commands.deinit(allocator);
    allocator.free(self.name);
    if (self.exists) |e| {
        allocator.free(e);
    }
    switch (self.executor) {
        .native => {},
        .wasm => |*w| w.deinit(allocator),
    }
}

/// Updates dependencies with allocator and interners, ensuring proper memory management.
pub fn setDepends(
    self: *Target,
    allocator: std.mem.Allocator,
    interner: *StringInterner,
    depends: []const []const u8,
) !void {
    self.depends.deinit(allocator);
    self.depends = try interner.internAndGetBitSet(allocator, depends);
}

/// Initializes a Zig array with provided allocator and string interners.
pub fn setProvides(
    self: *Target,
    allocator: std.mem.Allocator,
    interner: *StringInterner,
    provides: []const []const u8,
) !void {
    self.provides.deinit(allocator);
    self.provides = try interner.internAndGetBitSet(allocator, provides);
}

/// Adds a command slice to the target with specified allocator and parameters.
pub fn addCommand(self: *Target, allocator: std.mem.Allocator, cmd: []const u8) !void {
    try self.commands.append(allocator, cmd);
}

/// Checks if a target is abstract by verifying its structure and return a boolean result.
pub fn isAbstract(self: *const Target) bool {
    return self.target_type == .abstract and
        self.commands.items.len == 0 and
        self.exists == null;
}

/// Checks if a dependency condition is met using allocator and bit set data.
pub fn dependsSatisfiedBy(self: *const Target, allocator: std.mem.Allocator, available: *const std.bit_set.DynamicBitSetUnmanaged) bool {
    if (self.depends.count() == 0) return true;
    var missing = self.depends.clone(allocator) catch return false;
    defer missing.deinit(allocator);
    var complement = available.clone(allocator) catch return false;
    defer complement.deinit(allocator);
    complement.toggleAll();
    missing.setIntersection(complement);
    return missing.count() == 0;
}

/// Validates dependencies for missing data structures; returns a bit set if all checks pass.
pub fn missingDepends(
    self: *const Target,
    allocator: std.mem.Allocator,
    available: *const std.bit_set.DynamicBitSetUnmanaged,
) !std.bit_set.DynamicBitSetUnmanaged {
    var missing = try self.depends.clone(allocator);
    errdefer missing.deinit(allocator);

    var complement = try available.clone(allocator);
    defer complement.deinit(allocator);

    complement.toggleAll();
    missing.setIntersection(complement);

    return missing;
}

/// Calculates the distance between two targets using bit set operations.
pub fn distanceFrom(self: *const Target, available: *const std.bit_set.DynamicBitSetUnmanaged) usize {
    var dist: usize = 0;
    var iter = self.depends.iterator(.{});
    while (iter.next()) |idx| {
        if (idx >= available.bit_length or !available.isSet(idx)) {
            dist += 1;
        }
    }
    return dist;
}

/// Formats a target string using specified format options and writer.
pub fn format(
    self: *const Target,
    comptime fmt: []const u8,
    options: std.fmt.FormatOptions,
    writer: anytype,
) !void {
    _ = fmt;
    _ = options;
    try writer.print("{s} [{s}]", .{ self.name, @tagName(self.target_type) });
    if (self.essential) {
        try writer.writeAll(" [essential]");
    }
}

// ---------------------------------------------------------------------------
// TargetSchema — reflection schema for Target, parameterised on a StringInterner
// ---------------------------------------------------------------------------
//
// TargetSchema is constructed once per interner and vends DynamicEditable views
// over any Target instance.  This keeps the hot DAG path free of reflection
// overhead while giving TUI editors, SQLite hydration, and WASM IPC a single
// composable path to read/write Target fields — including the DynamicBitSet
// `depends` and `provides` fields — via the BitSetConstraint vtable.
//
// Usage:
//   var schema = TargetSchema.init(&interner);
//   // ...populate schema.accessors (done by init) ...
//   var view = try schema.viewOf(allocator, &my_target);
//   defer view.deinit();
//   try view.set("depends", "compile,link", .coder);
//   const s = try view.get("provides", .coder);
//   defer allocator.free(s);

pub const TargetSchema = struct {
    const Self = @This();
    const ACCESSOR_COUNT = 5;

    // Runtime vtables for bitset fields — stored here so they are stable for
    // the lifetime of the schema.
    depends_vtable: reflection.ConstraintVTable,
    provides_vtable: reflection.ConstraintVTable,

    // Accessor table — built once in `create`, referencing the vtables above.
    accessors: [ACCESSOR_COUNT]reflection.Accessor,

    // Static vtables for scalar fields — comptime constants, always stable.
    const id_vtable = reflection.Constraint(i64);
    const essential_vtable = reflection.Constraint(bool);
    const name_vtable = reflection.Constraint([]const u8);

    /// Allocate and initialise a TargetSchema for the given interner.
    /// Free with `destroy` when done.
    pub fn create(allocator: std.mem.Allocator, interner: *StringInterner) !*Self {
        const self = try allocator.create(Self);
        self.depends_vtable = common.interner.bitSetConstraint(interner);
        self.provides_vtable = common.interner.bitSetConstraint(interner);
        // Build accessors now — pointers into self.depends_vtable /
        // self.provides_vtable are stable because self is heap-allocated.
        self.accessors[0] = .{
            .name = "id",
            .offset = @offsetOf(Target, "id"),
            .permissions = reflection.perm_coder,
            .constraint = &id_vtable,
            .type_tag = .int,
            .ownership = .value,
            .binary_size = @sizeOf(i64),
        };
        self.accessors[1] = .{
            .name = "name",
            .offset = @offsetOf(Target, "name"),
            .permissions = reflection.perm_staff,
            .constraint = &name_vtable,
            .type_tag = .string_owned,
            .ownership = .owned,
            .binary_size = 0,
        };
        self.accessors[2] = .{
            .name = "essential",
            .offset = @offsetOf(Target, "essential"),
            .permissions = reflection.perm_staff,
            .constraint = &essential_vtable,
            .type_tag = .bool,
            .ownership = .value,
            .binary_size = 1,
        };
        self.accessors[3] = .{
            .name = "depends",
            .offset = @offsetOf(Target, "depends"),
            .permissions = reflection.perm_staff,
            .constraint = &self.depends_vtable,
            .type_tag = .bitset,
            .ownership = .owned,
            .binary_size = 0,
        };
        self.accessors[4] = .{
            .name = "provides",
            .offset = @offsetOf(Target, "provides"),
            .permissions = reflection.perm_staff,
            .constraint = &self.provides_vtable,
            .type_tag = .bitset,
            .ownership = .owned,
            .binary_size = 0,
        };
        return self;
    }

    pub fn destroy(self: *Self, allocator: std.mem.Allocator) void {
        allocator.destroy(self);
    }

    /// Return a DynamicEditable view over `target`.
    /// The view borrows `target`'s memory and `self`'s accessor table —
    /// both must outlive the returned view.  Call `view.deinit()` when done.
    pub fn viewOf(
        self: *const Self,
        allocator: std.mem.Allocator,
        target: *Target,
    ) !reflection.DynamicEditable {
        const target_bytes: [*]u8 = @ptrCast(target);
        const buf = target_bytes[0..@sizeOf(Target)];
        return reflection.DynamicEditable.init(allocator, buf, &self.accessors);
    }
};

const testing = std.testing;

test "Target creation and basic operations" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    var target = try Target.init(testing.allocator, &interner, "build", .file);
    defer target.deinit(testing.allocator);

    try testing.expectEqualStrings("build", target.name);
    try testing.expectEqual(TargetType.file, target.target_type);
    try testing.expect(!target.isAbstract());
}

test "Target depends and provides" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    var target = try Target.init(testing.allocator, &interner, "compile", .command);
    defer target.deinit(testing.allocator);

    try target.setDepends(testing.allocator, &interner, &[_][]const u8{ "source.c", "header.h" });
    try target.setProvides(testing.allocator, &interner, &[_][]const u8{"object.o"});

    try testing.expect(target.depends.isSet(1));
    try testing.expect(target.depends.isSet(2));
    try testing.expect(target.provides.isSet(3));
}

test "Target GPA no leaks" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    const allocator = gpa.allocator();

    {
        var interner = StringInterner.init(allocator);
        defer interner.deinit();

        var target = try Target.init(allocator, &interner, "link", .command);
        defer target.deinit(allocator);

        try target.setDepends(allocator, &interner, &[_][]const u8{ "a.o", "b.o" });
        try target.setProvides(allocator, &interner, &[_][]const u8{"app"});
        try target.commands.append(allocator, try allocator.dupe(u8, "ld -o app a.o b.o"));

        try testing.expectEqualStrings("link", target.name);
        try testing.expect(!target.isAbstract());
    }

    try testing.expectEqual(.ok, gpa.deinit());
}

// ---------------------------------------------------------------------------
// dependsSatisfiedBy
// ---------------------------------------------------------------------------

test "Target: dependsSatisfiedBy — no deps always satisfied" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    var target = try Target.init(testing.allocator, &interner, "nodeps", .phony);
    defer target.deinit(testing.allocator);

    var available = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(testing.allocator, 4);
    defer available.deinit(testing.allocator);

    try testing.expect(target.dependsSatisfiedBy(testing.allocator, &available));
}

test "Target: dependsSatisfiedBy — satisfied when all deps available" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    _ = try interner.intern("x"); // bit 0
    _ = try interner.intern("y"); // bit 1

    var target = try Target.init(testing.allocator, &interner, "t", .phony);
    defer target.deinit(testing.allocator);
    try target.setDepends(testing.allocator, &interner, &[_][]const u8{ "x", "y" });

    var available = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(testing.allocator, interner.count());
    defer available.deinit(testing.allocator);
    available.set(0); // x
    available.set(1); // y

    try testing.expect(target.dependsSatisfiedBy(testing.allocator, &available));
}

test "Target: dependsSatisfiedBy — not satisfied when dep missing" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    _ = try interner.intern("p"); // bit 0
    _ = try interner.intern("q"); // bit 1

    var target = try Target.init(testing.allocator, &interner, "t2", .phony);
    defer target.deinit(testing.allocator);
    try target.setDepends(testing.allocator, &interner, &[_][]const u8{ "p", "q" });

    // Only "p" is available — "q" is missing.
    var available = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(testing.allocator, interner.count());
    defer available.deinit(testing.allocator);
    available.set(0); // p only

    try testing.expect(!target.dependsSatisfiedBy(testing.allocator, &available));
}

// ---------------------------------------------------------------------------
// missingDepends
// ---------------------------------------------------------------------------

test "Target: missingDepends — empty when all satisfied" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    _ = try interner.intern("r"); // bit 0

    var target = try Target.init(testing.allocator, &interner, "td", .phony);
    defer target.deinit(testing.allocator);
    try target.setDepends(testing.allocator, &interner, &[_][]const u8{"r"});

    var available = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(testing.allocator, interner.count());
    defer available.deinit(testing.allocator);
    available.set(0);

    var missing = try target.missingDepends(testing.allocator, &available);
    defer missing.deinit(testing.allocator);

    try testing.expectEqual(@as(usize, 0), missing.count());
}

test "Target: missingDepends — returns unsatisfied bits" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    _ = try interner.intern("s"); // bit 0
    _ = try interner.intern("u"); // bit 1

    var target = try Target.init(testing.allocator, &interner, "te", .phony);
    defer target.deinit(testing.allocator);
    try target.setDepends(testing.allocator, &interner, &[_][]const u8{ "s", "u" });

    // Nothing available.
    var available = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(testing.allocator, interner.count());
    defer available.deinit(testing.allocator);

    var missing = try target.missingDepends(testing.allocator, &available);
    defer missing.deinit(testing.allocator);

    try testing.expectEqual(@as(usize, 2), missing.count());
}

// ---------------------------------------------------------------------------
// distanceFrom
// ---------------------------------------------------------------------------

test "Target: distanceFrom — zero when no deps" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    var target = try Target.init(testing.allocator, &interner, "solo", .phony);
    defer target.deinit(testing.allocator);

    var available = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(testing.allocator, 4);
    defer available.deinit(testing.allocator);

    try testing.expectEqual(@as(usize, 0), target.distanceFrom(&available));
}

test "Target: distanceFrom — counts missing deps" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    _ = try interner.intern("d1"); // bit 0
    _ = try interner.intern("d2"); // bit 1
    _ = try interner.intern("d3"); // bit 2

    var target = try Target.init(testing.allocator, &interner, "tf", .phony);
    defer target.deinit(testing.allocator);
    try target.setDepends(testing.allocator, &interner, &[_][]const u8{ "d1", "d2", "d3" });

    // Only d1 is available → distance = 2.
    var available = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(testing.allocator, interner.count());
    defer available.deinit(testing.allocator);
    available.set(0);

    try testing.expectEqual(@as(usize, 2), target.distanceFrom(&available));
}

test "Target: distanceFrom — zero when all deps satisfied" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    _ = try interner.intern("e1"); // bit 0
    _ = try interner.intern("e2"); // bit 1

    var target = try Target.init(testing.allocator, &interner, "tg", .phony);
    defer target.deinit(testing.allocator);
    try target.setDepends(testing.allocator, &interner, &[_][]const u8{ "e1", "e2" });

    var available = try std.bit_set.DynamicBitSetUnmanaged.initEmpty(testing.allocator, interner.count());
    defer available.deinit(testing.allocator);
    available.set(0);
    available.set(1);

    try testing.expectEqual(@as(usize, 0), target.distanceFrom(&available));
}

// ---------------------------------------------------------------------------
// isAbstract edge cases
// ---------------------------------------------------------------------------

test "Target: abstract type with commands is NOT abstract" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    var target = try Target.init(testing.allocator, &interner, "hybrid", .abstract);
    defer target.deinit(testing.allocator);
    try target.commands.append(testing.allocator, try testing.allocator.dupe(u8, "echo"));

    try testing.expect(!target.isAbstract());
}

test "Target: abstract type with exists is NOT abstract" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    var target = try Target.init(testing.allocator, &interner, "ghost", .abstract);
    defer target.deinit(testing.allocator);
    target.exists = try testing.allocator.dupe(u8, "somefile");

    try testing.expect(!target.isAbstract());
}

test "Target: phony type is never abstract regardless" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    var target = try Target.init(testing.allocator, &interner, "ph", .phony);
    defer target.deinit(testing.allocator);

    try testing.expect(!target.isAbstract());
}

// ---------------------------------------------------------------------------
// format
// ---------------------------------------------------------------------------

test "Target: format includes name and type tag" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    var target = try Target.init(testing.allocator, &interner, "myname", .command);
    defer target.deinit(testing.allocator);

    var buf: [128]u8 = undefined;
    var fbs = std.io.fixedBufferStream(&buf);
    try target.format("", .{}, fbs.writer());
    const written = fbs.getWritten();

    try testing.expect(std.mem.indexOf(u8, written, "myname") != null);
    try testing.expect(std.mem.indexOf(u8, written, "command") != null);
}

test "Target: format includes [essential] for essential targets" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    var target = try Target.init(testing.allocator, &interner, "must", .file);
    defer target.deinit(testing.allocator);
    target.essential = true;

    var buf: [128]u8 = undefined;
    var fbs = std.io.fixedBufferStream(&buf);
    try target.format("", .{}, fbs.writer());
    const written = fbs.getWritten();

    try testing.expect(std.mem.indexOf(u8, written, "essential") != null);
}

// ---------------------------------------------------------------------------
// ExecutorKind
// ---------------------------------------------------------------------------

test "Target: executor defaults to native" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    var target = try Target.init(testing.allocator, &interner, "t", .command);
    defer target.deinit(testing.allocator);

    try testing.expectEqual(ExecutorKind.native, target.executor);
}

test "Target: id is i64" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    var target = try Target.init(testing.allocator, &interner, "t", .command);
    defer target.deinit(testing.allocator);
    target.id = -1;
    try testing.expectEqual(@as(i64, -1), target.id);
}

// ---------------------------------------------------------------------------
// TargetSchema
// ---------------------------------------------------------------------------

test "TargetSchema: viewOf set/get essential via reflection" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var interner = StringInterner.init(allocator);
    defer interner.deinit();

    var target = try Target.init(allocator, &interner, "build", .file);
    defer target.deinit(allocator);

    const schema = try TargetSchema.create(allocator, &interner);
    defer schema.destroy(allocator);
    var view = try schema.viewOf(allocator, &target);
    defer view.deinit();

    try view.set("essential", "true", .coder);
    try testing.expect(target.essential);

    const val = try view.get("essential", .coder);
    defer allocator.free(val);
    try testing.expectEqualStrings("true", val);
}

test "TargetSchema: viewOf set/get depends via BitSetConstraint" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var interner = StringInterner.init(allocator);
    defer interner.deinit();

    // Pre-intern some capability names so bit indices are stable.
    _ = try interner.intern("compile");
    _ = try interner.intern("link");
    _ = try interner.intern("test");

    var target = try Target.init(allocator, &interner, "build", .file);
    defer target.deinit(allocator);

    const schema = try TargetSchema.create(allocator, &interner);
    defer schema.destroy(allocator);
    var view = try schema.viewOf(allocator, &target);
    defer view.deinit();

    // Set depends to "compile,link" via the string path.
    try view.set("depends", "compile,link", .coder);

    const compile_bit = interner.getIndex("compile").?;
    const link_bit = interner.getIndex("link").?;
    const test_bit = interner.getIndex("test").?;

    try testing.expect(target.depends.isSet(compile_bit));
    try testing.expect(target.depends.isSet(link_bit));
    try testing.expect(!target.depends.isSet(test_bit));

    // Round-trip via get — order may vary; check both names are present.
    const got = try view.get("depends", .coder);
    defer allocator.free(got);
    try testing.expect(std.mem.indexOf(u8, got, "compile") != null);
    try testing.expect(std.mem.indexOf(u8, got, "link") != null);
}

test "TargetSchema: GPA no leaks" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var interner = StringInterner.init(allocator);
    defer interner.deinit();

    var target = try Target.init(allocator, &interner, "t", .phony);
    defer target.deinit(allocator);

    const schema = try TargetSchema.create(allocator, &interner);
    defer schema.destroy(allocator);
    var view = try schema.viewOf(allocator, &target);
    defer view.deinit();

    try view.set("depends", "a,b,c", .coder);
    try view.set("provides", "d", .coder);

    const d = try view.get("depends", .coder);
    allocator.free(d);
    const p = try view.get("provides", .coder);
    allocator.free(p);
}
