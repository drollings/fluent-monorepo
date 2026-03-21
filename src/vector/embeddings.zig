//! Re-export shim — embedding implementations live in src/common/embeddings.zig.
//! Tests have moved there too; update build.zig vector_tests root accordingly.

const common = @import("common");

pub const EmbeddingProvider = common.EmbeddingProvider;
pub const NoopEmbedding = common.NoopEmbedding;
pub const OllamaEmbedding = common.OllamaEmbedding;
pub const OpenAiEmbedding = common.OpenAiEmbedding;
pub const createEmbeddingProvider = common.createEmbeddingProvider;
pub const contentHashWithModel = common.contentHashWithModel;
pub const parseOllamaResponse = common.parseOllamaResponse;
pub const parseOpenAiResponse = common.parseOpenAiResponse;
