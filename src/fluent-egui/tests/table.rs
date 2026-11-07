use super::*;

fn planned() -> TablePlan {
    let mut plan = TablePlan::new(20.0).expect("valid row height");
    plan.register_column("time", 90.0).expect("registers");
    plan.register_column("close", 70.0).expect("registers");
    plan.register_column("volume", 60.0).expect("registers");
    plan
}

fn reference_window(
    total: usize,
    scroll_px: f32,
    height_px: f32,
    row_height_px: f32,
    overscan: usize,
) -> (usize, usize) {
    if total == 0
        || height_px.is_nan()
        || height_px <= 0.0
        || !row_height_px.is_finite()
        || row_height_px <= 0.0
        || !scroll_px.is_finite()
    {
        return (0, 0);
    }
    let scroll = scroll_px.max(0.0);
    let mut start = total;
    let mut end = 0;
    let mut any = false;
    for index in 0..total {
        let top = index as f32 * row_height_px;
        let bottom = top + row_height_px;
        let pad = overscan as f32 * row_height_px;
        if bottom > scroll - pad && top < scroll + height_px + pad {
            start = start.min(index);
            end = end.max(index + 1);
            any = true;
        }
    }
    if any {
        (start, end)
    } else {
        (total, total)
    }
}

#[test]
fn culling_matches_the_full_slice_window() {
    let mut state: u64 = 0x2545F4914F6CDD1D;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for row_height in [0.5f32, 1.0, 2.0, 7.25, 20.0] {
        let plan = TablePlan::new(row_height).expect("valid row height");
        for _ in 0..300 {
            let total = (next() % 500) as usize;
            let scroll = ((next() % 20_000) as f32) / 10.0 - 100.0;
            let height = ((next() % 5000) as f32) / 10.0;
            let (start, end) = plan.culled_rows(total, scroll, height);
            let (expect_start, expect_end) =
                reference_window(total, scroll, height, row_height, TABLE_OVERSCAN_ROWS);
            assert_eq!(
                (start, end),
                (expect_start, expect_end),
                "culled window matches the full slice (total={total}, scroll={scroll}, height={height}, row={row_height})"
            );
        }
    }
}

#[test]
fn sub_pixel_rows_never_fold() {
    let plan = TablePlan::new(0.5).expect("valid row height");
    let (start, end) = plan.culled_rows(100, 10.0, 20.0);
    let (expect_start, expect_end) = reference_window(100, 10.0, 20.0, 0.5, TABLE_OVERSCAN_ROWS);
    assert_eq!(
        (start, end),
        (expect_start, expect_end),
        "half-pixel rows still window row-for-row"
    );
    assert!(end - start > 40, "no folding collapses the window");
}

#[test]
fn sort_changes_never_refetch_rows() {
    let mut plan = planned();
    plan.select(3);
    plan.select(7);
    let before = plan.culled_rows(50, 100.0, 200.0);
    plan.set_sort(Some(("close".to_owned(), SortDirection::Descending)))
        .expect("known column sorts");
    assert_eq!(
        plan.culled_rows(50, 100.0, 200.0),
        before,
        "sorting reorders at paint time; the window never refetches"
    );
    assert!(
        plan.is_selected(3) && plan.is_selected(7),
        "selection survives a sort change"
    );
    plan.clear_sort();
    assert_eq!(
        plan.culled_rows(50, 100.0, 200.0),
        before,
        "clearing the sort restores the same window"
    );
}

#[test]
fn columns_register_validate_and_sort() {
    let mut plan = TablePlan::new(20.0).expect("valid row height");
    assert!(TablePlan::new(0.0).is_err(), "zero row height refuses");
    assert!(
        TablePlan::new(f32::NAN).is_err(),
        "non-finite row height refuses"
    );
    assert!(plan.register_column("", 10.0).is_err(), "blank ids refuse");
    assert!(
        plan.register_column("a", 0.0).is_err(),
        "zero width refuses"
    );
    plan.register_column("a", 10.0).expect("registers");
    assert!(
        plan.register_column("a", 10.0).is_err(),
        "duplicate ids refuse"
    );
    assert!(
        plan.set_sort(Some(("missing".to_owned(), SortDirection::Ascending)))
            .is_err(),
        "sorting an unknown column refuses"
    );
    assert!(
        plan.set_column_width("missing", 10.0).is_err(),
        "unknown columns refuse widths"
    );
    plan.set_column_width("a", 44.0).expect("width edits");
    assert_eq!(plan.column_width("a"), Some(44.0));
    plan.toggle(5);
    assert!(plan.is_selected(5), "toggle selects");
    plan.toggle(5);
    assert!(!plan.is_selected(5), "toggle unselects");
    plan.select(1);
    plan.select(2);
    plan.clear_selection();
    assert!(plan.selection().is_empty(), "clear empties");
}
