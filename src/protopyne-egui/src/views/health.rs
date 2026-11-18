//! Health/trading-state view-model: what the terminal shows, and which
//! affordances it shows, from one [`HealthView`](protopyne_view::HealthView)
//! read.
//!
//! The core stays the authority: the model echoes the core's
//! `allows_entries`/`allows_exits` verdict and the stable state tag, and
//! only *hides* affordances (display fail-closed) — it never shows one the
//! core refused. State tags are the neutral contract (`"Enabled"`,
//! `"Disabled"`, `"ExitOnly"`); an unrecognized tag reads as halted, so an
//! unknown future state fails closed on the glass.

use protopyne_view::{CommandStatus, HealthView};

/// Read-only health projection: liveness, the state tag, and the two
/// affordances the terminal may render.
#[derive(Debug, Clone, PartialEq)]
pub struct HealthModel {
    /// Liveness marker (`ok`).
    pub status: String,
    /// Serving crate version.
    pub version: String,
    /// Trading state, as the stable string tag the wire carries.
    pub trading_state: String,
    /// Whether new entries currently route, per the core.
    pub allows_entries: bool,
    /// Whether exits currently route, per the core.
    pub allows_exits: bool,
    /// Last operator command observed, if any.
    pub last_command: Option<CommandStatus>,
}

impl HealthModel {
    /// Project one health read; infallible — every field echoes the DTO.
    #[must_use]
    pub fn of(view: &HealthView) -> Self {
        Self {
            status: view.status.clone(),
            version: view.version.clone(),
            trading_state: view.trading_state.clone(),
            allows_entries: view.allows_entries,
            allows_exits: view.allows_exits,
            last_command: view.last_command.clone(),
        }
    }

    /// Whether the state tag reads anything but enabled (display predicate
    /// for the halted chip; the core's own verdicts are untouched).
    #[must_use]
    pub fn halted(&self) -> bool {
        self.trading_state != "Enabled"
    }

    /// Entry affordance: shown only when the core allows entries AND the tag
    /// reads enabled. The terminal can only hide, never show.
    #[must_use]
    pub fn entry_affordance(&self) -> bool {
        self.allows_entries && !self.halted()
    }

    /// Exit affordance: shown when the core allows exits and the tag is not
    /// the fail-closed rest state (exits route in both `Enabled` and
    /// `ExitOnly`; anything else hides them).
    #[must_use]
    pub fn exit_affordance(&self) -> bool {
        self.allows_exits && self.trading_state != "Disabled"
    }
}
