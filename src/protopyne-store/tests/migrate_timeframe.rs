//! The migration proof: an old file upgrades, a fresh file lands current.
//!
//! Schema version 2 adds the tape's spacing to the market row. A database
//! written before the column existed must upgrade with its rows reading as
//! minute tapes — the only spacing ever written — rather than refusing or
//! defaulting silently somewhere downstream. A fresh file lands at the current
//! version, pinned here so a new migration is a deliberate bump.

use super::*;
use rusqlite::params;

use crate::catalog::{SELECT_MARKET_SQL, market_of};

/// The V1 market table, frozen: every column the first migration created,
/// and nothing it did not. A test that built its "old file" from the live
/// DDL would pass against any schema; this one pins what V1 actually was.
const V1_MARKET_DDL: &str = "
CREATE TABLE market (
    symbol_id   INTEGER PRIMARY KEY,
    symbol      TEXT    NOT NULL,
    venue       TEXT    NOT NULL DEFAULT '',
    asset_class TEXT    NOT NULL,
    price_scale INTEGER NOT NULL,
    time_zone   TEXT    NOT NULL DEFAULT 'UTC',
    UNIQUE (symbol, venue)
);
";

#[test]
fn a_fresh_database_lands_at_the_current_version() {
    let conn = rusqlite::Connection::open_in_memory().expect("ephemeral database opens");
    assert_eq!(
        ensure_schema(&conn).expect("schema applies"),
        SCHEMA_VERSION
    );
    assert_eq!(SCHEMA_VERSION, 3, "the constant and the migrations agree");
}

#[test]
fn a_version_one_file_upgrades_with_minute_tapes() {
    let conn = rusqlite::Connection::open_in_memory().expect("ephemeral database opens");
    conn.execute_batch(V1_MARKET_DDL)
        .expect("old table creates");
    conn.execute(
        "INSERT INTO market(symbol, venue, asset_class, price_scale, time_zone)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params!["SPY", "test-venue", "stock", 2, "UTC"],
    )
    .expect("old row inserts");
    assert_eq!(
        ensure_schema(&conn).expect("schema upgrades"),
        SCHEMA_VERSION
    );
    let market = fluent_db::query::query_row(&conn, SELECT_MARKET_SQL, [], market_of)
        .expect("upgraded row reads")
        .expect("the row survived the upgrade");
    assert_eq!(market.symbol, "SPY");
    assert_eq!(
        market.timeframe,
        protopyne_types::Timeframe::M1,
        "pre-column rows hold the only spacing ever written"
    );
    assert_eq!(
        ensure_schema(&conn).expect("schema re-applies"),
        SCHEMA_VERSION,
        "migration is idempotent"
    );
}
