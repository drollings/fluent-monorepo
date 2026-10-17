//! Pane-scoped secondary-click detection plus a thin context-menu wrapper.
//!
//! The canvas keeps its single `Sense::drag` allocation (a click-sensing
//! widget would add drag latency, and a second `interact` on the axis strip
//! would steal gestures from the region router). Detection is therefore
//! headless: the caller polls the secondary button once per frame and routes
//! the hover point through [`detect_context_trigger`], which answers only
//! for the price axis strip — everywhere else raises nothing. Painting is a
//! thin wrapper over [`egui::Popup`] that raises one typed action per menu
//! row; domain menu contents live beside the caller, never here.

use protopyne_chart_model::ChartRow;

use crate::viewport::Viewport;
use crate::wheel::{Pos2, Rect};

/// A secondary click on a pane's axis strip: which pane, the nearest bar by
/// x (the strip has no slots of its own), and the pane-local anchor the menu
/// opens at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContextTrigger {
    /// Pane index in the caller's zone order.
    pub pane: usize,
    /// Nearest bar index by x, clamped into the rows.
    pub bar_index: usize,
    /// Pane-local anchor (`x` clamped into the plot, `y` verbatim).
    pub plot_pos: (f32, f32),
}

/// Route a hover point to a context trigger, or `None` when the point is not
/// on this pane's axis strip. Only the axis answers — plot, legend, divider,
/// and off-canvas points raise nothing, so a click elsewhere can never open
/// this pane's menu.
#[must_use]
pub fn detect_context_trigger(
    pane: usize,
    axis: Rect,
    plot: Rect,
    viewport: &Viewport,
    rows: &[ChartRow],
    hover: Pos2,
) -> Option<ContextTrigger> {
    if !hover.x.is_finite() || !hover.y.is_finite() || !axis.contains(hover) || rows.is_empty() {
        return None;
    }
    // The strip has no slots: name the nearest bar by x, clamped into the
    // plot so an axis hover still resolves against the pane's own mapping.
    let clamped_x = hover.x.clamp(plot.min_x, plot.max_x);
    let slot = viewport.x_to_index(clamped_x)?;
    if !slot.is_finite() {
        return None;
    }
    let bar_index = ((slot + 0.5).floor() as usize).min(rows.len() - 1);
    Some(ContextTrigger {
        pane,
        bar_index,
        plot_pos: (clamped_x - plot.min_x, hover.y - plot.min_y),
    })
}

/// Paint the menu at the trigger anchor: one button per `(action, label)`
/// row, returning the picked action. A closed menu paints nothing and picks
/// nothing; picking a row closes the menu (outside clicks close it through
/// the popup's own close behavior, written back into `open`).
pub fn paint_context_menu<T: Clone>(
    ui: &mut egui::Ui,
    id: egui::Id,
    at: egui::Pos2,
    open: &mut bool,
    items: &[(T, &str)],
) -> Option<T> {
    if items.is_empty() {
        return None;
    }
    let mut picked: Option<T> = None;
    egui::Popup::new(
        id,
        ui.ctx().clone(),
        egui::PopupAnchor::Position(at),
        egui::LayerId::new(egui::Order::Foreground, id),
    )
    .open_bool(open)
    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
    .show(|ui| {
        for (action, label) in items {
            if ui.button(*label).clicked() {
                picked = Some(action.clone());
            }
        }
    });
    if picked.is_some() {
        // Inside clicks do not close (outside-only behavior above), so a
        // pick closes explicitly; the write-back on `open` handles the
        // outside-click and escape paths.
        *open = false;
    }
    picked
}

#[cfg(test)]
#[path = "../tests/context_menu.rs"]
mod tests;
