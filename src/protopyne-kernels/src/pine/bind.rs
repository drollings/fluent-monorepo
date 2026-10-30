//! One argument binder for every call shape.
//!
//! A compiled call has already normalized its arguments: the compiler fills
//! defaults and drops names, so every VM call binds positionally. The tree
//! binds by name first and evaluates defaults itself (see
//! `EngineState::bind_call_args`), and the cross-engine bridge hands the
//! tree the caller's evaluated values in parameter order.
//!
//! All four VM shapes — a plain user call, a method call (whose receiver
//! occupies slot 0), a tree-initiated function call, and the cross-boundary
//! bridge — therefore share one fill rule and one arity refusal. Keeping it
//! in one place is what makes "the method path forgot the receiver offset"
//! a compile error instead of a silent off-by-one.

use crate::value::PineValue;
use crate::KernelError;

/// Positional argument filler over one pooled frame.
pub(crate) struct ArgBinder;

impl ArgBinder {
    /// Fill a frame parameter range from evaluated values: `values[i]` lands
    /// in `slots[offset + i]`, and a value count that does not match
    /// `params` refuses naming the call shape.
    ///
    /// `shape` names the call kind in the refusal (`block` for a user call,
    /// `function block` for a tree-initiated call), so each path keeps the
    /// wording its own diagnostics used before the binder was shared.
    #[inline]
    pub(crate) fn bind_frame(
        shape: &str,
        block: usize,
        slots: &mut [Option<PineValue>],
        offset: usize,
        params: usize,
        values: &[Option<PineValue>],
    ) -> Result<(), KernelError> {
        if values.len() != params {
            return Err(KernelError::Eval(format!(
                "arity mismatch calling {shape} {block}: {} args for {params} params",
                values.len()
            )));
        }
        for (index, value) in values.iter().enumerate() {
            if let Some(slot) = slots.get_mut(offset + index) {
                slot.clone_from(value);
            }
        }
        Ok(())
    }
}
