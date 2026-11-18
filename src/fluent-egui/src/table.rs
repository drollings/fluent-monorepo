//! Virtualized table plan: column registry, sort state, sparse selection,
//! and a headless row window.
//!
//! A [`TablePlan`] owns everything a data table needs except the rows
//! themselves: which columns exist and how wide they paint, the active sort
//! tuple, and the sparse set of selected row indices. Rows never enter the
//! plan — sorting reorders at paint time and the culled window borrows the
//! caller's slice — so a sort change recomputes widths only and never
//! refetches. Column compute nodes ride the caller's [`PanePlan`](crate::plan::PanePlan),
//! exactly like hot slots: there is no second DAG here.
//!
//! [`TablePlan::culled_rows`] is the O(visible) window behind the body
//! painter: the same rows the full slice would show, computed from scroll
//! and height without scanning history. Rows never fold (there is no
//! sub-pixel aggregation for text rows), so the window always matches the
//! naive full-slice scan, whatever the row height.
//!
//! Emitter guidance: paint the header from [`columns`](Self::columns), the
//! body from the caller's rows sliced to the culled window, and route every
//! refusal through [`paint_error_chip`](crate::paint::paint_error_chip) —
//! never a blank tile.

use std::collections::BTreeSet;

use crate::error::EguiError;

/// Rows of overscan kept above and below the visible window, mirroring the
/// candle cull: a row half-scrolled off the edge still paints, so a hard
/// clamp would flicker edge rows while scrolling.
const TABLE_OVERSCAN_ROWS: usize = 4;

/// Sort direction of the table's sort tuple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    /// Smallest values first.
    Ascending,
    /// Largest values first.
    Descending,
}

/// One registered column: its stable id and paint width.
#[derive(Debug, Clone, PartialEq)]
pub struct ColWidth {
    /// Stable column key; also the sort key.
    pub column: String,
    /// Paint width in pixels.
    pub width_px: f32,
}

/// A table's display plan: column registry, sort tuple, sparse selection,
/// and the row height the cull windows through.
#[derive(Debug, Clone)]
pub struct TablePlan {
    columns: Vec<ColWidth>,
    sort: Option<(String, SortDirection)>,
    selection: BTreeSet<usize>,
    row_height_px: f32,
}

impl TablePlan {
    /// Plan an empty table with fixed row heights; refuses non-positive and
    /// non-finite heights.
    pub fn new(row_height_px: f32) -> Result<Self, EguiError> {
        if !row_height_px.is_finite() || row_height_px <= 0.0 {
            return Err(EguiError::InvalidParam {
                what: "row_height_px".to_owned(),
                reason: "table rows need a positive finite height".to_owned(),
            });
        }
        Ok(Self {
            columns: Vec::new(),
            sort: None,
            selection: BTreeSet::new(),
            row_height_px,
        })
    }

    /// Fixed row height in pixels.
    #[must_use]
    pub fn row_height_px(&self) -> f32 {
        self.row_height_px
    }

    /// Register one column; refuses blank ids, duplicates, and
    /// non-positive or non-finite widths.
    pub fn register_column(&mut self, id: &str, width_px: f32) -> Result<(), EguiError> {
        if id.trim().is_empty() {
            return Err(EguiError::InvalidParam {
                what: "column".to_owned(),
                reason: "column id must not be empty".to_owned(),
            });
        }
        if self.columns.iter().any(|known| known.column == id) {
            return Err(EguiError::InvalidParam {
                what: id.to_owned(),
                reason: "duplicate column id".to_owned(),
            });
        }
        if !width_px.is_finite() || width_px <= 0.0 {
            return Err(EguiError::InvalidParam {
                what: id.to_owned(),
                reason: "column width must be positive and finite".to_owned(),
            });
        }
        self.columns.push(ColWidth {
            column: id.to_owned(),
            width_px,
        });
        Ok(())
    }

    /// Registered columns in registration order.
    #[must_use]
    pub fn columns(&self) -> &[ColWidth] {
        &self.columns
    }

    /// One column's paint width (`None` for unknown ids).
    #[must_use]
    pub fn column_width(&self, id: &str) -> Option<f32> {
        self.columns
            .iter()
            .find(|known| known.column == id)
            .map(|known| known.width_px)
    }

    /// Resize one column; refuses unknown ids and bad widths.
    pub fn set_column_width(&mut self, id: &str, width_px: f32) -> Result<(), EguiError> {
        if !width_px.is_finite() || width_px <= 0.0 {
            return Err(EguiError::InvalidParam {
                what: id.to_owned(),
                reason: "column width must be positive and finite".to_owned(),
            });
        }
        let column = self
            .columns
            .iter_mut()
            .find(|known| known.column == id)
            .ok_or_else(|| EguiError::InvalidParam {
                what: id.to_owned(),
                reason: "unknown column id".to_owned(),
            })?;
        column.width_px = width_px;
        Ok(())
    }

    /// Active sort tuple (`None` means registration order).
    #[must_use]
    pub fn sort(&self) -> Option<&(String, SortDirection)> {
        self.sort.as_ref()
    }

    /// Sort by one registered column; refuses unknown columns. Sorting only
    /// retags the display order — the culled window and the selection below
    /// are untouched, so no rows refetch.
    pub fn set_sort(&mut self, sort: Option<(String, SortDirection)>) -> Result<(), EguiError> {
        if let Some((column, _)) = &sort {
            if !self.columns.iter().any(|known| &known.column == column) {
                return Err(EguiError::InvalidParam {
                    what: column.clone(),
                    reason: "unknown column id".to_owned(),
                });
            }
        }
        self.sort = sort;
        Ok(())
    }

    /// Clear the sort back to registration order.
    pub fn clear_sort(&mut self) {
        self.sort = None;
    }

    /// Select one row index. Indices are display positions; out-of-range
    /// indices simply never paint.
    pub fn select(&mut self, index: usize) {
        self.selection.insert(index);
    }

    /// Unselect one row index, reporting whether it was selected.
    pub fn unselect(&mut self, index: usize) -> bool {
        self.selection.remove(&index)
    }

    /// Flip one row's selection.
    pub fn toggle(&mut self, index: usize) {
        if !self.selection.remove(&index) {
            self.selection.insert(index);
        }
    }

    /// Whether one row index is selected.
    #[must_use]
    pub fn is_selected(&self, index: usize) -> bool {
        self.selection.contains(&index)
    }

    /// Clear the whole selection.
    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    /// Selected row indices in order.
    #[must_use]
    pub fn selection(&self) -> &BTreeSet<usize> {
        &self.selection
    }

    /// The `[start, end)` row window to paint for a scrolled viewport:
    /// visible rows plus overscan each side, clamped to the history. Rows
    /// never fold at any height, so this always matches the naive
    /// full-slice scan over the same inputs.
    #[must_use]
    pub fn culled_rows(
        &self,
        total_rows: usize,
        scroll_px: f32,
        viewport_height_px: f32,
    ) -> (usize, usize) {
        if total_rows == 0
            || viewport_height_px.is_nan()
            || viewport_height_px <= 0.0
            || !self.row_height_px.is_finite()
            || self.row_height_px <= 0.0
        {
            return (0, 0);
        }
        if !scroll_px.is_finite() {
            return (0, 0);
        }
        let row = self.row_height_px;
        let scroll = scroll_px.max(0.0);
        let height = viewport_height_px;
        let pad = TABLE_OVERSCAN_ROWS as f32 * row;
        let floored = ((scroll - pad) / row - 1.0).floor();
        let start = if floored < 0.0 {
            0
        } else {
            (floored as usize).saturating_add(1).min(total_rows)
        };
        let end = (((scroll + height + pad) / row).ceil() as usize).min(total_rows);
        (start.min(end), end)
    }
}

#[cfg(test)]
#[path = "../tests/table.rs"]
mod tests;
