const std = @import("std");
const common = @import("common");
const pattern = common.pattern;

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
/// Uses text-based heuristics from common.pattern module.
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

    const node_source = tree.getNodeSource(node);

    if (pattern.detectRingBuffer(node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Ring Buffer"),
            .type = .Domain,
            .ref = try allocator.dupe(u8, ".guidance/skills/domain_patterns/SKILL.md#ring-buffer"),
        });
    }

    if (pattern.detectStatePersistence(node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "State Persistence"),
            .type = .Domain,
            .ref = try allocator.dupe(u8, ".guidance/skills/domain_patterns/SKILL.md#state-persistence"),
        });
    }

    if (pattern.detectFactory(tree, node, node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Factory (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, ".guidance/skills/gof-patterns/SKILL.md#factory"),
        });
    }

    if (pattern.detectSingleton(tree, node, node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Singleton (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, ".guidance/skills/gof-patterns/SKILL.md#singleton"),
        });
    }

    if (pattern.detectBuilder(tree, node, node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Builder (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, ".guidance/skills/gof-patterns/SKILL.md#builder"),
        });
    }

    if (pattern.detectAdapter(tree, node, node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Adapter (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, ".guidance/skills/gof-patterns/SKILL.md#adapter"),
        });
    }

    if (pattern.detectDecorator(node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Decorator (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, ".guidance/skills/gof-patterns/SKILL.md#decorator"),
        });
    }

    if (pattern.detectProxy(node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Proxy (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, ".guidance/skills/gof-patterns/SKILL.md#proxy"),
        });
    }

    if (pattern.detectStrategy(tree, node, node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Strategy (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, ".guidance/skills/gof-patterns/SKILL.md#strategy"),
        });
    }

    if (pattern.detectObserver(tree, node, node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Observer (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, ".guidance/skills/gof-patterns/SKILL.md#observer"),
        });
    }

    if (pattern.detectTemplateMethod(tree, node, node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Template Method (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, ".guidance/skills/gof-patterns/SKILL.md#template-method"),
        });
    }

    return try patterns.toOwnedSlice(allocator);
}
