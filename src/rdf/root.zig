/// rdf/root.zig — RDF parsing module umbrella
///
/// Named module `rdf`: re-exports the Turtle lexer, parser, N-Quads parser,
/// and normalization helpers.
const std = @import("std");

pub const lexer = @import("lexer.zig");
pub const parser = @import("parser.zig");
pub const normalize = @import("normalize.zig");
pub const nquads = @import("nquads.zig");

// Flat convenience re-exports for common types.
pub const Parser = parser.Parser;
pub const Triple = parser.Triple;
pub const Term = parser.Term;
