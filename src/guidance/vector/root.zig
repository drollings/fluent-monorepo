//! guidance vector module — cosine search, embeddings, hybrid merge.

pub const math = @import("math.zig");
pub const embeddings = @import("embeddings.zig");

pub const EmbeddingProvider = embeddings.EmbeddingProvider;
pub const NoopEmbedding = embeddings.NoopEmbedding;
pub const OllamaEmbedding = embeddings.OllamaEmbedding;
pub const OpenAiEmbedding = embeddings.OpenAiEmbedding;
pub const createEmbeddingProvider = embeddings.createEmbeddingProvider;
pub const contentHashWithModel = embeddings.contentHashWithModel;

pub const cosineSimilarity = math.cosineSimilarity;
pub const vecToBytes = math.vecToBytes;
pub const bytesToVec = math.bytesToVec;
pub const hybridMerge = math.hybridMerge;
pub const ScoredResult = math.ScoredResult;
pub const IdScore = math.IdScore;
