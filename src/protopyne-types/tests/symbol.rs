use super::*;
use fluent_wvr::validate::Validate;

#[test]
fn round_trip_intern_resolve() {
    let mut index = SymbolIndex::new();
    let first = index.intern("SPY").expect("intern SPY");
    let second = index.intern("SPY").expect("re-intern SPY");
    assert_eq!(first, second, "same spelling maps to one key");
    assert_eq!(index.resolve(first), Some("SPY"));
    assert_eq!(index.len(), 1);
    assert!(!index.is_empty());
}

#[test]
fn keys_are_copy() {
    let mut index = SymbolIndex::new();
    let key = index.intern("SPY").expect("intern SPY");
    let moved = key;
    assert_eq!(key.as_u32(), moved.as_u32());
}

#[test]
fn indexes_are_isolated() {
    let mut left = SymbolIndex::new();
    let mut right = SymbolIndex::new();
    left.intern("AAPL").expect("left interns AAPL");
    assert_eq!(
        right.lookup("AAPL"),
        None,
        "no cross-talk: right never saw AAPL"
    );
    // Identical spellings intern independently per index.
    let left_spy = left.intern("SPY").expect("left interns SPY");
    let right_spy = right.intern("SPY").expect("right interns SPY");
    assert_eq!(left.resolve(left_spy), Some("SPY"));
    assert_eq!(right.resolve(right_spy), Some("SPY"));
    assert_eq!(left.len(), 2);
    assert_eq!(right.len(), 1);
}

#[test]
fn lookup_never_inserts() {
    let index = SymbolIndex::new();
    assert_eq!(index.lookup("SPY"), None);
    assert!(index.is_empty(), "reads must not grow the table");
}

#[test]
fn empty_symbol_refused() {
    let mut index = SymbolIndex::new();
    assert!(index.intern("").is_err());
    assert!(index.is_empty());
}

#[test]
fn full_table_refused_fail_closed() {
    let mut index = SymbolIndex::with_max_symbols(2);
    index.intern("SPY").expect("first");
    index.intern("AAPL").expect("second");
    let err = index.intern("MSFT").expect_err("third must refuse");
    assert!(matches!(err, crate::error::TypesError::TooManySymbols(_)));
    // Re-interning a known spelling still works at capacity.
    assert!(index.intern("SPY").is_ok());
}

#[test]
fn index_validate_bounds() {
    assert!(Validate::validate(&SymbolIndex::new()).is_ok());
    assert!(Validate::validate(&SymbolIndex::with_max_symbols(0)).is_err());
}
