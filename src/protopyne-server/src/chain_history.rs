//! Historical option-chain ingest: file-backed per-bar chain quotes behind the
//! time-parameterized leg read.
//!
//! The prototype has no live chain feed, so history arrives as a CSV file:
//! one row per contract per bar timestamp, parsed into
//! [`ChainQuote`](protopyne_types::ChainQuote) values through the same
//! fail-closed row discipline as the bar ingest (every malformed row is a
//! [`ServerError::BadRow`](crate::error::ServerError) carrying its 1-based
//! line number; rows are never skipped silently). [`HistoricalChainSource`]
//! serves those quotes through
//! [`LegQuoteSource::leg_quote_at`](protopyne_venue::LegQuoteSource::leg_quote_at)
//! — the newest quote for a contract at-or-before the bar being decided — so
//! a replay prices legs as of its cursor with no lookahead.
//!
//! Credentials arrive as a config **file path**, never inline secrets: when
//! [`ChainFeedConfig::credentials_path`] names a file, it must exist at
//! load time (fail closed); the fetch path reads it. File ingest below is
//! the boot reader; fetching is an explicit call that lands the same shape,
//! never a boot side effect, and nothing here dials the network at boot.
//!
//! Scope rule: the gateway filters loaded chains to the replay window's
//! `[first_ts, last_ts]` — the user's chart range selects within loaded
//! history, and chain coverage of the full history covers every chart range.

use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use fluent_concurrency::io::net::NetCapability;
use rust_decimal::Decimal;

use protopyne_types::{ChainQuote, Greeks, Money, OptionContract, OptionKind};
use protopyne_venue::{ChainSnapshotSource, LegQuoteSource, Quote};

use crate::csv::{
    header_positions, optional_column, parse_decimal_field, parse_int_field, parse_timestamp_field,
};
use crate::error::ServerError;

/// Where historical chains come from and which credentials the fetch path
/// may read. Loaded from JSON with defaults for every field, so
/// older config files (which name no chain feed) keep loading with chains
/// absent — the gate stays exactly where it was.
#[derive(Debug, Clone, bon::Builder, serde::Serialize, serde::Deserialize)]
#[builder(start_fn = new)]
/// Where historical chains come from and which credentials the fetch path
/// may read. Loaded from JSON with defaults for every field, so
/// older config files (which name no chain feed) keep loading with chains
/// absent — the gate stays exactly where it was.
pub struct ChainFeedConfig {
    /// Chain history file path (CSV, one row per contract per bar). `None`
    /// means no chain data: every multi-leg proposal keeps refusing with the
    /// missing feed named.
    #[serde(default)]
    pub chain_data_path: Option<String>,
    /// Credentials file path for the fetch path. `None` until the operator
    /// provisions one; a named-but-missing file refuses at load time rather
    /// than failing mid-fetch.
    #[serde(default)]
    pub credentials_path: Option<String>,
}

impl Default for ChainFeedConfig {
    fn default() -> Self {
        Self::new().build()
    }
}

impl fluent_wvr::validate::Validate for ChainFeedConfig {
    type Err = ServerError;
    /// Fail-closed self-consistency check: blank paths refuse, and a named
    /// credentials file must exist — a fetch that cannot authenticate must
    /// never start.
    fn validate(&self) -> Result<(), Self::Err> {
        if self
            .chain_data_path
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(ServerError::Backtest(
                "chain_data_path must not be blank".to_owned(),
            ));
        }
        let Some(path) = &self.credentials_path else {
            return Ok(());
        };
        if path.trim().is_empty() {
            return Err(ServerError::Backtest(
                "credentials_path must not be blank".to_owned(),
            ));
        }
        if !Path::new(path).exists() {
            return Err(ServerError::Backtest(format!(
                "credentials file missing: {path}"
            )));
        }
        Ok(())
    }
}

impl ChainFeedConfig {
    /// Load from `path`, or default (chains absent) when missing or broken —
    /// the shared loader warns loudly on broken files, and absence is the
    /// safe default the gate already handles.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        common_core::config::load_json_or_default(path)
    }
}

/// Required columns after lowercasing the header row: the contract identity
/// (`ts`, `underlying`, `expiry`, `strike`, `kind`) plus the touch
/// (`bid`, `ask`). Sizes, open interest, IV, and greeks default when absent.
const REQUIRED_CHAIN_COLUMNS: [&str; 7] =
    ["ts", "underlying", "expiry", "strike", "kind", "bid", "ask"];

fn parse_chain_decimal(field: &str, line: usize, column: &str) -> Result<Decimal, ServerError> {
    parse_decimal_field(field, line, column)
}

fn parse_chain_ts(field: &str, line: usize) -> Result<DateTime<Utc>, ServerError> {
    parse_timestamp_field(field, line)
}

fn parse_chain_expiry(field: &str, line: usize) -> Result<NaiveDate, ServerError> {
    NaiveDate::parse_from_str(field.trim(), "%Y-%m-%d").map_err(|_| ServerError::BadRow {
        line,
        reason: format!("invalid expiry (YYYY-MM-DD): {field:?}"),
    })
}

fn parse_chain_kind(field: &str, line: usize) -> Result<OptionKind, ServerError> {
    match field.trim().to_lowercase().as_str() {
        "call" | "c" => Ok(OptionKind::Call),
        "put" | "p" => Ok(OptionKind::Put),
        _ => Err(ServerError::BadRow {
            line,
            reason: format!("invalid kind (call/put): {field:?}"),
        }),
    }
}

fn parse_chain_u32(field: &str, line: usize, column: &str) -> Result<u32, ServerError> {
    parse_int_field(field, line, column)
}

fn parse_chain_u64(field: &str, line: usize, column: &str) -> Result<u64, ServerError> {
    parse_int_field(field, line, column)
}

fn parse_chain_f64(field: &str, line: usize, column: &str) -> Result<f64, ServerError> {
    field.parse::<f64>().map_err(|_| ServerError::BadRow {
        line,
        reason: format!("invalid number in column '{column}': {field:?}"),
    })
}

/// Parse chain CSV content into point-in-time contract quotes, in file order.
/// Empty trailing fields read as absent (sizes/OI default to zero, IV and a
/// missing greek column to `None`); a non-empty field that will not parse
/// refuses with the line number. Greeks parse as a unit: all four columns
/// present means all four cells must parse — a blank or malformed cell
/// refuses rather than inventing `0.0` for one leg of the set. Contract and
/// quote validation run per row through the types' own validators — ingest
/// never invents a quote the vocabulary would refuse.
pub fn parse_chain_csv(content: &str) -> Result<Vec<ChainQuote>, ServerError> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .trim(csv::Trim::All)
        .from_reader(content.as_bytes());

    let headers = reader
        .headers()
        .map_err(|e| ServerError::BadHeader(format!("cannot read header row: {e}")))?;
    let positions = header_positions(headers, &REQUIRED_CHAIN_COLUMNS)?;
    let (ts_c, und_c, exp_c, strike_c, kind_c, bid_c, ask_c) = (
        positions[0],
        positions[1],
        positions[2],
        positions[3],
        positions[4],
        positions[5],
        positions[6],
    );
    // Optional columns resolve once, before the row loop: the header borrow
    // must end before `records()` takes the reader mutably.
    let (bid_size_c, ask_size_c, oi_c, iv_c, delta_c, gamma_c, theta_c, vega_c) = (
        optional_column(headers, "bid_size"),
        optional_column(headers, "ask_size"),
        optional_column(headers, "open_interest"),
        optional_column(headers, "iv"),
        optional_column(headers, "delta"),
        optional_column(headers, "gamma"),
        optional_column(headers, "theta"),
        optional_column(headers, "vega"),
    );

    let mut quotes = Vec::new();
    for (row_idx, record) in reader.records().enumerate() {
        let line = row_idx + 2;
        let record = record.map_err(|e| ServerError::BadRow {
            line,
            reason: format!("unreadable row: {e}"),
        })?;
        let field = |index: usize| record.get(index).unwrap_or("").trim();
        let contract = OptionContract::new()
            .underlying(field(und_c).to_owned())
            .expiry(parse_chain_expiry(field(exp_c), line)?)
            .strike(Money::new(parse_chain_decimal(
                field(strike_c),
                line,
                "strike",
            )?))
            .kind(parse_chain_kind(field(kind_c), line)?)
            .build();
        contract.validate().map_err(|e| ServerError::BadRow {
            line,
            reason: format!("invalid contract: {e}"),
        })?;
        let bid = Money::new(parse_chain_decimal(field(bid_c), line, "bid")?);
        let ask = Money::new(parse_chain_decimal(field(ask_c), line, "ask")?);
        let bid_size = bid_size_c.map_or(Ok(0), |c| {
            let raw = field(c);
            if raw.is_empty() {
                Ok(0)
            } else {
                parse_chain_u32(raw, line, "bid_size")
            }
        })?;
        let ask_size = ask_size_c.map_or(Ok(0), |c| {
            let raw = field(c);
            if raw.is_empty() {
                Ok(0)
            } else {
                parse_chain_u32(raw, line, "ask_size")
            }
        })?;
        let open_interest = oi_c.map_or(Ok(0), |c| {
            let raw = field(c);
            if raw.is_empty() {
                Ok(0)
            } else {
                parse_chain_u64(raw, line, "open_interest")
            }
        })?;
        // NOT-A-LADDER: optional-column parse — absent/empty reads as none,
        // malformed refuses; no candidate walk, no skip/stop verdicts.
        let iv = iv_c.map_or(Ok(None), |c| {
            let raw = field(c);
            if raw.is_empty() {
                Ok(None)
            } else {
                parse_chain_f64(raw, line, "iv").map(Some)
            }
        })?;
        let greeks = match (delta_c, gamma_c, theta_c, vega_c) {
            (Some(d), Some(g), Some(t), Some(v)) => {
                let (rd, rg, rt, rv) = (field(d), field(g), field(t), field(v));
                if rd.is_empty() && rg.is_empty() && rt.is_empty() && rv.is_empty() {
                    None
                } else {
                    // A present set parses whole: any blank or malformed cell
                    // refuses with its column named, so no leg of the set is
                    // ever an invented zero.
                    Some(
                        Greeks::new()
                            .delta(parse_chain_f64(rd, line, "delta")?)
                            .gamma(parse_chain_f64(rg, line, "gamma")?)
                            .theta(parse_chain_f64(rt, line, "theta")?)
                            .vega(parse_chain_f64(rv, line, "vega")?)
                            .build(),
                    )
                }
            }
            _ => None,
        };
        let quote = ChainQuote::new()
            .contract(contract)
            .bid(bid)
            .ask(ask)
            .bid_size(bid_size)
            .ask_size(ask_size)
            .open_interest(open_interest)
            .maybe_iv(iv)
            .maybe_greeks(greeks)
            .timestamp(parse_chain_ts(field(ts_c), line)?)
            .build();
        quote.validate().map_err(|e| ServerError::BadRow {
            line,
            reason: format!("invalid chain quote: {e}"),
        })?;
        quotes.push(quote);
    }
    Ok(quotes)
}

/// Load a chain history file: missing path refuses without reading, so an
/// unconfigured feed never touches the filesystem.
pub fn load_chain_history(path: &Path) -> Result<Vec<ChainQuote>, ServerError> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        ServerError::Backtest(format!("chain history unreadable: {}: {e}", path.display()))
    })?;
    parse_chain_csv(&content)
}

/// Point-in-time chain reader over loaded history: the newest quote for a
/// contract at-or-before the instant being decided. Quotes are held sorted
/// by `(contract, timestamp)` at construction, so every read is a bounded
/// scan with no lookahead — a bar never sees a quote stamped after it.
pub struct HistoricalChainSource {
    quotes: Vec<ChainQuote>,
}

impl HistoricalChainSource {
    /// Index `quotes` for point-in-time reads. Empty history is legal here
    /// (every read answers `None`); the gateway decides whether an empty
    /// window keeps the gate or refuses the open.
    #[must_use]
    pub fn new(mut quotes: Vec<ChainQuote>) -> Self {
        quotes.sort_by(|a, b| {
            a.contract
                .occ_symbol()
                .cmp(&b.contract.occ_symbol())
                .then(a.timestamp.cmp(&b.timestamp))
        });
        Self { quotes }
    }

    /// Quotes in the file, in indexed order.
    #[must_use]
    pub fn quotes(&self) -> &[ChainQuote] {
        &self.quotes
    }

    /// Whether any quote is held at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.quotes.is_empty()
    }

    /// Narrow to the replay window: quotes stamped within `[start, end]`.
    /// The gateway calls this with the history's first/last bar timestamps,
    /// so chain coverage follows the user's chart range without ever loading
    /// a wider window per session.
    #[must_use]
    pub fn window(&self, start: DateTime<Utc>, end: DateTime<Utc>) -> Self {
        Self::new(
            self.quotes
                .iter()
                .filter(|quote| quote.timestamp >= start && quote.timestamp <= end)
                .cloned()
                .collect(),
        )
    }

    fn newest_at_or_before(
        &self,
        contract: &OptionContract,
        at: DateTime<Utc>,
    ) -> Option<&ChainQuote> {
        let occ = contract.occ_symbol();
        self.quotes
            .iter()
            .rfind(|quote| quote.contract.occ_symbol() == occ && quote.timestamp <= at)
    }
}

fn quote_of(chain: &ChainQuote) -> Quote {
    Quote::new()
        .bid(chain.bid)
        .ask(chain.ask)
        .bid_size(u64::from(chain.bid_size))
        .ask_size(u64::from(chain.ask_size))
        .ts(chain.timestamp)
        .build()
}

impl LegQuoteSource for HistoricalChainSource {
    fn leg_quote(&self, contract: &OptionContract) -> Option<Quote> {
        let occ = contract.occ_symbol();
        self.quotes
            .iter()
            .rfind(|quote| quote.contract.occ_symbol() == occ)
            .map(quote_of)
    }

    fn leg_quote_at(&self, contract: &OptionContract, at: DateTime<Utc>) -> Option<Quote> {
        self.newest_at_or_before(contract, at).map(quote_of)
    }

    /// The screen's question without the window's clone: a short-circuiting
    /// scan over the indexed quotes.
    fn covers(&self, underlying: &str, at: DateTime<Utc>) -> bool {
        self.quotes
            .iter()
            .any(|quote| quote.contract.underlying == underlying && quote.timestamp <= at)
    }

    fn chain_snapshot(&self, underlying: &str, at: DateTime<Utc>) -> Vec<ChainQuote> {
        self.quotes
            .iter()
            .filter(|quote| quote.contract.underlying == underlying && quote.timestamp <= at)
            .cloned()
            .collect()
    }
}

/// Shareable handle form: sessions install one `Arc` on their venue.
#[must_use]
/// Shareable handle form: sessions install one `Arc` on their venue.
pub fn shared_chain_source(quotes: Vec<ChainQuote>) -> Arc<HistoricalChainSource> {
    Arc::new(HistoricalChainSource::new(quotes))
}

/// Read the opaque fetch token from a credentials file (`{"token": "..."}`).
/// The file must be owner-only (no group/other permission bits) — a
/// world-readable secret refuses before it is read — and the token never
/// reaches any error string: refusals name the failure, never the secret.
pub fn read_fetch_token(credentials_path: &str) -> Result<String, ServerError> {
    let path = Path::new(credentials_path);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(path).map_or(0, |metadata| metadata.permissions().mode());
        if mode & 0o077 != 0 {
            return Err(ServerError::Backtest(format!(
                "credentials file must be owner-only: {}",
                path.display()
            )));
        }
    }
    let content = std::fs::read_to_string(path).map_err(|_| {
        ServerError::Backtest(format!("credentials file unreadable: {}", path.display()))
    })?;
    let wire: serde_json::Value = serde_json::from_str(&content)
        .map_err(|_| ServerError::Backtest("credentials file is not valid JSON".to_owned()))?;
    let token = wire
        .get("token")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            ServerError::Backtest("credentials file holds no usable token".to_owned())
        })?;
    Ok(token.to_owned())
}

/// Render quotes as exactly the CSV shape [`parse_chain_csv`] reads, so
/// fetch output and hand-written fixtures are interchangeable. Greeks ride
/// as four columns (empty when absent); the reader treats all-empty as
/// absent, so the shape round-trips losslessly.
#[must_use]
/// Render quotes as exactly the CSV shape [`parse_chain_csv`] reads, so
/// fetch output and hand-written fixtures are interchangeable. Greeks ride
/// as four columns (empty when absent); the reader treats all-empty as
/// absent, so the shape round-trips losslessly.
pub fn chain_quotes_to_csv(quotes: &[ChainQuote]) -> String {
    let mut csv =
        "ts,underlying,expiry,strike,kind,bid,ask,bid_size,ask_size,open_interest,iv,delta,gamma,theta,vega\n"
            .to_owned();
    for quote in quotes {
        use std::fmt::Write as _;
        let contract = &quote.contract;
        let optional = |value: Option<f64>| value.map_or_else(String::new, |v| v.to_string());
        let (delta, gamma, theta, vega) = quote.greeks.map_or_else(
            || (String::new(), String::new(), String::new(), String::new()),
            |greeks| {
                (
                    greeks.delta.to_string(),
                    greeks.gamma.to_string(),
                    greeks.theta.to_string(),
                    greeks.vega.to_string(),
                )
            },
        );
        let _ = writeln!(
            csv,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            quote.timestamp.timestamp(),
            contract.underlying,
            contract.expiry.format("%Y-%m-%d"),
            contract.strike.as_decimal(),
            contract.kind,
            quote.bid.as_decimal(),
            quote.ask.as_decimal(),
            quote.bid_size,
            quote.ask_size,
            quote.open_interest,
            optional(quote.iv),
            delta,
            gamma,
            theta,
            vega,
        );
    }
    csv
}

/// Fetch one chain snapshot for `underlying` and land it at `out_path` as
/// the CSV shape the boot loader reads. Credentials resolve before anything
/// dials; the fetched quotes re-parse through [`parse_chain_csv`] before
/// the write, so a snapshot the reader would refuse never lands; the file
/// is written once, atomically, on full success — a partial window refuses
/// with no file left behind.
pub async fn fetch_and_write_chain_history(
    net: &NetCapability,
    transport: &dyn ChainSnapshotSource,
    credentials_path: &str,
    underlying: &str,
    out_path: &Path,
) -> Result<usize, ServerError> {
    let token = read_fetch_token(credentials_path)?;
    let quotes = transport
        .fetch_chain_snapshot(net, underlying, &token)
        .await
        .map_err(|error| ServerError::Backtest(format!("chain fetch refused: {error}")))?;
    if quotes.is_empty() {
        return Err(ServerError::Backtest(
            "chain fetch returned no quotes: refusing a partial window".to_owned(),
        ));
    }
    let csv = chain_quotes_to_csv(&quotes);
    parse_chain_csv(&csv).map_err(|error| {
        ServerError::Backtest(format!("fetched chains fail the landed shape: {error}"))
    })?;
    common_core::io::write_atomic(out_path, csv.as_bytes())?;
    Ok(quotes.len())
}

#[cfg(test)]
#[path = "../tests/chain_history.rs"]
mod tests;