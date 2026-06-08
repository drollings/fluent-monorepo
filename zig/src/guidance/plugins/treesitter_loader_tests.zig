//! Tests for treesitter_loader.zig.
//! Moved by `guidance codehealth --fix`. Edit as needed.
const std = @import("std");
const treesitter_loader_mod = @import("treesitter_loader.zig");

test "grammar loading" {
    // Test that grammars can be loaded
    const grammar = treesitter_loader_mod.pythonGrammar();
    try treesitter_loader_mod.validateGrammarABI(grammar);
}
