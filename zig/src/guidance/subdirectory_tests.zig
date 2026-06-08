//! Shim root for subdirectory test files that require src/guidance/ as their module root.
//!
//! Zig 0.16 module path constraint: when a test file uses @import("../types.zig"), the
//! relative "../" path must stay within the module root directory. Tests in subdirectories
//! (comments/, health/, plugins/, query/, sync/) cannot use their own directory as module
//! root because types.zig, hash.zig, etc. live in the parent src/guidance/ directory.
//!
//! This file's module root IS src/guidance/, so all ten test files can resolve their
//! cross-directory relative imports correctly.
//!
//! Dependencies (union of all ten tests):
//!   common + llm          — sync.zig → enhancer.zig; line_verify.zig; inserter.zig
//!   vector + sqlite3      — health.zig; strategy.zig → staged.zig → vector_db
//!   treesitter C libs     — treesitter_extractor.zig
comptime {
    _ = @import("comments/core_tests.zig");
    _ = @import("comments/header_tests.zig");
    _ = @import("comments/inserter_tests.zig");
    _ = @import("comments/sync_tests.zig");
    _ = @import("health/health_tests.zig");
    _ = @import("plugins/markdown_plugin_tests.zig");
    _ = @import("plugins/treesitter_extractor_tests.zig");
    _ = @import("plugins/zig_plugin_tests.zig");
    _ = @import("query/strategy_tests.zig");
    _ = @import("sync/line_verify_tests.zig");
}
