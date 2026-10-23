use super::*;

/// One cell identity for the store-level assertions below.
fn cell(decl: u32) -> ScopeKey {
    ScopeKey::new(Some("local".to_owned()), Vec::new(), NodeId::new(decl))
}

/// One cell identity reached through a call expression.
fn called_cell(node: u32, decl: u32) -> ScopeKey {
    ScopeKey::new(
        Some("local".to_owned()),
        vec![CallSite::new(Some("local".to_owned()), NodeId::new(node))],
        NodeId::new(decl),
    )
}

#[test]
fn var_persists_across_bars_while_varip_resets() {
    let mut store = VarStore::new();
    let trend = cell(1);
    let intra = cell(2);
    store.set_var(trend.clone(), "trend", PineValue::Float(1.5));
    store.set_varip(intra.clone(), "intra", PineValue::Int(7));

    store.next_bar();

    assert_eq!(store.get_var(&trend), Some(PineValue::Float(1.5)));
    assert_eq!(store.get_varip(&intra), None);

    store.set_varip(intra.clone(), "intra", PineValue::Int(8));
    store.next_bar();
    store.next_bar();
    assert_eq!(store.get_var(&trend), Some(PineValue::Float(1.5)));
    assert_eq!(store.get_varip(&intra), None);
}

#[test]
fn unknown_names_read_as_na() {
    let store = VarStore::new();
    assert_eq!(store.get_var(&cell(1)), None);
    assert_eq!(store.get_varip(&cell(1)), None);
}

#[test]
fn one_spelling_in_two_scopes_is_two_cells() {
    let mut store = VarStore::new();
    let outer = cell(1);
    let inner = called_cell(9, 2);
    store.set_var(outer.clone(), "x", PineValue::Float(1.0));
    store.set_var(inner.clone(), "x", PineValue::Float(2.0));

    assert_eq!(store.get_var(&outer), Some(PineValue::Float(1.0)));
    assert_eq!(store.get_var(&inner), Some(PineValue::Float(2.0)));

    // The inspection view holds one value per spelling and reports the
    // outermost cell, so a script-level binding is never masked by a
    // helper's local.
    assert_eq!(
        store.snapshot().get("x").cloned(),
        Some(Some(PineValue::Float(1.0)))
    );

    store.clear_var(&inner);
    assert_eq!(store.get_var(&inner), None);
    assert_eq!(store.get_var(&outer), Some(PineValue::Float(1.0)));
}
