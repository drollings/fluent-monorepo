//! guidance vector module — cosine search, embeddings, hybrid merge.

const common = @import("common");

pub const math = @import("math.zig");
pub const vector_db = @import("vector_db.zig");
pub const hnsw = @import("hnsw.zig");
pub const simhash = @import("simhash.zig");

pub const GuidanceDb = vector_db.GuidanceDb;
pub const SearchResult = vector_db.SearchResult;
pub const SemanticAliases = vector_db.SemanticAliases;
pub const syncDatabase = vector_db.syncDatabase;
pub const loadSemanticAliases = vector_db.loadSemanticAliases;
pub const CodehealthDirective = vector_db.CodehealthDirective;
pub const parseCodehealthDirective = vector_db.parseCodehealthDirective;
pub const DbSyncBuilder = vector_db.DbSyncBuilder;
pub const sqlite = vector_db.c;

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

// SimHash exports (from simhash.zig)
pub const SIMHASH_BITS = simhash.SIMHASH_BITS;
pub const SIMHASH_DIMS = simhash.SIMHASH_DIMS;
pub const embeddingHash = simhash.embeddingHash;
pub const hammingDistance = simhash.hammingDistance;
pub const similar = simhash.similar;
pub const maxHamming = simhash.maxHamming;
pub const TokenSimHash = simhash.TokenSimHash;

pub const quantized_embedding = @import("quantized_embedding.zig");
pub const QuantizedEmbedding = quantized_embedding.QuantizedEmbedding;
