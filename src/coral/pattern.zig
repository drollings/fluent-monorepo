const std = @import("std");
const string = @import("common");

/// Pattern type classification.
pub const PatternType = enum {
    Domain,
    GoF,
};

/// A detected design pattern with name, type, and optional skill reference.
pub const Pattern = struct {
    name: []const u8,
    type: PatternType,
    ref: ?[]const u8 = null,
};

/// Detect design patterns from a Zig AST node's source text.
/// Uses text-based heuristics analogous to the Python PatternDetector.
/// Both the tree and the node index are used to extract source text for inspection.
pub fn detectPatterns(allocator: std.mem.Allocator, tree: *const std.zig.Ast, node: std.zig.Ast.Node.Index) ![]Pattern {
    var patterns: std.ArrayList(Pattern) = .{};
    errdefer {
        for (patterns.items) |p| {
            allocator.free(p.name);
            if (p.ref) |r| allocator.free(r);
        }
        patterns.deinit(allocator);
    }

    // Obtain full source text of this node for text-based heuristics.
    const node_source = tree.getNodeSource(node);

    // --- Domain Patterns ---

    if (detectRingBuffer(node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Ring Buffer"),
            .type = .Domain,
            .ref = try allocator.dupe(u8, ".guidance/skills/domain_patterns/SKILL.md#ring-buffer"),
        });
    }

    if (detectStatePersistence(node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "State Persistence"),
            .type = .Domain,
            .ref = try allocator.dupe(u8, ".guidance/skills/domain_patterns/SKILL.md#state-persistence"),
        });
    }

    // --- GoF Creational Patterns ---

    if (detectFactory(tree, node, node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Factory (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, ".guidance/skills/gof-patterns/SKILL.md#factory"),
        });
    }

    if (detectSingleton(tree, node, node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Singleton (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, ".guidance/skills/gof-patterns/SKILL.md#singleton"),
        });
    }

    if (detectBuilder(tree, node, node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Builder (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, ".guidance/skills/gof-patterns/SKILL.md#builder"),
        });
    }

    // --- GoF Structural Patterns ---

    if (detectAdapter(tree, node, node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Adapter (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, ".guidance/skills/gof-patterns/SKILL.md#adapter"),
        });
    }

    if (detectDecorator(node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Decorator (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, ".guidance/skills/gof-patterns/SKILL.md#decorator"),
        });
    }

    if (detectProxy(node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Proxy (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, ".guidance/skills/gof-patterns/SKILL.md#proxy"),
        });
    }

    // --- GoF Behavioral Patterns ---

    if (detectStrategy(tree, node, node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Strategy (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, ".guidance/skills/gof-patterns/SKILL.md#strategy"),
        });
    }

    if (detectObserver(tree, node, node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Observer (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, ".guidance/skills/gof-patterns/SKILL.md#observer"),
        });
    }

    if (detectTemplateMethod(tree, node, node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Template Method (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, ".guidance/skills/gof-patterns/SKILL.md#template-method"),
        });
    }

    return patterns.toOwnedSlice(allocator);
}

/// Detects pattern names in a Zig AST node, returning a slice of byte slices.
pub fn detectPatternNames(allocator: std.mem.Allocator, tree: *const std.zig.Ast, node: std.zig.Ast.Node.Index) ![][]const u8 {
    const full_patterns = try detectPatterns(allocator, tree, node);
    defer {
        for (full_patterns) |p| {
            allocator.free(p.name);
            if (p.ref) |r| allocator.free(r);
        }
        allocator.free(full_patterns);
    }

    var names: std.ArrayList([]const u8) = .{};
    for (full_patterns) |p| {
        try names.append(allocator, try allocator.dupe(u8, p.name));
    }
    return names.toOwnedSlice(allocator);
}

// ---------------------------------------------------------------------------
// Domain Patterns
// ---------------------------------------------------------------------------

fn detectRingBuffer(source: []const u8) bool {
    const keywords = [_][]const u8{ "ring", "ringbuffer", "circular", "fifo", "deque" };
    return string.containsAnyWord(source, &keywords);
}

fn detectStatePersistence(source: []const u8) bool {
    const keywords = [_][]const u8{ "self.state", ".state =", "state: State", "state: enum" };
    return string.containsAny(source, &keywords);
}

// ---------------------------------------------------------------------------
// GoF Creational
// ---------------------------------------------------------------------------

fn detectFactory(tree: *const std.zig.Ast, node: std.zig.Ast.Node.Index, source: []const u8) bool {
    _ = tree;
    _ = node;
    const keywords = [_][]const u8{ "factory", "Factory", "fn create", "fn make", "pub fn create", "pub fn make" };
    return string.containsAny(source, &keywords);
}

fn detectSingleton(tree: *const std.zig.Ast, node: std.zig.Ast.Node.Index, source: []const u8) bool {
    _ = tree;
    _ = node;
    const instance_field = string.containsIgnoreCase(source, "_instance");
    const accessor = string.containsAny(source, &[_][]const u8{ "getInstance", "get_instance", "fn instance(" });
    return instance_field or accessor;
}

fn detectBuilder(tree: *const std.zig.Ast, node: std.zig.Ast.Node.Index, source: []const u8) bool {
    _ = tree;
    _ = node;
    const has_builder = string.containsIgnoreCase(source, "builder");
    const has_build = string.containsAny(source, &[_][]const u8{ "fn build(", "pub fn build(" });
    if (has_builder and has_build) return true;
    const returns_self = blk: {
        var count: u32 = 0;
        var i: usize = 0;
        while (i < source.len) {
            if (std.mem.indexOf(u8, source[i..], "return self;")) |off| {
                count += 1;
                i += off + 1;
            } else break;
        }
        break :blk count >= 2;
    };
    return returns_self and has_build;
}

// ---------------------------------------------------------------------------
// GoF Structural
// ---------------------------------------------------------------------------

fn detectAdapter(tree: *const std.zig.Ast, node: std.zig.Ast.Node.Index, source: []const u8) bool {
    _ = tree;
    _ = node;
    const keywords = [_][]const u8{ "fn adapt", "fn convert", "fn transform", "fn to_", "fn from_", "pub fn to_", "pub fn from_" };
    return string.containsAny(source, &keywords);
}

fn detectDecorator(source: []const u8) bool {
    const wrapped_field = string.containsAny(source, &[_][]const u8{ "wrapped:", "component:", "_inner:", "wrappee:" });
    if (!wrapped_field) return false;
    const delegates = string.containsAny(source, &[_][]const u8{ "self.wrapped.", "self.component.", "self._inner.", "self.wrappee." });
    return delegates;
}

fn detectProxy(source: []const u8) bool {
    const has_subject = string.containsAny(source, &[_][]const u8{ "_real:", "_subject:", "_target:", "_delegate:", "_proxied:" });
    if (!has_subject) return false;
    const access_signals = [_][]const u8{ "cache", "lazy", "permission", "auth", "log", "throttl", "rate_limit", "check" };
    return string.containsAny(source, &access_signals);
}

// ---------------------------------------------------------------------------
// GoF Behavioral
// ---------------------------------------------------------------------------

fn detectStrategy(tree: *const std.zig.Ast, node: std.zig.Ast.Node.Index, source: []const u8) bool {
    _ = tree;
    _ = node;
    const has_strategy_attr = string.containsAny(source, &[_][]const u8{ "strategy:", "algorithm:", "_strategy:", "_algorithm:" });
    const has_executor = string.containsAny(source, &[_][]const u8{ "fn execute(", "fn run(", "fn apply(", "fn calculate(", "fn compute(", "fn perform(" });
    return has_strategy_attr and has_executor;
}

fn detectObserver(tree: *const std.zig.Ast, node: std.zig.Ast.Node.Index, source: []const u8) bool {
    _ = tree;
    _ = node;
    const has_subscribe = string.containsAny(source, &[_][]const u8{ "fn attach(", "fn subscribe(", "fn add_listener(", "fn add_observer(", "fn register(" });
    const has_notify = string.containsAny(source, &[_][]const u8{ "fn notify(", "fn emit(", "fn dispatch(", "fn publish(", "fn trigger(", "fn fire(" });
    if (has_notify and has_subscribe) return true;
    const has_collection = string.containsAny(source, &[_][]const u8{ "observers:", "listeners:", "subscribers:", "_handlers:" });
    return has_collection and has_notify;
}

fn detectTemplateMethod(tree: *const std.zig.Ast, node: std.zig.Ast.Node.Index, source: []const u8) bool {
    _ = tree;
    _ = node;
    const has_unreachable = string.containsIgnoreCase(source, "unreachable");
    const calls_hooks = blk: {
        var count: u32 = 0;
        var i: usize = 0;
        while (i < source.len) {
            if (std.mem.indexOf(u8, source[i..], "self._")) |off| {
                count += 1;
                i += off + 1;
            } else break;
        }
        break :blk count >= 2;
    };
    return has_unreachable and calls_hooks;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "detectRingBuffer" {
    try std.testing.expect(detectRingBuffer("pub const RingBuffer = struct { ... }"));
    try std.testing.expect(detectRingBuffer("// circular queue implementation"));
    try std.testing.expect(!detectRingBuffer("pub fn add(a: i32, b: i32) i32 { return a + b; }"));
}

test "detectFactory" {
    try std.testing.expect(detectFactory(undefined, undefined, "pub fn create(alloc: Allocator) !*Foo { ... }"));
    try std.testing.expect(detectFactory(undefined, undefined, "pub const FooFactory = struct {}"));
    try std.testing.expect(!detectFactory(undefined, undefined, "pub fn parse(input: []u8) !Foo {}"));
}

test "detectObserver" {
    try std.testing.expect(detectObserver(
        undefined,
        undefined,
        "fn subscribe(self: *Self, cb: Callback) void {} fn notify(self: *Self) void {}",
    ));
    try std.testing.expect(!detectObserver(undefined, undefined, "fn compute(x: f64) f64 { return x; }"));
}

