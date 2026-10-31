//! Stale-name guard for the `fluent-db` doc stubs. See the `common-core`
//! `doc_stubs` suite for the contract: explicit imports (existence proofs)
//! plus stub-mention assertions.

#[allow(unused_imports)]
use fluent_db::cache::TtlCache;
#[allow(unused_imports)]
use fluent_db::capability::DbCapability;
#[allow(unused_imports)]
use fluent_db::hnsw::HnswIndex;
#[allow(unused_imports)]
use fluent_db::pool::SqlitePool;
#[allow(unused_imports)]
use fluent_db::query::query_row;
#[allow(unused_imports)]
use fluent_db::store::SqliteStore;

const VISION: &str = include_str!("../../../doc/db/VISION.md");
const ARCHITECTURE: &str = include_str!("../../../doc/db/ARCHITECTURE.md");

#[test]
fn stubs_name_only_real_public_items() {
    for name in [
        "SqlitePool",
        "SqliteStore",
        "DbCapability",
        "query_row",
        "TtlCache",
        "HnswIndex",
    ] {
        assert!(
            VISION.contains(name) || ARCHITECTURE.contains(name),
            "doc stub must keep naming `{name}`"
        );
    }
}
