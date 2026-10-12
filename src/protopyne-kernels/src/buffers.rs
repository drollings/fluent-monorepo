//! Native `BufferFloat` mirror: the shipped buffer library stays byte-identical
//! on disk as the executable contract; this module is its pure-Rust mirror
//! for the hot methods only (`init/push/get/size/clear`).
//!
//! Everything here folds the Pine-visible field table exactly: `data` slots
//! hold float-or-absence, `head` absence selects array mode, ring writes
//! invalidate `_linear` while array writes keep it, and `init` passes
//! `_linear` through untouched — all pinned by `tests/native_buffers.rs`
//! against hand-derived oracles from the shipped method bodies, plus the
//! engine agreement test over the real DVP slice.
//!
//! Anything outside the fast path (analytics, `getLinearized`, `compact`,
//! iteration helpers, all of `BufferInt`, misshaped state, named args, bad
//! arity) declines (`Ok(None)`): the engine runs the Pine body exactly as
//! today, so serving can never miscompute — only go faster.
//!
//! Performance discipline: the bridge mutates the taken field table in
//! place and never copies the `data` array — not on hydrate (borrowed
//! views only), not on serve (one slot written per `push`, none per
//! `get`/`size`). Per-call full-array copies (clone plus drop, ~60µs on
//! the DVP ratio) are what made the interpreted `reconcile` path slow;
//! reintroducing them here would reproduce the bottleneck natively.

use crate::error::KernelError;
use crate::native_libs::FieldTable;
use crate::value::PineValue;

/// Methods served natively on `BufferFloat` (v1 scope). Every other method
/// declines to Pine.
const SERVED: &[&str] = &["init", "push", "get", "size", "clear"];

/// Serve one `BufferFloat` method call by mutating `fields` in place and
/// yielding the call value. `Ok(None)` declines (fields untouched — the
/// engine runs the Pine body exactly as today); `Ok(Some(value))` reports
/// the table already updated. `named_args` declines too: positional binding
/// would misbind named calls.
///
/// Every arm validates fully before writing anything, so a decline never
/// leaves a half-mutated table behind.
pub fn serve_buffer_method(
    type_name: &str,
    method: &str,
    fields: &mut FieldTable,
    args: &[Option<PineValue>],
    named_args: bool,
) -> Result<Option<Option<PineValue>>, KernelError> {
    if type_name != "BufferFloat" || !SERVED.contains(&method) || named_args {
        return Ok(None);
    }
    match method {
        "init" => serve_init(fields, args),
        "push" => serve_push(fields, args),
        "get" => serve_get(fields, args),
        "size" => serve_size(fields, args),
        "clear" => serve_clear(fields, args),
        _ => Ok(None),
    }
}

fn bridge_invalid(field: &str, detail: &str) -> KernelError {
    KernelError::InvalidInput(format!("native BufferFloat bridge: field '{field}' {detail}"))
}

/// Borrowed buffer view: the `data` slots plus the scalar bookkeeping. No
/// allocation, no per-slot parsing — the served methods only need indexing
/// and length math. Lifetimes tie the view to the table borrow.
struct BufferView<'a> {
    data: &'a mut Vec<Option<PineValue>>,
    max_size: usize,
    head: Option<usize>,
    count: u64,
}

fn field_usize(fields: &FieldTable, name: &str) -> Result<Option<usize>, KernelError> {
    match fields.get(name) {
        None => Err(bridge_invalid(name, "is missing")),
        Some(None) => Ok(None),
        Some(Some(PineValue::Int(value))) => {
            if *value >= 0 {
                Ok(Some(*value as usize))
            } else {
                Err(bridge_invalid(name, "is negative"))
            }
        }
        Some(Some(PineValue::Float(value))) => {
            if value.is_finite() && value.fract() == 0.0 && *value >= 0.0 {
                Ok(Some(*value as usize))
            } else {
                Err(bridge_invalid(name, &format!("float {value} is not a non-negative int")))
            }
        }
        Some(Some(other)) => Err(bridge_invalid(name, &format!("is not an int ({other:?})"))),
    }
}

/// Borrow the table's buffer state, validating shape first. Anything
/// untranslatable (missing keys, misshaped slots, ring length desync)
/// errors — and every caller maps that to a decline, so Pine owns the
/// refusal exactly as today.
fn view(fields: &mut FieldTable) -> Result<BufferView<'_>, KernelError> {
    let max_size = field_usize(fields, "max_size")?.ok_or_else(|| bridge_invalid("max_size", "is na"))?;
    let head = field_usize(fields, "head")?;
    if head.is_some_and(|head| head >= max_size && max_size > 0) {
        return Err(bridge_invalid("head", "is past max_size"));
    }
    let count = match field_usize(fields, "count")? {
        Some(value) => value as u64,
        None => return Err(bridge_invalid("count", "is na")),
    };
    let data = match fields.get_mut("data") {
        Some(Some(PineValue::Array(slots))) => slots,
        _ => return Err(bridge_invalid("data", "is not an array")),
    };
    if head.is_some() && data.len() != max_size {
        return Err(bridge_invalid("data", "ring length differs from max_size"));
    }
    Ok(BufferView {
        data,
        max_size,
        head,
        count,
    })
}

fn arg_usize(args: &[Option<PineValue>], index: usize) -> Result<Option<usize>, KernelError> {
    match args.get(index) {
        None => Ok(None),
        Some(None) => Ok(None),
        Some(Some(PineValue::Int(value))) => {
            if *value >= 0 {
                Ok(Some(*value as usize))
            } else {
                Err(bridge_invalid("arg", "is negative"))
            }
        }
        Some(Some(PineValue::Float(value))) => {
            if value.is_finite() && value.fract() == 0.0 && *value >= 0.0 {
                Ok(Some(*value as usize))
            } else {
                Err(bridge_invalid("arg", &format!("float {value} is not a non-negative int")))
            }
        }
        Some(Some(other)) => Err(bridge_invalid("arg", &format!("is not an int ({other:?})"))),
    }
}

fn arg_bool(args: &[Option<PineValue>], index: usize) -> Result<Option<bool>, KernelError> {
    match args.get(index) {
        None => Ok(None),
        Some(None) => Ok(None),
        Some(Some(PineValue::Bool(value))) => Ok(Some(*value)),
        Some(Some(other)) => Err(bridge_invalid("arg", &format!("is not a bool ({other:?})"))),
    }
}

fn set_scalar(fields: &mut FieldTable, name: &str, value: PineValue) {
    fields.insert(name.to_owned(), Some(value));
}

/// `init(buffer_size = 20, ring_mode = false)`: fresh storage (ring keeps a
/// full absent array, array mode an empty one), counters reset. `_linear`
/// passes through untouched — the oracle never writes it here.
///
/// Representation rule (probed oracle, pinned by the engine agreement
/// test): the bookkeeping writes land as `Float` even for int literals, so
/// native writes `Float` too, never `Int`. (`size()` is the one exception:
/// array mode returns the `array.size` builtin, an `Int`.)
fn serve_init(
    fields: &mut FieldTable,
    args: &[Option<PineValue>],
) -> Result<Option<Option<PineValue>>, KernelError> {
    if args.len() > 2 {
        return Ok(None);
    }
    let size = match arg_usize(args, 0) {
        Ok(Some(value)) => value,
        _ => return Ok(None),
    };
    let ring = match arg_bool(args, 1) {
        Ok(Some(value)) => value,
        Ok(None) if args.len() < 2 => false,
        _ => return Ok(None),
    };
    fields.insert(
        "data".to_owned(),
        Some(if ring {
            PineValue::Array(vec![None; size])
        } else {
            PineValue::Array(Vec::new())
        }),
    );
    set_scalar(fields, "max_size", PineValue::Float(size as f64));
    fields.insert(
        "head".to_owned(),
        if ring {
            Some(PineValue::Float(0.0))
        } else {
            None
        },
    );
    set_scalar(fields, "count", PineValue::Float(0.0));
    set_scalar(fields, "needs_compaction", PineValue::Bool(false));
    let value = PineValue::Object(crate::value::UserInstance::new(
        "BufferFloat",
        fields.clone(),
    ));
    Ok(Some(Some(value)))
}

/// `push(value)`: array mode appends (flagging past-max growth), ring mode
/// overwrites at head and invalidates `_linear`. One slot written, nothing
/// copied. Absence stores as absence; ints unify to float exactly like the
/// oracle's method-call args.
fn serve_push(
    fields: &mut FieldTable,
    args: &[Option<PineValue>],
) -> Result<Option<Option<PineValue>>, KernelError> {
    if args.len() != 1 {
        return Ok(None);
    }
    let slot = match &args[0] {
        None => None,
        Some(PineValue::Float(value)) => {
            if value.is_finite() {
                Some(PineValue::Float(*value))
            } else {
                None
            }
        }
        Some(PineValue::Int(value)) => Some(PineValue::Float(*value as f64)),
        Some(_) => return Ok(None),
    };
    // Phase 1: validate shape and perform the one slot write while the
    // table is borrowed. Scalars come out owned for phase 2.
    enum Pushed {
        Array { count: u64, over: bool },
        Ring { count: u64, head: usize },
    }
    let pushed = {
        let buffer = match view(fields) {
            Ok(buffer) => buffer,
            Err(_) => return Ok(None),
        };
        match buffer.head {
            None => {
                buffer.data.push(slot);
                let over = buffer.data.len() > buffer.max_size;
                Pushed::Array {
                    count: buffer.count + 1,
                    over,
                }
            }
            Some(head) => {
                if buffer.max_size == 0 || head >= buffer.data.len() {
                    // Zero-capacity ring (or a desynced table): Pine refuses
                    // the out-of-range write, so decline and let it.
                    return Ok(None);
                }
                buffer.data[head] = slot;
                Pushed::Ring {
                    count: buffer.count + 1,
                    head: (head + 1) % buffer.max_size.max(1),
                }
            }
        }
    };
    // Phase 2: scalar writes on the free table.
    match pushed {
        Pushed::Array { count, over } => {
            if over {
                set_scalar(fields, "needs_compaction", PineValue::Bool(true));
            }
            set_scalar(fields, "count", PineValue::Float(count as f64));
        }
        Pushed::Ring { count, head } => {
            set_scalar(fields, "head", PineValue::Float(head as f64));
            fields.insert("_linear".to_owned(), None);
            set_scalar(fields, "count", PineValue::Float(count as f64));
        }
    }
    let value = PineValue::Object(crate::value::UserInstance::new(
        "BufferFloat",
        fields.clone(),
    ));
    Ok(Some(Some(value)))
}

/// `get(index)`: newest-first read in both modes; out-of-bounds reads
/// absence. Pure read — the table is never written.
fn serve_get(
    fields: &mut FieldTable,
    args: &[Option<PineValue>],
) -> Result<Option<Option<PineValue>>, KernelError> {
    if args.len() != 1 {
        return Ok(None);
    }
    // Signed: negative indices read absence per the oracle (`index < 0`
    // short-circuits first), so they serve `na` rather than declining.
    let index = match &args[0] {
        None => return Ok(None),
        Some(PineValue::Int(value)) => *value,
        Some(PineValue::Float(value))
            if value.is_finite() && value.fract() == 0.0 =>
        {
            *value as i64
        }
        Some(_) => return Ok(None),
    };
    let buffer = match view(fields) {
        Ok(buffer) => buffer,
        Err(_) => return Ok(None),
    };
    let value = match buffer.head {
        None => {
            if index < 0 || index as usize >= buffer.data.len() {
                None
            } else {
                buffer.data[buffer.data.len() - 1 - index as usize].clone()
            }
        }
        Some(head) => {
            if index < 0 || index as usize >= buffer.max_size || index as u64 >= buffer.count {
                None
            } else {
                let index = index as usize;
                let pos = (head + buffer.max_size - 1 - index) % buffer.max_size.max(1);
                buffer.data[pos].clone()
            }
        }
    };
    Ok(Some(value))
}

/// `size()`: grown length in array mode (the `array.size` builtin, an
/// `Int`), `min(count, max)` in ring mode (a `math.min` over floats).
/// Pure read — the table is never written.
fn serve_size(
    fields: &mut FieldTable,
    args: &[Option<PineValue>],
) -> Result<Option<Option<PineValue>>, KernelError> {
    if !args.is_empty() {
        return Ok(None);
    }
    let buffer = match view(fields) {
        Ok(buffer) => buffer,
        Err(_) => return Ok(None),
    };
    let size = match buffer.head {
        None => PineValue::Int(buffer.data.len() as i64),
        Some(_) => PineValue::Float(buffer.count.min(buffer.max_size as u64) as f64),
    };
    Ok(Some(Some(size)))
}

/// `clear()`: array mode empties (head stays absent), ring mode fills
/// absent and resets head (invalidating `_linear`); counters reset.
fn serve_clear(
    fields: &mut FieldTable,
    args: &[Option<PineValue>],
) -> Result<Option<Option<PineValue>>, KernelError> {
    if !args.is_empty() {
        return Ok(None);
    }
    let (ring, max_size) = match view(fields) {
        Ok(buffer) => (buffer.head.is_some(), buffer.max_size),
        Err(_) => return Ok(None),
    };
    if ring {
        fields.insert(
            "data".to_owned(),
            Some(PineValue::Array(vec![None; max_size])),
        );
        set_scalar(fields, "head", PineValue::Float(0.0));
        fields.insert("_linear".to_owned(), None);
    } else {
        fields.insert("data".to_owned(), Some(PineValue::Array(Vec::new())));
    }
    set_scalar(fields, "count", PineValue::Float(0.0));
    set_scalar(fields, "needs_compaction", PineValue::Bool(false));
    let value = PineValue::Object(crate::value::UserInstance::new(
        "BufferFloat",
        fields.clone(),
    ));
    Ok(Some(Some(value)))
}

#[cfg(test)]
#[path = "../tests/native_buffers.rs"]
mod tests;
