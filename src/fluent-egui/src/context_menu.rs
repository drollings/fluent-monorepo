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
use crate::wheel::{menu_anchor, Pos2, Rect};

/// A secondary click on a pane's axis strip: which pane, the nearest bar by
/// x (the strip has no slots of its own), and the pane-local anchor the menu
/// opens at.
#[derive(Debug, Clone, Copy, PartialEq)]
/// A secondary click on a pane's axis strip: which pane, the nearest bar by
/// x (the strip has no slots of its own), and the pane-local anchor the menu
/// opens at.
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
/// Route a hover point to a context trigger, or `None` when the point is not
/// on this pane's axis strip. Only the axis answers — plot, legend, divider,
/// and off-canvas points raise nothing, so a click elsewhere can never open
/// this pane's menu.
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

/// One menu row: the action a pick raises, its display label, whether the
/// row answers clicks, and whether a divider follows it.
#[derive(Debug, Clone, PartialEq, Eq)]
/// One menu row: the action a pick raises, its display label, whether the
/// row answers clicks, and whether a divider follows it.
pub struct MenuRow<T> {
    /// Action raised when the operator picks this row.
    pub action: T,
    /// Display label painted on the row.
    pub label: String,
    /// Whether the row answers clicks; disabled rows paint dimmed and never
    /// yield picks.
    pub enabled: bool,
    /// Whether a divider follows this row.
    pub divider_after: bool,
}

impl<T> From<(T, &str)> for MenuRow<T> {
    /// Legacy tuple spelling: enabled rows with no divider.
    fn from((action, label): (T, &str)) -> Self {
        Self {
            action,
            label: label.to_owned(),
            enabled: true,
            divider_after: false,
        }
    }
}

/// Paint the menu at the trigger anchor: one button per row, returning the
/// picked action. A closed menu paints nothing and picks nothing; picking a
/// row closes the menu (outside clicks close it through the popup's own close
/// behavior, written back into `open`).
///
/// The opening click never lands a pick: picks are suppressed until two
/// frames after the menu opened, so the gesture that raised the menu cannot
/// also choose from it. Disabled rows never yield picks on any frame. The
/// popup anchors through the flip-aware placement, so a trigger near a
/// screen edge flips the card instead of clipping it; the card size is
/// measured from the previous frame, so the first frame anchors unflipped.
pub fn paint_context_menu<T: Clone>(
    ui: &mut egui::Ui,
    id: egui::Id,
    at: egui::Pos2,
    open: &mut bool,
    items: &[impl Into<MenuRow<T>> + Clone],
) -> Option<T> {
    if items.is_empty() {
        return None;
    }
    let size_id = id.with("menu-size");
    if !*open {
        ui.data_mut(|data| {
            data.insert_temp(id, u64::MAX);
            data.remove_temp::<(f32, f32)>(size_id);
        });
        return None;
    }
    // Two-frame open guard: the frame the menu opened on, remembered per
    // menu, so the opening gesture cannot land a pick.
    let frame = ui.ctx().cumulative_frame_nr();
    let opened = ui.data_mut(|data| {
        let slot: &mut u64 = data.get_temp_mut_or(id, u64::MAX);
        if *slot == u64::MAX {
            *slot = frame;
        }
        *slot
    });
    let live = frame.saturating_sub(opened) >= 2;
    // Flip-aware anchor: the measured card size from the previous frame
    // places this frame's origin, so near-edge triggers flip.
    let screen = ui.ctx().content_rect();
    let headless_screen = Rect::new(
        Pos2::new(screen.min.x, screen.min.y),
        Pos2::new(screen.max.x, screen.max.y),
    );
    let trigger = Pos2::new(at.x, at.y);
    let measured: Option<(f32, f32)> = ui.data(|data| data.get_temp(size_id));
    let anchor = match measured {
        Some((width, height)) => {
            let placed = menu_anchor(headless_screen, (width, height), trigger);
            egui::Pos2::new(placed.min_x, placed.min_y)
        }
        None => at,
    };
    let mut popup = egui::Popup::new(
        id,
        ui.ctx().clone(),
        egui::PopupAnchor::Position(anchor),
        egui::LayerId::new(egui::Order::Foreground, id),
    )
    .open_bool(open)
    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
    if measured.is_some() {
        popup = popup
            .align(egui::RectAlign::TOP_START)
            .align_alternatives(&[]);
    }
    let mut picked: Option<T> = None;
    let shown = popup.show(|ui| {
        for item in items {
            let row_item: MenuRow<T> = item.clone().into();
            if row_item.enabled {
                if ui.button(&row_item.label).clicked() && live {
                    picked = Some(row_item.action.clone());
                }
            } else {
                ui.add_enabled(false, egui::Button::new(&row_item.label));
            }
            if row_item.divider_after {
                ui.separator();
            }
        }
    });
    if let Some(inner) = shown {
        let size = inner.response.rect.size();
        ui.data_mut(|data| {
            data.insert_temp(size_id, (size.x, size.y));
        });
    }
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