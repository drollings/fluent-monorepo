//! TreeSitterExtractor — walks tree-sitter syntax trees and extracts guidance members.
//!
//! Provides language-specific extraction logic for converting tree-sitter nodes
//! into guidance Member types.

const std = @import("std");
const c = @cImport({
    @cInclude("tree_sitter/api.h");
});
const types = @import("../types.zig");
const hash = @import("../hash.zig");

const Member = types.Member;
const MemberType = types.MemberType;
const Param = types.Param;

/// Extracts members from a tree-sitter syntax tree
pub const MemberExtractor = struct {
    allocator: std.mem.Allocator,
    language_name: []const u8,
    source: [:0]const u8,
    members: std.ArrayList(Member),

    const Self = @This();

    pub fn init(allocator: std.mem.Allocator, lang_name: []const u8, source: [:0]const u8) Self {
        return .{
            .allocator = allocator,
            .language_name = lang_name,
            .source = source,
            .members = .empty,
        };
    }

    pub fn deinit(self: *Self) void {
        self.members.deinit(self.allocator);
    }

    /// Extract all top-level members from the syntax tree
    pub fn extract(self: *Self, root: c.TSNode) ![]Member {
        try self.walkNode(root, 0);
        return try self.members.toOwnedSlice(self.allocator);
    }

    /// Recursively walk the syntax tree
    fn walkNode(self: *Self, node: c.TSNode, depth: usize) !void {
        if (c.ts_node_is_error(node)) return;

        const kind = c.ts_node_kind(node);
        const kind_str = std.mem.sliceTo(kind, 0);

        // Try to extract as member
        if (try self.extractMember(node, kind_str, depth)) {
            // Member extracted successfully
        }

        // Recurse into children
        const child_count = c.ts_node_named_child_count(node);
        var i: u32 = 0;
        while (i < child_count) : (i += 1) {
            const child = c.ts_node_named_child(node, i);
            try self.walkNode(child, depth + 1);
        }
    }

    /// Try to extract a member from a node
    fn extractMember(self: *Self, node: c.TSNode, kind: []const u8, depth: usize) !bool {
        _ = depth;
        const member_type = try self.getNodeType(kind);
        if (member_type == null) return false;

        const name = try self.getNodeName(node, kind) orelse return false;
        const line = c.ts_node_start_point(node).row + 1; // 1-based
        const signature = try self.getSignature(node, kind, member_type.?);

        // Compute match_hash
        const match_hash = try hash.apiHash(self.allocator, name, &.{}, null);

        // Extract parameters and return type for functions
        var params: []const Param = &.{};
        var returns: ?[]const u8 = null;
        if (member_type.? == .fn_decl or member_type.? == .method) {
            params = try self.extractParams(node, kind);
            returns = try self.extractReturnType(node, kind);
        }

        // Check visibility
        const is_pub = self.isPublic(node, kind);

        const member = Member{
            .type = member_type.?,
            .name = try self.allocator.dupe(u8, name),
            .line = @intCast(line),
            .signature = try self.allocator.dupe(u8, signature),
            .match_hash = match_hash,
            .params = params,
            .returns = returns,
            .comment = null, // Comments extracted separately
            .is_pub = is_pub,
            .members = &.{},
            .patterns = &.{},
            .tags = &.{},
            .skills = &.{},
            .capabilities = &.{},
            .equivalents = &.{},
            .comment_generated = false,
            .is_anchor = false,
        };

        try self.members.append(self.allocator, member);
        return true;
    }

    /// Get member type from node kind
    fn getNodeType(self: *const Self, kind: []const u8) !?MemberType {
        if (std.mem.eql(u8, self.language_name, "python")) {
            if (std.mem.eql(u8, kind, "function_definition")) return .fn_decl;
            if (std.mem.eql(u8, kind, "class_definition")) return .@"struct";
            return null;
        }

        if (std.mem.eql(u8, self.language_name, "cpp")) {
            if (std.mem.eql(u8, kind, "function_definition")) return .fn_decl;
            if (std.mem.eql(u8, kind, "class_specifier")) return .@"struct";
            if (std.mem.eql(u8, kind, "struct_specifier")) return .@"struct";
            if (std.mem.eql(u8, kind, "enum_specifier")) return .@"enum";
            if (std.mem.eql(u8, kind, "namespace_definition")) return .@"struct";
            return null;
        }

        if (std.mem.eql(u8, self.language_name, "rust")) {
            if (std.mem.eql(u8, kind, "function_item")) return .fn_decl;
            if (std.mem.eql(u8, kind, "struct_item")) return .@"struct";
            if (std.mem.eql(u8, kind, "enum_item")) return .@"enum";
            if (std.mem.eql(u8, kind, "impl_item")) return .@"struct";
            if (std.mem.eql(u8, kind, "trait_item")) return .@"struct";
            if (std.mem.eql(u8, kind, "mod_item")) return .@"struct";
            return null;
        }

        if (std.mem.eql(u8, self.language_name, "go")) {
            if (std.mem.eql(u8, kind, "function_declaration")) return .fn_decl;
            if (std.mem.eql(u8, kind, "method_declaration")) return .method;
            if (std.mem.eql(u8, kind, "type_spec")) return .@"struct";
            if (std.mem.eql(u8, kind, "type_declaration")) return .@"struct";
            if (std.mem.eql(u8, kind, "interface_type")) return .@"struct";
            return null;
        }

        if (std.mem.eql(u8, self.language_name, "typescript") or std.mem.eql(u8, self.language_name, "tsx")) {
            if (std.mem.eql(u8, kind, "function_declaration")) return .fn_decl;
            if (std.mem.eql(u8, kind, "arrow_function")) return .fn_decl;
            if (std.mem.eql(u8, kind, "class_declaration")) return .@"struct";
            if (std.mem.eql(u8, kind, "interface_declaration")) return .@"struct";
            if (std.mem.eql(u8, kind, "type_alias_declaration")) return .@"struct";
            if (std.mem.eql(u8, kind, "enum_declaration")) return .@"enum";
            return null;
        }

        if (std.mem.eql(u8, self.language_name, "php")) {
            if (std.mem.eql(u8, kind, "function_definition")) return .fn_decl;
            if (std.mem.eql(u8, kind, "class_declaration")) return .@"struct";
            if (std.mem.eql(u8, kind, "interface_declaration")) return .@"struct";
            if (std.mem.eql(u8, kind, "trait_declaration")) return .@"struct";
            return null;
        }

        return null;
    }

    /// Get member name from node
    fn getNodeName(self: *const Self, node: c.TSNode, kind: []const u8) !?[]const u8 {
        // Language-specific name extraction
        if (std.mem.eql(u8, self.language_name, "python")) {
            if (std.mem.eql(u8, kind, "function_definition")) {
                const name_node = c.ts_node_named_child(node, 0);
                return try self.nodeToSlice(name_node);
            }
            if (std.mem.eql(u8, kind, "class_definition")) {
                const name_node = c.ts_node_named_child(node, 0);
                return try self.nodeToSlice(name_node);
            }
        }

        // Generic: look for identifier child
        const child_count = c.ts_node_named_child_count(node);
        var i: u32 = 0;
        while (i < child_count) : (i += 1) {
            const child = c.ts_node_named_child(node, i);
            const child_kind = std.mem.sliceTo(c.ts_node_kind(child), 0);
            if (std.mem.eql(u8, child_kind, "identifier")) {
                return try self.nodeToSlice(child);
            }
        }

        return null;
    }

    /// Get member signature
    fn getSignature(self: *const Self, node: c.TSNode, kind: []const u8, mtype: MemberType) ![]const u8 {
        var buf: std.ArrayList(u8) = .empty;
        errdefer buf.deinit(self.allocator);

        const name = try self.getNodeName(node, kind) orelse "unknown";

        switch (mtype) {
            .fn_decl, .method, .fn_private, .method_private => {
                try buf.appendSlice(self.allocator, "fn ");
                try buf.appendSlice(self.allocator, name);
                try buf.appendSlice(self.allocator, "(...)");
            },
            .@"struct" => {
                try buf.appendSlice(self.allocator, "struct ");
                try buf.appendSlice(self.allocator, name);
                try buf.appendSlice(self.allocator, " { ... }");
            },
            .@"enum" => {
                try buf.appendSlice(self.allocator, "enum ");
                try buf.appendSlice(self.allocator, name);
                try buf.appendSlice(self.allocator, " { ... }");
            },
            else => {
                try buf.appendSlice(self.allocator, name);
            },
        }

        return try buf.toOwnedSlice(self.allocator);
    }

    /// Extract function parameters
    fn extractParams(self: *const Self, node: c.TSNode, kind: []const u8) ![]const Param {
        _ = node;
        _ = kind;
        // TODO: Implement per-language parameter extraction
        return try self.allocator.dupe(Param, &.{});
    }

    /// Extract return type
    fn extractReturnType(self: *const Self, node: c.TSNode, kind: []const u8) !?[]const u8 {
        _ = self;
        _ = node;
        _ = kind;
        // TODO: Implement per-language return type extraction
        return null;
    }

    /// Check if member is public
    fn isPublic(self: *const Self, node: c.TSNode, kind: []const u8) bool {
        // Default to public; override for languages with visibility modifiers
        _ = kind;

        if (std.mem.eql(u8, self.language_name, "rust")) {
            // Check for 'pub' keyword in visibility_modifier child
            const child_count = c.ts_node_child_count(node);
            var i: u32 = 0;
            while (i < child_count) : (i += 1) {
                const child = c.ts_node_child(node, i);
                const child_kind = std.mem.sliceTo(c.ts_node_kind(child), 0);
                if (std.mem.eql(u8, child_kind, "visibility_modifier")) {
                    const start = c.ts_node_start_byte(child);
                    const end = c.ts_node_end_byte(child);
                    const text = self.source[start..end];
                    if (std.mem.indexOf(u8, text, "pub") != null) return true;
                }
            }
            return false;
        }

        if (std.mem.eql(u8, self.language_name, "cpp")) {
            // C++ visibility tracking is complex; default to public for now
            return true;
        }

        // Python, Go, TypeScript, PHP: all members are public by default
        return true;
    }

    /// Convert tree-sitter node to owned slice
    fn nodeToSlice(self: *const Self, node: c.TSNode) ![]const u8 {
        const start_byte = c.ts_node_start_byte(node);
        const end_byte = c.ts_node_end_byte(node);
        return try self.allocator.dupe(u8, self.source[start_byte..end_byte]);
    }
};
