//! Hot-slot control stack: registration into the pane plan, per-slot
//! isolation, instant eye toggles, isolated bolt settings, abort on removal,
//! and control hit-testing that never leaks to the canvas.

use super::*;
use fluent_wvr::{Describable, FieldAccess, WorkError, WorkOutput, WorkUnit};

use crate::annotations::annotate_at;
use crate::viewport::Viewport;
use protopyne_chart_model::ChartRow;
use protopyne_chart_scale::PriceScaleMode;

// NOTE: `Arc`, `ArcIntern`, `Component`, `FieldError`, `WorkContext`,
// `PanePlan`, `PaneHandle`, `SettingsTab`, and every `slot` item arrive via
// `super::*` (the `slot` module's own imports).

/// Domain-blind stand-in for one indicator's configuration: a calculation
/// input, a paint knob, and a visibility toggle behind one schema.
#[derive(Debug, Clone, FieldAccess, Describable)]
struct ProbeConfig {
    #[field(desc = "Lookback length", min = 1)]
    length: i64,
    #[field(desc = "Line color (#RRGGBB)")]
    line_color: String,
    #[field(desc = "Show the line")]
    show_line: bool,
}

impl WorkUnit for ProbeConfig {
    fn name(&self) -> &str {
        "probe_config"
    }

    fn depends(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn provides(&self) -> &[ArcIntern<str>] {
        &[]
    }

    fn execute(&self, _ctx: &WorkContext) -> Result<WorkOutput, WorkError> {
        Ok(WorkOutput::ok("probe ready"))
    }
}

fluent_wvr::impl_component!(ProbeConfig);

fn probe(length: i64) -> ProbeConfig {
    ProbeConfig { length, line_color: "#20C8F0".to_owned(), show_line: true }
}

fn stacked() -> (HotSlotStack, PanePlan) {
    let mut plan = PanePlan::new();
    let mut stack = HotSlotStack::new();
    stack
        .insert(
            "integration_lite",
            "Integration Lite",
            SlotAttachment::SubPane,
            Box::new(probe(14)),
            &["osc_fast", "osc_slow"],
            &[],
            0xA1,
            &mut plan,
        )
        .expect("first slot inserts");
    stack
        .insert(
            "fast_mas",
            "Fast MAs",
            SlotAttachment::MainOverlay,
            Box::new(probe(8)),
            &["fast_ma"],
            &[],
            0xB2,
            &mut plan,
        )
        .expect("second slot inserts");
    (stack, plan)
}

#[test]
fn slot_config_exposes_the_settings_authority() {
    let (mut stack, _) = stacked();
    let config = stack.config("integration_lite").expect("a slotted config reads");
    assert_eq!(config.get_field("length").expect("reads"), "14");
    stack.set_slot_field("integration_lite", "length", "21").expect("the field edits");
    assert_eq!(
        stack.config("integration_lite").expect("reads").get_field("length").expect("reads"),
        "21",
        "the borrow sees the same config the settings modal edits"
    );
    assert!(stack.config("missing").is_none(), "unknown slots answer None");
}

#[test]
fn hot_slot_stack_registers_and_isolates_independent_components() {
    let (mut stack, plan) = stacked();
    assert!(stack.contains("integration_lite"), "first slot registers");
    assert!(stack.contains("fast_mas"), "second slot registers");
    assert_eq!(stack.len(), 2);
    for node in ["osc_fast", "osc_slow", "fast_ma"] {
        assert!(plan.contains(node), "slot node {node} registers into the pane plan");
    }
    // One slot's field write never touches the other slot's component or
    // fingerprint.
    stack.set_slot_field("integration_lite", "length", "21").expect("field writes");
    assert_eq!(stack.get_slot_field("integration_lite", "length").expect("reads"), "21");
    assert_eq!(
        stack.get_slot_field("fast_mas", "length").expect("reads"),
        "8",
        "the other slot keeps its own value"
    );
    assert_eq!(stack.slot_fingerprint("fast_mas"), Some(0xB2), "the other fingerprint holds");
    // Unknown slots and fields refuse with the slot or field named.
    assert!(stack.set_slot_field("missing", "length", "1").is_err(), "unknown slot refuses");
    assert!(
        stack.set_slot_field("integration_lite", "length", "0").is_err(),
        "the field contract refuses below its minimum"
    );
    assert!(stack.get_slot_field("missing", "length").is_err(), "unknown slot reads refuse");
    // Duplicates and blanks refuse instead of aliasing a live slot.
    let mut plan = plan;
    let refused = stack
        .insert(
            "integration_lite",
            "Dupe",
            SlotAttachment::MainOverlay,
            Box::new(probe(1)),
            &["dupe_node"],
            &[],
            0xC3,
            &mut plan,
        )
        .expect_err("duplicate id refuses");
    assert!(refused.to_string().contains("integration_lite"), "refusal names the id: {refused}");
    assert!(
        stack
            .insert(
                "",
                "Blank",
                SlotAttachment::MainOverlay,
                Box::new(probe(1)),
                &["blank_node"],
                &[],
                0xC3,
                &mut plan,
            )
            .is_err(),
        "blank ids refuse"
    );
}

#[test]
fn eye_toggle_flips_visibility_instantly_and_preserves_settings() {
    let (mut stack, _) = stacked();
    stack.set_slot_field("integration_lite", "length", "21").expect("configure");
    stack.set_slot_field("integration_lite", "line_color", "#B42020").expect("configure");
    stack.set_readouts(
        "integration_lite",
        vec![ReadoutValue { label: "fast".to_owned(), value: Some(42.0) }],
    )
    .expect("readouts set");
    assert!(stack.visible("integration_lite").expect("reads"), "slots start shown");
    stack.toggle_visibility("integration_lite").expect("eye toggles");
    assert!(!stack.visible("integration_lite").expect("reads"), "the eye hides instantly");
    stack.toggle_visibility("integration_lite").expect("eye toggles back");
    assert!(stack.visible("integration_lite").expect("reads"), "the eye shows again");
    // Every configured field and readout survives both flips.
    assert_eq!(stack.get_slot_field("integration_lite", "length").expect("reads"), "21");
    assert_eq!(
        stack.get_slot_field("integration_lite", "line_color").expect("reads"),
        "#B42020"
    );
    assert_eq!(
        stack.readouts("integration_lite").expect("reads"),
        &[ReadoutValue { label: "fast".to_owned(), value: Some(42.0) }],
        "readouts survive the toggle"
    );
    assert!(stack.toggle_visibility("missing").is_err(), "unknown slots refuse the eye");
}

#[test]
fn bolt_action_opens_isolated_tabbed_settings_modal() {
    let (mut stack, _) = stacked();
    assert!(!stack.settings_open("fast_mas"), "settings start closed");
    stack.open_settings("fast_mas").expect("bolt opens");
    assert!(stack.settings_open("fast_mas"), "the bolt opens fast_mas settings");
    assert!(!stack.settings_open("integration_lite"), "the other slot stays closed");
    // The modal cycles all three tabs on the open slot only.
    for tab in SettingsTab::all() {
        stack.set_settings_tab("fast_mas", tab).expect("tab selects");
        assert_eq!(stack.settings_tab("fast_mas"), Some(tab), "the tab holds");
    }
    assert_eq!(stack.settings_tab("integration_lite"), Some(SettingsTab::Inputs), "untouched");
    // Editing through the open slot never reaches the closed one.
    stack.set_slot_field("fast_mas", "length", "34").expect("edits");
    assert_eq!(stack.get_slot_field("fast_mas", "length").expect("reads"), "34");
    assert_eq!(stack.get_slot_field("integration_lite", "length").expect("reads"), "14");
    stack.close_settings("fast_mas").expect("bolt closes");
    assert!(!stack.settings_open("fast_mas"), "the bolt closes");
    assert!(stack.open_settings("missing").is_err(), "unknown slots refuse the bolt");
}

#[test]
fn removing_slot_fires_stream_abort_on_pane_handle() {
    let (mut stack, _) = stacked();
    let handle = stack.handle("fast_mas").expect("handle reads");
    assert!(!handle.is_closed(), "handles start live");
    stack.remove("fast_mas").expect("removal unslots");
    assert!(handle.is_closed(), "removal closes the pane handle");
    assert!(!stack.contains("fast_mas"), "the slot is gone");
    assert!(stack.contains("integration_lite"), "the other slot survives");
    assert!(stack.remove("fast_mas").is_err(), "double removal refuses");
}

#[test]
fn control_hit_test_distinguishes_eye_bolt_and_card_from_canvas() {
    let (stack, _) = stacked();
    let rects: Vec<SlotControlRect> = stack.control_rects();
    assert_eq!(rects.len(), 2, "every slot owns one control rect triple");
    let first: &HotSlot = stack.get(&rects[0].slot_id).expect("slotted");
    for rect in &rects {
        let eye = (rect.eye.center().x, rect.eye.center().y);
        assert_eq!(
            stack.hit_test_controls(eye),
            Some(SlotControlHit::Eye(rect.slot_id.clone())),
            "the eye zone names its slot"
        );
        let bolt = (rect.bolt.center().x, rect.bolt.center().y);
        assert_eq!(
            stack.hit_test_controls(bolt),
            Some(SlotControlHit::Bolt(rect.slot_id.clone())),
            "the bolt zone names its slot"
        );
        // Inside the card but clear of both controls: the card hit.
        let card = (rect.card.min.x + 5.0, rect.card.center().y);
        assert!(!rect.eye.contains(egui::Pos2::new(card.0, card.1)), "probe point is clear");
        assert_eq!(
            stack.hit_test_controls(card),
            Some(SlotControlHit::Card(rect.slot_id.clone())),
            "the card body names its slot"
        );
    }
    // The chart canvas answers no control: pointer events there reach the
    // candle annotation path untouched.
    assert_eq!(stack.hit_test_controls((1000.0, 800.0)), None, "canvas clicks pass through");
    assert_eq!(stack.hit_test_controls((-50.0, -50.0)), None, "off-stack clicks pass through");
    assert_eq!(first.title(), "Integration Lite", "the slot reads back");
}

/// The stack and its settings modal render headlessly: an untouched frame
/// raises no actions and refuses nothing, on every tab.
#[test]
fn paint_stack_and_settings_modal_render_headlessly() {
    let (mut stack, _) = stacked();
    let theme = ThemeConfig::default();
    egui::__run_test_ui(|ui| {
        let actions: Vec<HotSlotAction> =
            paint_hot_slot_stack(ui, &mut stack, &theme).expect("the stack paints");
        assert!(actions.is_empty(), "an untouched frame raises no actions: {actions:?}");
    });
    stack.open_settings("fast_mas").expect("bolt opens");
    for tab in SettingsTab::all() {
        stack.set_settings_tab("fast_mas", tab).expect("tab selects");
        egui::__run_test_ui(|ui| {
            let outcome = paint_slot_settings_modal(ui, &mut stack, "fast_mas", &theme)
                .expect("the modal paints");
            assert_eq!(
                outcome,
                SettingsOutcome::default(),
                "an untouched modal writes nothing and commits nothing"
            );
        });
    }
    egui::__run_test_ui(|ui| {
        assert!(
            paint_slot_settings_modal(ui, &mut stack, "integration_lite", &theme).is_err(),
            "a closed modal refuses instead of painting another slot's schema"
        );
    });
}

/// The staged settings buffer isolates a draft from the live config: edits
/// land in the map, the live config is untouched until commit, and a value
/// the field contract refuses never reaches the live config.
#[test]
fn staged_settings_buffer_isolates_edits_from_the_live_config() {
    let mut config: Box<dyn Component> = Box::new(probe(14));
    let mut pending = seed_fields(&*config);
    assert_eq!(pending.get("length").map(String::as_str), Some("14"));
    {
        let mut target = MapFields { values: &mut pending };
        target.set_field("length", "99").expect("a staged write accepts");
        assert_eq!(target.get_field("length").expect("reads"), "99");
    }
    assert_eq!(config.get_field("length").expect("reads"), "14", "the live config is untouched");
    for (name, value) in &pending {
        config.set_field(name, value).expect("valid staged values commit");
    }
    assert_eq!(config.get_field("length").expect("reads"), "99", "commit applies the draft");
    // An out-of-range staged value is refused at the field's own contract.
    let mut staged = seed_fields(&*config);
    staged.insert("length".to_owned(), "0".to_owned());
    let refused = staged.iter().any(|(name, value)| config.set_field(name, value).is_err());
    assert!(refused, "the field contract refuses a below-minimum staged value");
}

/// Open seeds the staged buffer and clears any other slot's; commit replays it
/// through the live contract and drops it; discard and close drop it untouched.
#[test]
fn settings_commit_discard_and_close_manage_the_staged_buffer() {
    let (mut stack, _) = stacked();
    assert!(!stack.has_pending_settings("integration_lite"));
    stack.open_settings("integration_lite").expect("opens");
    assert!(stack.has_pending_settings("integration_lite"), "open seeds the buffer");
    assert!(!stack.has_pending_settings("fast_mas"), "opening one clears the other");
    stack
        .slots
        .get_mut("integration_lite")
        .expect("slotted")
        .pending
        .as_mut()
        .expect("seeded")
        .insert("length".to_owned(), "42".to_owned());
    let refused = stack.commit_settings("integration_lite").expect("commit");
    assert!(refused.is_empty(), "a valid staged value commits cleanly: {refused:?}");
    assert_eq!(stack.get_slot_field("integration_lite", "length").expect("reads"), "42");
    assert!(!stack.has_pending_settings("integration_lite"), "commit drops the buffer");
    stack.open_settings("integration_lite").expect("reopens");
    stack.discard_settings("integration_lite").expect("discard drops the draft");
    assert!(!stack.has_pending_settings("integration_lite"));
    stack.open_settings("integration_lite").expect("reopens again");
    stack.close_settings("integration_lite").expect("close drops the draft");
    assert!(!stack.has_pending_settings("integration_lite"));
}

/// A candle whose wick spans `[low, high]` with a finite close at every bar.
fn wick_row(time: i64, high: f64, low: f64, close: f64) -> ChartRow {
    ChartRow { time, open: close, high, low, close, volume: 1_000 }
}

/// Calibration (task value: operator click disambiguation): a candle wick
/// lying directly under an eye or six-sided-bolt control is genuinely
/// annotatable there, yet the overlay gate intercepts every control click
/// before the canvas path, so no click behind the legend creates an
/// annotation.
#[test]
fn overlay_control_clicks_never_leak_to_canvas_annotation() {
    // A price pane whose wick band spans the whole control stack band, so a
    // pointer under any control names a real candle the canvas would
    // otherwise annotate.
    let pane = Viewport::new(PriceScaleMode::Normal, 90.0, 110.0, 400.0, 8.0, 90.0, false)
        .expect("valid pane");
    let rows: Vec<ChartRow> =
        (0..40).map(|i| wick_row(i64::from(i), 108.5, 104.0, 107.0)).collect();
    let (stack, _) = stacked();
    let mut false_annotations = 0_u32;
    for rect in stack.control_rects() {
        for point in [
            (rect.eye.center().x, rect.eye.center().y),
            (rect.bolt.center().x, rect.bolt.center().y),
            (rect.card.min.x + 5.0, rect.card.center().y),
        ] {
            // The control zone intercepts the pointer...
            assert!(
                stack.hit_test_controls(point).is_some(),
                "control zone at {point:?} intercepts the pointer"
            );
            // ...and a candle wick really is annotatable there, so the gate is
            // load-bearing rather than vacuous.
            assert!(
                annotate_at(point, &pane, &rows).is_some(),
                "a candle wick sits under the control at {point:?}"
            );
            // The shell routes a control hit away from the canvas: the canvas
            // path never runs, so no annotation is created behind the overlay.
            let routed = match stack.hit_test_controls(point) {
                Some(_) => None,
                None => annotate_at(point, &pane, &rows),
            };
            if routed.is_some() {
                false_annotations += 1;
            }
        }
    }
    assert_eq!(false_annotations, 0, "no control click leaks to a canvas annotation");
}

/// A stack of `count` slots, each with its own plan node.
fn many(count: usize) -> HotSlotStack {
    let mut plan = PanePlan::new();
    let mut stack = HotSlotStack::new();
    for index in 0..count {
        stack
            .insert(
                &format!("slot_{index}"),
                &format!("Slot {index}"),
                SlotAttachment::MainOverlay,
                Box::new(probe(8)),
                &[&format!("node_{index}")],
                &[],
                0xC0 + index as u64,
                &mut plan,
            )
            .expect("slot inserts");
    }
    stack
}

#[test]
fn content_height_counts_cards_and_gaps() {
    assert!(close_px(HotSlotStack::new().content_height(), 0.0), "an empty stack has no column");
    assert!(close_px(many(1).content_height(), STACK_CARD_HEIGHT), "one card is one card tall");
    let three = many(3).content_height();
    assert!(
        close_px(three, 2.0 * (STACK_CARD_HEIGHT + STACK_CARD_GAP) + STACK_CARD_HEIGHT),
        "three cards carry two gaps between them: {three}"
    );
}

#[test]
fn scrolling_stops_at_both_ends_of_the_column() {
    let mut stack = many(6);
    let viewport = 60.0;
    let slack = stack.content_height() - viewport;

    stack.scroll_by(1_000.0, viewport);
    assert!(close_px(stack.scroll_offset(), 0.0), "scrolling up past the first card clamps at the top");

    stack.scroll_by(-1_000.0, viewport);
    assert!(close_px(stack.scroll_offset(), slack), "scrolling down stops at the content end");

    // A viewport taller than the column leaves nothing to scroll.
    let mut tall = many(3);
    tall.scroll_by(-50.0, 10_000.0);
    assert!(close_px(tall.scroll_offset(), 0.0), "a column that fits never scrolls");
}

#[test]
fn scrolling_moves_cards_and_hit_tests_together() {
    let mut stack = many(6);
    stack.set_origin((100.0, 50.0));
    let viewport = 60.0;
    let second_before = stack
        .hit_test_controls((110.0, 50.0 + STACK_CARD_HEIGHT + STACK_CARD_GAP))
        .and_then(|hit| match hit {
            SlotControlHit::Card(id) => Some(id),
            _ => None,
        });
    assert_eq!(second_before.as_deref(), Some("slot_1"), "the second card starts below the first");

    stack.scroll_by(-(STACK_CARD_HEIGHT + STACK_CARD_GAP), viewport);
    assert!(
        close_px(stack.scroll_offset(), STACK_CARD_HEIGHT + STACK_CARD_GAP),
        "the wheel moved the column by exactly one card"
    );
    // The same point now names the card that scrolled up into it: painter and
    // hit-test share one layout truth.
    let second_after = stack
        .hit_test_controls((110.0, 50.0 + STACK_CARD_HEIGHT + STACK_CARD_GAP))
        .and_then(|hit| match hit {
            SlotControlHit::Card(id) => Some(id),
            _ => None,
        });
    assert_eq!(second_after.as_deref(), Some("slot_2"), "hit-testing follows the scroll");
    let rects = stack.control_rects();
    assert_eq!(rects.len(), 6, "scrolling never drops a card");
    assert!(
        close_px(rects[1].card.min.y, 50.0),
        "the second card scrolled up to the origin: {}",
        rects[1].card.min.y
    );
    assert!(
        close_px(rects[0].card.min.y, 50.0 - (STACK_CARD_HEIGHT + STACK_CARD_GAP)),
        "the first card moved up by exactly one slot: {}",
        rects[0].card.min.y
    );
}

#[test]
fn scrolling_refuses_a_non_finite_wheel() {
    let mut stack = many(6);
    stack.scroll_by(f32::NAN, 60.0);
    assert!(close_px(stack.scroll_offset(), 0.0), "a degenerate wheel holds the list");
}

#[test]
fn the_list_region_is_the_scrolling_column() {
    let mut stack = many(6);
    assert_eq!(
        HotSlotStack::new().list_rect(60.0),
        None,
        "an empty stack has no region to scroll"
    );
    stack.set_origin((100.0, 50.0));
    let rect = stack.list_rect(60.0).expect("a populated stack has a region");
    assert_eq!(rect.min, egui::pos2(100.0, 50.0), "the region starts at the stack origin");
    assert!(
        close_px(rect.height(), 60.0),
        "the region is the viewport, not the whole column: {}",
        rect.height()
    );
    // A viewport taller than the column clips the region to the column.
    let tall = stack.list_rect(10_000.0).expect("still a region");
    assert!(
        close_px(tall.height(), stack.content_height()),
        "the region never exceeds the cards it holds"
    );
    assert_eq!(stack.list_rect(0.0), None, "a zero-height viewport has nothing to scroll");
    assert_eq!(stack.list_rect(f32::NAN), None, "a degenerate viewport has nothing to scroll");
}

#[test]
fn removing_a_slot_reclamps_a_now_overshooting_scroll() {
    let mut stack = many(6);
    stack.set_origin((100.0, 50.0));
    stack.scroll_by(-1_000.0, 60.0);
    assert!(stack.scroll_offset() > 0.0, "the column scrolled to its end");
    let end = stack.scroll_offset();

    stack.remove("slot_5").expect("removes the last card");
    assert!(
        stack.scroll_offset() <= end,
        "removing a card pulls the scroll back inside the shorter column"
    );
    assert!(
        close_px(stack.scroll_offset(), stack.content_height().min(stack.scroll_offset())),
        "the offset stays a real scroll position"
    );
}

fn close_px(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() <= 1e-3 * expected.abs().max(1.0)
}
