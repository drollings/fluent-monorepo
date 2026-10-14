//! Hot-slotted indicator control stack: isolated configs, eye/bolt
//! controls, and an isolated tabbed settings pane.
//!
//! Each [`HotSlot`] owns one indicator's configuration behind an isolated
//! `Box<dyn Component>`: editing one slot's fields through [`FieldAccess`]
//! never reaches another slot, and every slot carries its own parameter
//! fingerprint for cache keys plus its own [`PaneHandle`] abort line. The
//! stack renders the upper-left/upper-right overlay cards (title, live
//! readouts, eye and six-sided-bolt controls) and routes pointer clicks on
//! those cards through [`HotSlotStack::hit_test_controls`] before the canvas
//! annotation path ever sees them. The bolt opens a settings pane over that
//! slot's own schema only, tabbed through [`SettingsTab`].
//!
//! Pane compute nodes ride the caller's [`PanePlan`]: inserting a slot
//! registers its node names there, so DAG order, readiness, and cancellation
//! stay in the one graph instead of a second scheduler.

use std::collections::HashMap;

use fluent_wvr::prelude::*;

use crate::config::ThemeConfig;
use crate::error::EguiError;
use crate::feed::PaneHandle;
use crate::fields::{FieldEdits, SettingsTab, paint_tabbed_schema_fields, schema_fields};
use crate::paint::{
    eye_icon_geom, hex_bolt_icon_geom, paint_eye_icon, paint_hex_bolt_icon, resolve_color,
};
use crate::plan::PanePlan;

/// Where a slot's series paints: over the price pane or in its own sub-pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotAttachment {
    /// Series paints over the main chart (moving averages, ribbons).
    MainOverlay,
    /// Series paints in a dedicated sub-pane (oscillators, histograms).
    SubPane,
}

/// One live readout on a slot card: a named display value.
///
/// Values are display `f64` rendered as text; holes render as a dash rather
/// than a substituted number.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadoutValue {
    /// Readout name shown before the value.
    pub label: String,
    /// Current display value; `None` renders as unready, never as zero.
    pub value: Option<f64>,
}

/// The three pointer zones one slot card owns: the eye toggle, the bolt
/// settings opener, and the card body itself.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotControlRect {
    /// Owning slot id.
    pub slot_id: String,
    /// Eye toggle zone (right side of the card).
    pub eye: egui::Rect,
    /// Bolt settings zone (right of the eye zone).
    pub bolt: egui::Rect,
    /// Whole card zone, controls included.
    pub card: egui::Rect,
}

/// What a pointer inside the overlay stack hit, naming its slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotControlHit {
    /// Inside the eye toggle zone.
    Eye(String),
    /// Inside the bolt settings zone.
    Bolt(String),
    /// Inside the card body, clear of both controls.
    Card(String),
}

/// What one frame of the overlay stack raised: toggles and settings opens
/// come from the painter; the remaining variants are dispatched by the
/// windowing shell from [`HotSlotStack::hit_test_controls`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotSlotAction {
    /// The eye flipped a slot's visibility.
    ToggledVisibility(String),
    /// The bolt opened a slot's settings pane (every other pane closed).
    OpenedSettings(String),
    /// The shell closed a slot's settings pane.
    ClosedSettings(String),
    /// The shell removed a slot (its pane handle already closed).
    Removed(String),
    /// The shell selected a slot card.
    SelectedCard(String),
}

/// One hot-slotted indicator: an isolated config plus its overlay state.
///
/// The config is the only per-slot compute truth; visibility, settings-tab,
/// readouts, fingerprint, and the abort handle are overlay bookkeeping that
/// never leaks across slots.
pub struct HotSlot {
    id: String,
    title: String,
    attachment: SlotAttachment,
    config: Box<dyn Component>,
    visible: bool,
    settings_open: bool,
    settings_tab: SettingsTab,
    handle: PaneHandle,
    readouts: Vec<ReadoutValue>,
    fingerprint: u64,
    plan_nodes: Vec<String>,
}

impl HotSlot {
    /// Slot id (stable key for hit-tests, actions, and cache scoping).
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Display title rendered on the card.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Where the slot's series paints.
    #[must_use]
    pub fn attachment(&self) -> SlotAttachment {
        self.attachment
    }

    /// Whether the slot currently paints (the eye state).
    #[must_use]
    pub fn visible(&self) -> bool {
        self.visible
    }

    /// Whether the slot's settings pane is open.
    #[must_use]
    pub fn settings_open(&self) -> bool {
        self.settings_open
    }

    /// Active settings tab.
    #[must_use]
    pub fn settings_tab(&self) -> SettingsTab {
        self.settings_tab
    }

    /// Live readouts rendered on the card.
    #[must_use]
    pub fn readouts(&self) -> &[ReadoutValue] {
        &self.readouts
    }

    /// Parameter fingerprint this slot registered under.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    /// Pane-plan node names this slot registered.
    #[must_use]
    pub fn plan_nodes(&self) -> &[String] {
        &self.plan_nodes
    }

    /// The slot's abort line (shared with feed and recompute tasks).
    #[must_use]
    pub fn handle(&self) -> PaneHandle {
        self.handle.clone()
    }
}

/// Card width in pixels; cards stack vertically from the stack origin.
const STACK_CARD_WIDTH: f32 = 220.0;
/// Card height in pixels: two compact text rows (title + readouts).
const STACK_CARD_HEIGHT: f32 = 40.0;
/// Vertical gap between stacked cards.
const STACK_CARD_GAP: f32 = 6.0;
/// Eye/bolt control square size.
const STACK_CONTROL_SIZE: f32 = 20.0;
/// Inset of the controls from the card edges.
const STACK_CONTROL_PAD: f32 = 5.0;
/// Default stack origin (below the top bar, clear of the canvas edge).
const STACK_ORIGIN: (f32, f32) = (8.0, 36.0);

/// The overlay stack: insertion-ordered slots with one shared origin.
///
/// Hit-test rects derive from the origin plus slot order, so the painter and
/// [`HotSlotStack::hit_test_controls`] always agree on where the controls
/// are — there is exactly one layout truth.
pub struct HotSlotStack {
    order: Vec<String>,
    slots: HashMap<String, HotSlot>,
    origin: (f32, f32),
}

impl HotSlotStack {
    /// Create an empty stack at the default origin.
    #[must_use]
    pub fn new() -> Self {
        Self { order: Vec::new(), slots: HashMap::new(), origin: STACK_ORIGIN }
    }

    /// Stack origin (top-left of the first card).
    #[must_use]
    pub fn origin(&self) -> (f32, f32) {
        self.origin
    }

    /// Move the stack origin; hit-test rects follow on the next query.
    pub fn set_origin(&mut self, origin: (f32, f32)) {
        self.origin = origin;
    }

    /// Insert a slot and register its pane-plan nodes under `fingerprint`.
    ///
    /// Refuses blank ids/titles, duplicate ids, empty node lists, and nodes
    /// the plan already holds — a refusal leaves both stack and plan
    /// untouched.
    pub fn insert(
        &mut self,
        id: &str,
        title: &str,
        attachment: SlotAttachment,
        config: Box<dyn Component>,
        plan_nodes: &[&str],
        plan_deps: &[&str],
        fingerprint: u64,
        plan: &mut PanePlan,
    ) -> Result<(), EguiError> {
        if id.trim().is_empty() {
            return Err(EguiError::InvalidParam {
                what: "slot_id".to_owned(),
                reason: "slot id must not be empty".to_owned(),
            });
        }
        if title.trim().is_empty() {
            return Err(EguiError::InvalidParam {
                what: "title".to_owned(),
                reason: "slot title must not be empty".to_owned(),
            });
        }
        if self.slots.contains_key(id) {
            return Err(EguiError::InvalidParam {
                what: id.to_owned(),
                reason: "duplicate slot id".to_owned(),
            });
        }
        if plan_nodes.is_empty() {
            return Err(EguiError::InvalidParam {
                what: "plan_nodes".to_owned(),
                reason: "a slot registers at least one plan node".to_owned(),
            });
        }
        for node in plan_nodes {
            if plan.contains(node) {
                return Err(EguiError::Dependency {
                    reason: format!("plan node {node:?} is already registered"),
                });
            }
        }
        let deps: Vec<String> = plan_deps.iter().map(|d| (*d).to_owned()).collect();
        let dep_refs: Vec<&str> = deps.iter().map(String::as_str).collect();
        for node in plan_nodes {
            plan.register_node(node, &dep_refs, fingerprint)?;
        }
        self.order.push(id.to_owned());
        self.slots.insert(
            id.to_owned(),
            HotSlot {
                id: id.to_owned(),
                title: title.to_owned(),
                attachment,
                config,
                visible: true,
                settings_open: false,
                settings_tab: SettingsTab::Inputs,
                handle: PaneHandle::new(),
                readouts: Vec::new(),
                fingerprint,
                plan_nodes: plan_nodes.iter().map(|n| (*n).to_owned()).collect(),
            },
        );
        Ok(())
    }

    /// Remove a slot, closing its pane handle first so feed and recompute
    /// tasks observe the abort. Unknown ids refuse.
    pub fn remove(&mut self, id: &str) -> Result<(), EguiError> {
        let slot = self.slots.get(id).ok_or_else(|| unknown_slot(id))?;
        slot.handle.close();
        self.slots.remove(id);
        self.order.retain(|known| known != id);
        Ok(())
    }

    /// Whether `id` is slotted.
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.slots.contains_key(id)
    }

    /// Slot count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether no slot is stacked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Slot ids in stack (paint) order.
    #[must_use]
    pub fn slot_ids(&self) -> Vec<String> {
        self.order.clone()
    }

    /// Borrow one slot.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&HotSlot> {
        self.slots.get(id)
    }

    /// Whether the slot paints (`None` for unknown ids).
    #[must_use]
    pub fn visible(&self, id: &str) -> Option<bool> {
        self.slots.get(id).map(|slot| slot.visible)
    }

    /// Flip a slot's eye state instantly; fields, readouts, and fingerprint
    /// are untouched. Unknown ids refuse.
    pub fn toggle_visibility(&mut self, id: &str) -> Result<(), EguiError> {
        let slot = self.slots.get_mut(id).ok_or_else(|| unknown_slot(id))?;
        slot.visible = !slot.visible;
        Ok(())
    }

    /// Open one slot's settings pane and close every other, so the tabbed
    /// editor always shows exactly one slot's schema. Unknown ids refuse.
    pub fn open_settings(&mut self, id: &str) -> Result<(), EguiError> {
        if !self.slots.contains_key(id) {
            return Err(unknown_slot(id));
        }
        for (known, slot) in &mut self.slots {
            slot.settings_open = known == id;
        }
        Ok(())
    }

    /// Close one slot's settings pane. Unknown ids refuse.
    pub fn close_settings(&mut self, id: &str) -> Result<(), EguiError> {
        let slot = self.slots.get_mut(id).ok_or_else(|| unknown_slot(id))?;
        slot.settings_open = false;
        Ok(())
    }

    /// Whether the slot's settings pane is open (`false` for unknown ids).
    #[must_use]
    pub fn settings_open(&self, id: &str) -> bool {
        self.slots.get(id).is_some_and(|slot| slot.settings_open)
    }

    /// The slot's active settings tab (`None` for unknown ids).
    #[must_use]
    pub fn settings_tab(&self, id: &str) -> Option<SettingsTab> {
        self.slots.get(id).map(|slot| slot.settings_tab)
    }

    /// Select the slot's settings tab. Unknown ids refuse.
    pub fn set_settings_tab(&mut self, id: &str, tab: SettingsTab) -> Result<(), EguiError> {
        let slot = self.slots.get_mut(id).ok_or_else(|| unknown_slot(id))?;
        slot.settings_tab = tab;
        Ok(())
    }

    /// Borrow one slot's configuration — the settings authority the slot
    /// owns. A behavior plugin reads the same config the settings modal
    /// edits, so an edit and its recompute never diverge. Unknown ids answer
    /// `None`.
    #[must_use]
    pub fn config(&self, id: &str) -> Option<&dyn Component> {
        self.slots.get(id).map(|slot| &*slot.config)
    }

    /// Write one field through the slot's own `FieldAccess` contract; the
    /// field's own validation refuses, never the stack. Unknown ids refuse.
    pub fn set_slot_field(
        &mut self,
        id: &str,
        name: &str,
        value: &str,
    ) -> Result<(), EguiError> {
        let slot = self.slots.get_mut(id).ok_or_else(|| unknown_slot(id))?;
        slot.config.set_field(name, value).map_err(|error| EguiError::InvalidParam {
            what: name.to_owned(),
            reason: error.to_string(),
        })
    }

    /// Read one field through the slot's own `FieldAccess` contract.
    pub fn get_slot_field(&self, id: &str, name: &str) -> Result<String, EguiError> {
        let slot = self.slots.get(id).ok_or_else(|| unknown_slot(id))?;
        slot.config.get_field(name).map_err(|error| EguiError::InvalidParam {
            what: name.to_owned(),
            reason: error.to_string(),
        })
    }

    /// Restamp a slot's parameter fingerprint after its fields change, so
    /// cache keys follow the edited config. Unknown ids refuse.
    pub fn restamp_slot(&mut self, id: &str, fingerprint: u64) -> Result<(), EguiError> {
        let slot = self.slots.get_mut(id).ok_or_else(|| unknown_slot(id))?;
        slot.fingerprint = fingerprint;
        Ok(())
    }

    /// The slot's parameter fingerprint (`None` for unknown ids).
    #[must_use]
    pub fn slot_fingerprint(&self, id: &str) -> Option<u64> {
        self.slots.get(id).map(|slot| slot.fingerprint)
    }

    /// Replace a slot's live card readouts. Unknown ids refuse.
    pub fn set_readouts(
        &mut self,
        id: &str,
        readouts: Vec<ReadoutValue>,
    ) -> Result<(), EguiError> {
        let slot = self.slots.get_mut(id).ok_or_else(|| unknown_slot(id))?;
        slot.readouts = readouts;
        Ok(())
    }

    /// A slot's live card readouts (`None` for unknown ids).
    #[must_use]
    pub fn readouts(&self, id: &str) -> Option<&[ReadoutValue]> {
        self.slots.get(id).map(|slot| slot.readouts.as_slice())
    }

    /// Clone a slot's abort line (shared with its feed and recompute tasks).
    #[must_use]
    pub fn handle(&self, id: &str) -> Option<PaneHandle> {
        self.slots.get(id).map(|slot| slot.handle.clone())
    }

    /// Pointer zones for every slot, in stack order. This is the layout the
    /// painter draws and the hit-test reads — one computation, never two.
    #[must_use]
    pub fn control_rects(&self) -> Vec<SlotControlRect> {
        self.order
            .iter()
            .filter_map(|id| {
                let card = egui::Rect::from_min_size(
                    egui::Pos2::new(self.origin.0, self.origin.1 + self.card_top(id)?),
                    egui::Vec2::new(STACK_CARD_WIDTH, STACK_CARD_HEIGHT),
                );
                let bolt = egui::Rect::from_min_size(
                    egui::Pos2::new(
                        card.max.x - STACK_CONTROL_PAD - STACK_CONTROL_SIZE,
                        card.min.y + (STACK_CARD_HEIGHT - STACK_CONTROL_SIZE) / 2.0,
                    ),
                    egui::Vec2::splat(STACK_CONTROL_SIZE),
                );
                let eye = egui::Rect::from_min_size(
                    egui::Pos2::new(
                        bolt.min.x - STACK_CONTROL_PAD - STACK_CONTROL_SIZE,
                        bolt.min.y,
                    ),
                    egui::Vec2::splat(STACK_CONTROL_SIZE),
                );
                Some(SlotControlRect { slot_id: id.clone(), eye, bolt, card })
            })
            .collect()
    }

    /// Route a pointer before the canvas path: control zones answer with
    /// their slot, anywhere else answers `None` so the event reaches candle
    /// annotation untouched. Eye wins over bolt wins over card.
    #[must_use]
    pub fn hit_test_controls(&self, point: (f32, f32)) -> Option<SlotControlHit> {
        let pointer = egui::Pos2::new(point.0, point.1);
        for rect in self.control_rects() {
            if rect.eye.contains(pointer) {
                return Some(SlotControlHit::Eye(rect.slot_id));
            }
            if rect.bolt.contains(pointer) {
                return Some(SlotControlHit::Bolt(rect.slot_id));
            }
            if rect.card.contains(pointer) {
                return Some(SlotControlHit::Card(rect.slot_id));
            }
        }
        None
    }

    /// Top offset of one card above the stack origin (`None` for unknown ids).
    fn card_top(&self, id: &str) -> Option<f32> {
        self.order.iter().position(|known| known == id).map(|index| {
            // Slot counts are single digits, so the `usize`-to-`f32` cast is
            // exact for every realistic stack.
            index as f32 * (STACK_CARD_HEIGHT + STACK_CARD_GAP)
        })
    }
}

impl Default for HotSlotStack {
    fn default() -> Self {
        Self::new()
    }
}

/// Unknown-slot refusal naming the id.
fn unknown_slot(id: &str) -> EguiError {
    EguiError::InvalidParam { what: id.to_owned(), reason: "unknown slot id".to_owned() }
}

/// One slot's `FieldAccess` face for the tabbed editor: forwards to the
/// slot's own config contract without moving it out of the stack.
struct SlotFields<'a> {
    config: &'a mut Box<dyn Component>,
}

impl FieldAccess for SlotFields<'_> {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        self.config.set_field(name, value)
    }

    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        self.config.get_field(name)
    }

    fn field_names(&self) -> &'static [&'static str] {
        self.config.field_names()
    }
}

/// Paint the overlay stack: one framed card per slot (title plus live
/// readouts), an eye toggle, and a bolt opener, each at the rect
/// [`HotSlotStack::control_rects`] reports.
///
/// Clicks raise [`HotSlotAction`]s (eye toggles flip instantly, bolt opens
/// isolate to one slot); an untouched frame raises nothing. A theme that
/// cannot resolve the frame, eye, or bolt keys refuses with the key named.
pub fn paint_hot_slot_stack(
    ui: &mut egui::Ui,
    stack: &mut HotSlotStack,
    theme: &ThemeConfig,
) -> Result<Vec<HotSlotAction>, EguiError> {
    let grid = resolve_color(theme, "grid")?;
    let mut actions = Vec::new();
    for rect in stack.control_rects() {
        let Some(slot) = stack.slots.get_mut(&rect.slot_id) else {
            continue;
        };
        ui.painter().rect_stroke(rect.card, 4.0, egui::Stroke::new(1.0, grid), egui::StrokeKind::Middle);
        let mut card_text = slot.title.clone();
        let live: Vec<String> = slot
            .readouts
            .iter()
            .filter_map(|readout| {
                readout.value.map(|value| format!("{} {value:.1}", readout.label))
            })
            .collect();
        if !live.is_empty() {
            card_text.push('\n');
            card_text.push_str(&live.join("  "));
        }
        let text_rect = egui::Rect::from_min_max(
            egui::Pos2::new(rect.card.min.x + STACK_CONTROL_PAD, rect.card.min.y + 2.0),
            egui::Pos2::new(rect.eye.min.x - STACK_CONTROL_PAD, rect.card.max.y - 2.0),
        );
        // Bounded single-line rows: a card never reflows its readouts onto
        // extra lines (which would overflow the fixed card height); long
        // values truncate instead of wrapping.
        ui.put(
            text_rect,
            egui::Label::new(card_text).wrap_mode(egui::TextWrapMode::Truncate),
        );
        let eye_response = ui.allocate_rect(rect.eye, egui::Sense::click());
        if eye_response.clicked() {
            slot.visible = !slot.visible;
            actions.push(HotSlotAction::ToggledVisibility(slot.id.clone()));
        }
        if let Some(eye) = eye_icon_geom(eye_center(&rect.eye), eye_radius(), slot.visible) {
            paint_eye_icon(ui.painter(), &eye, "fast", theme)?;
        }
        let bolt_response = ui.allocate_rect(rect.bolt, egui::Sense::click());
        if bolt_response.clicked() {
            let opened = slot.id.clone();
            for other in stack.slots.values_mut() {
                other.settings_open = other.id == opened;
            }
            actions.push(HotSlotAction::OpenedSettings(opened));
        }
        if let Some(bolt) = hex_bolt_icon_geom(bolt_center(&rect.bolt), bolt_radius()) {
            paint_hex_bolt_icon(ui.painter(), &bolt, "slow", theme)?;
        }
    }
    Ok(actions)
}

/// Icon center inside an eye zone.
fn eye_center(zone: &egui::Rect) -> (f32, f32) {
    (zone.center().x, zone.center().y)
}

/// Eye radius inside a control zone (inset so the stroke stays in-bounds).
fn eye_radius() -> f32 {
    STACK_CONTROL_SIZE / 2.0 - 2.0
}

/// Icon center inside a bolt zone.
fn bolt_center(zone: &egui::Rect) -> (f32, f32) {
    (zone.center().x, zone.center().y)
}

/// Bolt rim radius inside a control zone.
fn bolt_radius() -> f32 {
    STACK_CONTROL_SIZE / 2.0 - 2.0
}

/// Paint one slot's settings pane: title plus the tabbed schema editor over
/// that slot's own config. Nothing renders for a closed pane — that refuses
/// instead of painting another slot's schema.
pub fn paint_slot_settings_modal(
    ui: &mut egui::Ui,
    stack: &mut HotSlotStack,
    slot_id: &str,
    theme: &ThemeConfig,
) -> Result<FieldEdits, EguiError> {
    let accent = resolve_color(theme, "fast")?;
    let slot = stack.slots.get_mut(slot_id).ok_or_else(|| unknown_slot(slot_id))?;
    if !slot.settings_open {
        return Err(EguiError::InvalidParam {
            what: slot_id.to_owned(),
            reason: "slot settings are closed".to_owned(),
        });
    }
    let title = slot.title.clone();
    ui.vertical(|pane| {
        pane.colored_label(accent, format!("{title} settings"));
    });
    let fields = schema_fields(&slot.config.describe());
    let mut target = SlotFields { config: &mut slot.config };
    Ok(paint_tabbed_schema_fields(ui, &mut slot.settings_tab, &fields, &mut target))
}

#[cfg(test)]
#[path = "../tests/slot.rs"]
mod tests;
