//! Staged dialog lifecycle: drafts that commit through live contracts.
//!
//! A [`DialogState`] owns one dialog's draft buffers, seeded from the live
//! config and style on [`open_staged`](DialogState::open_staged) and
//! committed through those same contracts on apply. Malformed drafts refuse
//! per field without closing the dialog; cancel discards without ever
//! touching live state. The seeding/commit spelling is the slot settings
//! pane's ([`seed_staged`](crate::fields::seed_staged),
//! [`commit_staged`](crate::fields::commit_staged)), shared, not copied.
//! [`show`](DialogState::show) frames caller-supplied repaintable content
//! with the shared modal metrics; it never commits — the caller applies
//! through the live targets it owns.

use std::collections::HashMap;

use fluent_wvr::FieldAccess;

use crate::config::ThemeConfig;
use crate::error::EguiError;
use crate::fields::{commit_staged, seed_staged};
use crate::paint::{resolve_color, ACTION_PILL_PX, MODAL_CORNER_PX};

/// What one dialog frame raised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DialogCommit {
    /// No action button was pressed.
    #[default]
    None,
    /// Apply the staged edits but keep the dialog open.
    Apply,
    /// Discard the staged edits and close the dialog.
    Cancel,
    /// Apply the staged edits and close the dialog.
    Ok,
}

/// One dialog frame: the action button pressed, if any. Committing is the
/// caller's job — it owns the live targets — so the outcome only names the
/// button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DialogOutcome {
    /// Action button pressed this frame, if any.
    pub commit: DialogCommit,
}

/// A dialog's lifecycle: staged drafts over two live authorities plus the
/// frame metrics. Drafts route by name — a name the style authority
/// declares stages into the style buffer, everything else into the config
/// buffer — so one field never stages into both.
pub struct DialogState {
    title: String,
    open: bool,
    width_px: f32,
    scroll_max_px: f32,
    corner_px: f32,
    action_pill_px: f32,
    staged: HashMap<String, String>,
    style_staged: HashMap<String, String>,
    style_names: Vec<String>,
}

impl DialogState {
    /// A closed dialog with standard metrics: the shared modal corner and
    /// action pill, a 320px body over a scrollable region.
    #[must_use]
    pub fn new(title: &str) -> Self {
        Self {
            title: title.to_owned(),
            open: false,
            width_px: 320.0,
            scroll_max_px: 280.0,
            corner_px: MODAL_CORNER_PX,
            action_pill_px: ACTION_PILL_PX,
            staged: HashMap::new(),
            style_staged: HashMap::new(),
            style_names: Vec::new(),
        }
    }

    /// Body width in pixels.
    #[must_use]
    pub fn with_width(mut self, width_px: f32) -> Self {
        self.width_px = width_px;
        self
    }

    /// Tallest the content region grows before it scrolls.
    #[must_use]
    pub fn with_scroll_max(mut self, scroll_max_px: f32) -> Self {
        self.scroll_max_px = scroll_max_px;
        self
    }

    /// Display title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Whether the dialog is open.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Body width in pixels.
    #[must_use]
    pub fn width_px(&self) -> f32 {
        self.width_px
    }

    /// Seed both draft buffers from the live authorities and open the
    /// dialog. Reopening reseeds, so abandoned drafts never leak across
    /// opens.
    pub fn open_staged<C: FieldAccess + ?Sized, S: FieldAccess + ?Sized>(
        &mut self,
        config: &C,
        style: &S,
    ) {
        self.staged = seed_staged(config);
        self.style_staged = seed_staged(style);
        self.style_names = style
            .field_names()
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        self.open = true;
    }

    /// Write one draft value, routed like the seeded buffers. Drafts accept
    /// anything — the live contract judges on apply, never the staging.
    pub fn set_staged(&mut self, name: &str, value: &str) {
        if self.style_names.iter().any(|known| known == name) {
            self.style_staged.insert(name.to_owned(), value.to_owned());
        } else {
            self.staged.insert(name.to_owned(), value.to_owned());
        }
    }

    /// Read one draft value (`None` for undrafted names).
    #[must_use]
    pub fn staged(&self, name: &str) -> Option<&str> {
        if self.style_names.iter().any(|known| known == name) {
            self.style_staged.get(name).map(String::as_str)
        } else {
            self.staged.get(name).map(String::as_str)
        }
    }

    /// Commit both drafts through the live contracts: valid fields apply,
    /// refused ones report per field, and one refusal never stops the pass.
    /// Drafts reseed from live afterwards, so further edits start from
    /// committed truth. The dialog stays open; closing is the caller's.
    pub fn apply<C: FieldAccess + ?Sized, S: FieldAccess + ?Sized>(
        &mut self,
        config: &mut C,
        style: &mut S,
    ) -> Vec<String> {
        let mut refusals = commit_staged(config, &self.staged);
        refusals.extend(commit_staged(style, &self.style_staged));
        self.staged = seed_staged(&*config);
        self.style_staged = seed_staged(&*style);
        refusals
    }

    /// Discard the drafts and close.
    pub fn cancel(&mut self) {
        self.staged.clear();
        self.style_staged.clear();
        self.open = false;
    }

    /// Close without discarding: after an apply the drafts already reseed
    /// from live, so closing keeps nothing stale.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Paint the dialog frame around repaintable caller content plus a
    /// pinned Cancel/Apply/OK row. A closed dialog paints nothing. Refuses
    /// degenerate metrics or an unresolvable theme instead of painting a
    /// broken frame.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        theme: &ThemeConfig,
        content: &dyn Fn(&mut egui::Ui),
    ) -> Result<DialogOutcome, EguiError> {
        if !self.open {
            return Ok(DialogOutcome {
                commit: DialogCommit::None,
            });
        }
        if !self.width_px.is_finite() || self.width_px <= 0.0 {
            return Err(EguiError::InvalidParam {
                what: "width_px".to_owned(),
                reason: "dialog width must be positive and finite".to_owned(),
            });
        }
        if !self.scroll_max_px.is_finite() || self.scroll_max_px <= 0.0 {
            return Err(EguiError::InvalidParam {
                what: "scroll_max_px".to_owned(),
                reason: "dialog scroll region must be positive and finite".to_owned(),
            });
        }
        let accent = resolve_color(theme, "fast")?;
        let pill = egui::CornerRadius::from(self.action_pill_px);
        let mut commit = DialogCommit::None;
        let title = self.title.clone();
        let width = self.width_px;
        let scroll_max = self.scroll_max_px;
        let corner = self.corner_px;
        egui::Frame::popup(ui.style())
            .corner_radius(corner)
            .show(ui, |pane| {
                pane.set_width(width);
                pane.colored_label(accent, title);
                egui::ScrollArea::vertical()
                    .max_height(scroll_max)
                    .show(pane, |editor| {
                        content(editor);
                    });
                pane.horizontal(|row| {
                    if row
                        .add(egui::Button::new("Cancel").corner_radius(pill))
                        .clicked()
                    {
                        commit = DialogCommit::Cancel;
                    }
                    if row
                        .add(egui::Button::new("Apply").corner_radius(pill))
                        .clicked()
                    {
                        commit = DialogCommit::Apply;
                    }
                    if row
                        .add(egui::Button::new("OK").corner_radius(pill))
                        .clicked()
                    {
                        commit = DialogCommit::Ok;
                    }
                });
            });
        Ok(DialogOutcome { commit })
    }
}

#[cfg(test)]
#[path = "../tests/dialog.rs"]
mod tests;
