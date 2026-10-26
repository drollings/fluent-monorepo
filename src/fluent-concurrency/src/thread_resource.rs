//! Per-thread lazy-initialized resources.
//!
//! Each OS thread gets its own instance; the resource is created once per thread
//! on first access.  The `thread_local_resource!` macro generates the
//! `thread_local! RefCell<Option<T>>` declaration, and `with_tlr` provides the
//! ergonomic "take-or-default, use, put-back" accessor.

use std::cell::RefCell;

/// Access a thread-local resource declared with `thread_local_resource!`.
///
/// Initializes the resource to its default value on first access on each thread.
/// After `f` returns, the resource is stored back into the cell for reuse.
pub fn with_tlr<T: Default, R>(
    cell: &'static std::thread::LocalKey<RefCell<Option<T>>>,
    f: impl FnOnce(&mut T) -> R,
) -> R {
    cell.with(|c| {
        let mut val = c.borrow_mut().take().unwrap_or_default();
        let r = f(&mut val);
        *c.borrow_mut() = Some(val);
        r
    })
}

/// Declare a thread-local resource with a `Default` constructor.
///
/// Usage:
/// ```rust
/// fluent_concurrency::thread_local_resource!(static BUF: String);
///
/// let len = fluent_concurrency::thread_resource::with_tlr(&BUF, |buf| {
///     buf.push_str("hi");
///     buf.len()
/// });
/// assert_eq!(len, 2);
/// ```
///
/// Access with `with_tlr` (see the macro example above, which is a compiled
/// doctest exercising both faces).
#[macro_export]
macro_rules! thread_local_resource {
    (static $name:ident : $ty:ty) => {
        thread_local! {
            static $name: std::cell::RefCell<Option<$ty>> =
                const { std::cell::RefCell::new(None) };
        }
    };
}

#[cfg(test)]
#[path = "../tests/thread_resource.rs"]
mod tests;
