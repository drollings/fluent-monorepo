use crate::compat::{assert_matrix_covers, compat_for, COMPATIBILITY};
use crate::component::ComponentRegistry;
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

#[test]
fn registry_matches_matrix_exactly_with_no_orphans() {
    // Count equality in both directions: every kernel has exactly one row
    // and no row survives its kernel. `assert_matrix_covers` proves the
    // mapping; the lengths prove there is no padding on either side.
    let registry = builtin_registry().expect("builtins register");
    assert_matrix_covers(&registry).expect("matrix covers registry");
    assert_eq!(
        registry.len(),
        COMPATIBILITY.len(),
        "registry length equals matrix length, no orphans either way"
    );
    let components = ComponentRegistry::from_builtins().expect("components load");
    assert_eq!(
        components.ids(),
        registry.ids(),
        "component registry mirrors the builtin registry exactly"
    );
}
