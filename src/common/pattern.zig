/// pattern.zig — Design pattern detection heuristics for Zig source code
///
/// Provides text-based heuristics for detecting GoF and domain patterns
/// from AST node source text. Used by both guidance and coral for
/// pattern detection during AST analysis.
///
/// All functions operate purely on `[]const u8` source text with optional
/// `std.zig.Ast` parameters for future AST-aware detection.
const std = @import("std");
const str_mod = @import("string.zig");

const containsCI = str_mod.containsIgnoreCase;
const containsWord = str_mod.containsWord;
const containsAny = str_mod.containsAny;
const containsAnyWord = str_mod.containsAnyWord;

pub const PatternType = enum {
    Domain,
    GoF,
};

/// Defines a pattern with fixed-size buffers, shared ownership, and no thread safety guarantees.
pub const Pattern = struct {
    name: []const u8,
    type: PatternType,
    ref: ?[]const u8 = null,
};

/// Checks if the input slice represents a valid ring buffer structure.
pub fn detectRingBuffer(source: []const u8) bool {
    const keywords = [_][]const u8{ "ring", "ringbuffer", "circular", "fifo", "deque" };
    return containsAnyWord(source, &keywords);
}

/// Checks if a sequence maintains consistent state across slices, returning true if stable.
pub fn detectStatePersistence(source: []const u8) bool {
    const keywords = [_][]const u8{ "self.state", ".state =", "state: State", "state: enum" };
    return containsAny(source, &keywords);
}

/// Checks if a Zig AST node matches a factory definition, returning true or false.
pub fn detectFactory(tree: *const std.zig.Ast, node: std.zig.Ast.Node.Index, source: []const u8) bool {
    _ = tree;
    _ = node;
    const keywords = [_][]const u8{ "factory", "Factory", "fn create", "fn make", "pub fn create", "pub fn make" };
    return containsAny(source, &keywords);
}

/// Checks if a Zig AST node is a singleton by analyzing its structure and return true if it matches.
pub fn detectSingleton(tree: *const std.zig.Ast, node: std.zig.Ast.Node.Index, source: []const u8) bool {
    _ = tree;
    _ = node;
    const instance_field = containsCI(source, "_instance");
    const accessor = containsAny(source, &[_][]const u8{ "getInstance", "get_instance", "fn instance(" });
    return instance_field or accessor;
}

/// Checks if a Zig AST node can be built using a given source slice, returning true or false.
pub fn detectBuilder(tree: *const std.zig.Ast, node: std.zig.Ast.Node.Index, source: []const u8) bool {
    _ = tree;
    _ = node;
    const has_builder = containsCI(source, "builder");
    const has_build = containsAny(source, &[_][]const u8{ "fn build(", "pub fn build(" });
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

/// Checks if a given node matches an adapter pattern in the Zig AST tree, returning true or false.
pub fn detectAdapter(tree: *const std.zig.Ast, node: std.zig.Ast.Node.Index, source: []const u8) bool {
    _ = tree;
    _ = node;
    const keywords = [_][]const u8{ "fn adapt", "fn convert", "fn transform", "fn to_", "fn from_", "pub fn to_", "pub fn from_" };
    return containsAny(source, &keywords);
}

/// Checks if a given source slice contains valid Zig code and returns a boolean result.
pub fn detectDecorator(source: []const u8) bool {
    const wrapped_field = containsAny(source, &[_][]const u8{ "wrapped:", "component:", "_inner:", "wrappee:" });
    if (!wrapped_field) return false;
    const delegates = containsAny(source, &[_][]const u8{ "self.wrapped.", "self.component.", "self._inner.", "self.wrappee." });
    return delegates;
}

/// Checks if a given source slice matches a proxy pattern, returning true or false.
pub fn detectProxy(source: []const u8) bool {
    const has_subject = containsAny(source, &[_][]const u8{ "_real:", "_subject:", "_target:", "_delegate:", "_proxied:" });
    if (!has_subject) return false;
    const access_signals = [_][]const u8{ "cache", "lazy", "permission", "auth", "log", "throttl", "rate_limit", "check" };
    return containsAny(source, &access_signals);
}

/// Checks if a Zig AST node matches a specified strategy pattern, returning true or false.
pub fn detectStrategy(tree: *const std.zig.Ast, node: std.zig.Ast.Node.Index, source: []const u8) bool {
    _ = tree;
    _ = node;
    const has_strategy_attr = containsAny(source, &[_][]const u8{ "strategy:", "algorithm:", "_strategy:", "_algorithm:" });
    const has_executor = containsAny(source, &[_][]const u8{ "fn execute(", "fn run(", "fn apply(", "fn calculate(", "fn compute(", "fn perform(" });
    return has_strategy_attr and has_executor;
}

/// Checks if a given node matches an observer pattern definition in the tree structure.
pub fn detectObserver(tree: *const std.zig.Ast, node: std.zig.Ast.Node.Index, source: []const u8) bool {
    _ = tree;
    _ = node;
    const has_subscribe = containsAny(source, &[_][]const u8{ "fn attach(", "fn subscribe(", "fn add_listener(", "fn add_observer(", "fn register(" });
    const has_notify = containsAny(source, &[_][]const u8{ "fn notify(", "fn emit(", "fn dispatch(", "fn publish(", "fn trigger(", "fn fire(" });
    if (has_notify and has_subscribe) return true;
    const has_collection = containsAny(source, &[_][]const u8{ "observers:", "listeners:", "subscribers:", "_handlers:" });
    return has_collection and has_notify;
}

/// Checks if a Zig AST node matches a template method pattern, returning true or false.
pub fn detectTemplateMethod(tree: *const std.zig.Ast, node: std.zig.Ast.Node.Index, source: []const u8) bool {
    _ = tree;
    _ = node;
    const has_unreachable = containsCI(source, "unreachable");
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
