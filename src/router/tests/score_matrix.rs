use super::*;

#[test]
fn score_matrix_resolve_is_clone_free() {
    let m = ScoreMatrix::default();
    let scores = HashMap::from([
        ("completeness".into(), 0.8),
        ("risk".into(), 0.6),
        ("coherence".into(), 0.9),
        ("complexity".into(), 0.7),
    ]);
    let a = m.resolve(&scores);
    let b = m.resolve(&scores);
    assert_eq!(a.len(), b.len());
    assert_eq!(a[0].route_name, b[0].route_name);
}

#[test]
fn score_matrix_default_is_router_default() {
    let a = ScoreMatrix::default();
    let b = ScoreMatrix::router_default();
    assert_eq!(a.dimensions(), b.dimensions());
    assert_eq!(a.weights(), b.weights());
    assert_eq!(a.routes().len(), b.routes().len());
}

#[test]
fn non_finite_scores_do_not_panic_and_treat_as_no_match() {
    let matrix = ScoreMatrix::default();
    let scores = HashMap::from([
        ("coherence".into(), f64::NAN),
        ("complexity".into(), f64::INFINITY),
        ("completeness".into(), 0.9),
        ("risk".into(), 0.1),
    ]);
    let results = matrix.resolve(&scores);
    assert!(!results.is_empty(), "routes should still resolve");
    for r in &results {
        assert!(
            r.weighted_score.is_finite(),
            "weighted score must stay finite, got {}",
            r.weighted_score
        );
    }
}

#[test]
fn router_route_kind_round_trips_through_display_and_fromstr() {
    use crate::score_matrix::RouterRouteKind;
    for kind in [
        RouterRouteKind::Plan,
        RouterRouteKind::Rigor,
        RouterRouteKind::Local,
    ] {
        assert_eq!(kind.as_str(), kind.to_string());
        assert_eq!(kind.as_str().parse::<RouterRouteKind>().unwrap(), kind);
    }
}

#[test]
fn router_route_kind_rejects_unknown_with_typed_error() {
    use crate::score_matrix::RouterRouteKind;
    let err = "charisma".parse::<RouterRouteKind>().unwrap_err();
    assert_eq!(
        err,
        crate::score_matrix::UnknownRouterRoute("charisma".into())
    );
}

#[test]
fn router_default_built_from_enum_matches_string_built_matrix() {
    use crate::score_matrix::RouterRouteKind;
    let m = ScoreMatrix::router_default();
    for kind in [
        RouterRouteKind::Plan,
        RouterRouteKind::Rigor,
        RouterRouteKind::Local,
    ] {
        assert!(m.routes().contains_key(kind.as_str()));
    }
    let scores = HashMap::from([
        ("completeness".into(), 0.8),
        ("risk".into(), 0.6),
        ("coherence".into(), 0.9),
        ("complexity".into(), 0.7),
    ]);
    let resolved: Vec<String> = m
        .resolve(&scores)
        .iter()
        .map(|r| r.route_name.clone())
        .collect();
    assert_eq!(resolved, vec!["rigor".to_string()]);
}
