/// accessor.zig — Accessor, DynamicEditable, Editable(T), FieldMeta, TypeTag, OwnershipMode.
const std = @import("std");
const constraint_mod = @import("constraint.zig");
const permissions_mod = @import("permissions.zig");
const schema_version_mod = @import("schema_version.zig");
const validate_mod = @import("validate.zig");

pub const SchemaVersion = schema_version_mod.SchemaVersion;
pub const SCHEMA_CURRENT = schema_version_mod.SCHEMA_CURRENT;

pub const ConstraintVTable = constraint_mod.ConstraintVTable;
pub const Constraint = constraint_mod.Constraint;
pub const constraintSet = constraint_mod.constraintSet;
pub const constraintGet = constraint_mod.constraintGet;

pub const Role = permissions_mod.Role;
pub const RolePermissions = permissions_mod.RolePermissions;
pub const perm_all = permissions_mod.perm_all;
pub const perm_coder = permissions_mod.perm_coder;

// ============================================================
// § 1b  Type metadata enums
// ============================================================

/// Defines a type tag for enum-like accessors, managing invariants and ownership without thread safety.
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

/// Defines ownership semantics for Zig's accessor types, ensuring controlled access and invariants.
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

/// Defines a SQL type with accessor methods; manages schema and invariants; owned by the module.
pub const SqlType = enum(u8) {
    integer,
    real,
    text,
    blob,

    pub fn fromTypeTag(tag: TypeTag) SqlType {
        return switch (tag) {
            .int => .integer,
            .float => .real,
            .bool => .integer,
            .@"enum" => .integer,
            .string_owned => .text,
            .string_borrowed => .text,
            .string_rc => .text,
            .bitset => .blob,
            .array => .text,
            .vector => .blob,
            .optional => .text,
            .collection => .blob,
            .unknown => .text,
        };
    }

    pub fn toString(self: SqlType) []const u8 {
        return switch (self) {
            .integer => "INTEGER",
            .real => "REAL",
            .text => "TEXT",
            .blob => "BLOB",
        };
    }
};

// ============================================================
// § 3  Field metadata for AI agents and schema description
// ============================================================

/// Tracks field metadata for reflection; owned by the struct; ensures invariant access patterns.
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

    // ── Validation hooks (M6) ──────────────────────────────────────────────

    /// Custom validation function called on set() after type coercion.
    /// Receives the raw string value.  Return false to reject.
    /// Example: fn(v) bool { return v.len >= 8; }
    custom_validate: ?*const fn (value: []const u8) bool = null,

    /// For cross-field validation: names of other fields this field is
    /// validated against.  Informational — enforcement requires
    /// Editable.validateCrossField() to be called explicitly.
    cross_field: ?[]const []const u8 = null,
};

/// Conditional requirement entry for requires_when field.
pub const RequiresWhenEntry = struct {
    when_value: []const u8,
    required_fields: []const []const u8,
};

// ============================================================
// § 3b  Accessor descriptor
// ============================================================

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
    /// SQL type for database persistence.  Defaults to TEXT for safety.
    sql_type: SqlType = .text,
    /// For nested struct fields, points to the accessor for the parent struct
    /// field that contains this one.  Null for top-level fields.
    /// Always null in accessors generated by Editable(T); can be set
    /// explicitly in DynamicEditable accessor slices for UI navigation.
    parent: ?*const Accessor = null,

    // ── Schema versioning (M4) ─────────────────────────────────────────────

    /// Schema version in which this field was introduced.
    /// Loaders should skip this accessor if the stored version predates it.
    /// Default: v1.0 (all pre-versioning fields are implicitly v1.0).
    version_added: SchemaVersion = SCHEMA_CURRENT,

    /// Schema version in which this field was removed, if any.
    /// Loaders should treat the field as absent for versions >= this value.
    /// Null means the field has not been removed.
    version_removed: ?SchemaVersion = null,

    /// Optional migration function for reading values written under an older
    /// schema.  When non-null, called by the loader before `constraint.setFn`
    /// if the stored schema version is older than `version_added`:
    ///
    ///   const new_value = try accessor.migrate_from.?(old_raw, allocator);
    ///   defer allocator.free(new_value);
    ///   try accessor.constraint.setFn(allocator, field_ptr, new_value);
    ///
    /// The function receives the old serialized string and returns a new one
    /// that is compatible with the current constraint.  Ownership of the
    /// returned slice is transferred to the caller.
    migrate_from: ?*const fn (old_value: []const u8, allocator: std.mem.Allocator) anyerror![]const u8 = null,

    // ── Helpers ────────────────────────────────────────────────────────────

    /// Returns true if this accessor is present in `stored`.
    ///
    /// A field is present if:
    ///   - `stored` >= `version_added`
    ///   - `version_removed` is null OR `stored` < `version_removed`
    pub fn isPresentIn(self: *const Accessor, stored: SchemaVersion) bool {
        // Field must have been added at or before `stored`.
        const added = self.version_added;
        const was_added = added.major < stored.major or
            (added.major == stored.major and added.minor <= stored.minor);
        if (!was_added) return false;

        // Field must not have been removed at or before `stored`.
        if (self.version_removed) |removed| {
            const was_removed = removed.major < stored.major or
                (removed.major == stored.major and removed.minor <= stored.minor);
            if (was_removed) return false;
        }
        return true;
    }
};

// ============================================================
// § 4  Editable mixin  (compile-time-known structs)
// ============================================================

/// Converts a Zig type to an editable representation, accepting a host type and returning its editable form.
pub fn Editable(comptime Host: type) type {
    return struct {
        const Self = @This();

        /// Comptime-generated accessor table.
        /// Top-level fields produce one entry each; plain-data struct fields
        /// are flattened recursively with dot-notation names ("database.host").
        pub const accessors: [fieldCount()]Accessor = buildAccessors();

        /// O(1) name → index lookup backed by a perfect hash at comptime.
        /// Supports dot-notation keys for nested fields ("database.host").
        pub const accessor_map: std.StaticStringMap(usize) = buildMap();

        // ── comptime helpers ────────────────────────────────────────────────

        /// True if `T` is a plain data struct (no "editable" mixin field).
        /// Plain data structs are flattened; structs with `editable` are not.
        fn isPlainDataStruct(comptime T: type) bool {
            if (@typeInfo(T) != .@"struct") return false;
            for (std.meta.fields(T)) |f| {
                if (std.mem.eql(u8, f.name, "editable")) return false;
            }
            return true;
        }

        /// Count leaf fields recursively — plain data struct fields are
        /// expanded into their sub-fields (M5).
        fn countLeafFields(comptime T: type) usize {
            var n: usize = 0;
            for (std.meta.fields(T)) |f| {
                if (std.mem.eql(u8, f.name, "editable")) continue;
                if (isPlainDataStruct(f.type)) {
                    n += countLeafFields(f.type);
                } else {
                    n += 1;
                }
            }
            return n;
        }

        fn fieldCount() usize {
            return countLeafFields(Host);
        }

        /// Recursively fill `arr[i..]` with accessors for all leaf fields of
        /// type `T`, using `base_offset` as the byte offset from the Host root
        /// and `prefix` as the dot-notation parent path.
        fn fillLeafAccessors(
            comptime T: type,
            arr: []Accessor,
            i: *usize,
            comptime base_offset: usize,
            comptime prefix: []const u8,
        ) void {
            for (std.meta.fields(T)) |field| {
                if (std.mem.eql(u8, field.name, "editable")) continue;
                const name: []const u8 = comptime if (prefix.len > 0)
                    prefix ++ "." ++ field.name
                else
                    field.name;
                const offset: usize = comptime @offsetOf(T, field.name) + base_offset;

                if (isPlainDataStruct(field.type)) {
                    // Flatten nested plain-data struct recursively.
                    fillLeafAccessors(field.type, arr, i, offset, name);
                    continue;
                }

                // Allow the Host type to inject a custom vtable for any field via:
                //   pub fn fieldConstraint(comptime name: []const u8) ?*const ConstraintVTable
                const custom: ?*const ConstraintVTable = blk: {
                    if (@hasDecl(Host, "fieldConstraint")) {
                        break :blk Host.fieldConstraint(field.name);
                    }
                    break :blk null;
                };
                const vtable = custom orelse comptimeConstraint(field.type);

                // Allow the Host type to provide AI-readable metadata via:
                //   pub fn describeField(comptime name: []const u8) FieldMeta { ... }
                const meta: FieldMeta = blk: {
                    if (@hasDecl(T, "describeField")) {
                        break :blk T.describeField(field.name);
                    }
                    break :blk .{};
                };

                arr[i.*] = .{
                    .name = name,
                    .offset = offset,
                    .permissions = perm_all,
                    .constraint = vtable,
                    .type_tag = comptimeTypeTag(field.type),
                    .ownership = comptimeOwnership(field.type),
                    .binary_size = comptimeBinarySize(field.type),
                    .sql_type = SqlType.fromTypeTag(comptimeTypeTag(field.type)),
                    .meta = meta,
                };
                i.* += 1;
            }
        }

        fn buildAccessors() [fieldCount()]Accessor {
            var arr: [fieldCount()]Accessor = undefined;
            var i: usize = 0;
            fillLeafAccessors(Host, arr[0..], &i, 0, "");
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
        /// Plain-data structs (no "editable" field) are handled by flattening
        /// in fillLeafAccessors — this function is only called for leaf types.
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
        /// Errors: error.FieldNotFound, error.AccessDenied, ValidationError, plus any constraint parse errors.
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
            // M6: Run FieldMeta validation pipeline before type coercion.
            try validate_mod.validateValue(accessor.meta, value);
            const host_bytes: [*]u8 = @ptrCast(host);
            const field_ptr: *anyopaque = @ptrCast(host_bytes + accessor.offset);
            if (accessor.constraint.setCtxFn) |f|
                try f(accessor.constraint, allocator, field_ptr, value)
            else
                try accessor.constraint.setFn(allocator, field_ptr, value);
        }

        /// Validate all fields using their FieldMeta rules (M6).
        ///
        /// Reads the current value of each field and runs validateValue() on it.
        /// Returns the first error encountered, or void on success.
        pub fn validateAll(self: *const Self, allocator: std.mem.Allocator, role: Role) anyerror!void {
            for (accessors) |acc| {
                if (!acc.permissions.canRead(role)) continue;
                const host: *const Host = @alignCast(@fieldParentPtr("editable", self));
                const host_bytes: [*]const u8 = @ptrCast(host);
                const field_ptr: *const anyopaque = @ptrCast(host_bytes + acc.offset);
                const current_value = if (acc.constraint.getCtxFn) |f|
                    try f(acc.constraint, allocator, field_ptr)
                else
                    try acc.constraint.getFn(allocator, field_ptr);
                defer allocator.free(current_value);
                try validate_mod.validateValue(acc.meta, current_value);
            }
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
        pub fn describeSchema(allocator: std.mem.Allocator) ![]const u8 {
            return describeSchemaFromAccessors(allocator, @typeName(Host), &accessors);
        }

        /// Generate a list of field names for iteration.
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

/// Converts accessor metadata into a schema description using the provided allocator and accessors.
pub fn describeSchemaFromAccessors(
    allocator: std.mem.Allocator,
    type_name: []const u8,
    accessors_slice: []const Accessor,
) ![]const u8 {
    var buf: std.ArrayListUnmanaged(u8) = .empty;
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

/// Converts a Zig field description into a JSON-formatted string using an accessor and indentation.
fn describeFieldToJson(writer: anytype, accessor: Accessor, indent: usize) !void {
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

/// Converts a TypeTag to a JSON schema array of bytes, handling type metadata.
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

/// Describes role permissions by processing a list of operations and writing results to a writer.
fn describeRoleSet(writer: anytype, perms: RolePermissions, comptime operation: []const u8) !void {
    const role_names: [6][]const u8 = .{ "coder", "creator", "staff", "world", "tool", "user" };
    const roles: [6]Role = .{ .coder, .creator, .staff, .world, .tool, .user };
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

/// Converts a null-terminated byte slice into a Zig-safe string, handling escaped characters.
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
// § 5  DynamicEditable  (runtime-defined schemas, e.g. dynamic SQLite rows)
// ============================================================

/// Tracks dynamic editable fields; manages access control; owns runtime state.
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
    pub fn releaseAll(self: *DynamicEditable) void {
        for (self.accessors) |accessor| {
            if (accessor.constraint.releaseFn) |rel| {
                const field_ptr: *anyopaque = @ptrCast(self.buffer.ptr + accessor.offset);
                rel(self.allocator, field_ptr);
            }
        }
    }

    /// Set a field by name via string input, role-checked.
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

    /// Generate a JSON Schema describing all fields, types, constraints, and metadata.
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
    pub fn getFieldMeta(self: *const DynamicEditable, key: []const u8) ?FieldMeta {
        const index = self.name_map.get(key) orelse return null;
        return self.accessors[index].meta;
    }
};
