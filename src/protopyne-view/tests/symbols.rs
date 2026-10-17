use super::*;

#[test]
fn asset_classes_label_the_wire_tags() {
    assert_eq!(
        AssetClass::all().map(AssetClass::label),
        ["Stocks", "Funds", "Futures", "Crypto"]
    );
    for class in AssetClass::all() {
        assert!(!class.label().is_empty(), "no class ships a blank tab tag");
    }
}

#[test]
fn catalog_rows_carry_their_class_tag() {
    let rows = symbol_catalog();
    assert_eq!(rows.len(), SYMBOL_CATALOG.len());
    for ((symbol, name, class), row) in SYMBOL_CATALOG.iter().zip(rows.iter()) {
        assert_eq!(row.symbol, *symbol);
        assert_eq!(row.name, *name);
        assert_eq!(row.asset_class, class.label());
    }
}
