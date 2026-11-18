//! Schema-driven backtest authoring: run control and the results panel.
//!
//! The form renders from the served backtest schema (nested sections, leaf
//! kinds, hints) with no hand-written field list: one editor per leaf,
//! grouped under its section, and a new dial appears without a form edit.
//! Leaf drafts overlay the schema's default document; submit assembles the
//! edited document fail-closed (naming the leaf) and sends one envelope
//! through [`TerminalGateway::backtest_run`](protopyne_view::TerminalGateway::backtest_run).
//! The core alone binds, sizes, and refuses — a refused run keeps the
//! previous model and surfaces the core's reason, and the
//! [`BacktestFormError::Gateway`] variant keeps that refusal distinguishable
//! from local form validation. No trading semantics are validated locally:
//! the document is shape-checked only, and the server names any refused
//! value.
//!
//! Reads happen on click only: running stores the view under a deterministic
//! id, and the cached [`BacktestModel`] re-reads only on refresh. The paint
//! path awaits nothing.
//!
//! Paint-edge direction holds: this module names the gateway abstraction
//! only (pinned by the ledger tests), never the trading concretion, and
//! money crosses only as decimal text — the decimal boundary stays one-way.

use std::collections::HashMap;
use std::fmt::{Display, Formatter};

use fluent_egui::FieldKind;
use protopyne_view::{BacktestRunRef, BacktestRunView, GatewayError, TerminalGateway};

/// Sections of the served backtest schema, in render order.
///
/// Documented fork (M10.1 spike verdict): this form edits a server-served
/// JSON *document* through dotted paths (`set_dotted`), while
/// `fluent_egui::fields` edits `FieldAccess` structs. There is no struct
/// to point the shared editor at — a facade would be a new speculative
/// type, and a typed server contract is a public-surface change. The
/// shared part is already shared: leaf kinds map through
/// `FieldKind::of_schema`. Render order stays hardcoded here because the
/// served schema does not encode section order; deriving it from property
/// order would depend on JSON object ordering. Revisit only if the server
/// publishes a typed form contract.
const SECTIONS: [&str; 6] = ["indicator", "entry", "exit", "risk", "session", "display"];

/// One editable leaf, lifted from the schema's nested sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacktestFormField {
    /// Schema section; also the document key for this leaf's subtree.
    pub section: String,
    /// Full dotted leaf path (`section` dot the nested names).
    pub path: String,
    /// Editor kind from the leaf schema's `type`.
    pub kind: FieldKind,
    /// Format refinement from the leaf schema.
    pub format: Option<String>,
    /// Schema hint rendered beside the editor.
    pub description: String,
}

/// What a submit can refuse, with local validation kept distinct from the
/// core's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BacktestFormError {
    /// Nothing drafted and no run params set: nothing was sent.
    Empty,
    /// A drafted leaf is not its kind, or a run param is not its shape:
    /// nothing was sent.
    FieldParse {
        /// Dotted leaf path, or the run-param name.
        field: String,
        /// Parser reason.
        reason: String,
    },
    /// The gateway answered (including the core's spec refusal).
    Gateway(GatewayError),
}

impl Display for BacktestFormError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(formatter, "nothing drafted: nothing submitted"),
            Self::FieldParse { field, reason } => {
                write!(formatter, "field {field} refuses: {reason}")
            }
            Self::Gateway(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for BacktestFormError {}

/// One run action from the results panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BacktestAction {
    /// Assemble the envelope and run it; the stored view replaces the model.
    Run,
    /// Re-read one stored run id into the model.
    Refresh(String),
    /// Drop one stored run id; the model clears when it names the dropped run.
    Stop(String),
}

impl BacktestAction {
    /// Run one action through the gateway against the form and the cached
    /// model. A refused run keeps the previous model and returns the reason
    /// for the refusal line.
    pub fn apply(
        &self,
        gateway: &dyn TerminalGateway,
        form: &mut BacktestFormState,
        model: &mut Option<BacktestModel>,
    ) -> Result<(), String> {
        match self {
            Self::Run => match form.submit(gateway) {
                Ok(reference) => {
                    *model = Some(BacktestModel::pull(gateway, &reference.run_id));
                    Ok(())
                }
                Err(error) => Err(error.to_string()),
            },
            Self::Refresh(run_id) => {
                *model = Some(BacktestModel::pull(gateway, run_id));
                Ok(())
            }
            Self::Stop(run_id) => gateway
                .backtest_stop(run_id)
                .map_err(|error| error.to_string())
                .map(|()| {
                    if model.as_ref().is_some_and(|held| &held.run_id == run_id) {
                        *model = None;
                    }
                }),
        }
    }
}

/// The form: section-derived leaves, per-leaf draft text, the working spec
/// document seeded from the schema defaults, run params as text, and the
/// last outcome for the painter to echo.
#[derive(Debug, Clone)]
pub struct BacktestFormState {
    fields: Vec<BacktestFormField>,
    document: serde_json::Value,
    drafts: HashMap<String, String>,
    seed: String,
    initial_equity: String,
    start: String,
    end: String,
    last_error: Option<String>,
    last_run: Option<BacktestRunRef>,
}

impl Default for BacktestFormState {
    fn default() -> Self {
        Self {
            fields: Vec::new(),
            document: serde_json::Value::Null,
            drafts: HashMap::new(),
            seed: "7".to_owned(),
            initial_equity: "100000".to_owned(),
            start: String::new(),
            end: String::new(),
            last_error: None,
            last_run: None,
        }
    }
}

fn walk_properties(
    section: &str,
    prefix: &str,
    properties: &serde_json::Map<String, serde_json::Value>,
    fields: &mut Vec<BacktestFormField>,
) {
    for (name, hint) in properties {
        let path = if prefix.is_empty() {
            format!("{section}.{name}")
        } else {
            format!("{prefix}.{name}")
        };
        if let Some(nested) = hint
            .get("properties")
            .and_then(serde_json::Value::as_object)
        {
            walk_properties(section, &path, nested, fields);
            continue;
        }
        // The synthetic count leaf is schema machinery, not a submittable
        // value: the submitted document carries the rows, and their number
        // is the count. Lists stay at their default length through this
        // form; every row still edits.
        if path.ends_with(".count") {
            return;
        }
        fields.push(BacktestFormField {
            section: section.to_owned(),
            path,
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
            description: hint
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        });
    }
}

impl BacktestFormState {
    /// Build the section and leaf set from the served backtest schema: every
    /// named section fans out recursively to one editor per typed leaf, in
    /// schema order. The working document seeds from the schema's default
    /// document, so an undrafted submit runs the defaults unchanged.
    #[must_use]
    pub fn from_schema(schema: &serde_json::Value) -> Self {
        let mut fields = Vec::new();
        for section in SECTIONS {
            if let Some(properties) = schema
                .get(section)
                .and_then(|hint| hint.get("properties"))
                .and_then(serde_json::Value::as_object)
            {
                walk_properties(section, "", properties, &mut fields);
            }
        }
        let document = schema.get("defaults").cloned().unwrap_or_default();
        // The schema describes list capacity; the document carries the rows.
        // Only resolving paths paint: every rendered editor submits.
        fields.retain(|field| field_resolves(&document, &field.path));
        Self {
            fields,
            document,
            ..Self::default()
        }
    }

    /// Schema section names, in render order.
    #[must_use]
    pub fn sections(&self) -> Vec<String> {
        SECTIONS
            .iter()
            .map(|section| (*section).to_owned())
            .collect()
    }

    /// Schema leaf fields, in schema order.
    #[must_use]
    pub fn fields(&self) -> &[BacktestFormField] {
        &self.fields
    }

    /// Draft text for a leaf path, or empty when undrafted.
    #[must_use]
    pub fn draft_of(&self, path: &str) -> &str {
        self.drafts.get(path).map_or("", String::as_str)
    }

    /// Set a leaf draft.
    pub fn set_draft(&mut self, path: &str, text: String) {
        self.drafts.insert(path.to_owned(), text);
    }

    /// Run-param texts, for the painter's editors.
    #[must_use]
    pub fn seed_text(&self) -> &str {
        &self.seed
    }

    /// Run-param texts, for the painter's editors.
    #[must_use]
    pub fn initial_equity_text(&self) -> &str {
        &self.initial_equity
    }

    /// Run-param texts, for the painter's editors.
    #[must_use]
    pub fn start_text(&self) -> &str {
        &self.start
    }

    /// Run-param texts, for the painter's editors.
    #[must_use]
    pub fn end_text(&self) -> &str {
        &self.end
    }

    /// Set a run-param text.
    pub fn set_seed_text(&mut self, text: String) {
        self.seed = text;
    }

    /// Set a run-param text.
    pub fn set_initial_equity_text(&mut self, text: String) {
        self.initial_equity = text;
    }

    /// Set a run-param text.
    pub fn set_start_text(&mut self, text: String) {
        self.start = text;
    }

    /// Set a run-param text.
    pub fn set_end_text(&mut self, text: String) {
        self.end = text;
    }

    /// The last refusal line, for the painter to echo.
    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// The last stored run reference, for the painter to echo.
    #[must_use]
    pub fn last_run(&self) -> Option<&BacktestRunRef> {
        self.last_run.as_ref()
    }

    /// The working spec document with every draft applied, for hosts that
    /// render outside the painter (tests, headless harnesses).
    pub fn assembled_document(&self) -> Result<serde_json::Value, BacktestFormError> {
        let mut document = self.document.clone();
        for field in &self.fields {
            let Some(draft) = self.drafts.get(&field.path) else {
                continue;
            };
            let value = match field.kind {
                FieldKind::Boolean => match draft.as_str() {
                    "true" => serde_json::Value::Bool(true),
                    "false" => serde_json::Value::Bool(false),
                    _ => {
                        return Err(BacktestFormError::FieldParse {
                            field: field.path.clone(),
                            reason: "must be true or false".to_owned(),
                        })
                    }
                },
                FieldKind::Integer => match draft.parse::<i64>() {
                    Ok(number) => serde_json::Value::from(number),
                    Err(_) => {
                        return Err(BacktestFormError::FieldParse {
                            field: field.path.clone(),
                            reason: "must be an integer".to_owned(),
                        })
                    }
                },
                FieldKind::Number => match draft.parse::<f64>() {
                    Ok(number) if number.is_finite() => {
                        serde_json::to_value(number).map_err(|_| BacktestFormError::FieldParse {
                            field: field.path.clone(),
                            reason: "must be a number".to_owned(),
                        })?
                    }
                    _ => {
                        return Err(BacktestFormError::FieldParse {
                            field: field.path.clone(),
                            reason: "must be a finite number".to_owned(),
                        })
                    }
                },
                _ => serde_json::Value::String(draft.clone()),
            };
            set_dotted(&mut document, &field.path, value)?;
        }
        Ok(document)
    }

    fn run_params(
        &self,
    ) -> Result<(u64, String, Option<String>, Option<String>), BacktestFormError> {
        let seed = self
            .seed
            .parse::<u64>()
            .map_err(|_| BacktestFormError::FieldParse {
                field: "seed".to_owned(),
                reason: "must be an integer".to_owned(),
            })?;
        if self.initial_equity.trim().is_empty() || self.initial_equity.parse::<f64>().is_err() {
            return Err(BacktestFormError::FieldParse {
                field: "initial_equity".to_owned(),
                reason: "must be a number".to_owned(),
            });
        }
        for (name, text) in [("start", &self.start), ("end", &self.end)] {
            if !text.trim().is_empty() {
                text.parse::<chrono::DateTime<chrono::Utc>>().map_err(|_| {
                    BacktestFormError::FieldParse {
                        field: name.to_owned(),
                        reason: "must be RFC 3339".to_owned(),
                    }
                })?;
            }
        }
        let start = (!self.start.trim().is_empty()).then(|| self.start.clone());
        let end = (!self.end.trim().is_empty()).then(|| self.end.clone());
        Ok((seed, self.initial_equity.clone(), start, end))
    }

    /// Assemble the envelope and send it through
    /// [`TerminalGateway::backtest_run`](protopyne_view::TerminalGateway::backtest_run).
    /// A refused leaf or run param sends nothing; a refused run keeps the
    /// caller's model and records the core's reason on the refusal line.
    pub fn submit(
        &mut self,
        gateway: &dyn TerminalGateway,
    ) -> Result<BacktestRunRef, BacktestFormError> {
        self.last_error = None;
        if self.drafts.is_empty()
            && self.seed.trim().is_empty()
            && self.initial_equity.trim().is_empty()
        {
            return Err(BacktestFormError::Empty);
        }
        let spec = self.assembled_document()?;
        let (seed, initial_equity, start, end) = self.run_params()?;
        let params = protopyne_view::BacktestRunParams {
            spec,
            seed,
            initial_equity,
            start,
            end,
            grant: None,
        };
        match gateway.backtest_run(params) {
            Ok(reference) => {
                self.last_run = Some(reference.clone());
                Ok(reference)
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
                Err(BacktestFormError::Gateway(error))
            }
        }
    }
}

/// Whether a dotted leaf path resolves in the working document: sections by
/// name, list rows by index. The form paints only resolving paths, so every
/// rendered editor submits.
#[must_use]
pub fn field_resolves(document: &serde_json::Value, path: &str) -> bool {
    let mut parts: Vec<&str> = path.split('.').collect();
    let Some(_leaf) = parts.pop() else {
        return false;
    };
    let Some(top) = parts.first() else {
        return false;
    };
    if !SECTIONS.contains(top) {
        return false;
    }
    let mut cursor = document;
    for part in parts {
        cursor = match (cursor, part.parse::<usize>()) {
            (serde_json::Value::Array(rows), Ok(index)) => match rows.get(index) {
                Some(next) => next,
                None => return false,
            },
            _ => match cursor.get(part) {
                Some(next) => next,
                None => return false,
            },
        };
    }
    true
}

/// Set a dotted path inside a JSON document, creating intermediate objects.
/// A missing intermediate object is built; a non-object on the path refuses
/// naming the segment, so a draft can never silently rewrite a scalar.
fn set_dotted(
    document: &mut serde_json::Value,
    path: &str,
    value: serde_json::Value,
) -> Result<(), BacktestFormError> {
    let mut parts: Vec<&str> = path.split('.').collect();
    let Some(leaf) = parts.pop() else {
        return Err(BacktestFormError::FieldParse {
            field: path.to_owned(),
            reason: "empty path".to_owned(),
        });
    };
    // The first segment names the document section the schema declared.
    let Some(top) = parts.first() else {
        return Err(BacktestFormError::FieldParse {
            field: path.to_owned(),
            reason: "a leaf path names its section".to_owned(),
        });
    };
    if !SECTIONS.contains(top) {
        return Err(BacktestFormError::FieldParse {
            field: path.to_owned(),
            reason: "unknown section".to_owned(),
        });
    }
    let mut cursor = document;
    for part in parts {
        cursor = cursor
            .as_object_mut()
            .and_then(|object| {
                object
                    .entry(part.to_owned())
                    .or_insert(serde_json::Value::Object(Default::default()));
                object.get_mut(part)
            })
            .ok_or_else(|| BacktestFormError::FieldParse {
                field: path.to_owned(),
                reason: format!("{part} is not an object"),
            })?;
    }
    if let Some(object) = cursor.as_object_mut() {
        object.insert(leaf.to_owned(), value);
        Ok(())
    } else {
        Err(BacktestFormError::FieldParse {
            field: path.to_owned(),
            reason: "parent is not an object".to_owned(),
        })
    }
}

/// One cached backtest read: the stored run id plus its projected view, or
/// the reason the read refused. Reads happen on click only; the painter
/// never re-reads.
#[derive(Debug, Clone)]
pub struct BacktestModel {
    run_id: String,
    view: Option<BacktestRunView>,
    error: Option<String>,
}

impl BacktestModel {
    /// Read one stored run through the gateway, once. A refusal keeps no
    /// stale view: the model holds the reason instead.
    #[must_use]
    pub fn pull(gateway: &dyn TerminalGateway, run_id: &str) -> Self {
        match gateway.backtest_state(run_id) {
            Ok(view) => Self {
                run_id: run_id.to_owned(),
                view: Some(view),
                error: None,
            },
            Err(error) => Self {
                run_id: run_id.to_owned(),
                view: None,
                error: Some(error.to_string()),
            },
        }
    }

    /// The stored run id this model reads.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// The projected view, when the read succeeded.
    #[must_use]
    pub fn view(&self) -> Option<&BacktestRunView> {
        self.view.as_ref()
    }

    /// The read refusal, when the read failed.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

/// Paint the backtest panel: the schema-driven form, run control, the
/// refusal line, and the cached results (metrics, trade table, final
/// status). Gateway reads happen on click only — the painter sends nothing
/// by itself.
pub fn paint_backtest_panel(
    ui: &mut egui::Ui,
    form: &mut BacktestFormState,
    model: &mut Option<BacktestModel>,
    gateway: Option<&dyn TerminalGateway>,
) {
    let Some(gateway) = gateway else {
        ui.label("Connect a gateway to run a backtest.");
        return;
    };
    ui.heading("Backtest");
    for section in form.sections() {
        ui.collapsing(section.clone(), |body| {
            for field in form.fields().to_vec() {
                if field.section != section {
                    continue;
                }
                let path = field.path.clone();
                if field.kind == FieldKind::Boolean {
                    let mut value = form.draft_of(&path) == "true";
                    if body
                        .checkbox(&mut value, path.clone())
                        .on_hover_text(field.description.clone())
                        .changed()
                    {
                        form.set_draft(&path, value.to_string());
                    }
                } else {
                    let mut draft = form.draft_of(&path).to_owned();
                    body.horizontal(|row| {
                        row.label(path.clone());
                        if row
                            .add(
                                egui::TextEdit::singleline(&mut draft)
                                    .hint_text(field.description.clone()),
                            )
                            .changed()
                        {
                            form.set_draft(&path, draft);
                        }
                    });
                }
            }
        });
    }
    ui.collapsing("run", |body| {
        for (name, get, set) in [
            (
                "seed",
                BacktestFormState::seed_text as fn(&BacktestFormState) -> &str,
                BacktestFormState::set_seed_text as fn(&mut BacktestFormState, String),
            ),
            (
                "initial_equity",
                BacktestFormState::initial_equity_text,
                BacktestFormState::set_initial_equity_text,
            ),
            (
                "start",
                BacktestFormState::start_text,
                BacktestFormState::set_start_text,
            ),
            (
                "end",
                BacktestFormState::end_text,
                BacktestFormState::set_end_text,
            ),
        ] {
            let mut text = get(form).to_owned();
            body.horizontal(|row| {
                row.label(name);
                if row.add(egui::TextEdit::singleline(&mut text)).changed() {
                    set(form, text);
                }
            });
        }
        if body.button("Run backtest").clicked() {
            match BacktestAction::Run.apply(gateway, form, model) {
                Ok(()) => {}
                Err(reason) => {
                    form.last_error = Some(reason);
                }
            }
        }
        if let Some(error) = form.last_error() {
            body.colored_label(egui::Color32::RED, error.to_owned());
        }
        if let Some(reference) = form.last_run() {
            body.label(format!("stored run: {}", reference.run_id));
        }
    });
    let mut stopped: Option<String> = None;
    let mut refreshed: Option<String> = None;
    if let Some(held) = model {
        ui.collapsing(format!("results: {}", held.run_id()), |body| {
            if let Some(error) = held.error() {
                body.colored_label(egui::Color32::RED, error.to_owned());
            }
            let id = held.run_id().to_owned();
            body.horizontal(|row| {
                if row.button("Refresh").clicked() {
                    refreshed = Some(id.clone());
                }
                if row.button("Stop").clicked() {
                    stopped = Some(id.clone());
                }
            });
            if let Some(view) = held.view() {
                for metric in &view.metrics {
                    body.label(format!(
                        "{}: {}",
                        metric.label,
                        metric.value.as_deref().unwrap_or("-")
                    ));
                }
                egui::ScrollArea::vertical().show(body, |scroll| {
                    egui::Grid::new("backtest_trades").show(scroll, |grid| {
                        grid.label("side");
                        grid.label("qty");
                        grid.label("entry");
                        grid.label("exit");
                        grid.label("pnl");
                        grid.label("reason");
                        grid.end_row();
                        for trade in &view.trades {
                            grid.label(if trade.long { "long" } else { "short" });
                            grid.label(trade.quantity.to_string());
                            grid.label(format!("{} @ {}", trade.entry_at, trade.entry_price));
                            grid.label(format!("{} @ {}", trade.exit_at, trade.exit_price));
                            grid.label(trade.pnl.clone());
                            grid.label(trade.exit_reason.clone());
                            grid.end_row();
                        }
                    });
                });
                if let Some(status) = &view.status {
                    crate::widgets::paint_status_table(body, status);
                }
                for warning in &view.warnings {
                    body.label(format!("note: {warning}"));
                }
            }
        });
    }
    if let Some(id) = refreshed {
        if let Some(held) = model {
            *held = BacktestModel::pull(gateway, &id);
        }
    }
    if let Some(id) = stopped {
        // A stopped run leaves no stale model behind: the next paint reads
        // nothing until the next run or refresh.
        let _ = gateway.backtest_stop(&id);
        *model = None;
    }
}

#[cfg(test)]
#[path = "../tests/backtest.rs"]
mod tests;
