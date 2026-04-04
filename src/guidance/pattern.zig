const std = @import("std");
const types = @import("types.zig");
const common = @import("common");
const pattern = common.pattern;

/// Analyzes a Zig AST node to detect patterns using the provided allocator.
pub fn detectPatterns(allocator: std.mem.Allocator, tree: *const std.zig.Ast, node: std.zig.Ast.Node.Index) ![]types.Pattern {
    var patterns: std.ArrayList(types.Pattern) = .{};
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
            .ref = try allocator.dupe(u8, "guidance/skills/domain_patterns/SKILL.md#ring-buffer"),
        });
    }

    if (pattern.detectStatePersistence(node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "State Persistence"),
            .type = .Domain,
            .ref = try allocator.dupe(u8, "guidance/skills/domain_patterns/SKILL.md#state-persistence"),
        });
    }

    if (pattern.detectFactory(tree, node, node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Factory (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, "guidance/skills/gof-patterns/SKILL.md#factory"),
        });
    }

    if (pattern.detectSingleton(tree, node, node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Singleton (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, "guidance/skills/gof-patterns/SKILL.md#singleton"),
        });
    }

    if (pattern.detectBuilder(tree, node, node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Builder (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, "guidance/skills/gof-patterns/SKILL.md#builder"),
        });
    }

    if (pattern.detectAdapter(tree, node, node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Adapter (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, "guidance/skills/gof-patterns/SKILL.md#adapter"),
        });
    }

    if (pattern.detectDecorator(node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Decorator (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, "guidance/skills/gof-patterns/SKILL.md#decorator"),
        });
    }

    if (pattern.detectProxy(node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Proxy (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, "guidance/skills/gof-patterns/SKILL.md#proxy"),
        });
    }

    if (pattern.detectStrategy(tree, node, node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Strategy (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, "guidance/skills/gof-patterns/SKILL.md#strategy"),
        });
    }

    if (pattern.detectObserver(tree, node, node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Observer (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, "guidance/skills/gof-patterns/SKILL.md#observer"),
        });
    }

    if (pattern.detectTemplateMethod(tree, node, node_source)) {
        try patterns.append(allocator, .{
            .name = try allocator.dupe(u8, "Template Method (GoF)"),
            .type = .GoF,
            .ref = try allocator.dupe(u8, "guidance/skills/gof-patterns/SKILL.md#template-method"),
        });
    }

    return try patterns.toOwnedSlice(allocator);
}

/// Detects pattern names in a Zig AST node using an allocator and returns their slice.
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
