//! Orientation plane: project, ownership, and domain cards first.
//!
//! `Orientation` and `Ownership` queries start here, never in code
//! recall. Cards render with source anchors and code paths, so every
//! answer jumps deterministically from card to doc to code.

use crate::query::search_db::{GuidanceSearchDb, ProjectCard};
use common_core::sort::sorted_by_vec;

/// One card finding for the read engine.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One card finding for the read engine.
pub struct CardFinding {
    /// Card key (`crate:guidance-core`).
    pub id: String,
    /// `workspace` | `crate` | `domain`.
    pub kind: String,
    /// Card title.
    pub title: String,
    /// Card body.
    pub body: String,
    /// Doc anchor the card cites.
    pub source: String,
    /// Code paths the card points at.
    pub code_paths: Vec<String>,
}

/// The orientation plane: deterministic cards over the committed
/// `project_cards` table.
pub struct OrientationPlane {
    cards: Vec<ProjectCard>,
}

impl OrientationPlane {
    /// Load every stored card, key-ordered (the store already sorts).
    pub fn load(db: &GuidanceSearchDb) -> Result<Self, String> {
        let cards = db
            .project_cards_all()
            .map_err(|error| format!("project cards: {error}"))?;
        Ok(Self { cards })
    }

    /// Cards for an orientation question: the workspace card first,
    /// then crates, then domains.
    #[must_use]
    pub fn orientation_cards(&self) -> Vec<CardFinding> {
        let out: Vec<CardFinding> = self
            .cards
            .iter()
            .filter(|card| matches!(card.kind.as_str(), "workspace" | "crate" | "domain"))
            .map(CardFinding::from)
            .collect();
        // M2.5: shared stable sorter; kind-rank then id, verbatim.
        sorted_by_vec(out, |a, b| {
            fn rank(kind: &str) -> u8 {
                match kind {
                    "workspace" => 0,
                    "crate" => 1,
                    _ => 2,
                }
            }
            (rank(&a.kind), &a.id).cmp(&(rank(&b.kind), &b.id))
        })
    }

    /// Cards for an ownership question: the owning crate card plus its
    /// domain card. Empty when nothing is stored.
    #[must_use]
    pub fn ownership_cards(&self, package: &str) -> Vec<CardFinding> {
        let mut out = Vec::new();
        if let Some(card) = self
            .cards
            .iter()
            .find(|card| card.kind == "crate" && card.title == package)
        {
            let paths = code_paths_of(card);
            out.push(CardFinding::from(card));
            let domain = self.cards.iter().find(|candidate| {
                candidate.kind == "domain"
                    && code_paths_of(candidate)
                        .iter()
                        .any(|path| paths.contains(path))
            });
            if let Some(domain) = domain {
                out.push(CardFinding::from(domain));
            }
        }
        out
    }

    /// Whether the plane can answer at all (readiness input: an empty
    /// table degrades orientation, never blocks it silently).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }
}

impl CardFinding {
    fn json_list(raw: &str) -> Vec<String> {
        serde_json::from_str(raw).unwrap_or_default()
    }
}

impl From<&ProjectCard> for CardFinding {
    fn from(card: &ProjectCard) -> Self {
        Self {
            id: card.key.clone(),
            kind: card.kind.clone(),
            title: card.title.clone(),
            body: card.body.clone(),
            source: if card.anchor.is_empty() {
                card.source_path.clone()
            } else {
                format!("{}#{}", card.source_path, card.anchor)
            },
            code_paths: Self::json_list(&card.code_paths_json),
        }
    }
}

/// Workspace-relative code paths of one card.
fn code_paths_of(card: &ProjectCard) -> Vec<String> {
    serde_json::from_str(&card.code_paths_json).unwrap_or_default()
}
