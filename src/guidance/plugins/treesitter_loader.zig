//! TreeSitterLoader — loads and manages tree-sitter language grammars.
//!
//! Each language grammar is exposed as a C function returning TSLanguage*.
//! This module provides Zig wrappers for those functions.
//!
//! Grammars are loaded from tree-sitter-grammars/ directory, which contains
//! submodules for each language parser.

const std = @import("std");
const c = @cImport({
    @cInclude("tree_sitter/api.h");
});

/// Language grammar descriptor
pub const Grammar = struct {
    name: []const u8,
    extensions: []const []const u8,
    language_fn: *const fn () callconv(.C) *const c.TSLanguage,

    /// Get the TSLanguage pointer
    pub fn getLanguage(self: *const Grammar) *const c.TSLanguage {
        return self.language_fn();
    }
};

/// All supported tree-sitter grammars
pub const GrammarList = enum {
    python,
    cpp,
    rust,
    go,
    typescript,
    tsx,
    php,

    /// Get grammar descriptor by enum value
    pub fn getDescriptor(self: GrammarList) Grammar {
        return switch (self) {
            .python => pythonGrammar(),
            .cpp => cppGrammar(),
            .rust => rustGrammar(),
            .go => goGrammar(),
            .typescript => typescriptGrammar(),
            .tsx => tsxGrammar(),
            .php => phpGrammar(),
        };
    }
};

/// Python grammar (tree-sitter-python)
pub fn pythonGrammar() Grammar {
    return .{
        .name = "python",
        .extensions = &.{ ".py", ".pyw", ".pyi" },
        .language_fn = tree_sitter_python,
    };
}

extern "tree-sitter-python" fn tree_sitter_python() *const c.TSLanguage;

/// C++ grammar (tree-sitter-cpp)
pub fn cppGrammar() Grammar {
    return .{
        .name = "cpp",
        .extensions = &.{ ".cpp", ".c", ".cc", ".h", ".hpp", ".cxx", ".hxx" },
        .language_fn = tree_sitter_cpp,
    };
}

extern "tree-sitter-cpp" fn tree_sitter_cpp() *const c.TSLanguage;

/// Rust grammar (tree-sitter-rust)
pub fn rustGrammar() Grammar {
    return .{
        .name = "rust",
        .extensions = &.{".rs"},
        .language_fn = tree_sitter_rust,
    };
}

extern "tree-sitter-rust" fn tree_sitter_rust() *const c.TSLanguage;

/// Go grammar (tree-sitter-go)
pub fn goGrammar() Grammar {
    return .{
        .name = "go",
        .extensions = &.{".go"},
        .language_fn = tree_sitter_go,
    };
}

extern "tree-sitter-go" fn tree_sitter_go() *const c.TSLanguage;

/// TypeScript grammar (tree-sitter-typescript)
pub fn typescriptGrammar() Grammar {
    return .{
        .name = "typescript",
        .extensions = &.{ ".ts", ".tsx", ".mts", ".cts" },
        .language_fn = tree_sitter_typescript,
    };
}

extern "tree-sitter-typescript" fn tree_sitter_typescript() *const c.TSLanguage;

/// TSX grammar (tree-sitter-typescript tsx)
pub fn tsxGrammar() Grammar {
    return .{
        .name = "tsx",
        .extensions = &.{".tsx"},
        .language_fn = tree_sitter_tsx,
    };
}

extern "tree-sitter-tsx" fn tree_sitter_tsx() *const c.TSLanguage;

/// PHP grammar (tree-sitter-php)
pub fn phpGrammar() Grammar {
    return .{
        .name = "php",
        .extensions = &.{".php"},
        .language_fn = tree_sitter_php,
    };
}

extern "tree-sitter-php" fn tree_sitter_php() *const c.TSLanguage;

/// Find grammar by file extension
pub fn findGrammarByExtension(ext: []const u8) ?Grammar {
    const grammars = &.{
        pythonGrammar(),
        cppGrammar(),
        rustGrammar(),
        goGrammar(),
        typescriptGrammar(),
        tsxGrammar(),
        phpGrammar(),
    };

    for (grammars) |grammar| {
        for (grammar.extensions) |grammar_ext| {
            if (std.mem.eql(u8, ext, grammar_ext)) {
                return grammar;
            }
        }
    }
    return null;
}

/// Validate grammar ABI compatibility
pub fn validateGrammarABI(grammar: Grammar) !void {
    const lang = grammar.getLanguage();
    const abi_version = c.ts_language_abi_version(lang);

    if (abi_version < c.TREE_SITTER_MIN_COMPATIBLE_LANGUAGE_VERSION or
        abi_version > c.TREE_SITTER_LANGUAGE_VERSION)
    {
        std.debug.print("Grammar '{s}' has incompatible ABI version {d}. Expected [{d}-{d}]\n", .{ grammar.name, abi_version, c.TREE_SITTER_MIN_COMPATIBLE_LANGUAGE_VERSION, c.TREE_SITTER_LANGUAGE_VERSION });
        return error.IncompatibleABIVersion;
    }
}
