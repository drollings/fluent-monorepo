//! Live cert-sandbox tests for protopyne-venue.
//!
//! Compiled ONLY with the `live-ai` feature. Every test is `#[ignore]`d and
//! read-only (boot, login, query-absent, truth mirrors, read storm) — order
//! entry stays out of automation; the order-path soak is the scheduled
//! manual run with committed numbers (see `doc/protopyne-rs/RUNBOOKS.md`).
//! Runs only via `make venue-test-live`; skips cleanly without the env
//! contract (`TASTYTRADE_BASE_URL`, `TASTYTRADE_ACCOUNT`, `TASTYTRADE_LOGIN`,
//! `TASTYTRADE_PASSWORD`).

#![cfg(feature = "live-ai")]

#[path = "live/cert_sandbox.rs"]
mod cert_sandbox;
#[path = "live/soak.rs"]
mod soak;
