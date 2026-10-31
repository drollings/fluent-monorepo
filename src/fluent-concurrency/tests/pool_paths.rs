//! Facade compatibility: every public item is reachable both through the
//! stable `pool::` facade path and through its owning leaf module, naming
//! the same type on both paths.

use fluent_concurrency::pool::{
    Limiter, PoolError, PriorityRequestPool, PriorityResultPool, Queue, ResultPool, ResultPoolError,
};

fn assert_same_type<T>(_: T, _: T) {}

#[test]
fn facade_and_leaf_name_the_same_types() {
    assert_same_type::<Queue<u8>>(
        Queue::new(1),
        fluent_concurrency::pool::queue::Queue::new(1),
    );
    assert_same_type::<Limiter>(
        Limiter::new(1),
        fluent_concurrency::pool::limiter::Limiter::new(1),
    );
    fn takes_pool_error(_: PoolError) {}
    fn takes_leaf_error(_: fluent_concurrency::pool::traits::PoolError) {}
    let check = takes_pool_error;
    let _: fn(PoolError) = check;
    let _: fn(fluent_concurrency::pool::traits::PoolError) = check;
    let _ = takes_leaf_error;
}

#[test]
fn result_pool_error_variants_match_across_paths() {
    let facade: ResultPoolError<String> = ResultPoolError::Canceled;
    let leaf: fluent_concurrency::pool::result::ResultPoolError<String> = ResultPoolError::Canceled;
    assert!(matches!(facade, ResultPoolError::Canceled));
    assert!(matches!(leaf, ResultPoolError::Canceled));
    let _ = (
        ResultPool::<u8, u8, String>::worker_count as fn(&ResultPool<u8, u8, String>) -> usize,
        PriorityResultPool::<u8, u8, String>::worker_count
            as fn(&PriorityResultPool<u8, u8, String>) -> usize,
    );
}
