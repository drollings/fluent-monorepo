use std::cmp::Ordering;

use common_core::sort::{dedup_sorted, sorted_by_vec, sorted_vec};

#[test]
fn sorted_vec_matrix() {
    assert_eq!(sorted_vec(Vec::<i32>::new()), Vec::<i32>::new());
    assert_eq!(sorted_vec(vec![1]), vec![1]);
    assert_eq!(sorted_vec(vec![1, 2, 3]), vec![1, 2, 3]);
    assert_eq!(sorted_vec(vec![3, 2, 1]), vec![1, 2, 3]);
    // Duplicates survive a plain sort (dedup is `dedup_sorted`'s job).
    assert_eq!(sorted_vec(vec![2, 1, 2, 1]), vec![1, 1, 2, 2]);
    assert_eq!(
        sorted_vec(vec!["b".to_string(), "a".to_string()]),
        vec!["a".to_string(), "b".to_string()]
    );
}

#[test]
fn sorted_by_vec_length_and_stability() {
    // The `collapse_set` pattern: ascending length.
    assert_eq!(
        sorted_by_vec(
            vec!["bb".to_string(), "a".to_string(), "ccc".to_string()],
            |x, y| x.len().cmp(&y.len())
        ),
        vec!["a".to_string(), "bb".to_string(), "ccc".to_string()]
    );
    // Stable: equal keys keep insertion order.
    let order: Vec<(i32, &str)> = sorted_by_vec(
        vec![(1, "first"), (1, "second"), (0, "zero")],
        |x, y| x.0.cmp(&y.0),
    );
    assert_eq!(order, vec![(0, "zero"), (1, "first"), (1, "second")]);
    // Empty and single are fixed points.
    assert_eq!(
        sorted_by_vec(Vec::<i32>::new(), |x, y| x.cmp(y)),
        Vec::<i32>::new()
    );
    assert_eq!(sorted_by_vec(vec![7], |x, y| x.cmp(y)), vec![7]);
}

#[test]
fn dedup_sorted_matrix() {
    assert_eq!(dedup_sorted(Vec::<i32>::new()), Vec::<i32>::new());
    assert_eq!(dedup_sorted(vec![1]), vec![1]);
    assert_eq!(dedup_sorted(vec![2, 1, 2, 1, 3]), vec![1, 2, 3]);
    assert_eq!(dedup_sorted(vec![1, 2, 3]), vec![1, 2, 3]);
    assert_eq!(dedup_sorted(vec!["b", "a", "b"]), vec!["a", "b"]);
}

/// Documented recipe for the anchor pattern (score-descending, id
/// tiebreak, NaN compares `Equal`): shown here, owned by the scoring
/// call site — deliberately not a shared helper, so NaN/epsilon policy
/// never drifts into generic code.
fn sort_by_score_then_id<T>(values: Vec<T>, score: impl Fn(&T) -> f64, id: impl Fn(&T) -> &str) -> Vec<T> {
    sorted_by_vec(values, |a, b| {
        score(b)
            .partial_cmp(&score(a))
            .unwrap_or(Ordering::Equal)
            .then_with(|| id(a).cmp(id(b)))
    })
}

#[test]
fn recipe_score_desc_then_id_with_nan() {
    let items = vec![
        ("b".to_string(), 0.1),
        ("a".to_string(), 0.1),
        ("c".to_string(), 0.3),
        ("n2".to_string(), f64::NAN),
        ("n1".to_string(), f64::NAN),
    ];
    let ordered = sort_by_score_then_id(items, |(_, s)| *s, |(id, _)| id);
    let ids: Vec<&str> = ordered.iter().map(|(id, _)| id.as_str()).collect();
    // Highest score first; ties (including NaN∼Equal) break by id ascending.
    assert_eq!(ids[0], "c");
    assert_eq!(&ids[1..3], &["a", "b"]);
    assert_eq!(&ids[3..], &["n1", "n2"]);
}
