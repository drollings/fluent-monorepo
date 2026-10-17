//! Shipped symbol catalog: the one instrument list both consumers derive from.
//!
//! The mock gateway serves it as wire rows; the terminal builds its Symbol
//! Selection Modal from the same rows. The class taxonomy lives here, not in
//! widget code, so the tabs cannot drift from the served list.

use crate::views::SymbolInfoView;

/// Asset class behind the Symbol Selection Modal tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AssetClass {
    /// Equities and equity ETFs.
    Stocks,
    /// Funds and commodity trusts.
    Funds,
    /// Futures contracts.
    Futures,
    /// Digital assets.
    Crypto,
}

impl AssetClass {
    /// Every class, in tab order.
    #[must_use]
    pub fn all() -> [Self; 4] {
        [Self::Stocks, Self::Funds, Self::Futures, Self::Crypto]
    }

    /// Stable tag the wire rows and the modal tabs group by.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Stocks => "Stocks",
            Self::Funds => "Funds",
            Self::Futures => "Futures",
            Self::Crypto => "Crypto",
        }
    }
}

/// The shipped catalog, the terminal's default ticker first.
pub const SYMBOL_CATALOG: [(&str, &str, AssetClass); 5] = [
    ("GLD", "SPDR Gold Shares", AssetClass::Funds),
    ("SPY", "SPDR S&P 500 ETF Trust", AssetClass::Stocks),
    ("QQQ", "Invesco QQQ Trust", AssetClass::Stocks),
    ("BTC-USD", "Bitcoin", AssetClass::Crypto),
    ("ES", "E-mini S&P 500", AssetClass::Futures),
];

/// The catalog as wire rows, in catalog order.
#[must_use]
pub fn symbol_catalog() -> Vec<SymbolInfoView> {
    SYMBOL_CATALOG
        .iter()
        .map(|(symbol, name, class)| SymbolInfoView {
            symbol: (*symbol).to_owned(),
            name: (*name).to_owned(),
            asset_class: class.label().to_owned(),
        })
        .collect()
}

#[cfg(test)]
#[path = "../tests/symbols.rs"]
mod tests;
