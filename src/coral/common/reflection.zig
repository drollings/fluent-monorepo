/// reflection.zig — Coral Context field-level reflection, validation, and permission layer.
///
/// Design intent:
///   This module is the "source of truth" for data input, human input, serialization,
///   and database round-trips.  It is NOT intended for hot inner-loop access — use
///   direct field access or setFast/getFast for that.  The reflective set/get path is
///   for TUI editors, RPC handlers, CozoDB row hydration, and configuration loading.
///
/// Usage at a glance:
///   Static struct:
///     const MyStruct = struct {
///         port: u16 = 8080,
///         editable: Editable(MyStruct) = .{},
///     };
///     var s: MyStruct = .{};
///     try s.editable.set(allocator, "port", "9000", .player);   // validated, role-checked
///     s.editable.setFast("port", 9000);                         // zero-cost, trusted code
///
///   Dynamic (CozoDB row):
///     var dyn = try DynamicEditable.init(allocator, buffer, accessors);
///     defer dyn.deinit();
///     try dyn.set("mass", "3.14", .staff);
///
/// Ownership:
///   get() / DynamicEditable.get() return allocator-owned slices — caller must free.
///   set() on a string field allocates a copy — the struct owns that slice and must free it
///   on teardown.  There is no implicit destructor; provide a deinit() on your Host struct.
///
/// Error model:
///   Permission errors:    error.FieldNotFound, error.AccessDenied  (always from this module)
///   Constraint errors:    propagated as-is from the parser (e.g. std.fmt.ParseIntError,
///                         error.InvalidBool, error.InvalidEnum, error.ArrayTooLong …)
///   Allocation errors:    error.OutOfMemory  (from the supplied allocator)
///
const std = @import("std");

// ============================================================
// § 1  Role-based permission system
// ============================================================

/// The six access roles mirroring the original worldcore C++ permission model.
pub const Role = enum(u3) {
    coder,
    creator,
    staff,
    world,
    script,
    player,
};

/// 18-bit packed permission word: 6 roles × 3 operations.
/// Fits in a u32 (with padding), costs a single bitwise AND to query.
pub const RolePermissions = packed struct(u18) {
    coder_read: bool = false,
    creator_read: bool = false,
    staff_read: bool = false,
    world_read: bool = false,
    script_read: bool = false,
    player_read: bool = false,

    coder_write: bool = false,
    creator_write: bool = false,
    staff_write: bool = false,
    world_write: bool = false,
    script_write: bool = false,
    player_write: bool = false,

    coder_derive: bool = false,
    creator_derive: bool = false,
    staff_derive: bool = false,
    world_derive: bool = false,
    script_derive: bool = false,
    player_derive: bool = false,

    pub fn canRead(self: RolePermissions, role: Role) bool {
        return switch (role) {
            .coder => self.coder_read,
            .creator => self.creator_read,
            .staff => self.staff_read,
            .world => self.world_read,
            .script => self.script_read,
            .player => self.player_read,
        };
    }

    pub fn canWrite(self: RolePermissions, role: Role) bool {
        return switch (role) {
            .coder => self.coder_write,
            .creator => self.creator_write,
            .staff => self.staff_write,
            .world => self.world_write,
            .script => self.script_write,
            .player => self.player_write,
        };
    }

    pub fn canDerive(self: RolePermissions, role: Role) bool {
        return switch (role) {
            .coder => self.coder_derive,
            .creator => self.creator_derive,
            .staff => self.staff_derive,
            .world => self.world_derive,
            .script => self.script_derive,
            .player => self.player_derive,
        };
    }
};

/// All roles may read and write.  Suitable as a default for open/config data.
pub const perm_all: RolePermissions = .{
    .coder_read = true,
    .creator_read = true,
    .staff_read = true,
    .world_read = true,
    .script_read = true,
    .player_read = true,
    .coder_write = true,
    .creator_write = true,
    .staff_write = true,
    .world_write = true,
    .script_write = true,
    .player_write = true,
    .coder_derive = true,
    .creator_derive = true,
    .staff_derive = true,
    .world_derive = true,
    .script_derive = true,
    .player_derive = true,
};

/// Only the coder role has full access.  Use for engine-internal fields.
pub const perm_coder: RolePermissions = .{
    .coder_read = true,
    .coder_write = true,
    .coder_derive = true,
};

/// Coders and creators have full access; staff can read and write.
pub const perm_staff: RolePermissions = .{
    .coder_read = true,
    .coder_write = true,
    .coder_derive = true,
    .creator_read = true,
    .creator_write = true,
    .creator_derive = true,
    .staff_read = true,
    .staff_write = true,
};

/// All roles may read; only coders and creators may write.
pub const perm_public_read: RolePermissions = .{
    .coder_read = true,
    .creator_read = true,
    .staff_read = true,
    .world_read = true,
    .script_read = true,
    .player_read = true,
    .coder_write = true,
    .creator_write = true,
};

// ============================================================
// § 2  Constraint VTable
// ============================================================

// ============================================================
// § 1b  Type metadata enums
// ============================================================

/// Broad type category attached to each Accessor.
/// Used by binary IPC and the WASM schema descriptor to encode field types
/// without requiring comptime dispatch at the call site.
pub const TypeTag = enum(u8) {
    int,
    float,
    bool,
    string_owned, // []const u8 — allocator-owned copy
    string_borrowed, // []const u8 — borrowed, never freed by the reflection layer
    string_rc, // []const u8 — reference-counted; releaseFn decrements refcount
    @"enum",
    optional,
    array,
    vector,
    bitset, // DynamicBitSetUnmanaged — parameterised by a StringInterner
    collection, // ArrayListUnmanaged or similar — held by reference
    unknown,
};

/// Who owns the memory behind a field value.
pub const OwnershipMode = enum(u8) {
    /// The struct field holds a plain value; no heap allocation involved.
    value,
    /// The field is an allocator-owned heap allocation; the reflection layer
    /// will call `releaseFn` when replacing or releasing the field.
    owned,
    /// The field is a borrowed pointer; the reflection layer never frees it.
    borrowed,
    /// The field is reference-counted; the reflection layer calls `releaseFn`
    /// (which decrements the refcount) when replacing or releasing.
    rc,
};

// ============================================================
// § 2  Constraint VTable
// ============================================================

/// Type-erased vtable for parsing a string into a field value and serialising
/// a field value back to a string.  One static instance exists per type T —
/// the Accessor only holds a pointer to it, so per-field cost is one pointer.
///
/// Extended fields (context, releaseFn, convertFn, setBinaryFn, getBinaryFn)
/// are optional — null means "not applicable for this type".  The original
/// setFn / getFn always operate on UTF-8 string representations.
pub const ConstraintVTable = struct {
    /// Parse `input` and write the result to the field at `ptr`.
    /// An allocator is required for string/slice fields to avoid dangling pointers.
    setFn: *const fn (allocator: std.mem.Allocator, ptr: *anyopaque, input: []const u8) anyerror!void,

    /// Serialise the value at `ptr` to a caller-owned string.
    /// The caller must free the returned slice with the same allocator.
    getFn: *const fn (allocator: std.mem.Allocator, ptr: *const anyopaque) anyerror![]const u8,

    // ── Extended / optional vtable entries ───────────────────────────────

    /// Opaque context pointer for parameterised vtables (e.g. BitSetConstraint
    /// stores a `*StringInterner` here).  Null for stateless constraints.
    context: ?*const anyopaque = null,

    /// Release any heap memory owned by the field at `ptr`.
    /// Called by DynamicEditable.releaseAll() and before overwriting an owned
    /// field via set().  Null means the field owns no heap memory.
    releaseFn: ?*const fn (allocator: std.mem.Allocator, ptr: *anyopaque) void = null,

    /// Convert the bitset at `src_ptr` (encoded relative to `src_context`) into
    /// the bitset at `dst_ptr` (encoded relative to `self.context`), by matching
    /// capability names across two different StringInterners.
    /// Used for DAG duck-typing across module boundaries.
    /// Null for non-bitset constraints.
    convertFn: ?*const fn (
        self: *const ConstraintVTable,
        allocator: std.mem.Allocator,
        dst_ptr: *anyopaque,
        src_ptr: *const anyopaque,
        src_context: *const anyopaque,
    ) anyerror!void = null,

    /// Context-aware string setter — receives `self` so runtime-parameterised
    /// constraints (e.g. BitSetConstraint) can reach their stored context pointer.
    /// When non-null, DynamicEditable and Editable prefer this over setFn.
    setCtxFn: ?*const fn (
        vtable: *const ConstraintVTable,
        allocator: std.mem.Allocator,
        ptr: *anyopaque,
        input: []const u8,
    ) anyerror!void = null,

    /// Context-aware string getter — symmetric with setCtxFn.
    getCtxFn: ?*const fn (
        vtable: *const ConstraintVTable,
        allocator: std.mem.Allocator,
        ptr: *const anyopaque,
    ) anyerror![]const u8 = null,

    /// Write the binary representation of the field at `ptr` into `out_buf`.
    /// Returns the number of bytes written.  The binary layout is constraint-
    /// specific (e.g. bitset → length-prefixed usize word array; string → length
    /// prefix + UTF-8 bytes).  Null for types without a defined binary path.
    setBinaryFn: ?*const fn (
        vtable: *const ConstraintVTable,
        ptr: *const anyopaque,
        out_buf: []u8,
    ) anyerror!usize = null,

    /// Read a binary representation from `in_buf` and write it to the field at
    /// `ptr`.  The format must match what setBinaryFn produces.
    /// Null for types without a defined binary path.
    getBinaryFn: ?*const fn (
        vtable: *const ConstraintVTable,
        allocator: std.mem.Allocator,
        ptr: *anyopaque,
        in_buf: []const u8,
    ) anyerror!void = null,
};

/// Build a static ConstraintVTable for type T.
/// The returned value is typically stored as a comptime constant and referenced
/// by pointer from Accessor.  Supported types: int, float, bool, []const u8,
/// enum, optional, array, vector.
pub fn Constraint(comptime T: type) ConstraintVTable {
    // releaseFn is only meaningful for owned heap types.  For []const u8 we
    // free the slice and zero out the field so double-free is safe.
    const release: ?*const fn (std.mem.Allocator, *anyopaque) void = blk: {
        switch (@typeInfo(T)) {
            .pointer => |p| {
                if (p.size == .slice and p.child == u8) {
                    break :blk struct {
                        fn rel(allocator: std.mem.Allocator, ptr: *anyopaque) void {
                            const s: *[]const u8 = @ptrCast(@alignCast(ptr));
                            if (s.len > 0) {
                                allocator.free(s.*);
                                s.* = "";
                            }
                        }
                    }.rel;
                }
            },
            else => {},
        }
        break :blk null;
    };

    return .{
        .setFn = struct {
            fn set(allocator: std.mem.Allocator, ptr: *anyopaque, input: []const u8) anyerror!void {
                const typed_ptr: *align(@alignOf(T)) T = @ptrCast(@alignCast(ptr));
                try constraintSet(T, allocator, typed_ptr, input);
            }
        }.set,
        .getFn = struct {
            fn get(allocator: std.mem.Allocator, ptr: *const anyopaque) anyerror![]const u8 {
                const typed_ptr: *align(@alignOf(T)) const T = @ptrCast(@alignCast(ptr));
                return constraintGet(T, allocator, typed_ptr);
            }
        }.get,
        .releaseFn = release,
    };
}

// ─── internal helpers ────────────────────────────────────────────────────────
// Extracted so that:
//   (a) the VTable closures above are thin wrappers (no duplicated switch),
//   (b) the optional/array/vector cases can recurse without constructing a
//       new ConstraintVTable just to call through a function pointer, and
//   (c) setFast/getFast can call these directly, bypassing type erasure.

fn constraintSet(comptime T: type, allocator: std.mem.Allocator, typed_ptr: *T, input: []const u8) anyerror!void {
    switch (@typeInfo(T)) {
        .int => typed_ptr.* = try std.fmt.parseInt(T, input, 10),
        .float => typed_ptr.* = try std.fmt.parseFloat(T, input),
        .bool => {
            if (std.ascii.eqlIgnoreCase(input, "true") or std.mem.eql(u8, input, "1")) {
                typed_ptr.* = true;
            } else if (std.ascii.eqlIgnoreCase(input, "false") or std.mem.eql(u8, input, "0")) {
                typed_ptr.* = false;
            } else {
                return error.InvalidBool;
            }
        },
        .pointer => |ptr_info| {
            // Only []const u8 / []u8 string slices are supported.
            if (ptr_info.size == .slice and ptr_info.child == u8) {
                typed_ptr.* = try allocator.dupe(u8, input);
            } else {
                return error.UnsupportedPointerType;
            }
        },
        .@"enum" => {
            typed_ptr.* = std.meta.stringToEnum(T, input) orelse return error.InvalidEnum;
        },
        .optional => |opt_info| {
            if (std.mem.eql(u8, input, "null") or input.len == 0) {
                typed_ptr.* = null;
            } else {
                const Child = opt_info.child;
                var temp: Child = undefined;
                try constraintSet(Child, allocator, &temp, input);
                typed_ptr.* = temp;
            }
        },
        .array => |arr_info| {
            const Child = arr_info.child;
            var iter = std.mem.splitScalar(u8, input, ',');
            var i: usize = 0;
            while (iter.next()) |elem| : (i += 1) {
                if (i >= arr_info.len) return error.ArrayTooLong;
                const trimmed = std.mem.trim(u8, elem, " \t\n\r");
                try constraintSet(Child, allocator, &typed_ptr[i], trimmed);
            }
            if (i != arr_info.len) return error.ArrayTooShort;
        },
        .vector => |vec_info| {
            const Child = vec_info.child;
            var iter = std.mem.splitScalar(u8, input, ',');
            var i: usize = 0;
            while (iter.next()) |elem| : (i += 1) {
                if (i >= vec_info.len) return error.VectorTooLong;
                const trimmed = std.mem.trim(u8, elem, " \t\n\r");
                var val: Child = undefined;
                try constraintSet(Child, allocator, &val, trimmed);
                typed_ptr[i] = val;
            }
            if (i != vec_info.len) return error.VectorTooShort;
        },
        else => return error.UnsupportedType,
    }
}

fn constraintGet(comptime T: type, allocator: std.mem.Allocator, typed_ptr: *const T) anyerror![]const u8 {
    switch (@typeInfo(T)) {
        .int, .float => return std.fmt.allocPrint(allocator, "{d}", .{typed_ptr.*}),
        .bool => return std.fmt.allocPrint(allocator, "{}", .{typed_ptr.*}),
        .pointer => |ptr_info| {
            if (ptr_info.size == .slice and ptr_info.child == u8) {
                return allocator.dupe(u8, typed_ptr.*);
            }
            return error.UnsupportedPointerType;
        },
        .@"enum" => return allocator.dupe(u8, @tagName(typed_ptr.*)),
        .optional => |opt_info| {
            if (typed_ptr.*) |val| {
                return constraintGet(opt_info.child, allocator, &val);
            } else {
                return allocator.dupe(u8, "null");
            }
        },
        .array => |arr_info| {
            var buf: std.ArrayListUnmanaged(u8) = .{};
            errdefer buf.deinit(allocator);
            try buf.append(allocator, '[');
            for (typed_ptr.*, 0..) |elem, i| {
                if (i > 0) try buf.append(allocator, ',');
                const s = try constraintGet(arr_info.child, allocator, &elem);
                defer allocator.free(s);
                try buf.appendSlice(allocator, s);
            }
            try buf.append(allocator, ']');
            return buf.toOwnedSlice(allocator);
        },
        .vector => |vec_info| {
            var buf: std.ArrayListUnmanaged(u8) = .{};
            errdefer buf.deinit(allocator);
            try buf.append(allocator, '[');
            for (0..vec_info.len) |i| {
                if (i > 0) try buf.append(allocator, ',');
                const elem = typed_ptr[i];
                const s = try constraintGet(vec_info.child, allocator, &elem);
                defer allocator.free(s);
                try buf.appendSlice(allocator, s);
            }
            try buf.append(allocator, ']');
            return buf.toOwnedSlice(allocator);
        },
        else => return error.UnsupportedType,
    }
}

// ============================================================
// § 3  Field metadata for AI agents and schema description
// ============================================================

/// Field metadata for AI agent understanding and schema documentation.
/// Populated via comptime decls on the Host type:
///   pub fn describeField(comptime name: []const u8) FieldMeta { ... }
///
/// This enables deterministic schema output for:
///   - LLM tool definition (MCP tools/call schemas)
///   - TUI help text and autocomplete hints
///   - Knowledge graph relation hints
///   - Cross-module duck typing validation
pub const FieldMeta = struct {
    /// Human-readable description for AI agents and documentation.
    /// Example: "TCP port for the HTTP server (1-65535)"
    description: ?[]const u8 = null,

    /// For enums: list of valid values. Auto-populated from enum type.
    /// Example: ["pending", "active", "done"]
    enum_values: ?[]const []const u8 = null,

    /// For integers/floats: inclusive minimum value.
    min: ?i128 = null,

    /// For integers/floats: inclusive maximum value.
    max: ?i128 = null,

    /// For strings: regex pattern or format hint.
    /// Example: "^[a-z_]+$" or "uuidv7" or "comma-separated"
    pattern: ?[]const u8 = null,

    /// Default value as string (for reconstruction/initialization).
    /// Example: "8080" or "pending"
    default: ?[]const u8 = null,

    /// Example values for AI understanding. Multi-valued to show variety.
    /// Example: ["8080", "3000", "443"]
    examples: ?[]const []const u8 = null,

    /// For knowledge graph fields: relation/table this field references.
    /// Example: "context_nodes" or "targets"
    /// Enables AI agents to traverse the graph intelligently.
    relation: ?[]const u8 = null,

    /// For bitset fields: domain source for capability names.
    /// Example: "StringInterner" — signals that valid values come from interner
    bitset_domain: ?[]const u8 = null,

    /// Is this field deprecated? If so, AI should prefer alternatives.
    deprecated: bool = false,

    /// Human-readable deprecation message explaining migration path.
    deprecated_message: ?[]const u8 = null,

    /// Fields that must be set before this field is valid.
    /// Example: for "port", depends_on could be ["protocol"]
    depends_on: ?[]const []const u8 = null,

    /// Fields that become required when this field has a specific value.
    /// Enables conditional validation for AI agents.
    /// Key: this field's value, Value: list of required field names.
    requires_when: ?[]const RequiresWhenEntry = null,

    /// Units for numeric values. Helps AI understand scale.
    /// Example: "milliseconds" or "bytes" or "count"
    units: ?[]const u8 = null,

    /// Whether this field is part of the entity's identity/key.
    /// AI agents should prioritize identity fields in queries.
    identity: bool = false,

    /// Whether this field is mutable after creation.
    /// AI agents should check this before attempting updates.
    immutable: bool = false,

    /// Semantic category for AI understanding.
    /// Example: "identifier", "temporal", "spatial", "relational"
    category: ?[]const u8 = null,
};

/// Conditional requirement entry for requires_when field.
pub const RequiresWhenEntry = struct {
    when_value: []const u8,
    required_fields: []const []const u8,
};

// ============================================================
// § 3b  Accessor descriptor
// ============================================================

/// A single field descriptor used by both static (Editable) and dynamic
/// (DynamicEditable) schemas.  The constraint pointer references a statically
/// compiled VTable, so adding the same field to a dynamic schema costs no
/// additional code generation.
///
/// type_tag and ownership are cached here so that consumers (CozoDB hydration,
/// WASM IPC, TUI) can branch cheaply without re-dispatching through @typeInfo.
/// binary_size is the fixed wire size in bytes for WASM IPC; 0 means variable.
/// meta is field metadata for AI agent understanding and schema documentation.
pub const Accessor = struct {
    name: []const u8,
    offset: usize,
    permissions: RolePermissions,
    constraint: *const ConstraintVTable,
    type_tag: TypeTag = .unknown,
    ownership: OwnershipMode = .value,
    /// Fixed binary wire size in bytes.  0 = variable-length (length-prefixed).
    binary_size: u16 = 0,
    /// AI-readable metadata for schema description.
    meta: FieldMeta = .{},
};

// ============================================================
// § 4  Editable mixin  (compile-time-known structs)
// ============================================================

/// Zero-size mixin that adds reflective set/get to any struct.
///
/// Add one field to your struct:
///   editable: Editable(MyStruct) = .{},
///
/// The mixin generates a comptime accessor table and a StaticStringMap for
/// O(1) name→index lookup.  All fields default to `perm_all`; override by
/// providing a custom `permissions(comptime field_name: []const u8)` decl on
/// your Host type (not yet wired — use DynamicEditable for per-field perms).
pub fn Editable(comptime Host: type) type {
    return struct {
        const Self = @This();

        /// Comptime-generated accessor table (one entry per non-"editable" field).
        pub const accessors: [fieldCount()]Accessor = buildAccessors();

        /// O(1) name → index lookup backed by a perfect hash at comptime.
        pub const accessor_map: std.StaticStringMap(usize) = buildMap();

        // ── comptime helpers ────────────────────────────────────────────────

        fn fieldCount() usize {
            var n: usize = 0;
            for (std.meta.fields(Host)) |f| {
                if (!std.mem.eql(u8, f.name, "editable")) n += 1;
            }
            return n;
        }

        fn buildAccessors() [fieldCount()]Accessor {
            const fields = std.meta.fields(Host);
            var arr: [fieldCount()]Accessor = undefined;
            var i: usize = 0;
            for (fields) |field| {
                if (std.mem.eql(u8, field.name, "editable")) continue;
                // Allow the Host type to inject a custom vtable for any field via a
                // comptime decl:  pub fn fieldConstraint(comptime name: []const u8)
                //                     ?*const ConstraintVTable
                // Return non-null to override the default Constraint(T) for that field.
                const custom: ?*const ConstraintVTable = blk: {
                    if (@hasDecl(Host, "fieldConstraint")) {
                        break :blk Host.fieldConstraint(field.name);
                    }
                    break :blk null;
                };
                const vtable = custom orelse comptimeConstraint(field.type);

                // Allow the Host type to provide AI-readable metadata via:
                //   pub fn describeField(comptime name: []const u8) FieldMeta { ... }
                // This enables deterministic schema output for LLM tool definitions.
                const meta: FieldMeta = blk: {
                    if (@hasDecl(Host, "describeField")) {
                        break :blk Host.describeField(field.name);
                    }
                    break :blk .{};
                };

                arr[i] = .{
                    .name = field.name,
                    .offset = @offsetOf(Host, field.name),
                    .permissions = perm_all,
                    .constraint = vtable,
                    .type_tag = comptimeTypeTag(field.type),
                    .ownership = comptimeOwnership(field.type),
                    .binary_size = comptimeBinarySize(field.type),
                    .meta = meta,
                };
                i += 1;
            }
            return arr;
        }

        fn buildMap() std.StaticStringMap(usize) {
            const KVPair = struct { []const u8, usize };
            var kvs: [accessors.len]KVPair = undefined;
            for (accessors, 0..) |acc, i| kvs[i] = .{ acc.name, i };
            return std.StaticStringMap(usize).initComptime(kvs);
        }

        /// Comptime type validation: unsupported types trigger a clear compile error
        /// at Editable instantiation time rather than a runtime error.
        /// Types that require a runtime-parameterised vtable (bitsets, collections)
        /// must be injected via the Host.fieldConstraint hook; they are rejected here
        /// to force explicit registration rather than silently producing a broken vtable.
        fn comptimeConstraint(comptime T: type) *const ConstraintVTable {
            switch (@typeInfo(T)) {
                .int, .float, .bool, .@"enum", .optional, .array, .vector => {},
                .pointer => |p| if (!(p.size == .slice and p.child == u8))
                    @compileError("Unsupported pointer type for Editable — use fieldConstraint hook: " ++ @typeName(T)),
                .@"struct" => @compileError("Struct fields require a runtime vtable — use fieldConstraint hook: " ++ @typeName(T)),
                else => @compileError("Unsupported type for Editable — use fieldConstraint hook: " ++ @typeName(T)),
            }
            return &comptime Constraint(T);
        }

        /// Derive a TypeTag at comptime from a field type.
        fn comptimeTypeTag(comptime T: type) TypeTag {
            return switch (@typeInfo(T)) {
                .int => .int,
                .float => .float,
                .bool => .bool,
                .@"enum" => .@"enum",
                .optional => .optional,
                .array => .array,
                .vector => .vector,
                .pointer => |p| if (p.size == .slice and p.child == u8) .string_owned else .unknown,
                else => .unknown,
            };
        }

        /// Derive OwnershipMode at comptime from a field type.
        fn comptimeOwnership(comptime T: type) OwnershipMode {
            return switch (@typeInfo(T)) {
                // []const u8 slices are treated as owned copies when set via reflection.
                .pointer => |p| if (p.size == .slice and p.child == u8) .owned else .borrowed,
                else => .value,
            };
        }

        /// Fixed binary wire size in bytes; 0 for variable-length types.
        fn comptimeBinarySize(comptime T: type) u16 {
            return switch (@typeInfo(T)) {
                .int, .float, .bool, .@"enum" => @sizeOf(T),
                .array => |a| @as(u16, @intCast(@sizeOf(a.child) * a.len)),
                .vector => |v| @as(u16, @intCast(@sizeOf(v.child) * v.len)),
                // strings, optionals, structs → variable
                else => 0,
            };
        }

        // ── public API ──────────────────────────────────────────────────────

        /// Set a field by name via string input.  Permission-checked against `role`.
        /// Errors: error.FieldNotFound, error.AccessDenied, plus any constraint parse errors.
        pub fn set(
            self: *Self,
            allocator: std.mem.Allocator,
            key: []const u8,
            value: []const u8,
            role: Role,
        ) anyerror!void {
            const host: *Host = @alignCast(@fieldParentPtr("editable", self));
            const index = accessor_map.get(key) orelse return error.FieldNotFound;
            const accessor = accessors[index];
            if (!accessor.permissions.canWrite(role)) return error.AccessDenied;
            const host_bytes: [*]u8 = @ptrCast(host);
            const field_ptr: *anyopaque = @ptrCast(host_bytes + accessor.offset);
            if (accessor.constraint.setCtxFn) |f|
                try f(accessor.constraint, allocator, field_ptr, value)
            else
                try accessor.constraint.setFn(allocator, field_ptr, value);
        }

        /// Get a field by name, returned as a caller-owned string.  Permission-checked against `role`.
        /// Errors: error.FieldNotFound, error.AccessDenied, error.OutOfMemory, plus any constraint errors.
        pub fn get(
            self: *const Self,
            allocator: std.mem.Allocator,
            key: []const u8,
            role: Role,
        ) anyerror![]const u8 {
            const host: *const Host = @alignCast(@fieldParentPtr("editable", self));
            const index = accessor_map.get(key) orelse return error.FieldNotFound;
            const accessor = accessors[index];
            if (!accessor.permissions.canRead(role)) return error.AccessDenied;
            const host_bytes: [*]const u8 = @ptrCast(host);
            const field_ptr: *const anyopaque = @ptrCast(host_bytes + accessor.offset);
            if (accessor.constraint.getCtxFn) |f|
                return f(accessor.constraint, allocator, field_ptr)
            else
                return accessor.constraint.getFn(allocator, field_ptr);
        }

        /// Zero-cost set for trusted internal code: comptime key, no allocation, no permission check.
        /// The key must be a string literal known at compile time; unknown keys are a compile error.
        pub fn setFast(self: *Self, comptime key: []const u8, value: anytype) void {
            const host: *Host = @alignCast(@fieldParentPtr("editable", self));
            inline for (std.meta.fields(Host)) |field| {
                if (comptime std.mem.eql(u8, field.name, key)) {
                    @field(host.*, field.name) = value;
                    return;
                }
            }
            @compileError("Field not found: " ++ key);
        }

        /// Zero-cost get for trusted internal code: comptime key, returns strongly-typed value.
        pub fn getFast(self: *const Self, comptime key: []const u8) @TypeOf(@field(@as(Host, undefined), key)) {
            const host: *const Host = @alignCast(@fieldParentPtr("editable", self));
            inline for (std.meta.fields(Host)) |field| {
                if (comptime std.mem.eql(u8, field.name, key)) {
                    return @field(host.*, field.name);
                }
            }
            @compileError("Field not found: " ++ key);
        }

        // ── Schema description for AI agents ───────────────────────────────────

        /// Generate a JSON Schema describing all fields, types, constraints, and metadata.
        /// This is the primary entry point for AI agents to understand the structure.
        /// Output format follows JSON Schema conventions with Coral Context extensions.
        ///
        /// Example output:
        ///   {
        ///     "type": "object",
        ///     "properties": {
        ///       "port": {"type": "integer", "description": "TCP port", "minimum": 1, "maximum": 65535}
        ///     },
        ///     "required": ["port"]
        ///   }
        pub fn describeSchema(allocator: std.mem.Allocator) ![]const u8 {
            return describeSchemaFromAccessors(allocator, @typeName(Host), &accessors);
        }

        /// Generate a list of field names for iteration.
        /// Useful for AI agents to enumerate available fields.
        pub fn fieldNames(allocator: std.mem.Allocator) ![]const []const u8 {
            var names = try allocator.alloc([]const u8, accessors.len);
            for (accessors, 0..) |acc, i| {
                names[i] = acc.name;
            }
            return names;
        }

        /// Get metadata for a specific field by name.
        /// Returns null if field doesn't exist.
        pub fn getFieldMeta(key: []const u8) ?FieldMeta {
            const index = accessor_map.get(key) orelse return null;
            return accessors[index].meta;
        }
    };
}

// ============================================================
// § 4b  Schema description utilities
// ============================================================

/// Type name for JSON schema output (comptime known).
fn typeName(comptime T: type) []const u8 {
    return @typeName(T);
}

/// Generate JSON Schema from an accessor slice.
/// Used by both Editable.describeSchema and DynamicEditable.describeSchema.
fn describeSchemaFromAccessors(
    allocator: std.mem.Allocator,
    type_name: []const u8,
    accessors_slice: []const Accessor,
) ![]const u8 {
    var buf: std.ArrayListUnmanaged(u8) = .{};
    errdefer buf.deinit(allocator);
    const writer = buf.writer(allocator);

    // Open object
    try writer.writeAll("{\n");
    try writer.print("  \"$schema\": \"https://json-schema.org/draft/2020-12/schema\",\n", .{});
    try writer.print("  \"title\": \"{s}\",\n", .{type_name});
    try writer.print("  \"type\": \"object\",\n", .{});

    // Properties object
    try writer.writeAll("  \"properties\": {\n");

    for (accessors_slice, 0..) |acc, i| {
        if (i > 0) try writer.writeAll(",\n");
        try describeFieldToJson(writer, acc, 2);
    }

    try writer.writeAll("\n  },\n");

    // Required fields (identity fields + fields with no default)
    try writer.writeAll("  \"required\": [");
    var first_required = true;
    for (accessors_slice) |acc| {
        if (acc.meta.identity or acc.meta.default == null) {
            if (!first_required) try writer.writeAll(", ");
            first_required = false;
            try writer.print("\"{s}\"", .{acc.name});
        }
    }
    try writer.writeAll("],\n");

    // Field ordering hint (for deterministic AI input ordering)
    try writer.writeAll("  \"x-order\": [");
    for (accessors_slice, 0..) |acc, i| {
        if (i > 0) try writer.writeAll(", ");
        try writer.print("\"{s}\"", .{acc.name});
    }
    try writer.writeAll("],\n");

    // Role permissions map (Coral Context extension)
    try writer.writeAll("  \"x-permissions\": {\n");
    for (accessors_slice, 0..) |acc, i| {
        if (i > 0) try writer.writeAll(",\n");
        try writer.print("    \"{s}\": {{\"read\": \"", .{acc.name});
        try describeRoleSet(writer, acc.permissions, "read");
        try writer.writeAll("\", \"write\": \"");
        try describeRoleSet(writer, acc.permissions, "write");
        try writer.writeAll("\"}");
    }
    try writer.writeAll("\n  }\n");

    // Close object
    try writer.writeAll("}");

    return buf.toOwnedSlice(allocator);
}

/// Write a single field's JSON Schema property.
fn describeFieldToJson(writer: anytype, accessor: Accessor, indent: usize) !void {
    // Write indentation spaces
    var i: usize = 0;
    while (i < indent) : (i += 1) {
        try writer.writeByte(' ');
    }

    try writer.print("\"{s}\": {{\n", .{accessor.name});

    // Type
    i = 0;
    while (i < indent) : (i += 1) try writer.writeByte(' ');
    try writer.print("  \"type\": \"{s}\",\n", .{typeTagToJsonSchema(accessor.type_tag)});

    // Description (if provided)
    if (accessor.meta.description) |desc| {
        i = 0;
        while (i < indent) : (i += 1) try writer.writeByte(' ');
        try writer.writeAll("  \"description\": \"");
        try writeEscapedJsonString(writer, desc);
        try writer.writeAll("\",\n");
    }

    // Enum values (if provided)
    if (accessor.meta.enum_values) |values| {
        i = 0;
        while (i < indent) : (i += 1) try writer.writeByte(' ');
        try writer.writeAll("  \"enum\": [");
        for (values, 0..) |v, idx| {
            if (idx > 0) try writer.writeAll(", ");
            try writer.print("\"{s}\"", .{v});
        }
        try writer.writeAll("],\n");
    }

    // Numeric bounds
    if (accessor.meta.min) |min| {
        i = 0;
        while (i < indent) : (i += 1) try writer.writeByte(' ');
        try writer.print("  \"minimum\": {d},\n", .{min});
    }
    if (accessor.meta.max) |max| {
        i = 0;
        while (i < indent) : (i += 1) try writer.writeByte(' ');
        try writer.print("  \"maximum\": {d},\n", .{max});
    }

    // Pattern for strings
    if (accessor.meta.pattern) |pattern| {
        i = 0;
        while (i < indent) : (i += 1) try writer.writeByte(' ');
        try writer.writeAll("  \"pattern\": \"");
        try writeEscapedJsonString(writer, pattern);
        try writer.writeAll("\",\n");
    }

    // Default value
    if (accessor.meta.default) |def| {
        i = 0;
        while (i < indent) : (i += 1) try writer.writeByte(' ');
        try writer.writeAll("  \"default\": \"");
        try writeEscapedJsonString(writer, def);
        try writer.writeAll("\",\n");
    }

    // Examples
    if (accessor.meta.examples) |examples| {
        i = 0;
        while (i < indent) : (i += 1) try writer.writeByte(' ');
        try writer.writeAll("  \"examples\": [");
        for (examples, 0..) |ex, idx| {
            if (idx > 0) try writer.writeAll(", ");
            try writer.print("\"{s}\"", .{ex});
        }
        try writer.writeAll("],\n");
    }

    // Units (Coral Context extension)
    if (accessor.meta.units) |units| {
        i = 0;
        while (i < indent) : (i += 1) try writer.writeByte(' ');
        try writer.print("  \"x-units\": \"{s}\",\n", .{units});
    }

    // Category (Coral Context extension)
    if (accessor.meta.category) |cat| {
        i = 0;
        while (i < indent) : (i += 1) try writer.writeByte(' ');
        try writer.print("  \"x-category\": \"{s}\",\n", .{cat});
    }

    // Relation (knowledge graph hint)
    if (accessor.meta.relation) |rel| {
        i = 0;
        while (i < indent) : (i += 1) try writer.writeByte(' ');
        try writer.print("  \"x-relation\": \"{s}\",\n", .{rel});
    }

    // Bitset domain (capability names source)
    if (accessor.meta.bitset_domain) |domain| {
        i = 0;
        while (i < indent) : (i += 1) try writer.writeByte(' ');
        try writer.print("  \"x-bitset-domain\": \"{s}\",\n", .{domain});
    }

    // Identity flag
    if (accessor.meta.identity) {
        i = 0;
        while (i < indent) : (i += 1) try writer.writeByte(' ');
        try writer.writeAll("  \"x-identity\": true,\n");
    }

    // Immutability flag
    if (accessor.meta.immutable) {
        i = 0;
        while (i < indent) : (i += 1) try writer.writeByte(' ');
        try writer.writeAll("  \"x-immutable\": true,\n");
    }

    // Deprecation
    if (accessor.meta.deprecated) {
        i = 0;
        while (i < indent) : (i += 1) try writer.writeByte(' ');
        try writer.writeAll("  \"deprecated\": true");
        if (accessor.meta.deprecated_message) |msg| {
            try writer.writeAll(", \"x-deprecated-message\": \"");
            try writeEscapedJsonString(writer, msg);
            try writer.writeAll("\"");
        }
        try writer.writeAll(",\n");
    }

    // Dependencies
    if (accessor.meta.depends_on) |deps| {
        i = 0;
        while (i < indent) : (i += 1) try writer.writeByte(' ');
        try writer.writeAll("  \"x-depends-on\": [");
        for (deps, 0..) |dep, idx| {
            if (idx > 0) try writer.writeAll(", ");
            try writer.print("\"{s}\"", .{dep});
        }
        try writer.writeAll("],\n");
    }

    // Conditional requirements
    if (accessor.meta.requires_when) |reqs| {
        i = 0;
        while (i < indent) : (i += 1) try writer.writeByte(' ');
        try writer.writeAll("  \"x-requires-when\": {\n");
        for (reqs, 0..) |req, idx| {
            if (idx > 0) try writer.writeAll(",\n");
            i = 0;
            while (i < indent + 4) : (i += 1) try writer.writeByte(' ');
            try writer.print("\"{s}\": [", .{req.when_value});
            for (req.required_fields, 0..) |rf, j| {
                if (j > 0) try writer.writeAll(", ");
                try writer.print("\"{s}\"", .{rf});
            }
            try writer.writeAll("]");
        }
        i = 0;
        while (i < indent) : (i += 1) try writer.writeByte(' ');
        try writer.writeAll("  },\n");
    }

    // Binary size (WASM IPC hint)
    i = 0;
    while (i < indent) : (i += 1) try writer.writeByte(' ');
    try writer.print("  \"x-binary-size\": {d},\n", .{accessor.binary_size});

    // Ownership mode
    i = 0;
    while (i < indent) : (i += 1) try writer.writeByte(' ');
    try writer.print("  \"x-ownership\": \"{s}\"\n", .{@tagName(accessor.ownership)});

    // Close field object
    i = 0;
    while (i < indent) : (i += 1) try writer.writeByte(' ');
    try writer.writeAll("}");
}

/// Convert TypeTag to JSON Schema type string.
fn typeTagToJsonSchema(tag: TypeTag) []const u8 {
    return switch (tag) {
        .int => "integer",
        .float => "number",
        .bool => "boolean",
        .string_owned => "string",
        .string_borrowed => "string",
        .string_rc => "string",
        .@"enum" => "string",
        .optional => "null",
        .array => "array",
        .vector => "array",
        .bitset => "array",
        .collection => "array",
        .unknown => "object",
    };
}

/// Write role permission set as comma-separated role names.
/// operation: "read" or "write"
fn describeRoleSet(writer: anytype, perms: RolePermissions, comptime operation: []const u8) !void {
    const role_names: [6][]const u8 = .{ "coder", "creator", "staff", "world", "script", "player" };
    const roles: [6]Role = .{ .coder, .creator, .staff, .world, .script, .player };
    var first = true;
    for (role_names, roles) |name, role| {
        const allowed = if (std.mem.eql(u8, operation, "read"))
            perms.canRead(role)
        else if (std.mem.eql(u8, operation, "write"))
            perms.canWrite(role)
        else
            perms.canDerive(role);
        if (allowed) {
            if (!first) try writer.writeAll(", ");
            first = false;
            try writer.writeAll(name);
        }
    }
}

/// Write escaped JSON string.
fn writeEscapedJsonString(writer: anytype, s: []const u8) !void {
    for (s) |c| {
        switch (c) {
            '"' => try writer.writeAll("\\\""),
            '\\' => try writer.writeAll("\\\\"),
            '\n' => try writer.writeAll("\\n"),
            '\r' => try writer.writeAll("\\r"),
            '\t' => try writer.writeAll("\\t"),
            else => try writer.writeByte(c),
        }
    }
}

// ============================================================
// § 5  DynamicEditable  (runtime-defined schemas, e.g. CozoDB rows)
// ============================================================

/// Runtime field editor backed by a raw byte buffer and a slice of Accessors.
///
/// The same ConstraintVTable instances used by static Editable are reused here,
/// enabling zero additional code generation per dynamic schema.
///
/// Lifecycle:
///   var dyn = try DynamicEditable.init(allocator, buffer, accessors);
///   defer dyn.deinit();
pub const DynamicEditable = struct {
    buffer: []u8,
    accessors: []const Accessor,
    /// Zig 0.15 unmanaged hash map — allocator passed per-operation.
    name_map: std.StringHashMapUnmanaged(usize),
    /// Stored so that set/get callers don't have to re-supply it.
    allocator: std.mem.Allocator,

    pub fn init(
        allocator: std.mem.Allocator,
        buffer: []u8,
        accessors: []const Accessor,
    ) !DynamicEditable {
        var name_map: std.StringHashMapUnmanaged(usize) = .{};
        errdefer name_map.deinit(allocator);
        for (accessors, 0..) |acc, i| {
            try name_map.put(allocator, acc.name, i);
        }
        return .{
            .buffer = buffer,
            .accessors = accessors,
            .name_map = name_map,
            .allocator = allocator,
        };
    }

    pub fn deinit(self: *DynamicEditable) void {
        self.name_map.deinit(self.allocator);
    }

    /// Release heap memory owned by every field that has a releaseFn.
    /// Call this before freeing the backing buffer to avoid leaks.
    /// Safe to call multiple times only if the releaseFn is idempotent (e.g.
    /// sets the field to empty/null after releasing).
    pub fn releaseAll(self: *DynamicEditable) void {
        for (self.accessors) |accessor| {
            if (accessor.constraint.releaseFn) |rel| {
                const field_ptr: *anyopaque = @ptrCast(self.buffer.ptr + accessor.offset);
                rel(self.allocator, field_ptr);
            }
        }
    }

    /// Set a field by name via string input, role-checked.
    /// Uses setCtxFn when available (required for runtime-parameterised constraints
    /// such as BitSetConstraint), falling back to setFn for stateless constraints.
    pub fn set(self: *DynamicEditable, key: []const u8, value: []const u8, role: Role) anyerror!void {
        const index = self.name_map.get(key) orelse return error.FieldNotFound;
        const accessor = self.accessors[index];
        if (!accessor.permissions.canWrite(role)) return error.AccessDenied;
        const field_ptr: *anyopaque = @ptrCast(self.buffer.ptr + accessor.offset);
        if (accessor.constraint.setCtxFn) |f|
            try f(accessor.constraint, self.allocator, field_ptr, value)
        else
            try accessor.constraint.setFn(self.allocator, field_ptr, value);
    }

    /// Get a field by name as a caller-owned string, role-checked.
    /// Uses getCtxFn when available, falling back to getFn.
    pub fn get(self: *const DynamicEditable, key: []const u8, role: Role) anyerror![]const u8 {
        const index = self.name_map.get(key) orelse return error.FieldNotFound;
        const accessor = self.accessors[index];
        if (!accessor.permissions.canRead(role)) return error.AccessDenied;
        const field_ptr: *const anyopaque = @ptrCast(self.buffer.ptr + accessor.offset);
        if (accessor.constraint.getCtxFn) |f|
            return f(accessor.constraint, self.allocator, field_ptr)
        else
            return accessor.constraint.getFn(self.allocator, field_ptr);
    }

    // ── Schema description for AI agents ───────────────────────────────────

    /// Generate a JSON Schema describing all fields, types, constraints, and metadata.
    /// The type_name parameter should describe the entity (e.g., "Target", "ContextNode").
    pub fn describeSchema(self: *const DynamicEditable, allocator: std.mem.Allocator, type_name: []const u8) ![]const u8 {
        return describeSchemaFromAccessors(allocator, type_name, self.accessors);
    }

    /// Generate a list of field names for iteration.
    pub fn fieldNames(self: *const DynamicEditable, allocator: std.mem.Allocator) ![]const []const u8 {
        var names = try allocator.alloc([]const u8, self.accessors.len);
        for (self.accessors, 0..) |acc, i| {
            names[i] = acc.name;
        }
        return names;
    }

    /// Get metadata for a specific field by name.
    /// Returns null if field doesn't exist.
    pub fn getFieldMeta(self: *const DynamicEditable, key: []const u8) ?FieldMeta {
        const index = self.name_map.get(key) orelse return null;
        return self.accessors[index].meta;
    }
};

// ============================================================
// § 6  Tests
// ============================================================

const testing = std.testing;

// ── primitive types ──────────────────────────────────────────────────────────

const TestConfig = struct {
    port: u16 = 8080,
    timeout: f32 = 30.0,
    enabled: bool = false,
    editable: Editable(TestConfig) = .{},
};

test "Editable: set and get integer" {
    var config: TestConfig = .{};
    try config.editable.set(testing.allocator, "port", "9000", .player);
    try testing.expectEqual(@as(u16, 9000), config.port);
    const val = try config.editable.get(testing.allocator, "port", .player);
    defer testing.allocator.free(val);
    try testing.expectEqualStrings("9000", val);
}

test "Editable: set and get float" {
    var config: TestConfig = .{};
    try config.editable.set(testing.allocator, "timeout", "45.5", .player);
    try testing.expectEqual(@as(f32, 45.5), config.timeout);
    const val = try config.editable.get(testing.allocator, "timeout", .player);
    defer testing.allocator.free(val);
    try testing.expectEqualSlices(u8, "45.5", val[0..4]);
}

test "Editable: set and get bool" {
    var config: TestConfig = .{};
    try config.editable.set(testing.allocator, "enabled", "true", .player);
    try testing.expect(config.enabled);
    const val = try config.editable.get(testing.allocator, "enabled", .player);
    defer testing.allocator.free(val);
    try testing.expectEqualStrings("true", val);
}

test "Editable: field not found" {
    var config: TestConfig = .{};
    const result = config.editable.set(testing.allocator, "nonexistent", "value", .player);
    try testing.expectError(error.FieldNotFound, result);
}

test "Editable: zero-size mixin" {
    try testing.expectEqual(@as(usize, 0), @sizeOf(Editable(TestConfig)));
}

test "Editable: setFast and getFast" {
    var config: TestConfig = .{};
    config.editable.setFast("port", 1234);
    try testing.expectEqual(@as(u16, 1234), config.port);
    const val = config.editable.getFast("port");
    try testing.expectEqual(@as(u16, 1234), val);
}

// ── string field ─────────────────────────────────────────────────────────────

const TestString = struct {
    name: []const u8 = "default",
    editable: Editable(TestString) = .{},
};

test "Editable: string field set and get" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer if (gpa.deinit() == .leak) @panic("leak");
    const allocator = gpa.allocator();

    var obj: TestString = .{};
    try obj.editable.set(allocator, "name", "hello world", .coder);
    defer allocator.free(obj.name);
    try testing.expectEqualStrings("hello world", obj.name);
    const val = try obj.editable.get(allocator, "name", .coder);
    defer allocator.free(val);
    try testing.expectEqualStrings("hello world", val);
}

// ── enum field ───────────────────────────────────────────────────────────────

const Status = enum { pending, active, done };

const TestEnum = struct {
    status: Status = .pending,
    editable: Editable(TestEnum) = .{},
};

test "Editable: enum field set and get" {
    var obj: TestEnum = .{};
    try obj.editable.set(testing.allocator, "status", "active", .player);
    try testing.expectEqual(Status.active, obj.status);
    const val = try obj.editable.get(testing.allocator, "status", .player);
    defer testing.allocator.free(val);
    try testing.expectEqualStrings("active", val);
}

test "Editable: enum invalid value propagates error" {
    var obj: TestEnum = .{};
    const result = obj.editable.set(testing.allocator, "status", "invalid", .player);
    try testing.expectError(error.InvalidEnum, result);
}

// ── optional field ───────────────────────────────────────────────────────────

const TestOptional = struct {
    maybe: ?u32 = null,
    editable: Editable(TestOptional) = .{},
};

test "Editable: optional null" {
    var obj: TestOptional = .{ .maybe = 42 };
    try obj.editable.set(testing.allocator, "maybe", "null", .coder);
    try testing.expect(obj.maybe == null);
}

test "Editable: optional value" {
    var obj: TestOptional = .{};
    try obj.editable.set(testing.allocator, "maybe", "123", .coder);
    try testing.expect(obj.maybe != null);
    try testing.expectEqual(@as(u32, 123), obj.maybe.?);
}

// ── array field ──────────────────────────────────────────────────────────────

const TestArray = struct {
    scores: [3]u8 = .{ 0, 0, 0 },
    editable: Editable(TestArray) = .{},
};

test "Editable: array set and get" {
    var obj: TestArray = .{};
    try obj.editable.set(testing.allocator, "scores", "1, 2, 3", .player);
    try testing.expectEqual(@as(u8, 1), obj.scores[0]);
    try testing.expectEqual(@as(u8, 2), obj.scores[1]);
    try testing.expectEqual(@as(u8, 3), obj.scores[2]);
    const val = try obj.editable.get(testing.allocator, "scores", .player);
    defer testing.allocator.free(val);
    try testing.expectEqualStrings("[1,2,3]", val);
}

// ── vector field ─────────────────────────────────────────────────────────────

const TestVector = struct {
    vec: @Vector(4, f32) = .{ 0, 0, 0, 0 },
    editable: Editable(TestVector) = .{},
};

test "Editable: vector set and get" {
    var obj: TestVector = .{};
    try obj.editable.set(testing.allocator, "vec", "1.0, 2.0, 3.0, 4.0", .player);
    try testing.expectEqual(@as(f32, 1.0), obj.vec[0]);
    try testing.expectEqual(@as(f32, 2.0), obj.vec[1]);
    const val = try obj.editable.get(testing.allocator, "vec", .player);
    defer testing.allocator.free(val);
    try testing.expectEqualStrings("[1,2,3,4]", val);
}

// ── DynamicEditable ──────────────────────────────────────────────────────────

test "DynamicEditable: basic set and get" {
    var buffer align(4) = [_]u8{0} ** 8;

    const u32_vtable = Constraint(u32);
    const f32_vtable = Constraint(f32);

    const accessors = [_]Accessor{
        .{ .name = "id", .offset = 0, .permissions = perm_coder, .constraint = &u32_vtable },
        .{ .name = "value", .offset = 4, .permissions = perm_coder, .constraint = &f32_vtable },
    };

    var dyn = try DynamicEditable.init(testing.allocator, &buffer, &accessors);
    defer dyn.deinit();

    try dyn.set("id", "42", .coder);
    try dyn.set("value", "3.14", .coder);

    const id_val = try dyn.get("id", .coder);
    defer testing.allocator.free(id_val);
    try testing.expectEqualStrings("42", id_val);

    const f_val = try dyn.get("value", .coder);
    defer testing.allocator.free(f_val);
    try testing.expect(std.mem.startsWith(u8, f_val, "3.14"));
}

test "DynamicEditable: role-based permission" {
    var buffer align(4) = [_]u8{0} ** 4;
    const u32_vtable = Constraint(u32);

    const accessors = [_]Accessor{
        .{ .name = "admin_field", .offset = 0, .permissions = .{ .coder_write = true, .player_read = true }, .constraint = &u32_vtable },
    };

    var dyn = try DynamicEditable.init(testing.allocator, &buffer, &accessors);
    defer dyn.deinit();

    try dyn.set("admin_field", "100", .coder);
    const result = dyn.set("admin_field", "200", .player);
    try testing.expectError(error.AccessDenied, result);
}

test "RolePermissions: packed struct size" {
    try testing.expectEqual(@as(usize, 4), @sizeOf(RolePermissions));
}

test "RolePermissions: canRead and canWrite methods" {
    const perms: RolePermissions = .{ .staff_read = true, .staff_write = true };
    try testing.expect(perms.canRead(.staff));
    try testing.expect(perms.canWrite(.staff));
    try testing.expect(!perms.canRead(.player));
    try testing.expect(!perms.canWrite(.coder));
}

test "Editable: access denied on read" {
    // Editable defaults to perm_all; test AccessDenied via DynamicEditable which
    // supports per-field permission overrides.
    var buffer align(4) = [_]u8{0} ** 4;
    const u32_vtable = Constraint(u32);
    const accessors = [_]Accessor{
        .{ .name = "val", .offset = 0, .permissions = .{ .coder_read = true }, .constraint = &u32_vtable },
    };
    var dyn = try DynamicEditable.init(testing.allocator, &buffer, &accessors);
    defer dyn.deinit();
    const result = dyn.get("val", .player);
    try testing.expectError(error.AccessDenied, result);
}

// ── Schema description for AI agents ───────────────────────────────────────

const TestSchema = struct {
    port: u16 = 8080,
    host: []const u8 = "localhost",
    enabled: bool = true,
    level: enum { low, medium, high } = .medium,
    editable: Editable(TestSchema) = .{},

    pub fn describeField(comptime name: []const u8) FieldMeta {
        if (std.mem.eql(u8, name, "port")) {
            return .{
                .description = "TCP port for the server (1-65535)",
                .min = 1,
                .max = 65535,
                .default = "8080",
                .identity = true,
            };
        }
        if (std.mem.eql(u8, name, "host")) {
            return .{
                .description = "Hostname or IP address to bind",
                .examples = &.{ "localhost", "0.0.0.0", "127.0.0.1" },
            };
        }
        if (std.mem.eql(u8, name, "level")) {
            return .{
                .description = "Log level for the server",
                .enum_values = &.{ "low", "medium", "high" },
            };
        }
        return .{};
    }
};

test "Editable: describeSchema generates JSON" {
    const schema = try Editable(TestSchema).describeSchema(testing.allocator);
    defer testing.allocator.free(schema);

    // Check that JSON contains expected fields
    try testing.expect(std.mem.indexOf(u8, schema, "\"port\"") != null);
    try testing.expect(std.mem.indexOf(u8, schema, "\"host\"") != null);
    try testing.expect(std.mem.indexOf(u8, schema, "\"enabled\"") != null);
    try testing.expect(std.mem.indexOf(u8, schema, "\"level\"") != null);
    try testing.expect(std.mem.indexOf(u8, schema, "\"TCP port for the server") != null);
    try testing.expect(std.mem.indexOf(u8, schema, "$schema") != null);
}

test "Editable: fieldNames returns all fields" {
    const names = try Editable(TestSchema).fieldNames(testing.allocator);
    defer testing.allocator.free(names);
    try testing.expectEqual(@as(usize, 4), names.len);
}

test "Editable: getFieldMeta returns metadata" {
    const meta = Editable(TestSchema).getFieldMeta("port");
    try testing.expect(meta != null);
    try testing.expect(meta.?.description != null);
    try testing.expectEqual(@as(i128, 1), meta.?.min.?);
    try testing.expect(meta.?.identity);
}

test "DynamicEditable: describeSchema generates JSON" {
    var buffer align(8) = [_]u8{0} ** 8;
    const u32_vtable = Constraint(u32);
    const f32_vtable = Constraint(f32);

    const accessors = [_]Accessor{
        .{
            .name = "id",
            .offset = 0,
            .permissions = perm_all,
            .constraint = &u32_vtable,
            .type_tag = .int,
            .meta = .{ .description = "Unique identifier", .identity = true },
        },
        .{
            .name = "value",
            .offset = 4,
            .permissions = perm_all,
            .constraint = &f32_vtable,
            .type_tag = .float,
            .meta = .{ .description = "A floating point value" },
        },
    };

    var dyn = try DynamicEditable.init(testing.allocator, &buffer, &accessors);
    defer dyn.deinit();

    const schema = try dyn.describeSchema(testing.allocator, "TestEntity");
    defer testing.allocator.free(schema);

    try testing.expect(std.mem.indexOf(u8, schema, "\"id\"") != null);
    try testing.expect(std.mem.indexOf(u8, schema, "\"value\"") != null);
    try testing.expect(std.mem.indexOf(u8, schema, "Unique identifier") != null);
}
