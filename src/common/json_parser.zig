const std = @import("std");
const Target = @import("target.zig").Target;
const TargetType = @import("target.zig").TargetType;
const TargetRegistry = @import("registry.zig").TargetRegistry;
const StringInterner = @import("interner.zig").StringInterner;

pub const ParseError = error{
    InvalidJson,
    MissingName,
    InvalidTargetType,
    IoError,
    OutOfMemory,
};

/// Reads a file from the given path and parses its contents into a Zig-safe data structure.
pub fn parseFile(
    allocator: std.mem.Allocator,
    path: []const u8,
    registry: *TargetRegistry,
    interner: *StringInterner,
) !void {
    const file = std.fs.cwd().openFile(path, .{}) catch {
        return ParseError.IoError;
    };
    defer file.close();

    const contents = file.readToEndAlloc(allocator, std.math.maxInt(usize)) catch |err| {
        std.log.warn("Failed to read file '{s}': {}", .{ path, err });
        return ParseError.IoError;
    };
    defer allocator.free(contents);

    try parseJson(allocator, contents, registry, interner);
}

/// Converts a C-style JSON array into a Zig-safe slice, handling allocations and string interners.
pub fn parseJson(
    allocator: std.mem.Allocator,
    json_text: []const u8,
    registry: *TargetRegistry,
    interner: *StringInterner,
) !void {
    const parsed = std.json.parseFromSlice(
        std.json.Value,
        allocator,
        json_text,
        .{},
    ) catch |err| {
        std.log.debug("Failed to parse JSON: {}", .{err});
        return ParseError.InvalidJson;
    };
    defer parsed.deinit();

    const root = parsed.value;

    if (root != .object) {
        std.log.debug("JSON root must be an object", .{});
        return ParseError.InvalidJson;
    }

    var iter = root.object.iterator();
    while (iter.next()) |entry| {
        const name = entry.key_ptr.*;
        const value = entry.value_ptr.*;

        try parseTarget(allocator, name, value, registry, interner);
    }
}

/// Interprets a JSON string slice into a Zig target structure, handling allocator and registry parameters.
fn parseTarget(
    allocator: std.mem.Allocator,
    name: []const u8,
    value: std.json.Value,
    registry: *TargetRegistry,
    interner: *StringInterner,
) !void {
    if (value != .object) {
        std.log.warn("Target '{s}' must be an object", .{name});
        return;
    }

    const target_type = determineTargetType(value.object);

    var target = try allocator.create(Target);
    errdefer allocator.destroy(target);

    // Target.init always dupes the name; deinit always frees it
    target.* = try Target.init(allocator, interner, name, target_type);
    errdefer target.deinit(allocator);

    if (value.object.get("depends")) |depends_val| {
        if (depends_val == .array) {
            var dep_names: std.ArrayList([]const u8) = .{};
            defer dep_names.deinit(allocator);

            for (depends_val.array.items) |dep| {
                if (dep == .string) {
                    try dep_names.append(allocator, dep.string);
                }
            }

            try target.setDepends(allocator, interner, dep_names.items);
        }
    }

    if (value.object.get("provides")) |provides_val| {
        if (provides_val == .array) {
            var prov_names: std.ArrayList([]const u8) = .{};
            defer prov_names.deinit(allocator);

            for (provides_val.array.items) |prov| {
                if (prov == .string) {
                    try prov_names.append(allocator, prov.string);
                }
            }

            try target.setProvides(allocator, interner, prov_names.items);
        }
    }

    if (value.object.get("commands")) |commands_val| {
        if (commands_val == .array) {
            for (commands_val.array.items) |cmd| {
                if (cmd == .string) {
                    const owned_cmd = try allocator.dupe(u8, cmd.string);
                    try target.addCommand(allocator, owned_cmd);
                }
            }
        }
    }

    if (value.object.get("exists")) |exists_val| {
        if (exists_val == .string) {
            target.exists = try allocator.dupe(u8, exists_val.string);
        }
    }

    if (value.object.get("essential")) |essential_val| {
        if (essential_val == .bool) {
            target.essential = essential_val.bool;
        }
    }

    if (value.object.get("check_mtime")) |mtime_val| {
        if (mtime_val == .bool) {
            target.check_mtime = mtime_val.bool;
        }
    }

    try registry.add(target);
}

/// Determines the target type from a JSON object map, returning a TargetType or error.
fn determineTargetType(obj: std.json.ObjectMap) TargetType {
    if (obj.get("phony")) |val| {
        if (val == .bool and val.bool) {
            return .phony;
        }
    }

    if (obj.get("exists")) |val| {
        if (val == .string) {
            return .file;
        }
    }

    if (obj.get("commands")) |val| {
        if (val == .array and val.array.items.len > 0) {
            return .command;
        }
    }

    return .abstract;
}

/// Writes a Zig example string to memory using the provided allocator.
pub fn writeExample(allocator: std.mem.Allocator, path: []const u8) !void {
    _ = allocator;
    const file = std.fs.cwd().createFile(path, .{}) catch |err| {
        std.log.warn("Failed to create file '{s}': {}", .{ path, err });
        return ParseError.IoError;
    };
    defer file.close();

    const example =
        \\{
        \\  "default": {
        \\    "depends": ["build"],
        \\    "phony": true
        \\  },
        \\  "build": {
        \\    "depends": ["compile", "link"],
        \\    "phony": true
        \\  },
        \\  "compile": {
        \\    "depends": ["src/main.c"],
        \\    "provides": ["object"],
        \\    "commands": ["gcc -c src/main.c -o build/main.o"],
        \\    "exists": "build/main.o",
        \\    "check_mtime": true
        \\  },
        \\  "link": {
        \\    "depends": ["object"],
        \\    "commands": ["gcc build/main.o -o build/app"],
        \\    "exists": "build/app",
        \\    "check_mtime": true
        \\  },
        \\  "src/main.c": {
        \\    "exists": "src/main.c",
        \\    "essential": true
        \\  },
        \\  "clean": {
        \\    "commands": ["rm -rf build/"],
        \\    "phony": true
        \\  },
        \\  "all": {
        \\    "depends": ["default"],
        \\    "phony": true
        \\  }
        \\}
    ;

    try file.writeAll(example);
}

const testing = std.testing;

test "JSON parsing" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();

    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const json =
        \\{
        \\  "build": {
        \\    "depends": ["compile"],
        \\    "phony": true
        \\  },
        \\  "compile": {
        \\    "commands": ["gcc -c main.c"],
        \\    "exists": "main.o"
        \\  }
        \\}
    ;

    try parseJson(testing.allocator, json, &registry, &interner);

    try testing.expectEqual(@as(usize, 2), registry.count());
    try testing.expect(registry.get("build") != null);
    try testing.expect(registry.get("compile") != null);
}

test "JSON parsing GPA no leaks" {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    const allocator = gpa.allocator();

    {
        var interner = StringInterner.init(allocator);
        defer interner.deinit();

        var registry = TargetRegistry.init(allocator, &interner);
        defer registry.deinit();

        const json =
            \\{
            \\  "build": {
            \\    "depends": ["link"],
            \\    "phony": true
            \\  },
            \\  "link": {
            \\    "depends": ["compile"],
            \\    "commands": ["ld -o app obj.o"],
            \\    "exists": "app"
            \\  },
            \\  "compile": {
            \\    "commands": ["cc -c main.c"],
            \\    "exists": "obj.o"
            \\  }
            \\}
        ;

        try parseJson(allocator, json, &registry, &interner);

        try testing.expectEqual(@as(usize, 3), registry.count());
        try testing.expect(registry.get("build") != null);
        try testing.expect(registry.get("link") != null);
        try testing.expect(registry.get("compile") != null);
    }

    try testing.expectEqual(.ok, gpa.deinit());
}

test "JSON parsing: invalid JSON returns error" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const result = parseJson(testing.allocator, "this is not json", &registry, &interner);
    try testing.expectError(ParseError.InvalidJson, result);
}

test "JSON parsing: non-object root returns error" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const result = parseJson(testing.allocator, "[1, 2, 3]", &registry, &interner);
    try testing.expectError(ParseError.InvalidJson, result);
}

test "JSON parsing: empty object produces zero targets" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    try parseJson(testing.allocator, "{}", &registry, &interner);
    try testing.expectEqual(@as(usize, 0), registry.count());
}

test "JSON parsing: target type detection — phony" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const json =
        \\{"p": {"phony": true}}
    ;
    try parseJson(testing.allocator, json, &registry, &interner);

    const t = registry.get("p");
    try testing.expect(t != null);
    try testing.expectEqual(TargetType.phony, t.?.target_type);
}

test "JSON parsing: target type detection — file (exists key)" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const json =
        \\{"f": {"exists": "out/foo.o"}}
    ;
    try parseJson(testing.allocator, json, &registry, &interner);

    const t = registry.get("f");
    try testing.expect(t != null);
    try testing.expectEqual(TargetType.file, t.?.target_type);
    try testing.expectEqualStrings("out/foo.o", t.?.exists.?);
}

test "JSON parsing: target type detection — command (commands key)" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const json =
        \\{"c": {"commands": ["echo hi"]}}
    ;
    try parseJson(testing.allocator, json, &registry, &interner);

    const t = registry.get("c");
    try testing.expect(t != null);
    try testing.expectEqual(TargetType.command, t.?.target_type);
    try testing.expectEqual(@as(usize, 1), t.?.commands.items.len);
    try testing.expectEqualStrings("echo hi", t.?.commands.items[0]);
}

test "JSON parsing: target type detection — abstract (no distinguishing key)" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const json =
        \\{"a": {}}
    ;
    try parseJson(testing.allocator, json, &registry, &interner);

    const t = registry.get("a");
    try testing.expect(t != null);
    try testing.expectEqual(TargetType.abstract, t.?.target_type);
}

test "JSON parsing: essential and check_mtime fields" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const json =
        \\{
        \\  "src": {
        \\    "exists": "src/main.c",
        \\    "essential": true,
        \\    "check_mtime": true
        \\  }
        \\}
    ;
    try parseJson(testing.allocator, json, &registry, &interner);

    const t = registry.get("src");
    try testing.expect(t != null);
    try testing.expect(t.?.essential);
    try testing.expect(t.?.check_mtime);
}

test "JSON parsing: depends and provides are recorded" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const json =
        \\{
        \\  "dep": {},
        \\  "cap": {},
        \\  "worker": {
        \\    "depends": ["dep"],
        \\    "provides": ["cap"],
        \\    "commands": ["make"]
        \\  }
        \\}
    ;
    try parseJson(testing.allocator, json, &registry, &interner);

    const worker = registry.get("worker");
    try testing.expect(worker != null);
    try testing.expectEqual(@as(usize, 1), worker.?.depends.count());
    try testing.expectEqual(@as(usize, 1), worker.?.provides.count());
}

test "JSON parsing: multiple commands are all stored" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const json =
        \\{"multi": {"commands": ["step1", "step2", "step3"]}}
    ;
    try parseJson(testing.allocator, json, &registry, &interner);

    const t = registry.get("multi");
    try testing.expect(t != null);
    try testing.expectEqual(@as(usize, 3), t.?.commands.items.len);
    try testing.expectEqualStrings("step1", t.?.commands.items[0]);
    try testing.expectEqualStrings("step3", t.?.commands.items[2]);
}

test "JSON parsing: parseFile returns IoError for missing file" {
    var interner = StringInterner.init(testing.allocator);
    defer interner.deinit();
    var registry = TargetRegistry.init(testing.allocator, &interner);
    defer registry.deinit();

    const result = parseFile(testing.allocator, "/nonexistent/path/coral.json", &registry, &interner);
    try testing.expectError(ParseError.IoError, result);
}





