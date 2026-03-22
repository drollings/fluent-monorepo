//! guidance vector module — cosine search, embeddings, hybrid merge.

const common = @import("common");

pub const math = @import("math.zig");
pub const lance_db = @import("lance_db.zig");

pub const GuidanceDb = lance_db.GuidanceDb;
pub const SearchResult = lance_db.SearchResult;
pub const SemanticAliases = lance_db.SemanticAliases;
pub const syncDatabase = lance_db.syncDatabase;
pub const loadSemanticAliases = lance_db.loadSemanticAliases;

pub const EmbeddingProvider = common.EmbeddingProvider;
pub const NoopEmbedding = common.NoopEmbedding;
pub const OllamaEmbedding = common.OllamaEmbedding;
pub const OpenAiEmbedding = common.OpenAiEmbedding;
pub const createEmbeddingProvider = common.createEmbeddingProvider;
pub const contentHashWithModel = common.contentHashWithModel;

pub const cosineSimilarity = math.cosineSimilarity;
pub const vecToBytes = math.vecToBytes;
pub const bytesToVec = math.bytesToVec;
pub const hybridMerge = math.hybridMerge;
pub const ScoredResult = math.ScoredResult;
pub const IdScore = math.IdScore;
