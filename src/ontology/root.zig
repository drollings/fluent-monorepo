/// ontology/root.zig — Ontology processing module umbrella
///
/// Named module `ontology`: re-exports YAGO helpers, the triple mapper,
/// migration utilities, and inference engine.
/// Depends on `coral_db` for Library/ContextNode types and `rdf` for parsing.
const std = @import("std");

pub const yago = @import("yago.zig");
pub const mapper = @import("mapper.zig");
pub const migration = @import("migration.zig");
pub const inference = @import("inference.zig");

// Flat convenience re-exports.
pub const TripleMapper = mapper.TripleMapper;
pub const MappingConfig = mapper.MappingConfig;
pub const FlushResult = mapper.FlushResult;
