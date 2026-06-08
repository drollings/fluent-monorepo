//! LanguagePlugin — interface for language-specific AST providers.
//!
//! Each plugin handles a set of file extensions and knows how to:
//!   - parse source into structured members
//!   - extract imports/dependencies
//!   - extract module-level documentation
//!
//! The Zig native parser is implemented as a plugin wrapping ast_parser.zig.
//! External language providers (Python, Rust, …) can be wired in via the
//! ExternalPlugin wrapper which forks a subprocess.

const std = @import("std");
const types = @import("types.zig");

/// Represents a parsed file structure with ownership and invariants; manages parsing state internally.
pub const ParsedFile = struct {
    /// Module name derived from the file path (e.g. "src.foo.bar").
    module: []const u8,
    /// Source-relative path (e.g. "src/foo/bar.zig").
    source: []const u8,
    /// Language tag (e.g. "zig", "python").
    language: []const u8,
    /// File-level doc comment or module summary.
    module_comment: ?[]const u8,
    /// Top-level members extracted from the AST.
    members: []const types.Member,
};

pub const LanguagePlugin = struct {
    /// Human-readable name (e.g. "zig", "python").
    name: []const u8,
    /// File extensions handled by this plugin (e.g. &.{".zig", ".zon"}).
    extensions: []const []const u8,

    /// Parse `source` (null-terminated) for the file at `file_path`.
    /// Returns a ParsedFile whose strings are valid for the lifetime of `arena`.
    parseFn: *const fn (
        arena: std.mem.Allocator,
        source: [:0]const u8,
        file_path: []const u8,
    ) anyerror!ParsedFile,

    /// Extract import paths from `source`.
    /// Returned slice and strings are valid for the lifetime of `arena`.
    extractImportsFn: *const fn (
        arena: std.mem.Allocator,
        source: [:0]const u8,
    ) anyerror![]const []const u8,

    /// Convenience wrapper: call parseFn.
    pub fn parse(
        self: *const LanguagePlugin,
        arena: std.mem.Allocator,
        source: [:0]const u8,
        file_path: []const u8,
    ) !ParsedFile {
        return self.parseFn(arena, source, file_path);
    }

    /// Convenience wrapper: call extractImportsFn.
    pub fn extractImports(
        self: *const LanguagePlugin,
        arena: std.mem.Allocator,
        source: [:0]const u8,
    ) ![]const []const u8 {
        return self.extractImportsFn(arena, source);
    }
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
