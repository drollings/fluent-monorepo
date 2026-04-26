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
const treesitter_plugin = @import("plugins/treesitter_plugin.zig");

const LanguagePlugin = plugin_mod.LanguagePlugin;

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

        // Register tree-sitter plugins for non-Zig languages
        self.register(allocator, treesitter_plugin.pythonPlugin()) catch {};
        self.register(allocator, treesitter_plugin.cppPlugin()) catch {};
        self.register(allocator, treesitter_plugin.rustPlugin()) catch {};
        self.register(allocator, treesitter_plugin.goPlugin()) catch {};
        self.register(allocator, treesitter_plugin.typescriptPlugin()) catch {};
        self.register(allocator, treesitter_plugin.tsxPlugin()) catch {};
        self.register(allocator, treesitter_plugin.phpPlugin()) catch {};
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
        var seen: std.StringHashMapUnmanaged(void) = .empty;
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
