//! Hot-slotted indicator control stack: isolated configs, eye/bolt
//! controls, and an isolated tabbed settings pane.
//!
//! Each [`HotSlot`] owns one indicator's configuration *and* one
//! [`IndicatorStyle`] behind isolated `Box<dyn Component>`s: editing one
//! slot's fields through [`FieldAccess`] never reaches another slot, and
//! every slot carries its own parameter fingerprint for cache keys plus its
//! own [`PaneHandle`] abort line. The two staged buffers are committed
//! together, so the Style tab's colors and stroke width land on the same
//! Apply as the inputs that change the series. The
//! stack renders the upper-left/upper-right overlay cards (title, live
//! readouts, eye and six-sided-bolt controls) and routes pointer clicks on
//! those cards through [`HotSlotStack::hit_test_controls`] before the canvas
//! annotation path ever sees them. The bolt opens a settings pane over that
//! slot's own schema only, tabbed through [`SettingsTab`].
//!
//! Pane compute nodes ride the caller's [`PanePlan`]: inserting a slot
//! registers its node names there, so DAG order, readiness, and cancellation
//! stay in the one graph instead of a second scheduler.

use std::borrow::Cow;
use std::collections::HashMap;

use fluent_wvr::component_downcast_ref;
use fluent_wvr::prelude::*;

use crate::config::ThemeConfig;
use crate::error::EguiError;
use crate::feed::PaneHandle;
use crate::fields::{paint_tabbed_schema_fields_compact, schema_fields, FieldEdits, SettingsTab};
use crate::paint::{
    eye_icon_geom, hex_bolt_icon_geom, paint_eye_icon, paint_hex_bolt_icon, resolve_color,
};
use crate::plan::PanePlan;
use crate::style::IndicatorStyle;

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
/// settings opener, the close (unload) control, and the card body itself.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotControlRect {
    /// Owning slot id.
    pub slot_id: String,
    /// Eye toggle zone (left of the bolt).
    pub eye: egui::Rect,
    /// Bolt settings zone (right of the eye).
    pub bolt: egui::Rect,
    /// Close zone (right of the bolt): unloads the indicator immediately.
    /// Material "X" affordance — same touch size as eye/bolt, no confirm.
    pub close: egui::Rect,
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
    /// Inside the close (unload) zone.
    Close(String),
    /// Inside the card body, clear of all controls.
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
    /// The shell removed a slot (its pane handle already closed). The painter
    /// raises this directly from the X control — unload is immediate, no
    /// confirm, so workstation and touch behave identically.
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
    style: Box<dyn Component>,
    visible: bool,
    settings_open: bool,
    settings_tab: SettingsTab,
    /// Staged settings edits, seeded from the live config and style when the
    /// pane opens. Edits land here until Apply/OK commits them through the
    /// live `FieldAccess` contracts, so Cancel discards them without ever
    /// touching the chart. `None` while the pane is closed.
    pending: Option<PendingSettings>,
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

    /// The slot's paint overrides — the Style tab's live target.
    #[must_use]
    pub fn style(&self) -> &dyn Component {
        &*self.style
    }

    /// The slot's paint overrides as the concrete style every keyed emitter
    /// reads through [`IndicatorStyle::color`].
    ///
    /// Borrowed when the slot's style component really is an
    /// [`IndicatorStyle`] (the allocation-free frame path) and the no-op
    /// default otherwise, so a slot carrying some other style shape paints
    /// the theme rather than failing the frame.
    #[must_use]
    pub fn indicator_style(&self) -> Cow<'_, IndicatorStyle> {
        component_downcast_ref::<IndicatorStyle>(&*self.style)
            .map_or(Cow::Owned(IndicatorStyle::default()), Cow::Borrowed)
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
const STACK_CARD_WIDTH: f32 = 168.0;
/// Card height in pixels: two compact text rows (title + readouts).
const STACK_CARD_HEIGHT: f32 = 32.0;
/// Vertical gap between stacked cards.
const STACK_CARD_GAP: f32 = 3.0;
/// Eye/bolt control square size.
const STACK_CONTROL_SIZE: f32 = 16.0;
/// Inset of the controls from the card edges.
const STACK_CONTROL_PAD: f32 = 3.0;
/// Default stack origin (below the top bar, clear of the canvas edge).
const STACK_ORIGIN: (f32, f32) = (8.0, 36.0);

/// The overlay stack: insertion-ordered slots with one shared origin.
///
/// Hit-test rects derive from the origin plus slot order plus the scroll
/// offset, so the painter and [`HotSlotStack::hit_test_controls`] always
/// agree on where the controls are — there is exactly one layout truth.
/// Scrolling (a wheel over the legend) moves every card together and clamps
/// to the content, so the list can never be dragged off its own cards.
pub struct HotSlotStack {
    order: Vec<String>,
    slots: HashMap<String, HotSlot>,
    origin: (f32, f32),
    scroll_px: f32,
}

impl HotSlotStack {
    /// Create an empty stack at the default origin.
    #[must_use]
    pub fn new() -> Self {
        Self {
            order: Vec::new(),
            slots: HashMap::new(),
            origin: STACK_ORIGIN,
            scroll_px: 0.0,
        }
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

    /// Current scroll offset in pixels; `0.0` means the first card sits at
    /// the stack origin.
    #[must_use]
    pub fn scroll_offset(&self) -> f32 {
        self.scroll_px
    }

    /// Total height of the card column in pixels, gaps included.
    #[must_use]
    pub fn content_height(&self) -> f32 {
        if self.order.is_empty() {
            return 0.0;
        }
        // Slot counts are single digits, so the `usize`-to-`f32` cast is
        // exact for every realistic stack.
        (self.order.len() as f32 - 1.0) * (STACK_CARD_HEIGHT + STACK_CARD_GAP) + STACK_CARD_HEIGHT
    }

    /// Scroll the card column by a wheel gesture, clamped so the list stops
    /// at both ends: a positive `delta_px` scrolls up (toward the first card,
    /// matching the platform wheel convention), and the offset never exceeds
    /// `content_height - viewport_height`.
    pub fn scroll_by(&mut self, delta_px: f32, viewport_height: f32) {
        if !delta_px.is_finite() {
            return;
        }
        let slack = (self.content_height() - viewport_height.max(0.0)).max(0.0);
        self.scroll_px = (self.scroll_px - delta_px).clamp(0.0, slack);
    }

    /// The region a wheel scrolls instead of the chart: the card column
    /// clipped to `viewport_height` from the stack origin. `None` for an
    /// empty stack (there is no list to scroll).
    #[must_use]
    pub fn list_rect(&self, viewport_height: f32) -> Option<egui::Rect> {
        let height = self.content_height();
        if height <= 0.0 || !viewport_height.is_finite() || viewport_height <= 0.0 {
            return None;
        }
        Some(egui::Rect::from_min_size(
            egui::Pos2::new(self.origin.0, self.origin.1),
            egui::Vec2::new(STACK_CARD_WIDTH, height.min(viewport_height)),
        ))
    }

    /// Insert a slot with the default (no-op) style component and register
    /// its pane-plan nodes under `fingerprint`.
    ///
    /// The plain form of [`insert_with_style`](Self::insert_with_style): a
    /// slot added without a style still renders the Style tab, and every
    /// field in it inherits the theme.
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
        self.insert_with_style(
            id,
            title,
            attachment,
            config,
            Box::new(IndicatorStyle::default()),
            plan_nodes,
            plan_deps,
            fingerprint,
            plan,
        )
    }

    /// Insert a slot with its own paint-override component and register its
    /// pane-plan nodes under `fingerprint`.
    ///
    /// The config is the settings authority for *what* the indicator
    /// computes; the style component is the authority for *how* it paints
    /// (colors, stroke width). They are separate components because they
    /// have different lifetimes in a frame: a config edit moves cache keys
    /// and forces a recompute, a style edit only redraws. Refuses blank
    /// ids/titles, duplicate ids, empty node lists, and nodes the plan
    /// already holds — a refusal leaves both stack and plan untouched.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_with_style(
        &mut self,
        id: &str,
        title: &str,
        attachment: SlotAttachment,
        config: Box<dyn Component>,
        style: Box<dyn Component>,
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
                style,
                visible: true,
                settings_open: false,
                settings_tab: SettingsTab::Inputs,
                pending: None,
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
        // The column just shortened, so a scrolled offset may now overshoot
        // the content; re-clamp against the last known viewport extent.
        self.scroll_px = self.scroll_px.min(self.content_height());
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
    /// editor always shows exactly one slot's schema. Opening seeds that
    /// slot's staged buffer from its live config and drops any other slot's
    /// staged edits, so an abandoned draft never leaks into a new pane.
    /// Unknown ids refuse.
    pub fn open_settings(&mut self, id: &str) -> Result<(), EguiError> {
        if !self.slots.contains_key(id) {
            return Err(unknown_slot(id));
        }
        for (known, slot) in &mut self.slots {
            if known == id {
                slot.settings_open = true;
                slot.pending = Some(seed_pending(&*slot.config, &*slot.style));
            } else {
                slot.settings_open = false;
                slot.pending = None;
            }
        }
        Ok(())
    }

    /// Close one slot's settings pane, dropping any staged edits. Unknown ids
    /// refuse.
    pub fn close_settings(&mut self, id: &str) -> Result<(), EguiError> {
        let slot = self.slots.get_mut(id).ok_or_else(|| unknown_slot(id))?;
        slot.settings_open = false;
        slot.pending = None;
        Ok(())
    }

    /// Commit the slot's staged edits through the live `FieldAccess` contracts
    /// of its configuration *and* its style component, then drop the staged
    /// buffers. Returns the per-field refusal reasons (each naming its
    /// field); a field the live contract refuses is left at its committed
    /// value while every accepted field applies, and one component refusing
    /// never stops the other from committing. Unknown ids refuse.
    pub fn commit_settings(&mut self, id: &str) -> Result<Vec<String>, EguiError> {
        let slot = self.slots.get_mut(id).ok_or_else(|| unknown_slot(id))?;
        let Some(PendingSettings { config, style }) = slot.pending.take() else {
            return Ok(Vec::new());
        };
        let mut refusals = Vec::new();
        for (name, value) in config {
            if let Err(error) = slot.config.set_field(&name, &value) {
                refusals.push(format!("{name}: {error}"));
            }
        }
        for (name, value) in style {
            if let Err(error) = slot.style.set_field(&name, &value) {
                refusals.push(format!("{name}: {error}"));
            }
        }
        Ok(refusals)
    }

    /// Drop the slot's staged edits without applying them. Unknown ids
    /// refuse.
    pub fn discard_settings(&mut self, id: &str) -> Result<(), EguiError> {
        let slot = self.slots.get_mut(id).ok_or_else(|| unknown_slot(id))?;
        slot.pending = None;
        Ok(())
    }

    /// Whether a staged settings buffer is live for `id` (`false` for
    /// unknown ids).
    #[must_use]
    pub fn has_pending_settings(&self, id: &str) -> bool {
        self.slots
            .get(id)
            .is_some_and(|slot| slot.pending.is_some())
    }

    /// The overlayed card width in pixels, so a caller can place the stack
    /// against the chart's right edge.
    #[must_use]
    pub fn card_width(&self) -> f32 {
        STACK_CARD_WIDTH
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

    /// Borrow one slot's paint-override component — the Style tab's settings
    /// authority, isolated exactly like the config beside it. Unknown ids
    /// answer `None`.
    #[must_use]
    pub fn style(&self, id: &str) -> Option<&dyn Component> {
        self.slots.get(id).map(|slot| &*slot.style)
    }

    /// One slot's paint overrides as the concrete style every keyed emitter
    /// reads through. Unknown ids read as the no-op default, so a paint path
    /// never has to branch on whether the slot exists before it can draw.
    #[must_use]
    pub fn indicator_style(&self, id: &str) -> Cow<'_, IndicatorStyle> {
        self.slots.get(id).map_or_else(
            || Cow::Owned(IndicatorStyle::default()),
            HotSlot::indicator_style,
        )
    }

    /// Write one field through the slot's own `FieldAccess` contract; the
    /// field's own validation refuses, never the stack. Unknown ids refuse.
    pub fn set_slot_field(&mut self, id: &str, name: &str, value: &str) -> Result<(), EguiError> {
        let slot = self.slots.get_mut(id).ok_or_else(|| unknown_slot(id))?;
        slot.config
            .set_field(name, value)
            .map_err(|error| EguiError::InvalidParam {
                what: name.to_owned(),
                reason: error.to_string(),
            })
    }

    /// Write one Style field straight through the slot's style component's own
    /// `FieldAccess` contract; the field's own validation refuses, never the
    /// stack. This is the live path a caller uses outside the settings pane —
    /// inside the pane the same write arrives through
    /// [`commit_settings`](Self::commit_settings) on Apply. Unknown ids refuse.
    ///
    /// A style write needs no recompute: it changes how a series paints, never
    /// what a series is, so it deliberately does not touch the pane cache the
    /// way a config write does.
    pub fn set_style_field(&mut self, id: &str, name: &str, value: &str) -> Result<(), EguiError> {
        let slot = self.slots.get_mut(id).ok_or_else(|| unknown_slot(id))?;
        slot.style
            .set_field(name, value)
            .map_err(|error| EguiError::InvalidParam {
                what: name.to_owned(),
                reason: error.to_string(),
            })
    }

    /// Read one Style field through the slot's style component's own
    /// `FieldAccess` contract. Unknown ids refuse.
    pub fn get_style_field(&self, id: &str, name: &str) -> Result<String, EguiError> {
        let slot = self.slots.get(id).ok_or_else(|| unknown_slot(id))?;
        slot.style
            .get_field(name)
            .map_err(|error| EguiError::InvalidParam {
                what: name.to_owned(),
                reason: error.to_string(),
            })
    }

    /// Read one field through the slot's own `FieldAccess` contract.
    pub fn get_slot_field(&self, id: &str, name: &str) -> Result<String, EguiError> {
        let slot = self.slots.get(id).ok_or_else(|| unknown_slot(id))?;
        slot.config
            .get_field(name)
            .map_err(|error| EguiError::InvalidParam {
                what: name.to_owned(),
                reason: error.to_string(),
            })
    }

    /// Write one field into the slot's staged settings buffer (the settings
    /// pane's draft), when the pane is open. This is the series-picker path:
    /// a catalogue pick lands in the draft so Cancel still discards it and
    /// Apply commits it through the live contract. Refuses unknown ids or a
    /// closed pane — a draft write with nowhere to stage is a bug, not a
    /// state to paper over.
    pub fn set_staged_field(&mut self, id: &str, name: &str, value: &str) -> Result<(), EguiError> {
        let slot = self.slots.get_mut(id).ok_or_else(|| unknown_slot(id))?;
        let Some(pending) = slot.pending.as_mut() else {
            return Err(EguiError::InvalidParam {
                what: id.to_owned(),
                reason: "slot settings are closed".to_owned(),
            });
        };
        // Route exactly like `StagedFields`: style-owned names go to the style
        // buffer, everything else to the config buffer. The two key sets are
        // disjoint (style owns `color_*` + `line_width`), so the route is
        // unambiguous.
        let style_names = slot.style.field_names();
        if style_names.contains(&name) {
            pending.style.insert(name.to_owned(), value.to_owned());
        } else {
            pending.config.insert(name.to_owned(), value.to_owned());
        }
        Ok(())
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
    pub fn set_readouts(&mut self, id: &str, readouts: Vec<ReadoutValue>) -> Result<(), EguiError> {
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
    /// Layout right-to-left: close (X) at the far right, bolt left of it,
    /// eye left of the bolt; the title ends at the eye's left edge.
    #[must_use]
    pub fn control_rects(&self) -> Vec<SlotControlRect> {
        self.order
            .iter()
            .filter_map(|id| {
                let card = egui::Rect::from_min_size(
                    egui::Pos2::new(self.origin.0, self.origin.1 + self.card_top(id)?),
                    egui::Vec2::new(STACK_CARD_WIDTH, STACK_CARD_HEIGHT),
                );
                let close = egui::Rect::from_min_size(
                    egui::Pos2::new(
                        card.max.x - STACK_CONTROL_PAD - STACK_CONTROL_SIZE,
                        card.min.y + (STACK_CARD_HEIGHT - STACK_CONTROL_SIZE) / 2.0,
                    ),
                    egui::Vec2::splat(STACK_CONTROL_SIZE),
                );
                let bolt = egui::Rect::from_min_size(
                    egui::Pos2::new(
                        close.min.x - STACK_CONTROL_PAD - STACK_CONTROL_SIZE,
                        close.min.y,
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
                Some(SlotControlRect {
                    slot_id: id.clone(),
                    eye,
                    bolt,
                    close,
                    card,
                })
            })
            .collect()
    }

    /// Route a pointer before the canvas path: control zones answer with
    /// their slot, anywhere else answers `None` so the event reaches candle
    /// annotation untouched. Eye wins over bolt wins over close wins over card.
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
            if rect.close.contains(pointer) {
                return Some(SlotControlHit::Close(rect.slot_id));
            }
            if rect.card.contains(pointer) {
                return Some(SlotControlHit::Card(rect.slot_id));
            }
        }
        None
    }

    /// Top offset of one card above the stack origin (`None` for unknown ids).
    fn card_top(&self, id: &str) -> Option<f32> {
        self.order
            .iter()
            .position(|known| known == id)
            .map(|index| {
                // Slot counts are single digits, so the `usize`-to-`f32` cast is
                // exact for every realistic stack.
                index as f32 * (STACK_CARD_HEIGHT + STACK_CARD_GAP) - self.scroll_px
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
    EguiError::InvalidParam {
        what: id.to_owned(),
        reason: "unknown slot id".to_owned(),
    }
}

/// Seed a staged settings buffer from a live config: one string per declared
/// field, read through the config's own `FieldAccess` contract. A field whose
/// value cannot be read is skipped rather than staged as a substitute.
fn seed_fields(config: &dyn Component) -> HashMap<String, String> {
    config
        .field_names()
        .iter()
        .filter_map(|name| {
            config
                .get_field(name)
                .ok()
                .map(|value| ((*name).to_owned(), value))
        })
        .collect()
}

/// A settings pane's staged edits: one buffer per component the slot owns,
/// kept side by side so Cancel drops both and one commit writes both.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct PendingSettings {
    /// Fields staged for the slot's configuration.
    config: HashMap<String, String>,
    /// Fields staged for the slot's style component.
    style: HashMap<String, String>,
}

/// Seed a staged buffer pair from a live config and style: one string per
/// declared field, each read through that component's own `FieldAccess`
/// contract. A field whose value cannot be read is skipped rather than staged
/// as a substitute.
fn seed_pending(config: &dyn Component, style: &dyn Component) -> PendingSettings {
    PendingSettings {
        config: seed_fields(config),
        style: seed_fields(style),
    }
}

/// The staged settings buffers' `FieldAccess` face: edits land in an owned
/// map until the pane commits them, so the live components and the chart are
/// untouched by a draft.
///
/// A field name decides which buffer owns it — a name the style component
/// declares stages into the style buffer, everything else into the
/// configuration's. The two key sets are disjoint (the style component owns
/// `color_*` and `line_width` and no configuration declares those), so the
/// route is unambiguous and one field is never staged into both.
struct StagedFields<'a> {
    config: &'a mut HashMap<String, String>,
    style: &'a mut HashMap<String, String>,
    style_names: &'static [&'static str],
}

impl FieldAccess for StagedFields<'_> {
    fn set_field(&mut self, name: &str, value: &str) -> Result<(), FieldError> {
        let buffer = if self.style_names.contains(&name) {
            &mut *self.style
        } else {
            &mut *self.config
        };
        buffer.insert(name.to_owned(), value.to_owned());
        Ok(())
    }

    fn get_field(&self, name: &str) -> Result<String, FieldError> {
        let buffer = if self.style_names.contains(&name) {
            &*self.style
        } else {
            &*self.config
        };
        buffer
            .get(name)
            .cloned()
            .ok_or_else(|| FieldError::NotFound(name.to_owned()))
    }

    fn field_names(&self) -> &'static [&'static str] {
        // The editor iterates the schema's fields, never this list.
        &[]
    }
}

/// What the settings pane's action row raised this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsCommit {
    /// No action button was pressed.
    #[default]
    None,
    /// Apply the staged edits to the chart but keep the pane open.
    Apply,
    /// Discard the staged edits and close the pane.
    Cancel,
    /// Apply the staged edits and close the pane.
    Ok,
}

/// One settings-pane frame: the edits the pass staged plus the action button
/// the operator pressed, if any.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsOutcome {
    /// Field writes the editor accepted into the staged buffer.
    pub edits: FieldEdits,
    /// Action button pressed this frame, if any.
    pub commit: SettingsCommit,
}

/// Paint the overlay stack: one borderless compact card per slot (title plus
/// live readouts), an eye toggle, a bolt opener, and a close (unload) X, each
/// at the rect [`HotSlotStack::control_rects`] reports.
///
/// The cards carry no frame, so the legend floats over the candles without a
/// boundary box; only the controls are painted. Clicks raise [`HotSlotAction`]s
/// (eye toggles flip instantly, bolt opens isolate to one slot, X unloads
/// immediately with no confirm); an untouched frame raises nothing. A theme
/// that cannot resolve the eye or bolt key refuses with the key named.
pub fn paint_hot_slot_stack(
    ui: &mut egui::Ui,
    stack: &mut HotSlotStack,
    theme: &ThemeConfig,
) -> Result<Vec<HotSlotAction>, EguiError> {
    let mut actions = Vec::new();
    for rect in stack.control_rects() {
        let slot_id = rect.slot_id.clone();
        // Snapshot title/readouts/visibility without holding the borrow across
        // the bolt-open loop below (which mutates every slot).
        let (card_text, visible) = {
            let Some(slot) = stack.slots.get(&slot_id) else {
                continue;
            };
            let mut card_text = slot.title.clone();
            let live: Vec<String> = slot
                .readouts
                .iter()
                .filter_map(|readout| {
                    readout
                        .value
                        .map(|value| format!("{} {value:.1}", readout.label))
                })
                .collect();
            if !live.is_empty() {
                card_text.push('\n');
                card_text.push_str(&live.join("  "));
            }
            (card_text, slot.visible)
        };
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
            if let Some(slot) = stack.slots.get_mut(&slot_id) {
                slot.visible = !slot.visible;
            }
            actions.push(HotSlotAction::ToggledVisibility(slot_id.clone()));
        }
        if let Some(eye) = eye_icon_geom(eye_center(&rect.eye), eye_radius(), visible) {
            paint_eye_icon(ui.painter(), &eye, "fast", theme)?;
        }
        let bolt_response = ui.allocate_rect(rect.bolt, egui::Sense::click());
        if bolt_response.clicked() {
            for other in stack.slots.values_mut() {
                let open = other.id == slot_id;
                other.settings_open = open;
                if open {
                    other.pending = Some(seed_pending(&*other.config, &*other.style));
                } else {
                    other.pending = None;
                }
            }
            actions.push(HotSlotAction::OpenedSettings(slot_id.clone()));
        }
        if let Some(bolt) = hex_bolt_icon_geom(bolt_center(&rect.bolt), bolt_radius()) {
            paint_hex_bolt_icon(ui.painter(), &bolt, "slow", theme)?;
        }
        // Close (X) affordance right of the bolt: immediate unload, no confirm.
        // Painted as two crossing strokes in the theme grid ink so it reads as
        // chrome, never data; hovered state brightens via the response.
        let close_response = ui.allocate_rect(rect.close, egui::Sense::click());
        if close_response.clicked() {
            actions.push(HotSlotAction::Removed(slot_id.clone()));
        }
        let ink = resolve_color(theme, "grid")?;
        let pad = 4.0;
        let p0 = egui::pos2(rect.close.min.x + pad, rect.close.min.y + pad);
        let p1 = egui::pos2(rect.close.max.x - pad, rect.close.max.y - pad);
        let p2 = egui::pos2(rect.close.max.x - pad, rect.close.min.y + pad);
        let p3 = egui::pos2(rect.close.min.x + pad, rect.close.max.y - pad);
        let stroke = egui::Stroke::new(1.5, ink);
        ui.painter().line_segment([p0, p1], stroke);
        ui.painter().line_segment([p2, p3], stroke);
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

/// Paint one slot's settings pane: title, the compact tabbed schema editor
/// over that slot's *staged* buffer, and the Cancel / Apply / OK action row.
///
/// Edits never touch the live config here — they land in the staged buffer
/// the pane owns, so Cancel discards them without moving the chart. The
/// caller commits through [`HotSlotStack::commit_settings`] on Apply/OK (and
/// closes on OK/Cancel). Nothing renders for a closed pane — that refuses
/// instead of painting another slot's schema.
pub fn paint_slot_settings_modal(
    ui: &mut egui::Ui,
    stack: &mut HotSlotStack,
    slot_id: &str,
    theme: &ThemeConfig,
) -> Result<SettingsOutcome, EguiError> {
    let accent = resolve_color(theme, "fast")?;
    let slot = stack
        .slots
        .get_mut(slot_id)
        .ok_or_else(|| unknown_slot(slot_id))?;
    if !slot.settings_open {
        return Err(EguiError::InvalidParam {
            what: slot_id.to_owned(),
            reason: "slot settings are closed".to_owned(),
        });
    }
    let title = slot.title.clone();
    let config_fields = schema_fields(&slot.config.describe());
    let style_fields = schema_fields(&slot.style.describe());
    if slot.pending.is_none() {
        slot.pending = Some(seed_pending(&*slot.config, &*slot.style));
    }
    let HotSlot {
        pending,
        settings_tab,
        style,
        ..
    } = slot;
    let pending = pending.as_mut().expect("staged buffers are seeded above");
    let mut target = StagedFields {
        config: &mut pending.config,
        style: &mut pending.style,
        style_names: style.field_names(),
    };
    let mut commit = SettingsCommit::None;
    let outcome = ui.vertical(|pane| {
        pane.colored_label(accent, format!("{title} settings"));
        let edits = paint_tabbed_schema_fields_compact(
            pane,
            settings_tab,
            &config_fields,
            &style_fields,
            &mut target,
        );
        pane.horizontal(|row| {
            if row.button("Cancel").clicked() {
                commit = SettingsCommit::Cancel;
            }
            if row.button("Apply").clicked() {
                commit = SettingsCommit::Apply;
            }
            if row.button("OK").clicked() {
                commit = SettingsCommit::Ok;
            }
        });
        edits
    });
    Ok(SettingsOutcome {
        edits: outcome.inner,
        commit,
    })
}

#[cfg(test)]
#[path = "../tests/slot.rs"]
mod tests;
