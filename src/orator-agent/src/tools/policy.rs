//! Policy engine: `decide(call, ctx) -> Allow, Deny, or Ask`.
//!
//! T2 always asks — no persistent grants in v1, and the toggle that
//! would disable asking rejects the write. Sessions without an effectful
//! capability (Listen-in, Meeting) deny everything, and ambient-origin
//! calls deny even before tiers are consulted.

use fluent_wvr::{
    impl_component, Describable, FieldAccess, FieldError, WorkContext, WorkError, WorkOutput,
    WorkUnit,
};
use internment::ArcIntern;
use orator::SessionKind;

use crate::tools::tool::{ToolCall, ToolTier};

/// Where the call idea came from. Ambient speech is untrusted data: it
/// may be quoted, never executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Where the call idea came from. Ambient speech is untrusted data: it
/// may be quoted, never executed.
pub enum CallOrigin {
    /// The user asked (typed, hotkey, verified voice turn).
    Direct,
    /// Overheard speech, a notification body, fetched text.
    Ambient,
}

/// What the caller holds when asking.
#[derive(Debug, Clone, PartialEq, Eq)]
/// What the caller holds when asking.
pub struct PolicyCtx {
    /// Serving session kind.
    pub session: SessionKind,
    /// Whether the speaker verified (enrolled voice match).
    pub verified: bool,
    /// Whether the turn ingested untrusted content.
    pub tainted: bool,
    /// Call provenance.
    pub origin: CallOrigin,
}

/// Policy verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
/// Policy verdict.
pub enum Verdict {
    /// Run it.
    Allow,
    /// Never run it.
    Deny(String),
    /// Run it only after a human says yes.
    Ask(String),
}

/// The policy engine. A `Component` so rules stay hot-editable through
/// the same schema as every other tunable.
#[derive(Debug, Clone)]
/// The policy engine. A `Component` so rules stay hot-editable through
/// the same schema as every other tunable.
pub struct Policy {
    t1_require_verified: bool,
    disabled_tools: Vec<String>,
    t2_always_ask: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            t1_require_verified: true,
            disabled_tools: Vec::new(),
            t2_always_ask: true,
        }
    }
}

impl Policy {
    /// Policy with defaults from the tier table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Decide one call. Order matters: origin and session deny before
    /// tiers allow, and T2 asks no matter what.
    #[must_use]
    pub fn decide(&self, call: &ToolCall, tier: ToolTier, ctx: &PolicyCtx) -> Verdict {
        if self.disabled_tools.iter().any(|name| name == &call.name) {
            return Verdict::Deny(format!("tool {} is disabled", call.name));
        }
        if ctx.origin == CallOrigin::Ambient {
            return Verdict::Deny("ambient-origin calls never execute".to_string());
        }
        match ctx.session {
            orator::SessionKind::ListenIn | orator::SessionKind::Meeting => {
                return Verdict::Deny("session holds no effectful capability".to_string());
            }
            orator::SessionKind::Dictation | orator::SessionKind::Assistant => {}
        }
        match tier {
            ToolTier::T0 => Verdict::Allow,
            ToolTier::T1 => {
                if ctx.verified || !self.t1_require_verified {
                    Verdict::Allow
                } else {
                    Verdict::Ask(format!("{} touches the desktop", call.name))
                }
            }
            ToolTier::T2 => {
                if ctx.tainted {
                    Verdict::Ask(format!("tainted turn: {} needs approval", call.name))
                } else {
                    Verdict::Ask(format!("{} is effectful", call.name))
                }
            }
        }
    }
}

impl WorkUnit for Policy {
    fn name(&self) -> &str {
        "tools.policy"
    }
    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }
    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        Ok(WorkOutput::ok(format!(
            "t2_always_ask={}",
            self.t2_always_ask
        )))
    }
}

impl FieldAccess for Policy {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        // M15-S10 keep-decision: `disabled_tools` is a comma-split list
        // and `t2_always_ask` is locked on with a custom constraint
        // message — neither shape exists in the derive (its lists are
        // dotted/indexed, its bools plain). Unifying would change the
        // refusal strings or the list grammar. Schema pinned by
        // `policy_field_surface_pins_schema_and_table`.
        match name {
            "t1_require_verified" => {
                self.t1_require_verified = parse_bool(value)?;
                Ok(())
            }
            "disabled_tools" => {
                self.disabled_tools = value
                    .split(',')
                    .map(|entry| entry.trim().to_string())
                    .filter(|entry| !entry.is_empty())
                    .collect();
                Ok(())
            }
            "t2_always_ask" => {
                if !parse_bool(value)? {
                    return Err(FieldError::Constraint(
                        "t2_always_ask cannot be disabled: no persistent grants in v1".to_string(),
                    ));
                }
                self.t2_always_ask = true;
                Ok(())
            }
            _ => Err(FieldError::NotFound(name.into())),
        }
    }
    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        match name {
            "t1_require_verified" => Ok(self.t1_require_verified.to_string()),
            "disabled_tools" => Ok(self.disabled_tools.join(",")),
            "t2_always_ask" => Ok(self.t2_always_ask.to_string()),
            _ => Err(FieldError::NotFound(name.into())),
        }
    }
    fn field_names(&self) -> &'static [&'static str] {
        &["t1_require_verified", "disabled_tools", "t2_always_ask"]
    }
}

impl Describable for Policy {
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "t1_require_verified": { "type": "boolean", "description": "T1 needs a verified speaker" },
                "disabled_tools": { "type": "string", "description": "Comma-separated disabled tool names" },
                "t2_always_ask": { "type": "boolean", "description": "T2 always asks (locked on)" }
            },
            "required": ["t1_require_verified", "disabled_tools", "t2_always_ask"]
        })
    }
}

impl_component!(Policy);

fn parse_bool(value: &str) -> Result<bool, FieldError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(FieldError::Parse(format!(
            "expected true/false, got {value:?}"
        ))),
    }
}

#[cfg(test)]
#[path = "../../tests/tools_policy.rs"]
mod tests;