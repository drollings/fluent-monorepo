//! Schema-driven grant arming form.
//!
//! The form renders from the patch descriptor's nested schema (tier names,
//! leaf names, kinds, formats, hints) with no hand-written field list: one
//! typed editor per leaf, grouped under its tier, and a new tier or leaf
//! appears without a form edit. Leaf drafts assemble into per-tier boundary
//! strings; the envelope carries those strings, and the server parses each
//! through its own `FromStr` codec — the form performs no tier parse and
//! the gateway performs no second one.
//!
//! The form never mints a grant locally. Submit parses each drafted leaf
//! fail-closed (naming the leaf), refuses a partially drafted tier (a tier
//! submits whole or not at all — the core's tighten rule needs every leg),
//! and sends the patch envelope through
//! [`TerminalGateway::arm`](protopyne_view::TerminalGateway::arm). The core
//! alone validates, tightens, and refuses — a loosening surfaces the core's
//! refusal, and the [`GrantFormError::Gateway`] variant keeps that refusal
//! distinguishable from local form validation.
//!
//! Worst case before confirmation: the painter refreshes a preview through
//! [`TerminalGateway::preview_arm`](protopyne_view::TerminalGateway::preview_arm)
//! whenever the drafts change and renders the computed worst case beside
//! the editors. Submit stays unactionable until a successful preview covers
//! exactly the current drafts, so the operator always confirms against the
//! core's own number — and a caller that bypasses the painter still meets
//! the same freshness gate inside [`GrantFormState::submit`].
//!
//! The tier text beside the editors is a *display* read, and it is gated: the
//! painter refreshes at most once per
//! [`DEFAULT_TIER_READ_INTERVAL_FRAMES`] frames (or sooner when the grant's
//! content fingerprint changes), then paints how old the text is. A failed
//! read keeps the last good text and adds an "unavailable" chip — the form
//! never presents stale bytes as live. Paint code awaits nothing: this is a
//! frame-count gate, not [`fluent_egui::RefreshGate`], whose `Limiter` is
//! async and cannot be driven from a frame.
//!
//! Paint-edge direction holds: this module names the gateway abstraction
//! only (pinned by the ledger tests), never the trading concretion, and
//! money crosses only as validated decimal text — the decimal boundary
//! stays one-way.

use std::collections::{BTreeMap, HashMap};
use std::fmt::{Display, Formatter};

use common_core::hash::fingerprint_json;
use fluent_egui::FieldKind;
use protopyne_chart_model::DisplayOnly;
use protopyne_view::{ArmResponse, GatewayError, TerminalGateway, WorstCase};

/// One editable leaf, lifted from the descriptor's nested schema.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One editable leaf, lifted from the descriptor's nested schema.
pub struct GrantFormField {
    /// Descriptor tier name; also the envelope key for this tier.
    pub tier: String,
    /// Nested leaf name, or `None` when the tier itself is the leaf (the
    /// window end edits as one date-time value, not a document).
    pub leaf: Option<String>,
    /// Editor kind from the leaf schema's `type`.
    pub kind: FieldKind,
    /// Format refinement from the leaf schema (`decimal`, `date-time`).
    pub format: Option<String>,
    /// Schema hint rendered beside the editor.
    pub description: String,
}

impl GrantFormField {
    /// Draft key: the tier, or the tier dot the leaf.
    #[must_use]
    pub fn path(&self) -> String {
        match &self.leaf {
            Some(leaf) => format!("{}.{leaf}", self.tier),
            None => self.tier.clone(),
        }
    }
}

/// What a submit can refuse, with local validation kept distinct from the
/// core's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
/// What a submit can refuse, with local validation kept distinct from the
/// core's answer.
pub enum GrantFormError {
    /// No tier drafted: nothing was sent.
    Empty,
    /// A drafted leaf is not its kind: nothing was sent.
    FieldParse {
        /// Descriptor leaf path (`tier` or `tier.leaf`).
        field: String,
        /// Parser reason.
        reason: String,
    },
    /// A tier is drafted leaf by leaf but incomplete: nothing was sent.
    /// The core tightens whole tiers, so a partial tier has no meaning to
    /// preview or submit — the refusal names what is missing.
    PartialTier {
        /// Descriptor tier name.
        tier: String,
        /// Drafted-leaf paths still missing.
        missing: Vec<String>,
    },
    /// No fresh successful preview covers the current drafts: nothing was
    /// sent. The drafts changed since the preview, the preview refused, or
    /// no preview ran — confirmation without the core's current number is
    /// not confirmation.
    PreviewStale,
    /// The gateway answered (including the core's tighten refusal).
    Gateway(GatewayError),
}

impl Display for GrantFormError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(formatter, "no tier drafted: nothing submitted"),
            Self::FieldParse { field, reason } => {
                write!(formatter, "field {field} refuses: {reason}")
            }
            Self::PartialTier { tier, missing } => write!(
                formatter,
                "tier {tier} needs every leaf drafted, missing: {}",
                missing.join(", ")
            ),
            Self::PreviewStale => write!(
                formatter,
                "no fresh preview covers these drafts: preview before submit"
            ),
            Self::Gateway(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for GrantFormError {}

/// One descriptor tier, in schema order: its hint plus its leaves.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One descriptor tier, in schema order: its hint plus its leaves.
struct GrantTier {
    name: String,
    description: String,
}

/// Frames between grant-tier reads unless a content change forces one sooner.
///
/// A display read, so the interval trades freshness for bytes: half a second
/// at sixty frames, and the painter prints the age it actually is. The form
/// exposes [`GrantFormState::set_tier_interval`] for a host that wants a
/// different cadence (a slow gateway, a headless harness).
pub const DEFAULT_TIER_READ_INTERVAL_FRAMES: u64 = 30;

/// The form: descriptor-derived tiers and leaves, per-leaf draft text, the
/// live tier texts for display, the preview covering the drafts, and the
/// last outcome for the painter to echo.
#[derive(Debug, Clone)]
/// The form: descriptor-derived tiers and leaves, per-leaf draft text, the
/// live tier texts for display, the preview covering the drafts, and the
/// last outcome for the painter to echo.
pub struct GrantFormState {
    tiers: Vec<GrantTier>,
    fields: Vec<GrantFormField>,
    drafts: HashMap<String, String>,
    tier_texts: BTreeMap<String, String>,
    previewed: BTreeMap<String, String>,
    preview: Option<Result<WorstCase, String>>,
    // NOT-A-LADDER (M5): form field holding the last refusal text,
    // not an ordered candidate walk.
    last_error: Option<String>,
    last_response: Option<ArmResponse>,
    /// Outcome of the most recent attest click, for the painter to echo.
    attest_note: Option<String>,
    /// Refresh ticks: one per [`refresh_preview`](Self::refresh_preview) call,
    /// which the painter makes once per painted frame while the form is open.
    frame: u64,
    /// Frames between tier reads unless a change forces one sooner.
    tier_interval: u64,
    /// Frame of the last tier read *attempt*, successful or not: the interval
    /// is measured from here, so a failing gateway is not hammered per frame.
    tier_attempt_frame: Option<u64>,
    /// Frame of the last **successful** tier read: the age of the text on
    /// display. `None` when nothing good has been read yet.
    tier_read_frame: Option<u64>,
    /// Fingerprint of the newest grant content this form knows about: the
    /// last read's tier document, or a document a submit armed.
    grant_fingerprint: Option<u64>,
    /// The grant's content changed and no read has covered it yet: the next
    /// frame reads immediately instead of waiting for the interval.
    tier_change_pending: bool,
    /// Why the most recent tier read failed, if it did. Cleared by the next
    /// successful read.
    tier_failure: Option<String>,
}

impl Default for GrantFormState {
    fn default() -> Self {
        Self {
            tiers: Vec::new(),
            fields: Vec::new(),
            drafts: HashMap::new(),
            tier_texts: BTreeMap::new(),
            previewed: BTreeMap::new(),
            preview: None,
            last_error: None,
            last_response: None,
            attest_note: None,
            frame: 0,
            tier_interval: DEFAULT_TIER_READ_INTERVAL_FRAMES,
            tier_attempt_frame: None,
            tier_read_frame: None,
            grant_fingerprint: None,
            tier_change_pending: false,
            tier_failure: None,
        }
    }
}

impl GrantFormState {
    /// Build the tier and leaf set from a nested patch schema: every
    /// top-level property becomes one tier, and a tier carrying its own
    /// `properties` fans out to one editor per leaf, in schema order. A
    /// tier without nested properties (the window end) edits as one value.
    ///
    /// The properties are lifted from the schema document itself, so this
    /// form and every other schema-rendered control read a schema the one
    /// way — a second walk here would be a second spelling of "what does
    /// this descriptor declare".
    #[must_use]
    pub fn from_schema(schema: &serde_json::Value) -> Self {
        let mut tiers = Vec::new();
        let mut fields = Vec::new();
        let properties = schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (tier, hint) in &properties {
            let description = hint
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            tiers.push(GrantTier {
                name: tier.clone(),
                description: description.clone(),
            });
            match hint
                .get("properties")
                .and_then(serde_json::Value::as_object)
            {
                Some(leaves) => {
                    for (leaf, leaf_hint) in leaves {
                        fields.push(GrantFormField {
                            tier: tier.clone(),
                            leaf: Some(leaf.clone()),
                            kind: leaf_hint
                                .get("type")
                                .and_then(serde_json::Value::as_str)
                                .map_or(FieldKind::Text, |declared| {
                                    FieldKind::of_schema(Some(declared))
                                }),
                            format: leaf_hint
                                .get("format")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned),
                            description: leaf_hint
                                .get("description")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                        });
                    }
                }
                None => fields.push(GrantFormField {
                    tier: tier.clone(),
                    leaf: None,
                    kind: hint
                        .get("type")
                        .and_then(serde_json::Value::as_str)
                        .map_or(FieldKind::Text, |declared| {
                            FieldKind::of_schema(Some(declared))
                        }),
                    format: hint
                        .get("format")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    description,
                }),
            }
        }
        Self {
            tiers,
            fields,
            drafts: HashMap::new(),
            tier_texts: BTreeMap::new(),
            previewed: BTreeMap::new(),
            preview: None,
            last_error: None,
            last_response: None,
            ..Self::default()
        }
    }

    /// Descriptor tier names, in schema order.
    #[must_use]
    pub fn tier_names(&self) -> Vec<String> {
        self.tiers.iter().map(|tier| tier.name.clone()).collect()
    }

    /// Descriptor tier names, in schema order. Kept for callers written
    /// against the pre-leaf form, when one editor per tier meant tier names
    /// were the field paths; new code prefers [`tier_names`](Self::tier_names)
    /// for tiers and [`leaf_paths`](Self::leaf_paths) for leaves.
    #[must_use]
    pub fn field_paths(&self) -> Vec<String> {
        self.tier_names()
    }

    /// Descriptor leaf paths (`tier` or `tier.leaf`), in schema order.
    #[must_use]
    pub fn leaf_paths(&self) -> Vec<String> {
        self.fields.iter().map(GrantFormField::path).collect()
    }

    /// The descriptor-derived leaf fields, for the painter.
    #[must_use]
    pub fn fields(&self) -> &[GrantFormField] {
        &self.fields
    }

    /// Current draft text for one leaf path (empty when untouched).
    #[must_use]
    pub fn draft_of(&self, path: &str) -> &str {
        self.drafts.get(path).map_or("", String::as_str)
    }

    /// Replace the draft text for one leaf path.
    pub fn set_draft(&mut self, path: &str, text: String) {
        self.drafts.insert(path.to_owned(), text);
    }

    /// Live tier text for display beside a tier's editors (empty when the
    /// gateway holds nothing). Display only: placeholders never submit.
    #[must_use]
    pub fn tier_text_of(&self, tier: &str) -> &str {
        self.tier_texts.get(tier).map_or("", String::as_str)
    }

    /// Frame of the last **successful** tier read (see the type's `frame`
    /// note): the age of the text on display, `None` before the first good one.
    #[must_use]
    pub fn tier_read_frame(&self) -> Option<u64> {
        self.tier_read_frame
    }

    /// Frames between tier reads; at least one, so the form never becomes a
    /// per-frame read by accident.
    #[must_use]
    pub fn tier_interval(&self) -> u64 {
        self.tier_interval
    }

    /// Set the tier-read interval in frames. Zero is clamped to one — "read
    /// every frame" is expressible, "read negative" is not a thing.
    pub fn set_tier_interval(&mut self, frames: u64) {
        self.tier_interval = frames.max(1);
    }

    /// Declare what the grant now holds, so a change forces the next frame's
    /// read instead of waiting out the interval: a successful arm passes the
    /// document it sent, and any host that learns of an arm elsewhere passes
    /// the grant content it saw. Content equal to what is already on display
    /// (by [`fingerprint_json`]) forces nothing — a no-op never costs a read.
    pub fn note_grant_content(&mut self, document: &impl serde::Serialize) {
        let fingerprint = fingerprint_json(document);
        if self.grant_fingerprint != Some(fingerprint) {
            self.tier_change_pending = true;
        }
        self.grant_fingerprint = Some(fingerprint);
    }

    /// The staleness line painted beside the tiers: which frame the text on
    /// display was read at, and how many frames ago that was. One home for the
    /// sentence, so the painter and the pins cannot disagree about it.
    #[must_use]
    pub fn tier_age_line(&self) -> String {
        match self.tier_read_frame {
            Some(read) => format!(
                "tiers as of frame {read} ({} frames old)",
                self.frame.saturating_sub(read)
            ),
            None => "tiers not read yet".to_owned(),
        }
    }

    /// The "unavailable" chip, when the most recent tier read failed: the text
    /// on screen is the previous read's (or nothing at all), and saying so is
    /// the whole point — a stale tier is never painted as live bytes.
    #[must_use]
    pub fn tier_unavailable_chip(&self) -> Option<String> {
        let reason = self.tier_failure.as_deref()?;
        Some(match self.tier_read_frame {
            Some(_) => format!("unavailable — showing last good ({reason})"),
            None => format!("unavailable — nothing read yet ({reason})"),
        })
    }

    /// Whether this frame must read the tiers: a pending content change, or
    /// the interval having elapsed since the last attempt.
    fn tier_read_due(&self) -> bool {
        self.tier_change_pending
            || self
                .tier_attempt_frame
                .is_none_or(|attempt| self.frame.saturating_sub(attempt) >= self.tier_interval)
    }

    /// The preview covering the drafts, when one ran: the core's computed
    /// worst case, or the reason it refused.
    #[must_use]
    pub fn preview_outcome(&self) -> Option<Result<WorstCase, String>> {
        self.preview.clone()
    }

    /// Whether a successful preview covers exactly the current drafts. The
    /// painter gates the submit button on this; [`submit`](Self::submit)
    /// gates the write on it, so bypassing the painter changes nothing.
    #[must_use]
    pub fn is_preview_fresh(&self) -> bool {
        matches!(&self.preview, Some(Ok(_)))
            && self
                .assemble_tiers()
                .is_ok_and(|tiers| tiers == self.previewed)
    }

    /// Last submit refusal, if any, for the painter to echo.
    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Record a refusal for the painter to echo, returning it.
    fn record(&mut self, error: GrantFormError) -> GrantFormError {
        self.last_error = Some(error.to_string());
        error
    }

    /// Last accepted arm response, if any, for the painter to confirm.
    #[must_use]
    pub fn last_response(&self) -> Option<&ArmResponse> {
        self.last_response.as_ref()
    }

    /// Outcome of the most recent attest click, if any, for the painter.
    #[must_use]
    pub fn attest_note(&self) -> Option<&str> {
        self.attest_note.as_deref()
    }

    /// Re-attest the armed grant through the gateway, recording the outcome
    /// for the painter. Refuses without an armed grant id rather than
    /// attesting a name this form never armed.
    pub fn attest(&mut self, gateway: &dyn TerminalGateway) -> Result<(), String> {
        let Some(grant_id) = self
            .last_response
            .as_ref()
            .map(|response| response.grant_id.clone())
        else {
            let note = "nothing armed to attest".to_owned();
            self.attest_note = Some(note.clone());
            return Err(note);
        };
        match GrantAction::attest(&grant_id).apply(gateway) {
            Ok(()) => {
                let note = format!("attested {grant_id}: entries re-arm for a cadence");
                self.attest_note = Some(note);
                Ok(())
            }
            Err(error) => {
                let note = format!("attest refused: {error}");
                self.attest_note = Some(note.clone());
                Err(note)
            }
        }
    }

    /// Convert one drafted leaf to its JSON leg. Integers and numbers parse
    /// as scalars (a second tier parser this is not — scalar text has no
    /// other reading); decimal text crosses through the validated money
    /// path, so money rides decimal text and stays exact; date-time text
    /// rides verbatim because the core's `FromStr` is the single date
    /// parser and a second one here would be the duplication the envelope
    /// exists to avoid. A color field is text on the wire exactly like any
    /// other string, so it rides verbatim beside it.
    fn leaf_value(
        field: &GrantFormField,
        draft: &str,
    ) -> Result<serde_json::Value, GrantFormError> {
        let refuse = |reason: String| GrantFormError::FieldParse {
            field: field.path(),
            reason,
        };
        match field.kind {
            FieldKind::Integer => draft
                .parse::<i64>()
                .map(serde_json::Value::from)
                .map_err(|_| refuse(format!("{draft:?} is not a whole number"))),
            FieldKind::Number => draft
                .parse::<f64>()
                .map_err(|_| refuse(format!("{draft:?} is not a number")))
                .and_then(|value| {
                    serde_json::Number::from_f64(value)
                        .map(serde_json::Value::Number)
                        .ok_or_else(|| refuse("a real number must be finite".to_owned()))
                }),
            FieldKind::Boolean => match draft {
                "true" => Ok(serde_json::Value::Bool(true)),
                "false" => Ok(serde_json::Value::Bool(false)),
                _ => Err(refuse(format!("{draft:?} is not true or false"))),
            },
            FieldKind::Text if field.format.as_deref() == Some("decimal") => {
                DisplayOnly::new(draft.to_owned())
                    .try_into_money()
                    .map(|money| serde_json::Value::String(money.to_string()))
                    .map_err(|error| refuse(error.to_string()))
            }
            FieldKind::Text | FieldKind::Color | FieldKind::Select => {
                Ok(serde_json::Value::String(draft.to_owned()))
            }
        }
    }

    /// Assemble drafted leaves into per-tier boundary strings, in tier
    /// order. Empty drafts are skipped (their tiers stay out); a tier with
    /// some but not all leaves drafted refuses naming the missing ones.
    /// Construction only — no tier text is parsed here.
    fn assemble_tiers(&self) -> Result<BTreeMap<String, String>, GrantFormError> {
        let mut tiers = BTreeMap::new();
        for tier in &self.tiers {
            let leaves: Vec<&GrantFormField> = self
                .fields
                .iter()
                .filter(|field| field.tier == tier.name)
                .collect();
            let mut document = serde_json::Map::new();
            let mut missing = Vec::new();
            let mut drafted = 0;
            for field in &leaves {
                let draft = self.draft_of(&field.path());
                if draft.trim().is_empty() {
                    if field.leaf.is_some() {
                        missing.push(field.path());
                    }
                    continue;
                }
                drafted += 1;
                match &field.leaf {
                    Some(leaf) => {
                        document.insert(leaf.clone(), Self::leaf_value(field, draft.trim())?);
                    }
                    None => {
                        tiers.insert(tier.name.clone(), draft.trim().to_owned());
                    }
                }
            }
            if drafted == 0 {
                continue;
            }
            if !missing.is_empty() {
                return Err(GrantFormError::PartialTier {
                    tier: tier.name.clone(),
                    missing,
                });
            }
            if !document.is_empty() {
                // NOT-A-LADDER: construction from typed legs cannot fail; the fallback
                // keeps a display path panic-free if the shape ever did.
                tiers.insert(
                    tier.name.clone(),
                    serde_json::to_string(&document).unwrap_or_default(),
                );
            }
        }
        Ok(tiers)
    }

    /// The patch envelope for assembled tiers: tier name to boundary-string
    /// document. Whole-grant identity never rides here — the form tightens,
    /// it never mints.
    fn envelope(tiers: &BTreeMap<String, String>) -> serde_json::Value {
        serde_json::json!({ "patch": tiers })
    }

    /// Refresh the preview to cover the current drafts: pull the live tier
    /// texts for display, assemble the tiers, and preview a changed patch
    /// through the gateway. An unchanged patch keeps its preview (a repeat
    /// read re-renders, never re-asks); an empty patch clears it; a patch
    /// the leaves refuse records the local reason, so the painter shows why
    /// there is nothing to confirm.
    ///
    /// Called once per painted frame: the preview re-ask is change-gated, and
    /// so is the tier-text read this frame — at most once per
    /// [`tier_interval`](Self::tier_interval) frames, or immediately when
    /// [`note_grant_content`](Self::note_grant_content) reports a change. A
    /// failed read keeps the previous texts, clears any pending change (a
    /// failing gateway must not be re-asked every frame) and records the
    /// reason for [`tier_unavailable_chip`](Self::tier_unavailable_chip): a
    /// transport failure is display-stale, never an empty grant, and an empty
    /// grant still reads as empty only when the gateway says so.
    pub fn refresh_preview(&mut self, gateway: &dyn TerminalGateway) {
        self.frame = self.frame.saturating_add(1);
        if self.tier_read_due() {
            self.tier_attempt_frame = Some(self.frame);
            match gateway.grant_tiers() {
                Ok(tiers) => {
                    self.tier_texts = tiers;
                    self.grant_fingerprint = Some(fingerprint_json(&self.tier_texts));
                    self.tier_read_frame = Some(self.frame);
                    self.tier_failure = None;
                }
                Err(error) => self.tier_failure = Some(error.to_string()),
            }
            self.tier_change_pending = false;
        }
        let tiers = match self.assemble_tiers() {
            Ok(tiers) => tiers,
            Err(local) => {
                self.previewed = BTreeMap::new();
                self.preview = Some(Err(local.to_string()));
                return;
            }
        };
        if tiers.is_empty() {
            self.previewed = BTreeMap::new();
            self.preview = None;
            return;
        }
        if tiers == self.previewed && self.preview.is_some() {
            return;
        }
        let outcome = gateway
            .preview_arm(Self::envelope(&tiers))
            .map_err(|error| error.to_string());
        self.previewed = tiers;
        self.preview = Some(outcome);
    }

    /// Parse the drafted tiers, require a fresh successful preview covering
    /// exactly them, and arm through the gateway. Empty drafts are skipped
    /// (their tiers stay `None`); anything else must satisfy its leaf, a
    /// partial tier refuses, and an unfresh preview refuses — then the
    /// envelope wraps the tier strings as a patch, never a grant, and the
    /// outcome is recorded for the painter.
    pub fn submit(&mut self, gateway: &dyn TerminalGateway) -> Result<ArmResponse, GrantFormError> {
        self.last_error = None;
        self.last_response = None;
        let tiers = match self.assemble_tiers() {
            Ok(tiers) if tiers.is_empty() => return Err(self.record(GrantFormError::Empty)),
            Ok(tiers) => tiers,
            Err(error) => return Err(self.record(error)),
        };
        if !matches!(&self.preview, Some(Ok(_))) || self.previewed != tiers {
            return Err(self.record(GrantFormError::PreviewStale));
        }
        let envelope = Self::envelope(&tiers);
        match gateway.arm(envelope.clone()) {
            Ok(response) => {
                self.last_response = Some(response.clone());
                // The armed document is the grant's newest content, and the
                // tiers on screen describe the grant it replaced: the next
                // frame reads them again instead of waiting out the interval.
                self.note_grant_content(&envelope);
                Ok(response)
            }
            Err(error) => Err(self.record(GrantFormError::Gateway(error))),
        }
    }
}

/// One operator grant action: re-attest the live grant.
///
/// The action carries only the id; applying it invokes exactly one gateway
/// verb, so a rendered click can never fan out into a second write.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One operator grant action: re-attest the live grant.
/// 
/// The action carries only the id; applying it invokes exactly one gateway
/// verb, so a rendered click can never fan out into a second write.
pub enum GrantAction {
    /// Re-attest the live grant for another cadence.
    Attest {
        /// Grant to attest.
        grant_id: String,
    },
}

impl GrantAction {
    /// An attest click for `grant_id`.
    #[must_use]
    pub fn attest(grant_id: &str) -> Self {
        Self::Attest {
            grant_id: grant_id.to_owned(),
        }
    }

    /// Invoke the one gateway verb this action names.
    pub fn apply(&self, gateway: &dyn TerminalGateway) -> Result<(), GatewayError> {
        match self {
            Self::Attest { grant_id } => gateway.attest_grant(grant_id),
        }
    }
}
/// Thin painter over the form state: one typed editor per descriptor leaf
/// grouped under its tier, the live tier text as its hover, the previewed
/// worst case, and a submit button that stays unactionable until a
/// successful preview covers the drafts. No field knowledge lives here —
/// the rows iterate the state the schema built, and drafts write back only
/// on change (an unconditional write would dirty every tier per frame and
/// churn the preview it gates).
pub fn paint_grant_form(
    ui: &mut egui::Ui,
    form: &mut GrantFormState,
    gateway: &dyn TerminalGateway,
) {
    ui.heading("Arm grant (tighten only)");
    form.refresh_preview(gateway);
    // The read is gated, so the text has an age: print it, and never let a
    // failed refresh read as live bytes.
    ui.label(form.tier_age_line());
    if let Some(chip) = form.tier_unavailable_chip() {
        ui.colored_label(egui::Color32::YELLOW, chip);
    }
    let mut current_tier = String::new();
    for field in form.fields().to_vec() {
        if field.tier != current_tier {
            current_tier.clone_from(&field.tier);
            let tier_text = form.tier_text_of(&current_tier).to_owned();
            ui.label(format!("tier: {current_tier}"))
                .on_hover_text(tier_text);
        }
        let path = field.path();
        if field.kind == FieldKind::Boolean {
            let mut value = form.draft_of(&path) == "true";
            if ui
                .checkbox(&mut value, path.clone())
                .on_hover_text(field.description.clone())
                .changed()
            {
                form.set_draft(&path, value.to_string());
            }
        } else {
            let mut draft = form.draft_of(&path).to_owned();
            let hint = match field.format.as_deref() {
                Some("decimal") => format!("decimal major units ({})", field.description),
                Some("date-time") => format!("RFC 3339 ({})", field.description),
                _ => field.description.clone(),
            };
            ui.horizontal(|row| {
                row.label(path.clone());
                row.add(egui::TextEdit::singleline(&mut draft).hint_text(hint));
            });
            if draft != form.draft_of(&path) {
                form.set_draft(&path, draft);
            }
        }
    }
    match form.preview_outcome() {
        None => {
            ui.label("draft a tier to preview its worst case");
        }
        Some(Ok(worst)) => {
            ui.label(format!(
                "preview worst case: {} trades / {} contracts / {} loss / {} notional",
                worst.max_trades, worst.max_contracts, worst.max_loss, worst.max_notional,
            ));
        }
        Some(Err(reason)) => {
            ui.colored_label(egui::Color32::RED, format!("preview refuses: {reason}"));
        }
    }
    let fresh = form.is_preview_fresh();
    if ui
        .add_enabled(fresh, egui::Button::new("Submit tightening"))
        .clicked()
    {
        let _ = form.submit(gateway);
    }
    if let Some(error) = form.last_error() {
        ui.colored_label(egui::Color32::RED, error.to_owned());
    }
    if let Some(response) = form.last_response() {
        ui.label(format!(
            "armed {}: {} trades / {} contracts / {} loss / {} notional",
            response.grant_id,
            response.worst_case.max_trades,
            response.worst_case.max_contracts,
            response.worst_case.max_loss,
            response.worst_case.max_notional
        ));
    }
    if form.last_response().is_some() && ui.button("attest grant").clicked() {
        let _ = form.attest(gateway);
    }
    if let Some(note) = form.attest_note() {
        ui.label(note.to_owned());
    }
}

#[cfg(test)]
#[path = "../tests/grant_form.rs"]
mod grant_form_tests;