//! fluent-types: Shared data types (GuidanceDoc, Member, FileType, etc.).
//! No business logic — only type definitions, serde, and validation.

use internment::ArcIntern;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use std::collections::BTreeMap;

pub mod file_kind;
pub mod instance_id;
pub mod interlingua;
pub mod knowledge;
pub mod label_enum;
pub mod model_key;
pub mod model_ref;
pub mod openai_wire;
pub mod provenance;
pub mod rounds;
pub mod sidecar;

pub use file_kind::FileKind;
pub use interlingua::{
    id_for_namespace, lemma_id_for_str, local_id_of, property_id_for_iri, yago_class_id_for_iri,
    yago_entity_id_for_iri, ConceptMetadata, InterlinguaId, InterlinguaNamespace, LOCAL_BITS,
    LOCAL_MASK, NAMESPACE_BITS,
};
pub use knowledge::{KnowledgeCapability, KnowledgeError};
pub use openai_wire::{
    ContentPart, FunctionCall, FunctionDef, ImageUrl, RouterChoice, RouterMessage,
    RouterMessageContent, RouterRequest, RouterResponse, RouterTool, ToolCall, Usage,
};
pub use provenance::{AnnotationClaim, ClaimStatus, ProvenanceTier};
pub use rounds::{BoundedRounds, EscalationConfidence, SeverityThreshold};

pub const LOD_COUNT: usize = 6;

/// LOD0: full text (eager at creation, authoritative — every lazy tier
/// derives directly from it, never chained).
pub const LOD0_FULL_TEXT: u8 = 0;
/// LOD5: name/label (eager at creation via `derive_label`-class policy).
pub const LOD5_LABEL: u8 = 5;
/// Lazy LOD range: LOD1–LOD4 derive on demand, at most once.
pub const LAZY_LOD_RANGE: std::ops::RangeInclusive<u8> = 1..=4;

/// A deterministic context hit — a short-circuit fact/cached answer that
/// lets the escalation ladder avoid a frontier call.  `content` is the
/// verified answer/fact; `source` names the cache that produced it (e.g.
/// "coral-context" or "ledger") for audit provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// A deterministic context hit — a short-circuit fact/cached answer that
/// lets the escalation ladder avoid a frontier call.  `content` is the
/// verified answer/fact; `source` names the cache that produced it (e.g.
/// "coral-context" or "ledger") for audit provenance.
pub struct ContextHit {
    pub source: String,
    pub content: String,
    #[serde(default)]
    pub score: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Deterministic-fact lookup used by the escalation ladder **before** engaging
/// the frontier (VISION: "a `ContextHit` short-circuits").
///
/// This is the cross-crate boundary for context retrieval: `fluent-router`
/// consumes context through this trait only and never imports coral /
/// knowledge / ontology / rdf. Concrete implementations (coral-context's
/// cache, the router's ledger-backed cache) are composed at the binary.
pub trait ContextCache: Send + Sync {
    /// Look up a deterministic answer/fact for `query`. `None` means the
    /// cache has no short-circuit hit and the ladder must escalate.
    fn lookup(&self, query: &str) -> Option<ContextHit>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub i64);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub ArcIntern<str>);

impl NodeId {
    pub fn from_int(i: i64) -> Self {
        Self(i)
    }
    pub fn as_int(self) -> i64 {
        self.0
    }
}

impl SessionId {
    pub fn new(s: impl Into<ArcIntern<str>>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileType {
    Source,
    Markdown,
    Config,
    Data,
    Pdf,
    Audio,
    Unknown,
}

impl FileType {
    pub fn from_extension(ext: &str) -> Self {
        let known_source = [
            ".zig", ".zon", ".py", ".rs", ".go", ".ts", ".tsx", ".js", ".jsx", ".c", ".cpp", ".h",
            ".hpp", ".lua", ".rb", ".java", ".swift", ".kt",
        ];
        let known_markdown = [".md", ".markdown", ".mdx"];
        let known_config = [
            ".json", ".toml", ".yaml", ".yml", ".ini", ".env", ".cfg", ".conf",
        ];
        let known_data = [".csv", ".tsv", ".sqlite", ".db", ".sql", ".parquet"];
        let known_pdf = [".pdf"];
        let known_audio = [".mp3", ".wav", ".flac", ".ogg", ".m4a", ".wma"];
        let ext = ext.to_lowercase();
        if known_source.iter().any(|e| ext == *e) {
            return Self::Source;
        }
        if known_markdown.iter().any(|e| ext == *e) {
            return Self::Markdown;
        }
        if known_config.iter().any(|e| ext == *e) {
            return Self::Config;
        }
        if known_data.iter().any(|e| ext == *e) {
            return Self::Data;
        }
        if known_pdf.iter().any(|e| ext == *e) {
            return Self::Pdf;
        }
        if known_audio.iter().any(|e| ext == *e) {
            return Self::Audio;
        }
        Self::Unknown
    }

    pub fn to_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Markdown => "markdown",
            Self::Config => "config",
            Self::Data => "data",
            Self::Pdf => "pdf",
            Self::Audio => "audio",
            Self::Unknown => "unknown",
        }
    }

    /// Thin view over the ingest authority: the same table as
    /// [`file_kind::FileKind::coarse_type`] (the match lives there; this
    /// delegates so the two taxonomies cannot drift). See that method for
    /// the documented lossy buckets.
    #[must_use]
    pub fn from_kind(kind: file_kind::FileKind) -> Self {
        kind.coarse_type()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberType {
    #[default]
    FnDecl,
    FnPrivate,
    Struct,
    Enum,
    Union,
    EnumField,
    TestDecl,
    ComptimeBlock,
    Method,
    MethodPrivate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeType {
    Depends,
    Provides,
    Imports,
    Exports,
    Calls,
    CalledBy,
    Extends,
    Implements,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageKind {
    Prose,
    Code,
    Metadata,
    Insight,
    SkillDoc,
    CapabilityDoc,
    NotFound,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Param {
    #[serde(default)]
    pub name: SmolStr,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_name: Option<SmolStr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<SmolStr>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Member {
    #[serde(default)]
    pub type_name: MemberType,
    #[serde(default)]
    pub name: SmolStr,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_hash: Option<SmolStr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<SmolStr>,
    #[serde(default)]
    pub params: Vec<Param>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub returns: Option<SmolStr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<SmolStr>,
    #[serde(default)]
    pub tags: Vec<SmolStr>,
    #[serde(default)]
    pub is_pub: bool,
    #[serde(default)]
    pub members: Vec<Member>,
    #[serde(default)]
    pub equivalents: Vec<SmolStr>,
    #[serde(skip)]
    pub line: Option<u32>,
    /// Byte start of the member's source span, when parsed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_start: Option<u64>,
    /// Byte end (exclusive) of the member's source span, when parsed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_end: Option<u64>,
    /// Normalized item identity (`sig`/`body`/`doc` split), when parsed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_hash: Option<SmolStr>,
    /// Hash of the exact span bytes at parse time; citations re-hash
    /// live bytes against this, so a wrong excerpt is impossible.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range_hash: Option<SmolStr>,
    #[serde(default)]
    pub comment_generated: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Span {
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Skill {
    #[serde(default)]
    pub ref_path: SmolStr,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<SmolStr>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Meta {
    #[serde(default)]
    pub module: SmolStr,
    #[serde(default)]
    pub source: SmolStr,
    #[serde(default = "default_language")]
    pub language: SmolStr,
}

fn default_language() -> SmolStr {
    SmolStr::new("zig")
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CapabilityEval {
    #[serde(default)]
    pub capability_name: SmolStr,
    #[serde(default)]
    pub confidence: f32,
    #[serde(default)]
    pub evaluated_at_hash: SmolStr,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GuidanceDoc {
    #[serde(default)]
    pub meta: Meta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<SmolStr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<SmolStr>,
    #[serde(default)]
    pub keywords: Vec<SmolStr>,
    #[serde(default)]
    pub skills: Vec<Skill>,
    #[serde(default)]
    pub capabilities: Vec<SmolStr>,
    #[serde(default)]
    pub hashtags: Vec<SmolStr>,
    #[serde(default)]
    pub used_by: Vec<SmolStr>,
    #[serde(default)]
    pub members: Vec<Member>,
    #[serde(default)]
    pub equivalents: Vec<SmolStr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_eval: Option<CapabilityEval>,
    /// File/module-level generated summary, when produced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<CommentSummary>,
    /// Per-node generated summaries keyed by comment-block node key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub summaries: BTreeMap<String, CommentSummary>,
}

/// One generated comment summary: labelled generated commentary, never
/// source. Old sidecars parse without these fields; an older binary may
/// drop them on save, which is acceptable because summaries are derived
/// and regenerated.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
/// One generated comment summary: labelled generated commentary, never
/// source. Old sidecars parse without these fields; an older binary may
/// drop them on save, which is acceptable because summaries are derived
/// and regenerated.
pub struct CommentSummary {
    /// Generated summary text.
    #[serde(default)]
    pub text: SmolStr,
    /// Blake3 of the normalized comment text summarized.
    #[serde(default)]
    pub comment_hash: String,
    /// Blake3 of the node-code snippet summarized beside the comment;
    /// empty for comment-only and legacy rows. A code-only edit changes
    /// it, so the pass regenerates instead of serving stale text.
    #[serde(default)]
    pub code_hash: String,
    /// Model identity that produced the text.
    #[serde(default)]
    pub model: SmolStr,
    /// Prompt version that produced the text.
    #[serde(default)]
    pub prompt_version: u64,
    /// Sampling temperature used, as evidence of what was asked.
    #[serde(default)]
    pub temperature: f64,
    /// Fixed seed used, as evidence of what was asked.
    #[serde(default)]
    pub seed: u64,
    /// Generation time (ms epoch).
    #[serde(default)]
    pub generated_at: i64,
    /// Derivation status.
    #[serde(default)]
    pub status: OverlayStatus,
    /// Failure kind distinguishing transport `unavailable` from malformed
    /// output, without widening the shared status enum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<String>,
    /// Human-readable failure detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Cancelled,
}

/// The overlay kinds a node may carry (OVERLAYS §4.3).
///
/// The **source of truth** for each overlay's value is the corresponding node
/// field (`annotation` / `metadata["llm_overlay"]` / `embedding`); these
/// bookkeeping types are advisory/audit metadata stored under
/// `ContentNode.metadata["overlays"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// The overlay kinds a node may carry (OVERLAYS §4.3).
/// 
/// The **source of truth** for each overlay's value is the corresponding node
/// field (`annotation` / `metadata["llm_overlay"]` / `embedding`); these
/// bookkeeping types are advisory/audit metadata stored under
/// `ContentNode.metadata["overlays"]`.
pub enum OverlayKind {
    /// The `NodeAnnotation` (deterministic spacy parse, cheap).
    Spacy,
    /// LLM enrichment (summary/description).
    Llm,
    /// Dense vector.
    Embedding,
}

/// The lifecycle status of one overlay.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// The lifecycle status of one overlay.
pub enum OverlayStatus {
    /// Never asked for (the default).
    #[default]
    Absent,
    /// Derivation in flight.
    Pending,
    /// Derived and installed.
    Ready,
    /// Derivation failed; not retried until the node changes.
    Failed,
}

/// One overlay's bookkeeping entry (OVERLAYS §4.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One overlay's bookkeeping entry (OVERLAYS §4.3).
pub struct OverlayState {
    pub kind: OverlayKind,
    pub status: OverlayStatus,
    /// Which rung / model / embedder produced it.
    pub source: String,
    /// Unix secs when computed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
}

impl OverlayState {
    /// A fully-specified state entry.
    #[must_use]
    pub fn new(
        kind: OverlayKind,
        status: OverlayStatus,
        source: impl Into<String>,
        at: Option<u64>,
    ) -> Self {
        Self {
            kind,
            status,
            source: source.into(),
            at,
        }
    }

    /// The default "never asked" entry.
    #[must_use]
    pub fn absent(kind: OverlayKind) -> Self {
        Self::new(kind, OverlayStatus::Absent, "", None)
    }

    /// A derivation now in flight.
    #[must_use]
    pub fn pending(kind: OverlayKind, source: impl Into<String>) -> Self {
        Self::new(kind, OverlayStatus::Pending, source, None)
    }

    /// A completed derivation.
    #[must_use]
    pub fn ready(kind: OverlayKind, source: impl Into<String>, at: u64) -> Self {
        Self::new(kind, OverlayStatus::Ready, source, Some(at))
    }

    /// A failed derivation (permanent until reset).
    #[must_use]
    pub fn failed(kind: OverlayKind, source: impl Into<String>, at: u64) -> Self {
        Self::new(kind, OverlayStatus::Failed, source, Some(at))
    }
}

/// A rejected [`ContentNode::transition_overlay`] — a status change the
/// overlay lifecycle forbids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// A rejected [`ContentNode::transition_overlay`] — a status change the
/// overlay lifecycle forbids.
pub enum OverlayTransitionError {
    /// The overlay is already `Ready`; re-derivation is forbidden (at-most-once).
    AlreadyReady(OverlayKind),
    /// The overlay is `Failed`; permanent until the node changes (a reset to
    /// `Absent`).
    FailedPermanent(OverlayKind),
}

/// The `ContentNode.metadata` key under which the overlay bookkeeping lives.
pub const OVERLAYS_META_KEY: &str = "overlays";

/// Pure overlay bookkeeping ledger: the in-memory `Vec<OverlayState>` half of
/// `ContentNode.metadata["overlays"]`, without the per-call JSON round-trip.
/// Reads and lifecycle transitions run here; `ContentNode` only serializes
/// the ledger in and out of its advisory metadata at the boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
/// Pure overlay bookkeeping ledger: the in-memory `Vec<OverlayState>` half of
/// `ContentNode.metadata["overlays"]`, without the per-call JSON round-trip.
/// Reads and lifecycle transitions run here; `ContentNode` only serializes
/// the ledger in and out of its advisory metadata at the boundary.
pub struct OverlayLedger(Vec<OverlayState>);

impl OverlayLedger {
    /// Empty ledger (every kind reads as `Absent`).
    #[must_use]
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// All entries, in insertion order.
    #[must_use]
    pub fn states(&self) -> &[OverlayState] {
        &self.0
    }

    /// All entries, by value.
    #[must_use]
    pub fn into_states(self) -> Vec<OverlayState> {
        self.0
    }

    /// One overlay's bookkeeping, defaulting to `Absent` when never asked for.
    #[must_use]
    pub fn get(&self, kind: OverlayKind) -> OverlayState {
        self.0
            .iter()
            .find(|s| s.kind == kind)
            .cloned()
            .unwrap_or_else(|| OverlayState::absent(kind))
    }

    /// Upsert one overlay's bookkeeping (indexed by kind).
    pub fn upsert(&mut self, state: OverlayState) {
        if let Some(existing) = self.0.iter_mut().find(|s| s.kind == state.kind) {
            *existing = state;
        } else {
            self.0.push(state);
        }
    }

    /// Transition one overlay's status through the lifecycle (same rules as
    /// [`ContentNode::transition_overlay`]: `Ready` is never recomputed,
    /// `Failed` is permanent, the only exit from either terminal status is
    /// an explicit reset to `Absent`; equal-status no-ops are allowed).
    pub fn transition(
        &mut self,
        kind: OverlayKind,
        to: OverlayStatus,
        source: impl Into<String>,
        at: Option<u64>,
    ) -> Result<OverlayState, OverlayTransitionError> {
        let current = self.get(kind);
        match current.status {
            OverlayStatus::Ready if to != OverlayStatus::Ready && to != OverlayStatus::Absent => {
                return Err(OverlayTransitionError::AlreadyReady(kind));
            }
            OverlayStatus::Failed if to != OverlayStatus::Failed && to != OverlayStatus::Absent => {
                return Err(OverlayTransitionError::FailedPermanent(kind));
            }
            _ => {}
        }
        let next = OverlayState::new(kind, to, source, at);
        self.upsert(next.clone());
        Ok(next)
    }

    /// Decode the advisory metadata value (empty when absent or malformed —
    /// the metadata is advisory, never load-bearing).
    #[must_use]
    pub fn from_value(value: &serde_json::Value) -> Self {
        serde_json::from_value::<Vec<OverlayState>>(value.clone())
            .map(Self)
            .unwrap_or_default()
    }

    /// Encode back to a metadata value.
    #[must_use]
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(&self.0).expect("OverlayState serializes")
    }
}

/// A type-erased, `Arc`-shareable node overlay (the "ArcReady" overlay slot).
///
/// `ContentNode` lives in this crate, which must stay dependency-free of any
/// overlay producer (e.g. `spacy-rs`, which depends back on `fluent-types`), so
/// the overlay value is carried behind this opaque, downcastable slot instead
/// of a concrete type. The owning process (the router) installs the concrete
/// overlay behind an `Arc` and recovers it with [`NodeOverlay::as_any`] via the
/// typed read helper [`ContentNode::annotation_as`]. Nothing in this crate
/// knows a concrete overlay type.
///
/// Overlays are immutable after construction and never serialized with the node
/// (the node is durable without them and each overlay is lazily re-derivable
/// from LOD0), so no `Serialize`/`Deserialize` bound is required here.
pub trait NodeOverlay: std::any::Any + std::fmt::Debug + Send + Sync + 'static {
    /// Downcast the overlay to its concrete type.
    fn as_any(&self) -> &dyn std::any::Any;
}

impl dyn NodeOverlay {
    /// Recover an owned `Arc<T>` from the shared slot — the "ArcReady" handoff:
    /// the caller takes the same `Arc` out of the node, drops the node lock,
    /// and reads the immutable overlay lock-free. `None` when the slot holds a
    /// different concrete overlay type.
    #[must_use]
    pub fn downcast_arc<T: NodeOverlay>(self: std::sync::Arc<Self>) -> Option<std::sync::Arc<T>> {
        let any: std::sync::Arc<dyn std::any::Any + Send + Sync> = self;
        any.downcast::<T>().ok()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OriginRole {
    User,
    System,
    Assistant,
    Tool,
    Subagent,
    SelfOrigin,
    Other(String),
}

impl OriginRole {
    /// Single parser behind `From<String>`/`From<&str>` and both serde entry
    /// points: known names fold ASCII case, unknown strings keep their case
    /// in `Other`.
    fn parse(original: String) -> Self {
        match original.to_ascii_lowercase().as_str() {
            "user" => Self::User,
            "system" => Self::System,
            "assistant" => Self::Assistant,
            "tool" => Self::Tool,
            "subagent" => Self::Subagent,
            "self" => Self::SelfOrigin,
            _ => Self::Other(original),
        }
    }
    pub fn as_str(&self) -> &str {
        match self {
            Self::User => "user",
            Self::System => "system",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
            Self::Subagent => "subagent",
            Self::SelfOrigin => "self",
            Self::Other(s) => s.as_str(),
        }
    }
    pub fn is_user(&self) -> bool {
        matches!(self, Self::User)
    }
    pub fn is_system(&self) -> bool {
        matches!(self, Self::System)
    }
    pub fn is_assistant(&self) -> bool {
        matches!(self, Self::Assistant)
    }
    pub fn is_tool(&self) -> bool {
        matches!(self, Self::Tool)
    }
    pub fn is_subagent(&self) -> bool {
        matches!(self, Self::Subagent)
    }
    pub fn is_self_origin(&self) -> bool {
        matches!(self, Self::SelfOrigin)
    }
    pub fn is_other(&self) -> bool {
        matches!(self, Self::Other(_))
    }
}

impl std::fmt::Display for OriginRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<String> for OriginRole {
    fn from(s: String) -> Self {
        Self::parse(s)
    }
}
impl From<&str> for OriginRole {
    fn from(s: &str) -> Self {
        Self::from(s.to_string())
    }
}

// Custom deserialize that maps unknown strings to Other while preserving case.
/// Custom deserialize that maps unknown strings to Other while preserving case.
fn deserialize_origin_role<'de, D>(deserializer: D) -> Result<OriginRole, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    Ok(OriginRole::parse(s))
}

fn deserialize_origin_role_opt<'de, D>(deserializer: D) -> Result<Option<OriginRole>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt = Option::<String>::deserialize(deserializer)?;
    Ok(opt.map(OriginRole::parse))
}

// Override Serialize to emit string form
impl serde::Serialize for OriginRole {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for OriginRole {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserialize_origin_role(deserializer)
    }
}

/// Stable identity of a node's LOD0 text — the keying domain for ledger
/// annotations. The wire shape is a plain `u64` (`#[serde(transparent)]`),
/// computed with `common_core::hash::fnv1a64` so existing stored rows stay
/// joinable. `UNSTAMPED` (the `0` value) means there is no version to key
/// against: the node has no LOD0 content or has not yet been stamped.
///
/// A non-empty LOD0 hashes to `0` with probability 2⁻⁶⁴ and is treated as
/// unstamped (annotations rejected). This sentinel reservation is accepted,
/// not accidental.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default,
)]
#[serde(transparent)]
/// Stable identity of a node's LOD0 text — the keying domain for ledger
/// annotations. The wire shape is a plain `u64` (`#[serde(transparent)]`),
/// computed with `common_core::hash::fnv1a64` so existing stored rows stay
/// joinable. `UNSTAMPED` (the `0` value) means there is no version to key
/// against: the node has no LOD0 content or has not yet been stamped.
/// 
/// A non-empty LOD0 hashes to `0` with probability 2⁻⁶⁴ and is treated as
/// unstamped (annotations rejected). This sentinel reservation is accepted,
/// not accidental.
pub struct ContentHash(u64);

impl ContentHash {
    /// No version to key against.
    pub const UNSTAMPED: Self = Self(0);

    /// Hash LOD0 text; empty content maps to [`Self::UNSTAMPED`].
    #[must_use]
    pub fn from_lod0(content: &str) -> Self {
        if content.is_empty() {
            Self::UNSTAMPED
        } else {
            Self(common_core::hash::fnv1a64(content.as_bytes()))
        }
    }

    /// Whether this is the unstamped sentinel.
    #[must_use]
    pub fn is_unstamped(self) -> bool {
        self.0 == 0
    }

    /// Raw value for the database boundary only.
    #[must_use]
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

impl From<u64> for ContentHash {
    fn from(v: u64) -> Self {
        Self(v)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContentNode {
    // ── Core fields ──
    pub id: Option<NodeId>,
    pub name: SmolStr,
    pub source: String,
    /// A stable hash of LOD0 (the full text), the **keying domain** for ledger
    /// annotations. Computed by the node store's single write
    /// funnel; a mutation of LOD0 changes this hash and thereby invalidates the
    /// node's cached annotations — no staleness scheduler. [`ContentHash::UNSTAMPED`]
    /// when the node has no LOD0 content (or has not yet been stamped).
    #[serde(default)]
    pub content_hash: ContentHash,
    #[serde(default)]
    pub lod: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding: Option<Vec<f32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<u8>>,
    // ── Session fields ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_origin_role_opt",
        skip_serializing_if = "Option::is_none"
    )]
    pub role: Option<OriginRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_index: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_lod: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_status: Option<StepStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
    // ── Overlay slot (in-memory only) ──
    /// The node's derived overlay (e.g. the router ledger's `NodeAnnotation`),
    /// shared behind an `Arc` so every holder of the node reads the same
    /// immutable document without a copy or a lock. **Never serialized**
    /// (`#[serde(skip)]`): the node is durable without it and the overlay is
    /// lazily re-derivable from LOD0, so a reloaded node simply starts at
    /// `None`. Reading it costs one `Arc` clone under the node's `RwLock` read
    /// guard and is then lock-free.
    #[serde(skip)]
    pub annotation: Option<std::sync::Arc<dyn NodeOverlay>>,
}

impl ContentNode {
    pub fn content(&self) -> Option<&str> {
        self.lod.first().map(String::as_str)
    }

    /// The typed overlay (e.g. `NodeAnnotation`), downcast from the
    /// opaque slot. `None` when the node carries no overlay of type `T`.
    #[must_use]
    pub fn annotation_as<T: NodeOverlay>(&self) -> Option<&T> {
        self.annotation
            .as_deref()
            .and_then(|o| o.as_any().downcast_ref())
    }

    /// The node's overlay bookkeeping, from `metadata["overlays"]` (empty when
    /// absent or malformed — the metadata is advisory, never load-bearing).
    #[must_use]
    pub fn overlays(&self) -> Vec<OverlayState> {
        self.overlay_ledger().into_states()
    }

    /// One overlay's bookkeeping, defaulting to `Absent` when never asked for.
    #[must_use]
    pub fn overlay(&self, kind: OverlayKind) -> OverlayState {
        self.overlay_ledger().get(kind)
    }

    /// Upsert one overlay's bookkeeping into `metadata["overlays"]` (indexed
    /// by kind; allocates the metadata object on first use).
    pub fn set_overlay(&mut self, state: OverlayState) {
        let mut ledger = self.overlay_ledger();
        ledger.upsert(state);
        self.set_overlay_ledger(&ledger);
    }

    /// The node's overlay bookkeeping as a pure ledger (one metadata decode;
    /// transitions on the ledger touch no JSON until written back).
    #[must_use]
    pub fn overlay_ledger(&self) -> OverlayLedger {
        self.metadata
            .as_ref()
            .and_then(|m| m.get(OVERLAYS_META_KEY))
            .map(OverlayLedger::from_value)
            .unwrap_or_default()
    }

    /// Write a ledger back into `metadata["overlays"]` (allocates the
    /// metadata object on first use).
    pub fn set_overlay_ledger(&mut self, ledger: &OverlayLedger) {
        let meta = self.metadata.get_or_insert_with(|| serde_json::json!({}));
        meta[OVERLAYS_META_KEY] = ledger.to_value();
    }

    /// Transition one overlay's status through the lifecycle:
    ///
    /// `absent → pending → ready` and `absent → pending → failed` are the
    /// normal paths. A `ready` overlay is never recomputed (at-most-once) and a
    /// `failed` overlay is **permanent** — neither is retried. The only exit
    /// from either terminal status is an explicit reset to `Absent` (the node
    /// changed); equal-status no-ops are allowed.
    ///
    /// On success the transition is applied and the new entry returned.
    pub fn transition_overlay(
        &mut self,
        kind: OverlayKind,
        to: OverlayStatus,
        source: impl Into<String>,
        at: Option<u64>,
    ) -> Result<OverlayState, OverlayTransitionError> {
        let mut ledger = self.overlay_ledger();
        let next = ledger.transition(kind, to, source, at)?;
        self.set_overlay_ledger(&ledger);
        Ok(next)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnnHit {
    pub node_id: NodeId,
    pub distance: f32,
    pub name: SmolStr,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub node_id: NodeId,
    pub name: SmolStr,
    pub depth: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stage {
    pub kind: StageKind,
    pub content: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub query: String,
    pub stages: Vec<Stage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResult {
    pub filepath: String,
    pub members_added: usize,
    pub members_updated: usize,
    pub members_removed: usize,
    pub has_changes: bool,
    pub comments_generated: bool,
    pub source_modified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMatch {
    pub filename: SmolStr,
    pub filepath: SmolStr,
    #[serde(default)]
    pub description: SmolStr,
    #[serde(default)]
    pub line_context: SmolStr,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ASTAnalysis {
    pub filepath: SmolStr,
    #[serde(default)]
    pub functions: Vec<Member>,
    #[serde(default)]
    pub classes: Vec<Member>,
    #[serde(default)]
    pub imports: Vec<SmolStr>,
    #[serde(default)]
    pub patterns_detected: Vec<SmolStr>,
    #[serde(default)]
    pub token_count: usize,
    #[serde(default)]
    pub signature_preview: SmolStr,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuidanceInfo {
    pub path: SmolStr,
    #[serde(default)]
    pub comment: SmolStr,
    #[serde(default)]
    pub functions: Vec<Member>,
    #[serde(default)]
    pub classes: Vec<Member>,
    #[serde(default)]
    pub skills: Vec<SmolStr>,
    #[serde(default)]
    pub tags: Vec<SmolStr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetType {
    File,
    Phony,
    Abstract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorKind {
    Native,
    Docker,
    Wasm,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmTool {
    pub name: SmolStr,
    pub path: String,
    pub capabilities: Vec<SmolStr>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_roundtrip() {
        let id = NodeId::from_int(42);
        assert_eq!(id.as_int(), 42);
    }
    #[test]
    fn session_id_roundtrip() {
        let id = SessionId::new("test_session");
        assert_eq!(id.as_str(), "test_session");
    }
    #[test]
    fn types_are_distinct() {
        fn takes_node(_: NodeId) {}
        fn takes_session(_: &SessionId) {}
        takes_node(NodeId::from_int(1));
        let sess = SessionId::new("test");
        takes_session(&sess);
    }
    #[test]
    fn file_type_from_extension() {
        assert_eq!(FileType::from_extension(".zig"), FileType::Source);
        assert_eq!(FileType::from_extension(".md"), FileType::Markdown);
        assert_eq!(FileType::from_extension(".json"), FileType::Config);
        assert_eq!(FileType::from_extension(".xyz"), FileType::Unknown);
    }
    #[test]
    fn file_type_to_str() {
        assert_eq!(FileType::Source.to_str(), "source");
        assert_eq!(FileType::Markdown.to_str(), "markdown");
        assert_eq!(FileType::Config.to_str(), "config");
        assert_eq!(FileType::Data.to_str(), "data");
        assert_eq!(FileType::Pdf.to_str(), "pdf");
        assert_eq!(FileType::Audio.to_str(), "audio");
        assert_eq!(FileType::Unknown.to_str(), "unknown");
    }
    #[test]
    fn file_type_from_extension_data_pdf_audio() {
        assert_eq!(FileType::from_extension(".csv"), FileType::Data);
        assert_eq!(FileType::from_extension(".db"), FileType::Data);
        assert_eq!(FileType::from_extension(".pdf"), FileType::Pdf);
        assert_eq!(FileType::from_extension(".mp3"), FileType::Audio);
        assert_eq!(FileType::from_extension(".wav"), FileType::Audio);
    }
    #[test]
    fn member_serde_roundtrip() {
        let m = Member {
            type_name: MemberType::FnDecl,
            name: SmolStr::new("foo"),
            match_hash: None,
            signature: Some(SmolStr::new("fn foo()")),
            params: vec![],
            returns: None,
            comment: Some(SmolStr::new("test function")),
            tags: vec![SmolStr::new("public")],
            is_pub: true,
            members: vec![],
            equivalents: vec![],
            line: Some(10),
            span_start: None,
            span_end: None,
            item_hash: None,
            range_hash: None,
            comment_generated: false,
        };
        let json = serde_json::to_string(&m).unwrap();
        let m2: Member = serde_json::from_str(&json).unwrap();
        assert_eq!(m.name, m2.name);
        assert_eq!(m.is_pub, m2.is_pub);
    }
    #[test]
    fn guidance_doc_default_language_on_deserialize() {
        let json = r#"{"meta":{"module":"test","source":"test.zig"}}"#;
        let doc: GuidanceDoc = serde_json::from_str(json).unwrap();
        assert_eq!(doc.meta.language.as_str(), "zig");
    }
    #[test]
    fn guidance_doc_serde_roundtrip() {
        let doc = GuidanceDoc {
            meta: Meta {
                module: SmolStr::new("test"),
                source: SmolStr::new("test.zig"),
                language: SmolStr::new("zig"),
            },
            comment: Some(SmolStr::new("module comment")),
            detail: None,
            keywords: vec![SmolStr::new("api")],
            skills: vec![],
            capabilities: vec![],
            hashtags: vec![],
            used_by: vec![],
            members: vec![],
            equivalents: vec![],
            capability_eval: None,
            summary: None,
            summaries: std::collections::BTreeMap::new(),
        };
        let json = serde_json::to_string(&doc).unwrap();
        let doc2: GuidanceDoc = serde_json::from_str(&json).unwrap();
        assert_eq!(doc.meta.module, doc2.meta.module);
        assert_eq!(doc.comment, doc2.comment);
    }
    #[test]
    fn knn_hit_creation() {
        let hit = KnnHit {
            node_id: NodeId::from_int(1),
            distance: 0.95,
            name: SmolStr::new("test"),
        };
        assert_eq!(hit.node_id.as_int(), 1);
    }
    #[test]
    fn stage_kind_serde() {
        let kinds = [StageKind::Prose, StageKind::Code, StageKind::NotFound];
        for kind in &kinds {
            let json = serde_json::to_string(kind).unwrap();
            let k2: StageKind = serde_json::from_str(&json).unwrap();
            assert_eq!(*kind, k2);
        }
    }
    #[test]
    fn target_type_serde() {
        let t = TargetType::File;
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, "\"file\"");
        let t2: TargetType = serde_json::from_str(&json).unwrap();
        assert_eq!(t, t2);
    }
    #[test]
    fn executor_kind_serde() {
        let e = ExecutorKind::Wasm;
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(json, "\"wasm\"");
    }
    #[test]
    fn overlay_status_absent_pending_ready() {
        let mut node = ContentNode::default();
        assert_eq!(
            node.overlay(OverlayKind::Spacy).status,
            OverlayStatus::Absent
        );
        node.transition_overlay(OverlayKind::Spacy, OverlayStatus::Pending, "arceager", None)
            .expect("pending");
        assert_eq!(
            node.overlay(OverlayKind::Spacy).status,
            OverlayStatus::Pending
        );
        let ready = node
            .transition_overlay(
                OverlayKind::Spacy,
                OverlayStatus::Ready,
                "arceager",
                Some(1000),
            )
            .expect("ready");
        assert_eq!(ready.status, OverlayStatus::Ready);
        assert_eq!(ready.at, Some(1000));
        assert_eq!(
            node.overlay(OverlayKind::Spacy).status,
            OverlayStatus::Ready
        );
        // Other kinds stay untouched.
        assert_eq!(node.overlay(OverlayKind::Llm).status, OverlayStatus::Absent);
    }

    #[test]
    fn overlay_status_absent_pending_failed() {
        let mut node = ContentNode::default();
        node.transition_overlay(OverlayKind::Embedding, OverlayStatus::Pending, "bge", None)
            .expect("pending");
        let failed = node
            .transition_overlay(
                OverlayKind::Embedding,
                OverlayStatus::Failed,
                "bge",
                Some(2000),
            )
            .expect("failed");
        assert_eq!(failed.status, OverlayStatus::Failed);
        assert_eq!(
            node.overlay(OverlayKind::Embedding).status,
            OverlayStatus::Failed
        );
    }

    #[test]
    fn overlay_failed_is_permanent_until_reset() {
        let mut node = ContentNode::default();
        node.transition_overlay(OverlayKind::Llm, OverlayStatus::Pending, "llama", None)
            .expect("pending");
        node.transition_overlay(OverlayKind::Llm, OverlayStatus::Failed, "llama", Some(3000))
            .expect("failed");
        // A retry is rejected: a failed overlay is never re-derived.
        assert_eq!(
            node.transition_overlay(OverlayKind::Llm, OverlayStatus::Pending, "llama", None),
            Err(OverlayTransitionError::FailedPermanent(OverlayKind::Llm))
        );
        assert_eq!(
            node.transition_overlay(OverlayKind::Llm, OverlayStatus::Ready, "llama", Some(4000)),
            Err(OverlayTransitionError::FailedPermanent(OverlayKind::Llm))
        );
        // The only exit is an explicit reset (the node changed) → re-derive.
        node.transition_overlay(OverlayKind::Llm, OverlayStatus::Absent, "", None)
            .expect("reset");
        node.transition_overlay(OverlayKind::Llm, OverlayStatus::Pending, "llama", None)
            .expect("re-pending after reset");
        node.transition_overlay(OverlayKind::Llm, OverlayStatus::Ready, "llama", Some(5000))
            .expect("re-ready after reset");
        assert_eq!(node.overlay(OverlayKind::Llm).status, OverlayStatus::Ready);
    }

    #[test]
    fn overlay_ready_is_not_recomputed() {
        let mut node = ContentNode::default();
        node.transition_overlay(
            OverlayKind::Spacy,
            OverlayStatus::Ready,
            "arceager",
            Some(1),
        )
        .expect("ready");
        assert_eq!(
            node.transition_overlay(OverlayKind::Spacy, OverlayStatus::Pending, "llm", None),
            Err(OverlayTransitionError::AlreadyReady(OverlayKind::Spacy))
        );
        assert_eq!(
            node.transition_overlay(OverlayKind::Spacy, OverlayStatus::Failed, "llm", Some(2)),
            Err(OverlayTransitionError::AlreadyReady(OverlayKind::Spacy))
        );
        // Equal-status no-op is allowed.
        node.transition_overlay(
            OverlayKind::Spacy,
            OverlayStatus::Ready,
            "arceager",
            Some(3),
        )
        .expect("no-op");
        assert_eq!(
            node.overlay(OverlayKind::Spacy).status,
            OverlayStatus::Ready
        );
    }

    #[test]
    fn overlay_state_serde_roundtrip() {
        let state = OverlayState::ready(OverlayKind::Spacy, "arceager", 42);
        let json = serde_json::to_string(&state).expect("serialize");
        let back: OverlayState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, state);
        // Wire names are snake_case.
        assert_eq!(
            serde_json::to_string(&OverlayKind::Spacy).unwrap(),
            "\"spacy\""
        );
        assert_eq!(
            serde_json::to_string(&OverlayKind::Embedding).unwrap(),
            "\"embedding\""
        );
        assert_eq!(
            serde_json::to_string(&OverlayStatus::Pending).unwrap(),
            "\"pending\""
        );
        assert_eq!(
            serde_json::to_string(&OverlayStatus::Failed).unwrap(),
            "\"failed\""
        );
        // Absent entries drop the `at` field.
        let absent = OverlayState::absent(OverlayKind::Llm);
        let json = serde_json::to_string(&absent).expect("serialize");
        assert!(!json.contains("\"at\""));
        let back: OverlayState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, absent);
    }

    #[test]
    fn overlay_bookkeeping_lives_in_metadata_overlays() {
        let mut node = ContentNode::default();
        node.set_overlay(OverlayState::pending(OverlayKind::Spacy, "arceager"));
        // Stored under the namespaced key, as advisory JSON.
        let meta = node.metadata.as_ref().expect("metadata allocated");
        assert!(meta.get(OVERLAYS_META_KEY).is_some());
        // Survives a ContentNode serde round-trip (the durable content_json).
        let json = serde_json::to_string(&node).expect("serialize");
        let back: ContentNode = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            back.overlay(OverlayKind::Spacy).status,
            OverlayStatus::Pending
        );
        // Updating the same kind replaces, never duplicates.
        node.set_overlay(OverlayState::ready(OverlayKind::Spacy, "arceager", 9));
        assert_eq!(node.overlays().len(), 1);
        assert_eq!(
            node.overlay(OverlayKind::Spacy).status,
            OverlayStatus::Ready
        );
    }

    #[test]
    fn overlay_ledger_transition_matrix() {
        use OverlayStatus::{Absent, Failed, Pending, Ready};
        // Every (from → to) pair, exercised purely on the ledger: terminal
        // statuses exit only via reset; equal-status no-ops always pass.
        for kind in [OverlayKind::Spacy, OverlayKind::Llm, OverlayKind::Embedding] {
            for from in [Absent, Pending, Ready, Failed] {
                for to in [Absent, Pending, Ready, Failed] {
                    let mut ledger = OverlayLedger::default();
                    if from != Absent {
                        // Drive the ledger to `from` through a legal prefix.
                        ledger.transition(kind, Pending, "t", None).ok();
                        if from == Ready || from == Failed {
                            ledger.transition(kind, from, "t", Some(1)).ok();
                        }
                        assert_eq!(ledger.get(kind).status, from, "{kind:?} setup");
                    }
                    let result = ledger.transition(kind, to, "t", Some(2));
                    let terminal = from == Ready || from == Failed;
                    if !terminal || to == from || to == Absent {
                        assert!(result.is_ok(), "{kind:?} {from:?} → {to:?} must pass");
                        assert_eq!(ledger.get(kind).status, to);
                    } else {
                        assert!(result.is_err(), "{kind:?} {from:?} → {to:?} must reject");
                        assert_eq!(ledger.get(kind).status, from);
                    }
                }
            }
        }
    }

    #[test]
    fn overlay_ledger_roundtrips_through_node_metadata() {
        // The ledger is the pure half: node reads/writes agree with it entry
        // for entry, and unrelated kinds stay absent.
        let mut ledger = OverlayLedger::default();
        ledger
            .transition(OverlayKind::Spacy, OverlayStatus::Pending, "arceager", None)
            .expect("pending");
        ledger
            .transition(
                OverlayKind::Spacy,
                OverlayStatus::Ready,
                "arceager",
                Some(7),
            )
            .expect("ready");
        let mut node = ContentNode::default();
        node.set_overlay_ledger(&ledger);
        assert_eq!(node.overlays().len(), 1);
        let back = node.overlay_ledger();
        assert_eq!(back.get(OverlayKind::Spacy).status, OverlayStatus::Ready);
        assert_eq!(back.get(OverlayKind::Spacy).at, Some(7));
        assert_eq!(back.get(OverlayKind::Llm).status, OverlayStatus::Absent);
        // Delegated entry points agree with the ledger.
        assert_eq!(
            node.overlay(OverlayKind::Spacy),
            back.get(OverlayKind::Spacy)
        );
    }

    #[test]
    fn origin_role_unknown_preserves_case_everywhere() {
        // All three parse sites agree: known names fold case, unknown keeps it.
        assert_eq!(
            OriginRole::from("CuStOm".to_string()),
            OriginRole::Other("CuStOm".to_string())
        );
        assert_eq!(OriginRole::from("USER"), OriginRole::User);
        let de: OriginRole = serde_json::from_str("\"CuStOm\"").expect("deserialize");
        assert_eq!(de, OriginRole::Other("CuStOm".to_string()));
        let de_known: OriginRole = serde_json::from_str("\"SELF\"").expect("deserialize");
        assert_eq!(de_known, OriginRole::SelfOrigin);
        // The Option site (ContentNode.role) preserves case too.
        let node: ContentNode = serde_json::from_str(r#"{"name":"n","source":"s","role":"MiXeD"}"#)
            .expect("deserialize");
        assert_eq!(node.role, Some(OriginRole::Other("MiXeD".to_string())));
        // Serialize round-trips the preserved case.
        let json = serde_json::to_string(&OriginRole::Other("MiXeD".to_string())).unwrap();
        assert_eq!(json, "\"MiXeD\"");
    }

    #[test]
    fn content_node_defaults() {
        let node = ContentNode {
            id: None,
            name: SmolStr::new("root"),
            source: "full text".into(),
            content_hash: ContentHash::UNSTAMPED,
            lod: vec!["hello".into()],
            embedding: None,
            capabilities: None,
            session_id: None,
            request_id: None,
            role: None,
            turn_index: None,
            accepted: None,
            acceptance_score: None,
            active_lod: None,
            parent_id: None,
            step_id: None,
            step_status: None,
            metadata: None,
            created_at: None,
            annotation: None,
        };
        assert_eq!(node.lod.len(), 1);
        assert_eq!(node.content(), Some("hello"));
        assert!(node.embedding.is_none());
        assert!(node.annotation.is_none());
    }

    /// A concrete, dependency-free overlay for exercising the opaque slot.
    #[derive(Debug, Clone)]
    struct TestOverlay {
        value: u64,
    }

    impl NodeOverlay for TestOverlay {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[derive(Debug, Clone)]
    struct OtherOverlay;

    impl NodeOverlay for OtherOverlay {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[test]
    fn content_node_annotation_slot_downedcasts_and_shares() {
        let overlay = TestOverlay { value: 42 };
        let node = ContentNode {
            annotation: Some(std::sync::Arc::new(overlay)),
            ..ContentNode::default()
        };
        // Typed read through the opaque slot (the router's read path).
        let got = node.annotation_as::<TestOverlay>().expect("downcast");
        assert_eq!(got.value, 42);
        // Unknown overlay types read as absent, not a panic.
        assert!(node.annotation_as::<OtherOverlay>().is_none());
        // Shared: two holders read the same immutable overlay without a lock.
        let shared = node.annotation.clone().expect("shared slot");
        assert_eq!(
            shared.as_any().downcast_ref::<TestOverlay>().unwrap().value,
            42
        );
        // The node remains Debug/Clone/Default with the slot populated.
        let clone = node.clone();
        assert_eq!(clone.annotation_as::<TestOverlay>().unwrap().value, 42);
    }

    #[test]
    fn content_node_annotation_is_never_serialized() {
        let node = ContentNode {
            annotation: Some(std::sync::Arc::new(TestOverlay { value: 7 })),
            ..ContentNode::default()
        };
        let json = serde_json::to_string(&node).expect("serialize");
        assert!(
            !json.contains("annotation"),
            "the overlay is in-memory only and must not leak into content_json: {json}"
        );
        // A node deserialized from that durable JSON carries no overlay.
        let back: ContentNode = serde_json::from_str(&json).expect("deserialize");
        assert!(back.annotation.is_none());
    }

    #[test]
    fn content_node_back_compat_pre_annotation_field() {
        // A `ContentNode` serialized before the annotation slot existed (no
        // `annotation` key, no `..Default` churn) deserializes correctly with
        // `annotation: None` (OVERLAYS M4.2).
        let pre_field_json = r#"{
            "id": 7,
            "name": "legacy",
            "source": "old full text",
            "lod": ["old full text", "old summary"],
            "embedding": null,
            "capabilities": null,
            "session_id": null,
            "request_id": null,
            "role": null,
            "turn_index": null,
            "accepted": null,
            "acceptance_score": null,
            "active_lod": null,
            "parent_id": null,
            "step_id": null,
            "step_status": null,
            "metadata": null,
            "created_at": null
        }"#;
        let node: ContentNode = serde_json::from_str(pre_field_json).expect("deserialize");
        assert_eq!(node.name.as_str(), "legacy");
        assert_eq!(node.lod.len(), 2);
        assert!(node.annotation.is_none(), "legacy node has no overlay");
        // The node round-trips through the store's content_json unchanged.
        let json = serde_json::to_string(&node).expect("serialize");
        let back: ContentNode = serde_json::from_str(&json).expect("round trip");
        assert_eq!(back.name.as_str(), "legacy");
        assert!(back.annotation.is_none());
        // `content_hash` predates this node's serialization → defaults to UNSTAMPED.
        assert_eq!(
            node.content_hash,
            ContentHash::UNSTAMPED,
            "missing content_hash back-compat defaults to UNSTAMPED"
        );
    }
    #[test]
    fn content_hash_wire_is_plain_u64() {
        let json = serde_json::to_string(&ContentHash::UNSTAMPED).expect("serialize");
        assert_eq!(json, "0");
        let back: ContentHash = serde_json::from_str("0").expect("deserialize");
        assert_eq!(back, ContentHash::UNSTAMPED);
        assert!(back.is_unstamped());
        // A node whose JSON carries the old numeric shape still parses.
        let node: ContentNode = serde_json::from_str(
            r#"{"id":null,"name":"n","source":"s","content_hash":0,"lod":[]}"#,
        )
        .expect("old row parses");
        assert_eq!(node.content_hash, ContentHash::UNSTAMPED);
        // Serialized UNSTAMPED node emits the same `0` number.
        let out = serde_json::to_string(&node).expect("serialize");
        assert!(out.contains(r#""content_hash":0"#), "{out}");
    }

    #[test]
    fn content_hash_from_lod0_matches_fnv() {
        assert_eq!(ContentHash::from_lod0(""), ContentHash::UNSTAMPED);
        let h = ContentHash::from_lod0("x");
        assert!(!h.is_unstamped());
        assert_eq!(h.as_u64(), common_core::hash::fnv1a64(b"x"));
        assert_eq!(ContentHash::default(), ContentHash::UNSTAMPED);
    }

    #[test]
    fn wasm_tool_serde() {
        let tool = WasmTool {
            name: SmolStr::new("tokenizer"),
            path: "/usr/local/bin/guidance-tokenizer.wasm".into(),
            capabilities: vec![SmolStr::new("tokenize")],
        };
        let json = serde_json::to_string(&tool).unwrap();
        let t2: WasmTool = serde_json::from_str(&json).unwrap();
        assert_eq!(tool.name, t2.name);
    }
    #[test]
    fn context_hit_roundtrip() {
        let hit = ContextHit {
            source: "ledger".into(),
            content: "2+2 is 4".into(),
            score: 0.97,
            metadata: Some(serde_json::json!({ "node_id": 42 })),
        };
        let json = serde_json::to_string(&hit).unwrap();
        let back: ContextHit = serde_json::from_str(&json).unwrap();
        assert_eq!(back, hit);
        assert_eq!(back.source, "ledger");
    }
    #[test]
    fn context_hit_defaults_metadata_to_none() {
        let hit: ContextHit =
            serde_json::from_str(r#"{"source":"c","content":"x","score":0.5}"#).unwrap();
        assert!(hit.metadata.is_none());
        assert!((hit.score - 0.5).abs() < f32::EPSILON);
    }
}