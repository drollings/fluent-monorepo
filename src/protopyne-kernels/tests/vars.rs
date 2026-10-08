use super::*;

#[test]
fn var_persists_across_bars_while_varip_resets() {
    let mut store = VarStore::new();
    store.set_var("trend", PineValue::Float(1.5));
    store.set_varip("intra", PineValue::Int(7));

    store.next_bar();

    assert_eq!(store.get_var("trend"), Some(PineValue::Float(1.5)));
    assert_eq!(store.get_varip("intra"), None);

    store.set_varip("intra", PineValue::Int(8));
    store.next_bar();
    store.next_bar();
    assert_eq!(store.get_var("trend"), Some(PineValue::Float(1.5)));
    assert_eq!(store.get_varip("intra"), None);
}

#[test]
fn unknown_names_read_as_na() {
    let store = VarStore::new();
    assert_eq!(store.get_var("missing"), None);
    assert_eq!(store.get_varip("missing"), None);
}
