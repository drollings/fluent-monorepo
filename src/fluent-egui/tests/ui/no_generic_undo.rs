// Compile-fail ratchet for the no-undo invariant: undo is concrete over
// `AnnotationSet`, and there is no generic history type — so an
// authoritative (journal/order) type can never gain undo by instantiation.
// If this file ever compiles, the invariant is broken and the milestone
// gate must fail until the generic path is removed.

#[allow(dead_code)]
struct JournalRow {
    seq: i64,
}

fn main() {
    let _history = fluent_egui::AnnotationHistory::<JournalRow>::default();
}
