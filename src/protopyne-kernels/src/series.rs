//! Backward-indexed views over closed-bar history.
//!
//! `at(0)` is the current closed bar, `at(1)` the prior one. Anything out of
//! range is `None` (Pine `na`), never a panic. The view borrows; kernels copy
//! out the window they need instead of retaining references.

/// A cursor-anchored read-only view over a slice of history.
#[derive(Debug, Clone, Copy)]
/// Backward-indexed views over closed-bar history.
/// 
/// `at(0)` is the current closed bar, `at(1)` the prior one. Anything out of
/// range is `None` (Pine `na`), never a panic. The view borrows; kernels copy
/// out the window they need instead of retaining references.
/// A cursor-anchored read-only view over a slice of history.
pub struct SliceSeries<'a, T> {
    values: &'a [T],
    cursor: usize,
}

impl<'a, T> SliceSeries<'a, T> {
    /// Anchor a view at `cursor` (index of the current closed bar).
    #[must_use]
    pub fn new(values: &'a [T], cursor: usize) -> Self {
        Self { values, cursor }
    }

    /// Read `offset` bars back from the cursor. Out of range is `None`.
    #[must_use]
    pub fn at(&self, offset: usize) -> Option<&'a T> {
        self.values.get(self.cursor.checked_sub(offset)?)
    }

    /// Length of the underlying history, not the visible window.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether the underlying history is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// The anchoring cursor.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }
}

#[cfg(test)]
#[path = "../tests/series.rs"]
mod tests;