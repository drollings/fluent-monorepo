//! Stale-name guard for the `fluent-wvr` doc stubs. See the `common-core`
//! `doc_stubs` suite for the contract: explicit imports (existence proofs)
//! plus stub-mention assertions.

#[allow(unused_imports)]
use fluent_wvr::capability::CapabilitySet;
#[allow(unused_imports)]
use fluent_wvr::ladder::first_accept_in_order;
#[allow(unused_imports)]
use fluent_wvr::runtime::Runtime;
#[allow(unused_imports)]
use fluent_wvr::store::OutputStore;
#[allow(unused_imports)]
use fluent_wvr::traits::{Component, Describable, FieldAccess, WorkUnit};
#[allow(unused_imports)]
use fluent_wvr::work::{WorkContext, WorkError, WorkOutput};
#[allow(unused_imports)]
use fluent_wvr::wrapper::Instrumented;

const VISION: &str = include_str!("../../../doc/fluent-wvr/VISION.md");
const ARCHITECTURE: &str = include_str!("../../../doc/fluent-wvr/ARCHITECTURE.md");

#[test]
fn stubs_name_only_real_public_items() {
    for name in [
        "Component",
        "WorkUnit",
        "FieldAccess",
        "Describable",
        "WorkContext",
        "WorkError",
        "WorkOutput",
        "OutputStore",
        "CapabilitySet",
        "Runtime",
        "Instrumented",
        "first_accept_in_order",
    ] {
        assert!(
            VISION.contains(name) || ARCHITECTURE.contains(name),
            "doc stub must keep naming `{name}`"
        );
    }
}
