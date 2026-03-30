//! guidance vector module — cosine search, embeddings, hybrid merge.

const common = @import("common");

pub const math = @import("math.zig");
pub const vector_db = @import("vector_db.zig");
pub const hnsw = @import("hnsw.zig");

pub const GuidanceDb = vector_db.GuidanceDb;
pub const SearchResult = vector_db.SearchResult;
pub const SemanticAliases = vector_db.SemanticAliases;
pub const syncDatabase = vector_db.syncDatabase;
pub const loadSemanticAliases = vector_db.loadSemanticAliases;

pub const HnswIndex = hnsw.HnswIndex;

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
pub const hybridMergeThree = math.hybridMergeThree;
pub const ScoredResult = math.ScoredResult;
pub const IdScore = math.IdScore;
