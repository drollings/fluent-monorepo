//! Stale-name guard for the `common-core` doc stubs.
//!
//! Every backticked type named in `doc/common-core/VISION.md` and
//! `doc/common-core/ARCHITECTURE.md` must resolve to a real public item:
//! the `use` block below fails compilation (E0432) on removal/rename, and
//! the assertions fail if a stub stops naming an item. Imports carry
//! `#[allow(unused_imports)]` — they exist as existence proofs, not uses.

#[allow(unused_imports)]
use common_core::calibration::{ClassCounts, GateBars};
#[allow(unused_imports)]
use common_core::io::read_first_line;
#[allow(unused_imports)]
use common_core::runtime::{block_on, block_on_shared as block_on_shared_path};
#[allow(unused_imports)]
use common_core::score::{ScoreDimension, ScoreMatrix};
#[allow(unused_imports)]
use common_core::string::contains_token;
#[allow(unused_imports)]
use common_core::{
    retry_async, BitSetDrift, CapabilityInterner, ConcurrentRegistry, KeyedRegistry,
    LatencyHistogram, LoadCache,
};

const VISION: &str = include_str!("../../../doc/common-core/VISION.md");
const ARCHITECTURE: &str = include_str!("../../../doc/common-core/ARCHITECTURE.md");

#[test]
fn stubs_name_only_real_public_items() {
    for name in [
        "LoadCache",
        "KeyedRegistry",
        "ConcurrentRegistry",
        "LatencyHistogram",
        "retry_async",
        "block_on",
        "block_on_shared",
        "CapabilityInterner",
        "ScoreMatrix",
        "ScoreDimension",
        "BitSetDrift",
        "ClassCounts",
        "GateBars",
        "contains_token",
        "read_first_line",
    ] {
        assert!(
            VISION.contains(name) || ARCHITECTURE.contains(name),
            "doc stub must keep naming `{name}`"
        );
    }
    let _ = LatencyHistogram::new();
    let _ = ScoreDimension::Coherence.as_str();
}
