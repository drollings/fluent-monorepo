This directory holds third-party export artifacts: one JSON document per
indicator, carrying `source`, `source_version`, `exported_at`, `symbol`,
`timeframe`, `script_sha256`, and the `rows`.

Each file lands here through an operator TradingView run and is loaded by
`protopyne_kernels::external::load_external`, which refuses any document
missing provenance or whose hash is not the script it names. A committed
file may then back an `ExternalParityVerified` row via
`compat::external_evidence`.

Fixtures for the loader's refusal and comparison pins stay inline in
`tests/external.rs` and are never committed here: nothing in this
directory may be mistaken for third-party numbers.
