//! PluginRegistry — maps file extensions to LanguagePlugin descriptors.
//!
//! The registry is populated at startup with built-in plugins.  External
//! providers (Python, Rust, …) can be registered at runtime via the config.
//!
//! Usage:
//!   var reg = PluginRegistry.init(allocator);
//!   defer reg.deinit();
//!   if (reg.getForExtension(".zig")) |p| { ... }

const std = @import("std");
const plugin_mod = @import("plugin.zig");
const zig_plugin = @import("plugins/zig_plugin.zig");
const markdown_plugin = @import("plugins/markdown_plugin.zig");

const LanguagePlugin = plugin_mod.LanguagePlugin;

/// Manages plugin registration and lookup; owns the registry; ensures consistent access patterns.
pub const PluginRegistry = struct {
    allocator: std.mem.Allocator,
    /// Maps file extension (e.g. ".zig") → plugin descriptor.
    /// Values point into plugin storage below; no additional allocation needed.
    map: std.StringHashMapUnmanaged(*const LanguagePlugin),
    /// Owned plugin storage (avoids dangling pointers on register).
    plugins: std.ArrayList(LanguagePlugin),

    const Self = @This();

    /// Initialise the registry and register all built-in plugins.
    pub fn init(allocator: std.mem.Allocator) Self {
        var self = Self{
            .allocator = allocator,
            .map = .{},
            .plugins = .{},
        };
        // Register built-in plugins (ignore errors — only fails on OOM).
        self.register(allocator, zig_plugin.plugin()) catch {};
        self.register(allocator, markdown_plugin.plugin()) catch {};
        return self;
    }

    pub fn deinit(self: *Self) void {
        self.map.deinit(self.allocator);
        self.plugins.deinit(self.allocator);
    }

    /// Register a plugin.  Overwrites any existing registration for the same
    /// extension.  The plugin descriptor is copied into owned storage.
    pub fn register(self: *Self, allocator: std.mem.Allocator, p: LanguagePlugin) !void {
        try self.plugins.append(allocator, p);
        const stored: *const LanguagePlugin = &self.plugins.items[self.plugins.items.len - 1];
        for (p.extensions) |ext| {
            try self.map.put(allocator, ext, stored);
        }
    }

    /// Look up a plugin by file extension (including the leading dot).
    /// Returns null if no plugin is registered for the extension.
    pub fn getForExtension(self: *const Self, ext: []const u8) ?*const LanguagePlugin {
        return self.map.get(ext);
    }

    /// Look up a plugin by source file path (extracts the extension automatically).
    pub fn getForPath(self: *const Self, path: []const u8) ?*const LanguagePlugin {
        const ext = std.fs.path.extension(path);
        if (ext.len == 0) return null;
        return self.getForExtension(ext);
    }

    /// Return the names of all registered languages.
    /// Caller owns the returned slice (strings point into plugin storage).
    pub fn registeredLanguages(self: *const Self, allocator: std.mem.Allocator) ![]const []const u8 {
        var seen: std.StringHashMapUnmanaged(void) = .{};
        defer seen.deinit(allocator);
        var names: std.ArrayList([]const u8) = .{};
        errdefer names.deinit(allocator);
        for (self.plugins.items) |p| {
            if (!seen.contains(p.name)) {
                try seen.put(allocator, p.name, {});
                try names.append(allocator, p.name);
            }
        }
        return try names.toOwnedSlice(allocator);
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test "PluginRegistry init registers Zig plugin" {
    var reg = PluginRegistry.init(std.testing.allocator);
    defer reg.deinit();

    const p = reg.getForExtension(".zig");
    try std.testing.expect(p != null);
    try std.testing.expectEqualStrings("zig", p.?.name);
}

test "PluginRegistry getForPath" {
    var reg = PluginRegistry.init(std.testing.allocator);
    defer reg.deinit();

    const p = reg.getForPath("src/foo/bar.zig");
    try std.testing.expect(p != null);
    try std.testing.expectEqualStrings("zig", p.?.name);

    // Markdown is registered.
    const md = reg.getForPath("README.md");
    try std.testing.expect(md != null);
    try std.testing.expectEqualStrings("markdown", md.?.name);

    // Unknown extension returns null.
    const none = reg.getForPath("archive.tar");
    try std.testing.expect(none == null);
}

test "PluginRegistry registeredLanguages" {
    var reg = PluginRegistry.init(std.testing.allocator);
    defer reg.deinit();

    const langs = try reg.registeredLanguages(std.testing.allocator);
    defer std.testing.allocator.free(langs);

    try std.testing.expect(langs.len >= 1);
    var found_zig = false;
    for (langs) |l| if (std.mem.eql(u8, l, "zig")) {
        found_zig = true;
    };
    try std.testing.expect(found_zig);
}

test "PluginRegistry register custom plugin" {
    var reg = PluginRegistry.init(std.testing.allocator);
    defer reg.deinit();

    const dummy_parse = struct {
        fn f(arena: std.mem.Allocator, src: [:0]const u8, path: []const u8) anyerror!plugin_mod.ParsedFile {
            _ = arena;
            _ = src;
            return .{ .module = path, .source = path, .language = "lua", .module_comment = null, .members = &.{} };
        }
    }.f;
    const dummy_imports = struct {
        fn f(arena: std.mem.Allocator, src: [:0]const u8) anyerror![]const []const u8 {
            _ = arena;
            _ = src;
            return &.{};
        }
    }.f;

    const ext = [_][]const u8{".lua"};
    try reg.register(std.testing.allocator, .{
        .name = "lua",
        .extensions = &ext,
        .parseFn = dummy_parse,
        .extractImportsFn = dummy_imports,
    });

    const p = reg.getForExtension(".lua");
    try std.testing.expect(p != null);
    try std.testing.expectEqualStrings("lua", p.?.name);
}

