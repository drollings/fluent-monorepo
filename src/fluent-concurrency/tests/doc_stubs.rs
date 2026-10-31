//! Stale-name guard for the `fluent-concurrency` doc stubs. See the
//! `common-core` `doc_stubs` suite for the contract.

#[allow(unused_imports)]
use fluent_concurrency::batch::SupervisedBatch;
#[allow(unused_imports)]
use fluent_concurrency::credit_pool::CreditGatedPool;
#[allow(unused_imports)]
use fluent_concurrency::pool::{Limiter, ResultPool, WorkerPool};
#[allow(unused_imports)]
use fluent_concurrency::queue::PriorityQueue;
#[allow(unused_imports)]
use fluent_concurrency::scope::Scope;
#[allow(unused_imports)]
use fluent_concurrency::stream::StreamAbort;
#[allow(unused_imports)]
use fluent_concurrency::tokio_runtime;

const VISION: &str = include_str!("../../../doc/fluent-concurrency/VISION.md");
const ARCHITECTURE: &str = include_str!("../../../doc/fluent-concurrency/ARCHITECTURE.md");

#[test]
fn stubs_name_only_real_public_items() {
    for name in [
        "Scope",
        "SupervisedBatch",
        "WorkerPool",
        "ResultPool",
        "Limiter",
        "CreditGatedPool",
        "PriorityQueue",
        "StreamAbort",
        "tokio_runtime",
    ] {
        assert!(
            VISION.contains(name) || ARCHITECTURE.contains(name),
            "doc stub must keep naming `{name}`"
        );
    }
}
