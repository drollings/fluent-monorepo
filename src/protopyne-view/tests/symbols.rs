use super::*;

#[test]
fn asset_classes_label_the_wire_tags() {
    assert_eq!(
        AssetClass::all().map(AssetClass::label),
        ["Stocks", "Funds", "Futures", "Crypto", "Unknown"]
    );
    for class in AssetClass::all() {
        assert!(!class.label().is_empty(), "no class ships a blank tab tag");
        assert_eq!(
            AssetClass::parse(class.label()),
            Some(class),
            "every label parses back to its class"
        );
    }
    assert_eq!(
        AssetClass::parse("stock"),
        None,
        "an unparsable tag is never guessed into a real class"
    );
    assert_eq!(AssetClass::parse(""), None);
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

#[test]
fn discovered_rows_keep_shipped_order_then_sorted_newcomers() {
    // Deliberately unsorted input: discovery order is the caller's, and the
    // builder must not care.
    let tickers = ["ZZZ", "SPY", "AAA", "QQQ"]
        .iter()
        .map(|ticker| (*ticker).to_owned())
        .collect::<Vec<String>>();
    let rows = discovered_symbol_rows(&tickers, |_| None);
    // Shipped rows keep their shipped order (the wire stays byte-identical
    // for a shipped root); newcomers follow sorted by ticker spelling.
    let ids: Vec<&str> = rows.iter().map(|row| row.symbol.as_str()).collect();
    assert_eq!(ids, ["SPY", "QQQ", "AAA", "ZZZ"]);
    let unknown = rows
        .iter()
        .find(|row| row.symbol == "AAA")
        .expect("AAA is discovered");
    assert_eq!(
        unknown.name, "AAA",
        "an undeclared ticker displays as itself"
    );
    assert_eq!(unknown.asset_class, "Unknown");
}

#[test]
fn discovered_rows_declare_identity_and_never_guess_a_class() {
    let tickers = ["AAA", "SPY"]
        .iter()
        .map(|ticker| (*ticker).to_owned())
        .collect::<Vec<String>>();
    let rows = discovered_symbol_rows(&tickers, |ticker| match ticker {
        "AAA" => Some(InstrumentDecl::new(
            Some("Acme Rentals".to_owned()),
            Some(AssetClass::Futures),
        )),
        // A declaration for a shipped ticker: shipped truth still wins.
        "SPY" => Some(InstrumentDecl::new(Some("Wrong".to_owned()), None)),
        _ => None,
    });
    let aaa = rows
        .iter()
        .find(|row| row.symbol == "AAA")
        .expect("AAA is listed");
    assert_eq!(aaa.name, "Acme Rentals");
    assert_eq!(aaa.asset_class, "Futures");
    let spy = rows
        .iter()
        .find(|row| row.symbol == "SPY")
        .expect("SPY is listed");
    assert_eq!(spy.name, "SPDR S&P 500 ETF Trust");
    assert_eq!(spy.asset_class, "Stocks");
}

#[test]
fn discovered_rows_of_every_shipped_ticker_are_the_shipped_wire() {
    // The backward-compat pin: a root holding exactly the shipped tickers
    // serves the identical wire the shipped catalog serves. If fallback and
    // discovery ever disagree on these rows, this fails.
    let tickers = SYMBOL_CATALOG
        .iter()
        .map(|(symbol, ..)| (*symbol).to_owned())
        .collect::<Vec<String>>();
    let discovered = discovered_symbol_rows(&tickers, |_| None);
    let shipped = symbol_catalog();
    assert_eq!(discovered, shipped);
    assert_eq!(
        serde_json::to_string(&discovered).expect("rows serialize"),
        serde_json::to_string(&shipped).expect("rows serialize"),
        "byte-identical wire shape, not just equal fields"
    );
}

#[test]
fn default_symbol_prefers_the_named_ticker_else_first_sorted_else_refuses() {
    assert_eq!(
        pick_default_symbol("SPY", ["GLD", "SPY"]),
        Some("SPY".to_owned()),
        "the preferred ticker is taken when present"
    );
    assert_eq!(
        pick_default_symbol("GLD", ["ZZZ", "AAA"]),
        Some("AAA".to_owned()),
        "an absent preference falls back to the first spelling in sorted order, never input order"
    );
    assert_eq!(
        pick_default_symbol("GLD", std::iter::empty::<&str>()),
        None,
        "empty availability refuses instead of inventing a default"
    );
}

#[test]
fn a_declared_class_tag_parses_and_an_unknown_row_class_round_trips() {
    assert_eq!(AssetClass::parse("Unknown"), Some(AssetClass::Unknown));
    assert_eq!(
        InstrumentDecl::new(None, Some(AssetClass::Crypto)),
        InstrumentDecl::new(None, Some(AssetClass::Crypto))
    );
}
