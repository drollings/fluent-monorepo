use super::*;

/// Test-local naive mean used only to demonstrate what the warmup gate
/// measures. This helper exists solely so the CAL control below runs
/// against something shaped like a kernel output (leading `None` until
/// warmed).
fn naive_mean_for_control(values: &[f64], period: usize) -> Vec<Option<f64>> {
    values
        .iter()
        .enumerate()
        .map(|(i, _)| {
            if period == 0 || i + 1 < period {
                None
            } else {
                Some(values[i + 1 - period..=i].iter().sum::<f64>() / period as f64)
            }
        })
        .collect()
}

#[test]
fn adjust_expands_limit_by_warmup_minus_one() {
    let bounds = adjust_fetch_bounds(None, Some(100), Timeframe::M15, 14);
    assert_eq!(bounds.fetch_limit, Some(113));
    assert_eq!(bounds.original_limit, Some(100));
    assert_eq!(bounds.fetch_start_ms, None);
}

#[test]
fn adjust_backs_off_start_by_warmup_bars() {
    let start_ms = 1_700_000_000_000_i64;
    let bounds = adjust_fetch_bounds(Some(start_ms), None, Timeframe::M15, 14);
    assert_eq!(bounds.original_start_ms, Some(start_ms));
    assert_eq!(bounds.fetch_start_ms, Some(start_ms - 13 * 15 * 60_000));
    assert_eq!(bounds.fetch_limit, None);
}

#[test]
fn adjust_warmup_zero_or_one_adds_no_headroom() {
    for warmup in [0, 1] {
        let bounds = adjust_fetch_bounds(Some(1_000_000), Some(50), Timeframe::M1, warmup);
        assert_eq!(bounds.fetch_start_ms, Some(1_000_000), "warmup {warmup}");
        assert_eq!(bounds.fetch_limit, Some(50), "warmup {warmup}");
    }
}

#[test]
fn trim_of_adjust_returns_exactly_user_range() {
    for (limit, warmup) in [(10, 1), (10, 14), (50, 20), (1, 5), (100, 5), (30, 0)] {
        let bounds = adjust_fetch_bounds(None, Some(limit), Timeframe::M15, warmup);
        let fetched_len = bounds.fetch_limit.expect("limit set");
        let values: Vec<i32> = (0..fetched_len as i32).collect();
        let trimmed = trim_indicator_output(&values, warmup, Some(limit));
        assert_eq!(trimmed.len(), limit, "limit {limit} warmup {warmup}");
        let expected_start = fetched_len as i32 - limit as i32;
        assert_eq!(trimmed[0], expected_start);
    }
}

#[test]
fn cal_control_warmup_zero_leaves_pollution_visible() {
    let closes = [10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0];
    // No headroom fetched: the kernel output over the bare user range keeps
    // its leading `None`s after trimming — the pollution stays visible.
    let bare = naive_mean_for_control(&closes[..5], 3);
    let trimmed_bare = trim_indicator_output(&bare, 0, Some(5));
    assert_eq!(trimmed_bare.len(), 5);
    assert!(trimmed_bare[0].is_none());
    assert!(trimmed_bare[1].is_none());

    // Warmed fetch: headroom is fetched, the kernel warms up inside it, and
    // trimming removes exactly the headroom — every emitted value is real.
    let bounds = adjust_fetch_bounds(None, Some(5), Timeframe::M15, 3);
    let fetched_len = bounds.fetch_limit.expect("limit set");
    assert_eq!(fetched_len, 7);
    let warmed = naive_mean_for_control(&closes[..fetched_len], 3);
    let trimmed = trim_indicator_output(&warmed, 3, Some(5));
    assert_eq!(trimmed.len(), 5);
    assert!(trimmed.iter().all(Option::is_some));
}
