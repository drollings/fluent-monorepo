//! Shipped symbol catalog: the one instrument list both consumers derive from.
//!
//! The mock gateway serves it as wire rows; the terminal builds its Symbol
//! Selection Modal from the same rows. The class taxonomy lives here, not in
//! widget code, so the tabs cannot drift from the served list.
//!
//! [`discovered_symbol_rows`] turns ticker discovery (a server-side directory
//! read) into wire rows, keeping the shipped list as the offline fallback and
//! never a second truth.

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
    /// A ticker no shipped row and no sidecar declares. A real class, never a
    /// guess: an unknown instrument is shown as unknown.
    Unknown,
}

impl AssetClass {
    /// Every class, in tab order.
    #[must_use]
    pub fn all() -> [Self; 5] {
        [
            Self::Stocks,
            Self::Funds,
            Self::Futures,
            Self::Crypto,
            Self::Unknown,
        ]
    }

    /// Stable tag the wire rows and the modal tabs group by.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Stocks => "Stocks",
            Self::Funds => "Funds",
            Self::Futures => "Futures",
            Self::Crypto => "Crypto",
            Self::Unknown => "Unknown",
        }
    }

    /// Parse a wire class tag back to its class; an unrecognized tag is `None`
    /// so callers can refuse rather than guess.
    #[must_use]
    pub fn parse(tag: &str) -> Option<Self> {
        Self::all().into_iter().find(|class| class.label() == tag)
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

/// One ticker's declared identity, from a directory sidecar or an operator
/// manifest. `None` fields declare nothing: the caller falls back to the
/// shipped entry or to the ticker itself, never a guess.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InstrumentDecl {
    /// Declared display name.
    pub name: Option<String>,
    /// Declared asset class.
    pub class: Option<AssetClass>,
}

impl InstrumentDecl {
    /// A declaration naming a display name and/or an asset class.
    #[must_use]
    pub fn new(name: Option<String>, class: Option<AssetClass>) -> Self {
        Self { name, class }
    }
}

/// Wire rows for discovered `tickers`: the shipped entries keep their catalog
/// order first (so a root holding exactly the shipped tickers serves the
/// identical wire shape), then unmapped tickers sorted by spelling. The
/// shipped entry takes precedence over any declaration (the shipped list is
/// the offline fallback, not a second truth). An unmapped ticker displays as
/// itself and carries [`AssetClass::Unknown`] — never a guessed class. An
/// empty list yields an empty row list; the caller decides whether that
/// refuses.
#[must_use]
pub fn discovered_symbol_rows(
    tickers: &[String],
    declared: impl Fn(&str) -> Option<InstrumentDecl>,
) -> Vec<SymbolInfoView> {
    let present: std::collections::HashSet<&str> = tickers.iter().map(String::as_str).collect();
    let shipped_rows: Vec<SymbolInfoView> = SYMBOL_CATALOG
        .iter()
        .filter(|(symbol, ..)| present.contains(symbol))
        .map(|(symbol, name, class)| SymbolInfoView {
            symbol: (*symbol).to_owned(),
            name: (*name).to_owned(),
            asset_class: class.label().to_owned(),
        })
        .collect();
    let mut newcomers: Vec<&str> = tickers
        .iter()
        .map(String::as_str)
        .filter(|ticker| !SYMBOL_CATALOG.iter().any(|(symbol, ..)| symbol == ticker))
        .collect();
    newcomers.sort_unstable();
    newcomers.dedup();
    let newcomer_rows = newcomers.into_iter().map(|ticker| {
        let decl = declared(ticker).unwrap_or_default();
        SymbolInfoView {
            symbol: ticker.to_owned(),
            name: decl.name.unwrap_or_else(|| ticker.to_owned()),
            asset_class: decl.class.unwrap_or(AssetClass::Unknown).label().to_owned(),
        }
    });
    shipped_rows.into_iter().chain(newcomer_rows).collect()
}

/// The default-symbol rule every default picks applies: `preferred` when it is
/// present in `available`, else the first spelling in sorted order, else `None`
/// — the caller refuses with its own reason. Never a guess: an empty `available`
/// has no default.
#[must_use]
pub fn pick_default_symbol<'a>(
    preferred: &str,
    available: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    let available: Vec<&str> = available.into_iter().collect();
    if available.contains(&preferred) {
        return Some(preferred.to_owned());
    }
    let mut sorted = available;
    sorted.sort_unstable();
    sorted.first().map(|symbol| (*symbol).to_owned())
}

#[cfg(test)]
#[path = "../tests/symbols.rs"]
mod tests;
