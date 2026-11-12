//! Headless drawing capture: one registry, one lifecycle table, one sink.
//!
//! A drawing call records data — it never renders. `.new` mints an object
//! and returns its handle, setters (and `table.cell`/`merge_cells`) mutate
//! stored properties and log an update, getters read properties back, and
//! `.delete` retires the object. Every transition appends one
//! [`DrawingEvent`] carrying the evaluated-bar index and the call
//! arguments, so a consumer can replay the drawing exactly.
//!
//! Both engines call this module: the tree-walk evaluates arguments with
//! the surrounding scope, the compiled loop reads them from its frame, and
//! the values meet here. One registry means a handle minted by either
//! engine stays live for the other, ids never collide in a split run, and
//! every refusal wording has one home.

use super::builtins_core::CoreArg;
use super::eval::{DrawingArg, DrawingEvent, DrawingEventKind};
use crate::value::{DrawingHandle, DrawingKind, PineValue};
use crate::KernelError;
use std::collections::HashMap;

/// One live drawing object: its namespace plus current properties. Setters
/// mutate properties in place; getters read them back; `.delete` drops the
/// entry (later reads refuse instead of resurrecting it).
#[derive(Debug, Clone)]
/// One live drawing object: its namespace plus current properties. Setters
/// mutate properties in place; getters read them back; `.delete` drops the
/// entry (later reads refuse instead of resurrecting it).
struct DrawingObject {
    kind: DrawingKind,
    props: HashMap<String, Option<PineValue>>,
}

/// Every drawing store that lives exactly as long as one run.
#[derive(Debug, Default)]
/// Every drawing store that lives exactly as long as one run.
pub(crate) struct DrawingState {
    /// Per-run object number, unique across all four namespaces.
    next_id: u64,
    /// Live objects by id.
    objects: HashMap<u64, DrawingObject>,
    /// Lifecycle rows in execution order.
    events: Vec<DrawingEvent>,
}

impl DrawingState {
    /// Drain the recorded lifecycle rows into the run outcome.
    pub(crate) fn take_events(&mut self) -> Vec<DrawingEvent> {
        std::mem::take(&mut self.events)
    }

    /// Record one drawing call on `bar`. `namespace`/`method` are the
    /// source spellings; the shared table decides creation, mutation,
    /// reads, deletion, and every refusal.
    pub(crate) fn invoke(
        &mut self,
        kind: DrawingKind,
        method: &str,
        args: &[CoreArg],
        bar: usize,
    ) -> Result<Option<PineValue>, KernelError> {
        match method {
            "new" => self.mint(kind, args, bar),
            "delete" => self.delete(kind, args, bar),
            "cell" => self.cell(kind, args, bar),
            "merge_cells" => self.merge_cells(kind, args, bar),
            _ => self.access(kind, method, args, bar),
        }
    }

    /// Mint one recorded drawing object: positional creation properties bind
    /// by namespace, named arguments ride along as properties, a `Created`
    /// row records the call, and the handle returns to the script.
    fn mint(
        &mut self,
        kind: DrawingKind,
        args: &[CoreArg],
        bar: usize,
    ) -> Result<Option<PineValue>, KernelError> {
        let positional = positionals(args);
        let names: &[&str] = match kind {
            DrawingKind::Label => &["x", "y", "text"],
            DrawingKind::Table => &["position", "columns", "rows"],
            DrawingKind::Box | DrawingKind::Line => &["x1", "y1", "x2", "y2"],
        };
        if positional.len() < names.len() {
            return Err(refused(format!(
                "'{}.new' takes {} ({} given)",
                kind.namespace(),
                names.join(", "),
                positional.len()
            )));
        }
        let id = self.next_id;
        self.next_id += 1;
        let mut props = HashMap::with_capacity(args.len());
        for (name, value) in names.iter().zip(positional.iter()) {
            props.insert((*name).to_owned(), value.clone());
        }
        for arg in args {
            if let Some(name) = &arg.name {
                props.insert(name.clone(), arg.value.clone());
            }
        }
        self.objects.insert(id, DrawingObject { kind, props });
        let handle = DrawingHandle::new(kind, id);
        self.events.push(DrawingEvent {
            bar,
            kind: DrawingEventKind::Created,
            object: kind.namespace().to_owned(),
            key: handle.key(),
            props: Vec::new(),
            args: draw_args(args),
        });
        Ok(Some(PineValue::Drawing(handle)))
    }

    /// Retire one recorded drawing object: the single positional argument
    /// must be a live handle of the same namespace. A `Deleted` row records
    /// the call; later reads of the handle refuse.
    fn delete(
        &mut self,
        kind: DrawingKind,
        args: &[CoreArg],
        bar: usize,
    ) -> Result<Option<PineValue>, KernelError> {
        // Deleting absence drops silently with no recorded event: rolling
        // drawings guard with `line.delete(l[1])` over `na`-seeded holders,
        // and the reference runtime is a no-op there too. Any other misuse
        // still refuses through the shared handle check below.
        let absent = args
            .iter()
            .find(|arg| arg.name.is_none())
            .and_then(|arg| arg.value.clone())
            .is_none();
        if absent {
            return Ok(None);
        }
        let handle = self.handle(kind, "delete", args)?;
        self.objects.remove(&handle.id);
        self.events.push(DrawingEvent {
            bar,
            kind: DrawingEventKind::Deleted,
            object: kind.namespace().to_owned(),
            key: handle.key(),
            props: Vec::new(),
            args: draw_args(args),
        });
        Ok(None)
    }

    /// Record one `table.cell` write as an update row. The table must be a
    /// live handle; cell contents travel in the recorded arguments.
    fn cell(
        &mut self,
        kind: DrawingKind,
        args: &[CoreArg],
        bar: usize,
    ) -> Result<Option<PineValue>, KernelError> {
        if kind != DrawingKind::Table {
            return Err(refused(format!(
                "unknown method '{}.cell'",
                kind.namespace()
            )));
        }
        let handle = self.handle(kind, "cell", args)?;
        self.events.push(DrawingEvent {
            bar,
            kind: DrawingEventKind::Updated,
            object: kind.namespace().to_owned(),
            key: handle.key(),
            props: vec!["cell".to_owned()],
            args: draw_args(args),
        });
        Ok(None)
    }

    /// Record one `table.merge_cells` range merge as an update row. Merging
    /// is display-only (no Pine getter reads merge state back), so the
    /// range travels in the recorded arguments and nothing else changes.
    fn merge_cells(
        &mut self,
        kind: DrawingKind,
        args: &[CoreArg],
        bar: usize,
    ) -> Result<Option<PineValue>, KernelError> {
        if kind != DrawingKind::Table {
            return Err(refused(format!(
                "unknown method '{}.merge_cells'",
                kind.namespace()
            )));
        }
        let handle = self.handle(kind, "merge_cells", args)?;
        self.events.push(DrawingEvent {
            bar,
            kind: DrawingEventKind::Updated,
            object: kind.namespace().to_owned(),
            key: handle.key(),
            props: vec!["cells".to_owned()],
            args: draw_args(args),
        });
        Ok(None)
    }

    /// Run one setter or getter on a live drawing object. Setters mutate the
    /// recorded properties and log an update row; getters read a recorded
    /// property back. Anything else refuses with the unknown-method reason,
    /// so the recorder never becomes a silent sink.
    fn access(
        &mut self,
        kind: DrawingKind,
        method: &str,
        args: &[CoreArg],
        bar: usize,
    ) -> Result<Option<PineValue>, KernelError> {
        let positional = positionals(args);
        if args.len() != positional.len() {
            return Err(refused(format!(
                "'{}.{method}' takes positional arguments",
                kind.namespace()
            )));
        }
        // Setters map to (property, value index) pairs; getters map to one
        // property read. `set_xy` shapes write two properties at once.
        let updates: Vec<(&str, usize)> = match (kind, method) {
            (DrawingKind::Label, "set_x") => vec![("x", 1)],
            (DrawingKind::Label, "set_y") => vec![("y", 1)],
            (DrawingKind::Label, "set_textcolor") => vec![("textcolor", 1)],
            (DrawingKind::Label, "set_style") => vec![("style", 1)],
            (DrawingKind::Label, "set_tooltip") => vec![("tooltip", 1)],
            (DrawingKind::Label | DrawingKind::Box, "set_text") => vec![("text", 1)],
            (DrawingKind::Label | DrawingKind::Line, "set_color") => vec![("color", 1)],
            (DrawingKind::Box, "set_left") => vec![("left", 1)],
            (DrawingKind::Box, "set_top") => vec![("top", 1)],
            (DrawingKind::Box, "set_right") => vec![("right", 1)],
            (DrawingKind::Box, "set_bottom") => vec![("bottom", 1)],
            (DrawingKind::Box, "set_bgcolor") => vec![("bgcolor", 1)],
            (DrawingKind::Box, "set_border_color") => vec![("border_color", 1)],
            (DrawingKind::Line, "set_xy1") => vec![("x1", 1), ("y1", 2)],
            (DrawingKind::Line, "set_xy2") => vec![("x2", 1), ("y2", 2)],
            (DrawingKind::Line, "set_width") => vec![("width", 1)],
            (DrawingKind::Line, "set_x2") => vec![("x2", 1)],
            _ => vec![],
        };
        if !updates.is_empty() {
            let handle = self.handle(kind, method, args)?;
            let want = updates.iter().map(|(_, index)| *index).max().unwrap_or(0) + 1;
            if positional.len() != want {
                return Err(refused(format!(
                    "'{}.{method}' takes {} values ({} given)",
                    kind.namespace(),
                    want - 1,
                    positional.len() - 1
                )));
            }
            let object = self.objects.get_mut(&handle.id).ok_or_else(|| {
                refused(format!(
                    "unknown {} object '{}'",
                    kind.namespace(),
                    handle.key()
                ))
            })?;
            for (prop, index) in &updates {
                object
                    .props
                    .insert((*prop).to_owned(), positional[*index].clone());
            }
            self.events.push(DrawingEvent {
                bar,
                kind: DrawingEventKind::Updated,
                object: kind.namespace().to_owned(),
                key: handle.key(),
                props: updates.iter().map(|(prop, _)| (*prop).to_owned()).collect(),
                args: draw_args(args),
            });
            return Ok(None);
        }
        let prop = match (kind, method) {
            (DrawingKind::Label, "get_x") => "x",
            (DrawingKind::Box, "get_right") => "right",
            (DrawingKind::Line, "get_x2") => "x2",
            _ => {
                return Err(refused(format!(
                    "unknown method '{}.{method}'",
                    kind.namespace()
                )));
            }
        };
        let handle = self.handle(kind, method, args)?;
        if positional.len() != 1 {
            return Err(refused(format!(
                "'{}.{method}' takes the object, got {} arguments",
                kind.namespace(),
                positional.len()
            )));
        }
        let object = self.objects.get(&handle.id).ok_or_else(|| {
            refused(format!(
                "unknown {} object '{}'",
                kind.namespace(),
                handle.key()
            ))
        })?;
        Ok(object.props.get(prop).cloned().unwrap_or(None))
    }

    /// Read the leading handle argument of a drawing call: it must be
    /// present, a drawing handle, of the calling namespace, and live.
    fn handle(
        &self,
        kind: DrawingKind,
        method: &str,
        args: &[CoreArg],
    ) -> Result<DrawingHandle, KernelError> {
        let first = args
            .iter()
            .find(|arg| arg.name.is_none())
            .and_then(|arg| arg.value.clone());
        match first {
            Some(PineValue::Drawing(handle)) if handle.kind == kind => {
                let live = self
                    .objects
                    .get(&handle.id)
                    .is_some_and(|object| object.kind == kind);
                if live {
                    Ok(handle)
                } else {
                    Err(refused(format!(
                        "unknown {} object '{}'",
                        kind.namespace(),
                        handle.key()
                    )))
                }
            }
            Some(PineValue::Drawing(handle)) => Err(refused(format!(
                "'{}.{method}' takes a {} object, got {}",
                kind.namespace(),
                kind.namespace(),
                handle.key()
            ))),
            Some(other) => Err(refused(format!(
                "'{}.{method}' takes a {} object, got {other}",
                kind.namespace(),
                kind.namespace()
            ))),
            None => Err(refused(format!(
                "'{}.{method}' takes a {} object, got na",
                kind.namespace(),
                kind.namespace()
            ))),
        }
    }
}

/// Positional argument values in source order.
fn positionals(args: &[CoreArg]) -> Vec<Option<PineValue>> {
    args.iter()
        .filter(|arg| arg.name.is_none())
        .map(|arg| arg.value.clone())
        .collect()
}

/// Pack one drawing call's evaluated arguments as a lifecycle row: names
/// preserved, values cloned, absence kept as absence.
fn draw_args(args: &[CoreArg]) -> Vec<DrawingArg> {
    args.iter()
        .map(|arg| DrawingArg {
            name: arg.name.clone(),
            value: arg.value.clone(),
        })
        .collect()
}

/// One refusal wording for the whole module.
fn refused(message: impl Into<String>) -> KernelError {
    KernelError::Eval(message.into())
}