//! TreeSitterPlugin — universal AST parser using tree-sitter for non-Zig languages.
//!
//! This plugin wraps the tree-sitter C library to parse Python, C++, Rust, Go,
//! TypeScript, PHP, and JavaScript.
//!
//! Usage:
//!   const ts = @import("plugins/treesitter_plugin.zig");
//!   try registry.register(allocator, ts.pythonPlugin());

const std = @import("std");
const c = @cImport({
    @cInclude("tree_sitter/api.h");
});
const types = @import("../types.zig");
const plugin_mod = @import("../plugin.zig");
const loader = @import("treesitter_loader.zig");
const extractor_mod = @import("treesitter_extractor.zig");

const LanguagePlugin = plugin_mod.LanguagePlugin;
const ParsedFile = plugin_mod.ParsedFile;
const MemberExtractor = extractor_mod.MemberExtractor;

/// File extensions handled by Python plugin
const PYTHON_EXTS = [_][]const u8{ ".py", ".pyw", ".pyi" };

/// File extensions handled by C++ plugin
const CPP_EXTS = [_][]const u8{ ".cpp", ".c", ".cc", ".h", ".hpp", ".cxx", ".hxx" };

/// File extensions handled by Rust plugin
const RUST_EXTS = [_][]const u8{".rs"};

/// File extensions handled by Go plugin
const GO_EXTS = [_][]const u8{".go"};

/// File extensions handled by TypeScript plugin
const TS_EXTS = [_][]const u8{ ".ts", ".mts", ".cts" };

/// File extensions handled by TSX plugin
const TSX_EXTS = [_][]const u8{".tsx"};

/// File extensions handled by PHP plugin
const PHP_EXTS = [_][]const u8{".php"};

/// Create Python language plugin
pub fn pythonPlugin() LanguagePlugin {
    return .{
        .name = "python",
        .extensions = &PYTHON_EXTS,
        .parseFn = parsePython,
        .extractImportsFn = extractPythonImports,
    };
}

/// Create C++ language plugin
pub fn cppPlugin() LanguagePlugin {
    return .{
        .name = "cpp",
        .extensions = &CPP_EXTS,
        .parseFn = parseCpp,
        .extractImportsFn = extractCppImports,
    };
}

/// Create Rust language plugin
pub fn rustPlugin() LanguagePlugin {
    return .{
        .name = "rust",
        .extensions = &RUST_EXTS,
        .parseFn = parseRust,
        .extractImportsFn = extractRustImports,
    };
}

/// Create Go language plugin
pub fn goPlugin() LanguagePlugin {
    return .{
        .name = "go",
        .extensions = &GO_EXTS,
        .parseFn = parseGo,
        .extractImportsFn = extractGoImports,
    };
}

/// Create TypeScript language plugin
pub fn typescriptPlugin() LanguagePlugin {
    return .{
        .name = "typescript",
        .extensions = &TS_EXTS,
        .parseFn = parseTypeScript,
        .extractImportsFn = extractTypeScriptImports,
    };
}

/// Create TSX language plugin
pub fn tsxPlugin() LanguagePlugin {
    return .{
        .name = "tsx",
        .extensions = &TSX_EXTS,
        .parseFn = parseTsx,
        .extractImportsFn = extractTsxImports,
    };
}

/// Create PHP language plugin
pub fn phpPlugin() LanguagePlugin {
    return .{
        .name = "php",
        .extensions = &PHP_EXTS,
        .parseFn = parsePhp,
        .extractImportsFn = extractPhpImports,
    };
}

// ============================================================================
// Parse Functions
// ============================================================================

/// Parse Python source
fn parsePython(
    arena: std.mem.Allocator,
    source: [:0]const u8,
    file_path: []const u8,
) anyerror!ParsedFile {
    return parseWithGrammar(arena, source, file_path, "python", loader.pythonGrammar());
}

/// Parse C++ source
fn parseCpp(
    arena: std.mem.Allocator,
    source: [:0]const u8,
    file_path: []const u8,
) anyerror!ParsedFile {
    return parseWithGrammar(arena, source, file_path, "cpp", loader.cppGrammar());
}

/// Parse Rust source
fn parseRust(
    arena: std.mem.Allocator,
    source: [:0]const u8,
    file_path: []const u8,
) anyerror!ParsedFile {
    return parseWithGrammar(arena, source, file_path, "rust", loader.rustGrammar());
}

/// Parse Go source
fn parseGo(
    arena: std.mem.Allocator,
    source: [:0]const u8,
    file_path: []const u8,
) anyerror!ParsedFile {
    return parseWithGrammar(arena, source, file_path, "go", loader.goGrammar());
}

/// Parse TypeScript source
fn parseTypeScript(
    arena: std.mem.Allocator,
    source: [:0]const u8,
    file_path: []const u8,
) anyerror!ParsedFile {
    return parseWithGrammar(arena, source, file_path, "typescript", loader.typescriptGrammar());
}

/// Parse TSX source
fn parseTsx(
    arena: std.mem.Allocator,
    source: [:0]const u8,
    file_path: []const u8,
) anyerror!ParsedFile {
    return parseWithGrammar(arena, source, file_path, "tsx", loader.tsxGrammar());
}

/// Parse PHP source
fn parsePhp(
    arena: std.mem.Allocator,
    source: [:0]const u8,
    file_path: []const u8,
) anyerror!ParsedFile {
    return parseWithGrammar(arena, source, file_path, "php", loader.phpGrammar());
}

// ============================================================================
// Generic Parser
// ============================================================================

/// Parse source with the given grammar
fn parseWithGrammar(
    arena: std.mem.Allocator,
    source: [:0]const u8,
    file_path: []const u8,
    lang_name: []const u8,
    grammar: loader.Grammar,
) anyerror!ParsedFile {
    // Validate ABI version
    try loader.validateGrammarABI(grammar);

    // Create parser
    const parser = c.ts_parser_new() orelse return error.ParserCreateFailed;
    defer c.ts_parser_delete(parser);

    // Set language
    const lang = grammar.getLanguage();
    if (!c.ts_parser_set_language(parser, @ptrCast(lang))) {
        return error.LanguageNotSupported;
    }

    // Parse source
    const tree = c.ts_parser_parse_string(parser, null, source.ptr, @intCast(source.len)) orelse {
        return error.ParseFailed;
    };
    defer c.ts_tree_delete(tree);

    // Extract members
    var ext = MemberExtractor.init(arena, lang_name, source);
    defer ext.deinit();

    const root = c.ts_tree_root_node(tree);
    const members = try ext.extract(@bitCast(root));

    // Derive module name from file path
    const module = try deriveModule(arena, file_path);

    // Source-relative path
    const source_path = if (std.mem.startsWith(u8, file_path, "./"))
        file_path[2..]
    else
        file_path;

    return ParsedFile{
        .module = module,
        .source = source_path,
        .language = lang_name,
        .module_comment = null, // Extracted separately by sync engine
        .members = members,
    };
}

/// Derive module name from file path
fn deriveModule(arena: std.mem.Allocator, file_path: []const u8) ![]const u8 {
    // Strip leading "./"
    var path = file_path;
    if (std.mem.startsWith(u8, path, "./")) path = path[2..];

    // Strip extension
    if (std.mem.lastIndexOfScalar(u8, path, '.')) |dot| {
        path = path[0..dot];
    }

    // Replace path separators with dots
    const out = try arena.dupe(u8, path);
    for (out) |*ch| {
        if (ch.* == '/' or ch.* == std.fs.path.sep) ch.* = '.';
    }
    return out;
}

// ============================================================================
// Import Extraction
// ============================================================================

/// Extract Python imports
fn extractPythonImports(
    arena: std.mem.Allocator,
    source: [:0]const u8,
) anyerror![]const []const u8 {
    _ = source;
    // TODO: Implement with tree-sitter query
    return try arena.dupe([]const u8, &.{});
}

/// Extract C++ imports (#includes)
fn extractCppImports(
    arena: std.mem.Allocator,
    source: [:0]const u8,
) anyerror![]const []const u8 {
    _ = source;
    // TODO: Implement with tree-sitter query
    return try arena.dupe([]const u8, &.{});
}

/// Extract Rust imports (use statements)
fn extractRustImports(
    arena: std.mem.Allocator,
    source: [:0]const u8,
) anyerror![]const []const u8 {
    _ = source;
    // TODO: Implement with tree-sitter query
    return try arena.dupe([]const u8, &.{});
}

/// Extract Go imports
fn extractGoImports(
    arena: std.mem.Allocator,
    source: [:0]const u8,
) anyerror![]const []const u8 {
    _ = source;
    // TODO: Implement with tree-sitter query
    return try arena.dupe([]const u8, &.{});
}

/// Extract TypeScript imports
fn extractTypeScriptImports(
    arena: std.mem.Allocator,
    source: [:0]const u8,
) anyerror![]const []const u8 {
    _ = source;
    // TODO: Implement with tree-sitter query
    return try arena.dupe([]const u8, &.{});
}

/// Extract TSX imports
fn extractTsxImports(
    arena: std.mem.Allocator,
    source: [:0]const u8,
) anyerror![]const []const u8 {
    _ = source;
    // TODO: Implement with tree-sitter query
    return try arena.dupe([]const u8, &.{});
}

/// Extract PHP imports (use statements)
fn extractPhpImports(
    arena: std.mem.Allocator,
    source: [:0]const u8,
) anyerror![]const []const u8 {
    _ = source;
    // TODO: Implement with tree-sitter query
    return try arena.dupe([]const u8, &.{});
}
