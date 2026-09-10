//! Pinned embedding-model catalog: the single source of truth for model
//! metadata (mirrors, per-artifact sha256/size, dims, metric, batch / context
//! / concurrency limits).
//!
//! Every entry mirrors the upstream table field-for-field; the `catalog`
//! integration test pins the values so mirror drift fails here instead of at
//! download time.


/// Backend that serves a catalog entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingBackend {
    LlamaCpp,
    Model2Vec,
    TransformersJs,
    Qwen,
}

/// Pinned `(repo, revision)` mirror coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactMirror(pub &'static str, pub &'static str);

/// Hugging Face + ModelScope mirrors for one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactSources {
    pub hugging_face: ArtifactMirror,
    pub model_scope: ArtifactMirror,
}

/// One pinned file inside a model snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedArtifact {
    pub path: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

/// One pinned catalog row. Backend-specific fields are `None` for other
/// backends; every entry always carries reference/backend/provider/model,
/// dimension, metric, artifacts and `max_batch_size`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogEntry {
    pub reference: &'static str,
    pub backend: EmbeddingBackend,
    pub provider: &'static str,
    pub model: &'static str,
    pub dimension: u32,
    pub metric: &'static str,
    pub max_batch_size: usize,
    pub uri: Option<&'static str>,
    pub cache_file: Option<&'static str>,
    pub sources: Option<ArtifactSources>,
    pub artifacts: &'static [PinnedArtifact],
    pub context_size: Option<usize>,
    pub format: Option<&'static str>,
    pub kind: Option<&'static str>,
    pub default_endpoint: Option<&'static str>,
    pub max_input_tokens: Option<usize>,
    pub max_image_bytes: Option<u64>,
    pub dtype: Option<&'static str>,
    pub pooling: Option<&'static str>,
    pub normalize: bool,
    pub query_prefix: Option<&'static str>,
    pub document_prefix: Option<&'static str>,
    pub model_file: Option<&'static str>,
    pub embedding_tensor: Option<&'static str>,
    pub tokenizer_file: Option<&'static str>,
    pub default_concurrency: Option<usize>,
}

pub const DEFAULT_QWEN_TEXT_EMBEDDING_ENDPOINT: &str =
    "https://dashscope.aliyuncs.com/compatible-mode/v1/embeddings";
pub const DEFAULT_QWEN3_VL_EMBEDDING_ENDPOINT: &str =
    "https://dashscope.aliyuncs.com/api/v1/services/embeddings/multimodal-embedding/multimodal-embedding";

/// Builtin local default: first search without an index builds this, never an
/// implicit remote model.
pub const DEFAULT_LOCAL_EMBEDDING: &str = "local/potion-code-16m-v2";

const EMBEDDINGGEMMA_ARTIFACTS: &[PinnedArtifact] = &[PinnedArtifact {
    path: "embeddinggemma-300M-Q8_0.gguf",
    size: 333_590_944,
    sha256: "b5ce9d77a3fc4b3b39ccb5643c36777911cc4eb46a66962eadfa3f5f60490d63",
}];

const QWEN3_EMBEDDING_ARTIFACTS: &[PinnedArtifact] = &[PinnedArtifact {
    path: "Qwen3-Embedding-0.6B-Q8_0.gguf",
    size: 639_150_592,
    sha256: "06507c7b42688469c4e7298b0a1e16deff06caf291cf0a5b278c308249c3e439",
}];

const BGE_SMALL_ARTIFACTS: &[PinnedArtifact] = &[
    PinnedArtifact {
        path: "config.json",
        size: 867,
        sha256:
            "26ad1d93a1ba37422fc25472191cfa230010631fa6a01f9d9f81fa13df2d0917",
    },
    PinnedArtifact {
        path: "tokenizer.json",
        size: 533_603,
        sha256:
            "ea77de727ef7fd34d177b83b4b1f1d3bb8884c95c90b6554a0adb0b3b65350a9",
    },
    PinnedArtifact {
        path: "tokenizer_config.json",
        size: 1271,
        sha256:
            "eebe14d184cfbd65f6a11d2a5ff39385c4044c8a670a89acf1a13331e04faa60",
    },
    PinnedArtifact {
        path: "onnx/model_q4.onnx",
        size: 132_562,
        sha256:
            "266acb3edd98a1932f876d15bd8f7881a4955d0d53a6d5e79900f788b432de09",
    },
    PinnedArtifact {
        path: "onnx/model_q4.onnx_data",
        size: 61_185_024,
        sha256:
            "77aff1dfb1e0591a40b61a91e5a97796f14c2aff56706e345bef5cbb3613b8cc",
    },
];

const MINILM_ARTIFACTS: &[PinnedArtifact] = &[
    PinnedArtifact {
        path: "config.json",
        size: 794,
        sha256:
            "fe5da868b77bdb104140822a5af0837cb6450ad6de8ff3dfcc8dd44ddd3e3ae7",
    },
    PinnedArtifact {
        path: "tokenizer.json",
        size: 533_808,
        sha256:
            "07805d116826679de90b4edeb2222269c4b8753bc0981be4399f732b2708e904",
    },
    PinnedArtifact {
        path: "tokenizer_config.json",
        size: 1463,
        sha256:
            "e10bb633ba0d7f69ed342ae7de607f36b39ce53b455fbda69c71700bf57e6f66",
    },
    PinnedArtifact {
        path: "onnx/model_q4.onnx",
        size: 69_663,
        sha256:
            "e4dcb918111189b7686147e309379832fce83d4ecbf17c395961749b5788c786",
    },
    PinnedArtifact {
        path: "onnx/model_q4.onnx_data",
        size: 54_429_696,
        sha256:
            "56fb7a55115e900196115a74e399beb45c2f41ae00b99525d46fb52935c4ee2a",
    },
];

const POTION_RETRIEVAL_ARTIFACTS: &[PinnedArtifact] = &[
    PinnedArtifact {
        path: "model.safetensors",
        size: 129_210_456,
        sha256:
            "07609e5bd33aad37900b3fd62f4ec96f6daec88ca4d46b9d8b928bfababf6ea0",
    },
    PinnedArtifact {
        path: "tokenizer.json",
        size: 1_493_150,
        sha256:
            "7d75cbc54318138807c401b0f0c9721117c628b39de8e8e0edb6cb17e0ee7d18",
    },
];

const POTION_MULTILINGUAL_ARTIFACTS: &[PinnedArtifact] = &[
    PinnedArtifact {
        path: "model.safetensors",
        size: 512_361_560,
        sha256:
            "14b5eb39cb4ce5666da8ad1f3dc6be4346e9b2d601c073302fa0a31bf7943397",
    },
    PinnedArtifact {
        path: "tokenizer.json",
        size: 18_616_131,
        sha256:
            "19f1909063da3cfe3bd83a782381f040dccea475f4816de11116444a73e1b6a1",
    },
];

const POTION_CODE_ARTIFACTS: &[PinnedArtifact] = &[
    PinnedArtifact {
        path: "model.safetensors",
        size: 32_490_072,
        sha256:
            "75cf7a6c2171b230ad19b1e7d8e0b1aee86da5a02af8e7cacedd9921d227623c",
    },
    PinnedArtifact {
        path: "tokenizer.json",
        size: 1_024_340,
        sha256:
            "107bbdcbad4bff1d299b7a4c3a2fb17c52890688b7dd0e4c9deab79d3c4f3d45",
    },
];

const E5_SMALL_ARTIFACTS: &[PinnedArtifact] = &[
    PinnedArtifact {
        path: "config.json",
        size: 658,
        sha256:
            "cb99455288675345e1a4f411438d5d0adbba5fbd3a67ea4fb03c015433b996c1",
    },
    PinnedArtifact {
        path: "tokenizer.json",
        size: 17_082_730,
        sha256:
            "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39",
    },
    PinnedArtifact {
        path: "tokenizer_config.json",
        size: 443,
        sha256:
            "a1d6bc8734a6f635dc158508bef000f8e2e5a759c7d92f984b2c86e5ff53425b",
    },
    PinnedArtifact {
        path: "onnx/model_quantized.onnx",
        size: 118_308_185,
        sha256:
            "f80102d3f2a1229f387d3c81909990d8945513e347b0eab049f7de3c6f98c193",
    },
];

const JINA_CODE_ARTIFACTS: &[PinnedArtifact] = &[
    PinnedArtifact {
        path: "config.json",
        size: 1216,
        sha256:
            "e426aa684c7f9a95c5f020aa855faf93a24f065f5fad0c9e17b124670cabdea6",
    },
    PinnedArtifact {
        path: "tokenizer.json",
        size: 2_561_316,
        sha256:
            "b01c78a902aa4facb2f47f95449f48e2f7bbfea5d2472ee2f6ce92323c6f86e5",
    },
    PinnedArtifact {
        path: "tokenizer_config.json",
        size: 493,
        sha256:
            "f477aeb15ff9f78d3c1ddf2361d2b0b8b20cf55220f839f29a37f3a18efddd89",
    },
    PinnedArtifact {
        path: "onnx/model_quantized.onnx",
        size: 161_895_621,
        sha256:
            "ed45870251c9f0cf656e78aab0d37a23489066df8a222bb1c8caf8a45f2cb16d",
    },
];

const GTE_MODERNBERT_ARTIFACTS: &[PinnedArtifact] = &[
    PinnedArtifact {
        path: "config.json",
        size: 1184,
        sha256:
            "8ba54dc3d35d7194f5178a4194b649f146753e02dabd22bdca5c5cbac15069ed",
    },
    PinnedArtifact {
        path: "tokenizer.json",
        size: 3_583_228,
        sha256:
            "6c8aaa9a542084f2457eab775d4eeb51f92a70c0fd9de28d5edb0ddec3c08d30",
    },
    PinnedArtifact {
        path: "tokenizer_config.json",
        size: 20_867,
        sha256:
            "9654072f7c873161814043cf08cb5ed72f71d0b935abcd4e267935cb34352c21",
    },
    PinnedArtifact {
        path: "onnx/model_q4.onnx",
        size: 224_152_761,
        sha256:
            "5d1278a1ba749c06b82f9a2f65c2c1c5765d36f2eb88b4888de62ff12b0724a2",
    },
];

const NOMIC_ARTIFACTS: &[PinnedArtifact] = &[
    PinnedArtifact {
        path: "config.json",
        size: 2538,
        sha256:
            "9ab00bd92cee80a569f708140b7b6c1661a65891ff3765b1519e181ba2f2c92b",
    },
    PinnedArtifact {
        path: "tokenizer.json",
        size: 711_396,
        sha256:
            "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66",
    },
    PinnedArtifact {
        path: "tokenizer_config.json",
        size: 1191,
        sha256:
            "d7e0000bcc80134debd2222220427e6bf5fa20a669f40a0d0d1409cc18e0a9bc",
    },
    PinnedArtifact {
        path: "onnx/model_q4.onnx",
        size: 165_113_221,
        sha256:
            "314976b7b9fba83283f9c8a29ee680a159fa485f52104e3fa39d3d5858337003",
    },
];

/// The full pinned table, in upstream order.
pub static EMBEDDING_MODEL_CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        reference: "local/embeddinggemma-300m",
        backend: EmbeddingBackend::LlamaCpp,
        provider: "local",
        model: "embeddinggemma-300m",
        dimension: 768,
        metric: "cosine",
        max_batch_size: 16,
        uri: Some("hf:ggml-org/embeddinggemma-300M-GGUF/embeddinggemma-300M-Q8_0.gguf#0f741b5a6585bd53aeb15cd1372c56f2a0f65e12"),
        cache_file: Some("hf_ggml-org_embeddinggemma-300M-Q8_0.gguf"),
        sources: Some(ArtifactSources {
            hugging_face: ArtifactMirror(
                "ggml-org/embeddinggemma-300M-GGUF",
                "0f741b5a6585bd53aeb15cd1372c56f2a0f65e12",
            ),
            model_scope: ArtifactMirror(
                "ggml-org/embeddinggemma-300M-GGUF",
                "e2fab2963ac0943b1dc320cf0e267bd0e06e2e97",
            ),
        }),
        artifacts: EMBEDDINGGEMMA_ARTIFACTS,
        context_size: Some(2048),
        format: Some("embeddinggemma"),
        kind: None,
        default_endpoint: None,
        max_input_tokens: None,
        max_image_bytes: None,
        dtype: None,
        pooling: None,
        normalize: false,
        query_prefix: None,
        document_prefix: None,
        model_file: None,
        embedding_tensor: None,
        tokenizer_file: None,
        default_concurrency: None,
    },
    CatalogEntry {
        reference: "local/qwen3-embedding-0.6b",
        backend: EmbeddingBackend::LlamaCpp,
        provider: "local",
        model: "qwen3-embedding-0.6b",
        dimension: 1024,
        metric: "cosine",
        max_batch_size: 8,
        uri: Some("hf:Qwen/Qwen3-Embedding-0.6B-GGUF/Qwen3-Embedding-0.6B-Q8_0.gguf#370f27d7550e0def9b39c1f16d3fbaa13aa67728"),
        cache_file: Some("hf_Qwen_Qwen3-Embedding-0.6B-Q8_0.gguf"),
        sources: Some(ArtifactSources {
            hugging_face: ArtifactMirror(
                "Qwen/Qwen3-Embedding-0.6B-GGUF",
                "370f27d7550e0def9b39c1f16d3fbaa13aa67728",
            ),
            model_scope: ArtifactMirror(
                "Qwen/Qwen3-Embedding-0.6B-GGUF",
                "61ae123505fc6fa7f36d372fae1a30bc384241ea",
            ),
        }),
        artifacts: QWEN3_EMBEDDING_ARTIFACTS,
        context_size: Some(8192),
        format: Some("qwen3"),
        kind: None,
        default_endpoint: None,
        max_input_tokens: None,
        max_image_bytes: None,
        dtype: None,
        pooling: None,
        normalize: false,
        query_prefix: None,
        document_prefix: None,
        model_file: None,
        embedding_tensor: None,
        tokenizer_file: None,
        default_concurrency: None,
    },
    CatalogEntry {
        reference: "qwen/text-embedding-v4",
        backend: EmbeddingBackend::Qwen,
        provider: "qwen",
        model: "text-embedding-v4",
        dimension: 1024,
        metric: "cosine",
        max_batch_size: 10,
        uri: None,
        cache_file: None,
        sources: None,
        artifacts: &[],
        context_size: None,
        format: None,
        kind: Some("text"),
        default_endpoint: Some(DEFAULT_QWEN_TEXT_EMBEDDING_ENDPOINT),
        max_input_tokens: Some(8192),
        max_image_bytes: None,
        dtype: None,
        pooling: None,
        normalize: false,
        query_prefix: None,
        document_prefix: None,
        model_file: None,
        embedding_tensor: None,
        tokenizer_file: None,
        default_concurrency: None,
    },
    CatalogEntry {
        reference: "qwen/qwen3.7-text-embedding",
        backend: EmbeddingBackend::Qwen,
        provider: "qwen",
        model: "qwen3.7-text-embedding",
        dimension: 1024,
        metric: "cosine",
        max_batch_size: 20,
        uri: None,
        cache_file: None,
        sources: None,
        artifacts: &[],
        context_size: None,
        format: None,
        kind: Some("text"),
        default_endpoint: Some(DEFAULT_QWEN_TEXT_EMBEDDING_ENDPOINT),
        max_input_tokens: Some(128_000),
        max_image_bytes: None,
        dtype: None,
        pooling: None,
        normalize: false,
        query_prefix: None,
        document_prefix: None,
        model_file: None,
        embedding_tensor: None,
        tokenizer_file: None,
        default_concurrency: None,
    },
    CatalogEntry {
        reference: "qwen/qwen3-vl-embedding",
        backend: EmbeddingBackend::Qwen,
        provider: "qwen",
        model: "qwen3-vl-embedding",
        dimension: 2560,
        metric: "cosine",
        max_batch_size: 20,
        uri: None,
        cache_file: None,
        sources: None,
        artifacts: &[],
        context_size: None,
        format: None,
        kind: Some("multimodal"),
        default_endpoint: Some(DEFAULT_QWEN3_VL_EMBEDDING_ENDPOINT),
        max_input_tokens: Some(32_000),
        max_image_bytes: Some(10 * 1024 * 1024),
        dtype: None,
        pooling: None,
        normalize: false,
        query_prefix: None,
        document_prefix: None,
        model_file: None,
        embedding_tensor: None,
        tokenizer_file: None,
        default_concurrency: None,
    },
    CatalogEntry {
        reference: "local/bge-small-en-v1.5",
        backend: EmbeddingBackend::TransformersJs,
        provider: "local",
        model: "bge-small-en-v1.5",
        dimension: 384,
        metric: "cosine",
        max_batch_size: 4,
        uri: None,
        cache_file: None,
        sources: Some(ArtifactSources {
            hugging_face: ArtifactMirror(
                "onnx-community/bge-small-en-v1.5-ONNX",
                "4a9a46c7b88fa408e650a571a1800243f26309bd",
            ),
            model_scope: ArtifactMirror(
                "onnx-community/bge-small-en-v1.5-ONNX",
                "f246b360b061b613fe0b449f2e14de12f875dad7",
            ),
        }),
        artifacts: BGE_SMALL_ARTIFACTS,
        context_size: None,
        format: None,
        kind: None,
        default_endpoint: None,
        max_input_tokens: Some(512),
        max_image_bytes: None,
        dtype: Some("q4"),
        pooling: Some("cls"),
        normalize: true,
        query_prefix: Some("Represent this sentence for searching relevant passages: "),
        document_prefix: None,
        model_file: None,
        embedding_tensor: None,
        tokenizer_file: None,
        default_concurrency: None,
    },
    CatalogEntry {
        reference: "local/all-minilm-l6-v2",
        backend: EmbeddingBackend::TransformersJs,
        provider: "local",
        model: "all-minilm-l6-v2",
        dimension: 384,
        metric: "cosine",
        max_batch_size: 4,
        uri: None,
        cache_file: None,
        sources: Some(ArtifactSources {
            hugging_face: ArtifactMirror(
                "onnx-community/all-MiniLM-L6-v2-ONNX",
                "aff7a1dc4e8a1ea593e6ea21e95c22ef0a25966f",
            ),
            model_scope: ArtifactMirror(
                "onnx-community/all-MiniLM-L6-v2-ONNX",
                "e1da369847063d70f2fd772226551865bcab1c2d",
            ),
        }),
        artifacts: MINILM_ARTIFACTS,
        context_size: None,
        format: None,
        kind: None,
        default_endpoint: None,
        max_input_tokens: Some(256),
        max_image_bytes: None,
        dtype: Some("q4"),
        pooling: Some("mean"),
        normalize: true,
        query_prefix: None,
        document_prefix: None,
        model_file: None,
        embedding_tensor: None,
        tokenizer_file: None,
        default_concurrency: None,
    },
    CatalogEntry {
        reference: "local/potion-retrieval-32m",
        backend: EmbeddingBackend::Model2Vec,
        provider: "local",
        model: "potion-retrieval-32m",
        dimension: 512,
        metric: "cosine",
        max_batch_size: 256,
        uri: None,
        cache_file: None,
        sources: Some(ArtifactSources {
            hugging_face: ArtifactMirror(
                "minishlab/potion-retrieval-32M",
                "6fc8051fab2a1e0ee76689cf08c853792ac285e7",
            ),
            model_scope: ArtifactMirror(
                "minishlab/potion-retrieval-32M",
                "33da23fc75cb732b5370bf25adde3db74b0d65b3",
            ),
        }),
        artifacts: POTION_RETRIEVAL_ARTIFACTS,
        context_size: None,
        format: None,
        kind: None,
        default_endpoint: None,
        max_input_tokens: Some(1024),
        max_image_bytes: None,
        dtype: None,
        pooling: None,
        normalize: true,
        query_prefix: None,
        document_prefix: None,
        model_file: Some("model.safetensors"),
        embedding_tensor: Some("embeddings"),
        tokenizer_file: Some("tokenizer.json"),
        default_concurrency: Some(2),
    },
    CatalogEntry {
        reference: "local/potion-multilingual-128m",
        backend: EmbeddingBackend::Model2Vec,
        provider: "local",
        model: "potion-multilingual-128m",
        dimension: 256,
        metric: "cosine",
        max_batch_size: 256,
        uri: None,
        cache_file: None,
        sources: Some(ArtifactSources {
            hugging_face: ArtifactMirror(
                "minishlab/potion-multilingual-128M",
                "73908c3438cf03b6a01bcb9611d62b23d0726f08",
            ),
            model_scope: ArtifactMirror(
                "minishlab/potion-multilingual-128M",
                "e8524678123f281add99f9745ac33d6604434dd7",
            ),
        }),
        artifacts: POTION_MULTILINGUAL_ARTIFACTS,
        context_size: None,
        format: None,
        kind: None,
        default_endpoint: None,
        max_input_tokens: Some(1024),
        max_image_bytes: None,
        dtype: None,
        pooling: None,
        normalize: true,
        query_prefix: None,
        document_prefix: None,
        model_file: Some("model.safetensors"),
        embedding_tensor: Some("embeddings"),
        tokenizer_file: Some("tokenizer.json"),
        default_concurrency: Some(2),
    },
    CatalogEntry {
        reference: "local/potion-code-16m-v2",
        backend: EmbeddingBackend::Model2Vec,
        provider: "local",
        model: "potion-code-16m-v2",
        dimension: 256,
        metric: "cosine",
        max_batch_size: 256,
        uri: None,
        cache_file: None,
        sources: Some(ArtifactSources {
            hugging_face: ArtifactMirror(
                "minishlab/potion-code-16M-v2",
                "e9d2a44ca6a05ac6685f3b23709ea57eb7352d5b",
            ),
            model_scope: ArtifactMirror(
                "minishlab/potion-code-16M-v2",
                "3e922fde18f43b8db69f3381b6d468738d3dd2d7",
            ),
        }),
        artifacts: POTION_CODE_ARTIFACTS,
        context_size: None,
        format: None,
        kind: None,
        default_endpoint: None,
        max_input_tokens: Some(1024),
        max_image_bytes: None,
        dtype: None,
        pooling: None,
        normalize: true,
        query_prefix: None,
        document_prefix: None,
        model_file: Some("model.safetensors"),
        embedding_tensor: Some("embeddings"),
        tokenizer_file: Some("tokenizer.json"),
        default_concurrency: Some(2),
    },
    CatalogEntry {
        reference: "local/multilingual-e5-small",
        backend: EmbeddingBackend::TransformersJs,
        provider: "local",
        model: "multilingual-e5-small",
        dimension: 384,
        metric: "cosine",
        max_batch_size: 4,
        uri: None,
        cache_file: None,
        sources: Some(ArtifactSources {
            hugging_face: ArtifactMirror(
                "Xenova/multilingual-e5-small",
                "761b726dd34fb83930e26aab4e9ac3899aa1fa78",
            ),
            model_scope: ArtifactMirror(
                "Xenova/multilingual-e5-small",
                "252d0dcb679dda2c7b6fd5bbfed15df3c7feaebf",
            ),
        }),
        artifacts: E5_SMALL_ARTIFACTS,
        context_size: None,
        format: None,
        kind: None,
        default_endpoint: None,
        max_input_tokens: Some(512),
        max_image_bytes: None,
        dtype: Some("q8"),
        pooling: Some("mean"),
        normalize: true,
        query_prefix: Some("query: "),
        document_prefix: Some("passage: "),
        model_file: None,
        embedding_tensor: None,
        tokenizer_file: None,
        default_concurrency: None,
    },
    CatalogEntry {
        reference: "local/jina-embeddings-v2-base-code",
        backend: EmbeddingBackend::TransformersJs,
        provider: "local",
        model: "jina-embeddings-v2-base-code",
        dimension: 768,
        metric: "cosine",
        max_batch_size: 2,
        uri: None,
        cache_file: None,
        sources: Some(ArtifactSources {
            hugging_face: ArtifactMirror(
                "jinaai/jina-embeddings-v2-base-code",
                "516f4baf13dec4ddddda8631e019b5737c8bc250",
            ),
            model_scope: ArtifactMirror(
                "jinaai/jina-embeddings-v2-base-code",
                "91aa0a6aa801c408149324e32e8cd43f8502da8f",
            ),
        }),
        artifacts: JINA_CODE_ARTIFACTS,
        context_size: None,
        format: None,
        kind: None,
        default_endpoint: None,
        max_input_tokens: Some(8192),
        max_image_bytes: None,
        dtype: Some("q8"),
        pooling: Some("mean"),
        normalize: true,
        query_prefix: None,
        document_prefix: None,
        model_file: None,
        embedding_tensor: None,
        tokenizer_file: None,
        default_concurrency: None,
    },
    CatalogEntry {
        reference: "local/gte-modernbert-base",
        backend: EmbeddingBackend::TransformersJs,
        provider: "local",
        model: "gte-modernbert-base",
        dimension: 768,
        metric: "cosine",
        max_batch_size: 2,
        uri: None,
        cache_file: None,
        sources: Some(ArtifactSources {
            hugging_face: ArtifactMirror(
                "Alibaba-NLP/gte-modernbert-base",
                "e7f32e3c00f91d699e8c43b53106206bcc72bb22",
            ),
            model_scope: ArtifactMirror(
                "iic/gte-modernbert-base",
                "678f4ed93760af288132f4f9dc5b6daebdc48777",
            ),
        }),
        artifacts: GTE_MODERNBERT_ARTIFACTS,
        context_size: None,
        format: None,
        kind: None,
        default_endpoint: None,
        max_input_tokens: Some(8192),
        max_image_bytes: None,
        dtype: Some("q4"),
        pooling: Some("cls"),
        normalize: true,
        query_prefix: None,
        document_prefix: None,
        model_file: None,
        embedding_tensor: None,
        tokenizer_file: None,
        default_concurrency: None,
    },
    CatalogEntry {
        reference: "local/nomic-embed-text-v1.5",
        backend: EmbeddingBackend::TransformersJs,
        provider: "local",
        model: "nomic-embed-text-v1.5",
        dimension: 768,
        metric: "cosine",
        max_batch_size: 2,
        uri: None,
        cache_file: None,
        sources: Some(ArtifactSources {
            hugging_face: ArtifactMirror(
                "nomic-ai/nomic-embed-text-v1.5",
                "e9b6763023c676ca8431644204f50c2b100d9aab",
            ),
            model_scope: ArtifactMirror(
                "nomic-ai/nomic-embed-text-v1.5",
                "c6fb77fdf73531ee8319b34e46f7e749b59e74e8",
            ),
        }),
        artifacts: NOMIC_ARTIFACTS,
        context_size: None,
        format: None,
        kind: None,
        default_endpoint: None,
        max_input_tokens: Some(8192),
        max_image_bytes: None,
        dtype: Some("q4"),
        pooling: Some("mean"),
        normalize: true,
        query_prefix: Some("search_query: "),
        document_prefix: Some("search_document: "),
        model_file: None,
        embedding_tensor: None,
        tokenizer_file: None,
        default_concurrency: None,
    },
];

/// All pinned entries, in catalog order.
#[must_use]
pub fn list_embedding_models() -> &'static [CatalogEntry] {
    EMBEDDING_MODEL_CATALOG
}

/// Look up one entry by `provider/model` reference.
#[must_use]
pub fn get_embedding_model_catalog_entry(reference: &str) -> Option<&'static CatalogEntry> {
    EMBEDDING_MODEL_CATALOG
        .iter()
        .find(|entry| entry.reference == reference)
}
