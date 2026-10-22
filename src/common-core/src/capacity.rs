use std::num::NonZeroUsize;

/// Shared bounded-capacity guard: zero refuses with a reason.
pub fn nonzero_capacity(capacity: usize, what: &str) -> Result<NonZeroUsize, String> {
    NonZeroUsize::new(capacity)
        .ok_or_else(|| format!("{what} capacity must be non-zero, got {capacity}"))
}
