//! Schema-driven grant arming form.
//!
//! The form renders from a grant-patch descriptor (`field_names` plus
//! `describe`, carried as JSON) with no hand-written field list: tier names,
//! hints, and the submitted envelope all flow from the schema the strategy
//! crate derives, so the descriptor stays the single source of truth and a
//! new tier appears in the form without a form edit.
//!
//! The form never mints a grant locally. Drafts are tier-level JSON
//! documents; submit parses each non-empty draft fail-closed (naming the
//! tier), wraps the parsed tiers in a patch envelope, and sends it through
//! [`TerminalGateway::arm`](protopyne_view::TerminalGateway::arm). The core
//! alone validates, tightens, and refuses — a loosening surfaces the core's
//! refusal, and the [`GrantFormError::Gateway`] variant keeps that refusal
//! distinguishable from local form validation.
//!
//! Paint-edge direction holds: this module names the gateway abstraction
//! only (pinned by the ledger tests), never the trading concretion, and
//! money crosses only as rendered text — the decimal boundary stays one-way.

use std::collections::HashMap;
use std::fmt::{Display, Formatter};

use protopyne_view::{ArmResponse, GatewayError, TerminalGateway};

/// One editable tier, lifted from the descriptor's properties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantFormField {
    /// Descriptor property name; also the envelope key for this tier.
    pub path: String,
    /// Schema hint rendered beside the editor.
    pub description: String,
}

/// What a submit can refuse, with local validation kept distinct from the
/// core's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantFormError {
    /// No tier drafted: nothing was sent.
    Empty,
    /// A tier draft is not a JSON document: nothing was sent.
    FieldParse {
        /// Descriptor tier name.
        field: String,
        /// Parser reason.
        reason: String,
    },
    /// The gateway answered (including the core's tighten refusal).
    Gateway(GatewayError),
}

impl Display for GrantFormError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(formatter, "no tier drafted: nothing submitted"),
            Self::FieldParse { field, reason } => {
                write!(formatter, "tier {field} is not a JSON document: {reason}")
            }
            Self::Gateway(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for GrantFormError {}

/// The form: descriptor-derived fields, per-tier draft text, and the last
/// outcome for the painter to echo.
#[derive(Debug, Clone, Default)]
pub struct GrantFormState {
    fields: Vec<GrantFormField>,
    drafts: HashMap<String, String>,
    last_error: Option<String>,
    last_response: Option<ArmResponse>,
}

impl GrantFormState {
    /// Build the field set from a `describe()` schema: every property
    /// becomes one tier editor, in schema order.
    #[must_use]
    pub fn from_schema(schema: &serde_json::Value) -> Self {
        let mut fields = Vec::new();
        if let Some(properties) = schema.get("properties").and_then(serde_json::Value::as_object)
        {
            for (name, hint) in properties {
                let description = hint
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                fields.push(GrantFormField { path: name.clone(), description });
            }
        }
        Self {
            fields,
            drafts: HashMap::new(),
            last_error: None,
            last_response: None,
        }
    }

    /// Descriptor property names, in schema order.
    #[must_use]
    pub fn field_paths(&self) -> Vec<String> {
        self.fields.iter().map(|field| field.path.clone()).collect()
    }

    /// The descriptor-derived fields, for the painter.
    #[must_use]
    pub fn fields(&self) -> &[GrantFormField] {
        &self.fields
    }

    /// Current draft text for one tier (empty when untouched).
    #[must_use]
    pub fn draft_of(&self, path: &str) -> &str {
        self.drafts.get(path).map_or("", String::as_str)
    }

    /// Replace the draft text for one tier.
    pub fn set_draft(&mut self, path: &str, text: String) {
        self.drafts.insert(path.to_owned(), text);
    }

    /// Last submit refusal, if any, for the painter to echo.
    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Last accepted arm response, if any, for the painter to confirm.
    #[must_use]
    pub fn last_response(&self) -> Option<&ArmResponse> {
        self.last_response.as_ref()
    }

    /// Parse the drafted tiers and arm through the gateway. Empty drafts are
    /// skipped (their tiers stay `None`); anything else must parse as a JSON
    /// document or nothing is sent. The envelope wraps the parsed tiers as a
    /// patch — never a grant — and the outcome is recorded for the painter.
    pub fn submit(
        &mut self,
        gateway: &dyn TerminalGateway,
    ) -> Result<ArmResponse, GrantFormError> {
        self.last_error = None;
        self.last_response = None;
        let mut refused = |error: GrantFormError| {
            let text = error.to_string();
            self.last_error = Some(text);
            error
        };
        let mut patch = serde_json::Map::new();
        for field in &self.fields {
            let text = self.drafts.get(&field.path).map_or("", String::as_str).trim();
            if text.is_empty() {
                continue;
            }
            let value: serde_json::Value =
                serde_json::from_str(text).map_err(|e| {
                    refused(GrantFormError::FieldParse {
                        field: field.path.clone(),
                        reason: e.to_string(),
                    })
                })?;
            patch.insert(field.path.clone(), value);
        }
        if patch.is_empty() {
            return Err(refused(GrantFormError::Empty));
        }
        let document = serde_json::json!({ "patch": patch });
        match gateway.arm(document) {
            Ok(response) => {
                self.last_response = Some(response.clone());
                Ok(response)
            }
            Err(error) => Err(refused(GrantFormError::Gateway(error))),
        }
    }
}

/// Thin painter over the form state: one text editor per descriptor field,
/// a submit button, and the recorded outcome. No field knowledge lives
/// here — the rows iterate the state the schema built.
pub fn paint_grant_form(
    ui: &mut egui::Ui,
    form: &mut GrantFormState,
    gateway: &dyn TerminalGateway,
) {
    ui.heading("Arm grant (tighten only)");
    let paths = form.field_paths();
    for path in &paths {
        let description = form
            .fields()
            .iter()
            .find(|field| &field.path == path)
            .map_or("", |field| field.description.as_str());
        let mut draft = form.draft_of(path).to_owned();
        ui.horizontal(|row| {
            row.label(path);
            row.add(egui::TextEdit::singleline(&mut draft).hint_text(description));
        });
        form.set_draft(path, draft);
    }
    if ui.button("Submit tightening").clicked() {
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
}

#[cfg(test)]
#[path = "../tests/grant_form.rs"]
mod grant_form_tests;
