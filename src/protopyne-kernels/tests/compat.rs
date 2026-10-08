use crate::compat::{assert_matrix_covers, compat_for};
use crate::indicator::builtin_registry;

#[test]
fn matrix_covers_builtins_with_positive_finite_eps() {
    let registry = builtin_registry().expect("builtins register");
    assert_matrix_covers(&registry).expect("matrix covers registry");
    for id in registry.ids() {
        let row = compat_for(&id).expect("row present");
        assert!(row.eps.is_finite() && row.eps > 0.0);
    }
}
