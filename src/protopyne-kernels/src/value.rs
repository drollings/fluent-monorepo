//! Pine scalar values: one owned enum instead of three proxy types.
//!
//! Pine auto-casts `int` to `float` wherever a float is expected; no other
//! implicit conversion exists. [`PineValue::as_float`] encodes exactly that,
//! and the readers refuse every other mismatch — numbers are never truthy,
//! floats never truncate, and display handles never read as numbers.
//!
//! Absence (Pine `na`) is always `None` at the call site, never a sentinel;
//! IEEE `NaN` inside `f64` legs means the same absence and maps back to
//! `None` at the output boundary via `fin_or_none`. [`nz`] fills absence
//! with a default and [`fixnan`] forward-fills a leg without ever
//! resurrecting a leading warmup prefix.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

/// A Pine scalar. Old numeric variants keep their exact reader behavior;
/// `Str` and the opaque display handles travel beside numerics and never
/// enter arithmetic. `Array` and `IntMap` are structural values for the
/// script collection namespaces: elements may hold absence (`None`), and
/// neither reads as a number, an int, or a bool.
#[derive(Debug, Clone, PartialEq)]
pub enum PineValue {
    /// Pine `float`.
    Float(f64),
    /// Pine `int`; auto-casts to [`PineValue::Float`] on read.
    Int(i64),
    /// Pine `bool`.
    Bool(bool),
    /// Pine `string` (owned; produced by `str.*`, consumed by plots/labels).
    Str(String),
    /// Opaque display color (`color.*` lowers here; never arithmetic).
    ColorHandle(ColorHandle),
    /// Opaque enum member (`UTIL.Smoothings.WMA` lowers here).
    EnumHandle(EnumHandle),
    /// Pine array: ordered elements with absence per slot. Collection
    /// operators return new arrays (functional); nothing mutates in place.
    Array(Vec<Option<PineValue>>),
    /// Pine `map<int, int>`: the scheduler-state shape. Missing keys read
    /// as absence at the call site, never as a default.
    IntMap(std::collections::HashMap<i64, i64>),
    /// Pine `map<K, V>` for every other key/value shape (enum keys, object
    /// or color values, ...): entries keyed by the canonical scalar
    /// encoding ([`map_key_of`]), values riding whole with absence per
    /// slot. Iteration sorts by encoding, so dumps stay deterministic.
    /// `<int, int>` keeps the dedicated `IntMap` shape above.
    Map(std::collections::BTreeMap<String, (PineValue, Option<PineValue>)>),
    /// Owned user-type instance (`type` declarations with `method`
    /// dispatch). Fields address by name; instances never read as numbers,
    /// strings, or bools.
    Object(UserInstance),
    /// Opaque drawing object (`label`/`table`/`box`/`line` handles minted
    /// by `.new` and recorded by the engine's lifecycle log; never
    /// arithmetic, never a number/string/bool).
    Drawing(DrawingHandle),
    /// Pine matrix: a fixed row-by-column grid with absence per cell.
    /// Like arrays, operators return new grids (functional); `set` in
    /// value position writes back through the statement helper.
    Matrix(MatrixValue),
}

/// Owned two-dimensional grid: row-major cells under an explicit shape.
/// The shape is fixed at construction; every cell starts as the initial
/// value (absence when the constructor is called without one).
#[derive(Debug, Clone, PartialEq)]
pub struct MatrixValue {
    /// Row count (`matrix.rows`).
    pub rows: usize,
    /// Column count (`matrix.columns`).
    pub cols: usize,
    /// Row-major cells, exactly `rows * cols` entries.
    pub cells: Vec<Option<PineValue>>,
}

impl MatrixValue {
    /// Build a grid of one repeated cell, validating the shape first.
    #[must_use]
    pub fn filled(rows: usize, cols: usize, cell: Option<PineValue>) -> Self {
        Self {
            rows,
            cols,
            cells: vec![cell; rows.checked_mul(cols).unwrap_or(0)],
        }
    }

    /// Borrow one cell by row and column, or `None` off shape.
    #[must_use]
    pub fn get(&self, row: usize, col: usize) -> Option<Option<PineValue>> {
        let index = row.checked_mul(self.cols)?.checked_add(col)?;
        self.cells.get(index).cloned()
    }

    /// Replace one cell by row and column; off-shape writes are refused
    /// by the caller, never clamped.
    pub fn set(&mut self, row: usize, col: usize, cell: Option<PineValue>) -> bool {
        let Some(index) = row.checked_mul(self.cols).and_then(|base| base.checked_add(col)) else {
            return false;
        };
        let Some(slot) = self.cells.get_mut(index) else {
            return false;
        };
        *slot = cell;
        true
    }
}

/// Opaque display-color token. The inner packing is decided by the lowering
/// stage that mints it; comparison and display never inspect it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColorHandle(u32);

impl ColorHandle {
    /// Mint a handle from the lowering stage's packed representation.
    #[must_use]
    pub fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// The packed representation, for the lowering stage only.
    #[must_use]
    pub fn raw(self) -> u32 {
        self.0
    }
}

/// Opaque drawing-object token: which namespace minted it and which recorded
/// object it names. The engine's lifecycle log owns the properties; the
/// handle only identifies. Comparison never inspects properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DrawingHandle {
    /// Minting namespace (`label`, `table`, `box`, or `line`).
    pub kind: DrawingKind,
    /// Per-run object number, unique across all four namespaces.
    pub id: u64,
}

impl DrawingHandle {
    /// Name one recorded drawing object.
    #[must_use]
    pub fn new(kind: DrawingKind, id: u64) -> Self {
        Self { kind, id }
    }

    /// Stable key for lifecycle rows (`label#3`).
    #[must_use]
    pub fn key(self) -> String {
        format!("{}#{}", self.kind, self.id)
    }
}

/// The four recorded drawing namespaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DrawingKind {
    /// `label.*` markers.
    Label,
    /// `table.*` grids.
    Table,
    /// `box.*` zones.
    Box,
    /// `line.*` levels.
    Line,
}

impl DrawingKind {
    /// The namespace text that mints this kind.
    #[must_use]
    pub fn namespace(self) -> &'static str {
        match self {
            Self::Label => "label",
            Self::Table => "table",
            Self::Box => "box",
            Self::Line => "line",
        }
    }
}

impl fmt::Display for DrawingKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.namespace())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnumHandle {
    /// Declaring enum (`Smoothings`).
    pub enum_name: String,
    /// Member name (`WMA`).
    pub member: String,
}

impl EnumHandle {
    /// Name one enum member.
    #[must_use]
    pub fn new(enum_name: &str, member: &str) -> Self {
        Self {
            enum_name: enum_name.to_owned(),
            member: member.to_owned(),
        }
    }
}

impl fmt::Display for EnumHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.enum_name, self.member)
    }
}

/// Owned user-type instance: which declared type plus its field bindings
/// (`None` fields are Pine `na`). Methods receive the instance by mutable
/// reference through the executor; the value itself stays a plain cloneable
/// datum like every other variant.
#[derive(Debug, Clone, PartialEq)]
pub struct UserInstance {
    /// Declared type name (`Scheduler`).
    pub type_name: String,
    /// Field bindings in no particular order, behind a cheap-to-clone handle.
    pub fields: FieldTable,
}

impl UserInstance {
    /// Build an instance from evaluated field bindings.
    #[must_use]
    pub fn new(type_name: &str, fields: impl Into<FieldTable>) -> Self {
        Self {
            type_name: type_name.to_owned(),
            fields: fields.into(),
        }
    }
}

impl PineValue {
    /// Read as a float, applying Pine's int-to-float auto-cast.
    #[must_use]
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Self::Float(v) => Some(*v),
            Self::Int(v) => Some(*v as f64),
            Self::Bool(_)
            | Self::Str(_)
            | Self::ColorHandle(_)
            | Self::EnumHandle(_)
            | Self::Array(_)
            | Self::IntMap(_)
            | Self::Map(_)
            | Self::Object(_)
            | Self::Drawing(_)
            | Self::Matrix(_) => None,
        }
    }

    /// Read as an int. Floats never implicitly truncate.
    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(v) => Some(*v),
            Self::Float(_)
            | Self::Bool(_)
            | Self::Str(_)
            | Self::ColorHandle(_)
            | Self::EnumHandle(_)
            | Self::Array(_)
            | Self::IntMap(_)
            | Self::Map(_)
            | Self::Object(_)
            | Self::Drawing(_)
            | Self::Matrix(_) => None,
        }
    }

    /// Read as a bool. Numbers are never truthy by accident.
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(v) => Some(*v),
            Self::Float(_)
            | Self::Int(_)
            | Self::Str(_)
            | Self::ColorHandle(_)
            | Self::EnumHandle(_)
            | Self::Array(_)
            | Self::IntMap(_)
            | Self::Map(_)
            | Self::Object(_)
            | Self::Drawing(_)
            | Self::Matrix(_) => None,
        }
    }
}

/// Canonical encoding of one map key: scalar script values (ints by value,
/// floats by bits, bools, strings, packed colors, enum members) each name
/// a distinct entry. Collections, objects, drawings, and matrices are
/// never keys and read as `None`.
#[must_use]
pub fn map_key_of(value: &PineValue) -> Option<String> {
    match value {
        PineValue::Int(v) => Some(format!("int:{v}")),
        PineValue::Float(v) => Some(format!("float:{}", v.to_bits())),
        PineValue::Bool(v) => Some(format!("bool:{v}")),
        PineValue::Str(v) => Some(format!("str:{}:{v}", v.len())),
        PineValue::ColorHandle(v) => Some(format!("color:{}", v.raw())),
        PineValue::EnumHandle(handle) => {
            Some(format!("enum:{}.{}", handle.enum_name, handle.member))
        }
        PineValue::Array(_)
        | PineValue::IntMap(_)
        | PineValue::Map(_)
        | PineValue::Object(_)
        | PineValue::Drawing(_)
        | PineValue::Matrix(_) => None,
    }
}

/// Pine `nz(value, default)`: absence (`None`) becomes `default`, presence
/// passes through untouched.
#[must_use]
pub fn nz(value: Option<PineValue>, default: PineValue) -> PineValue {
    value.unwrap_or(default)
}

/// Pine `fixnan`: forward-fill absence with the last present value. A `NaN`
/// leg entry counts as absent (legs use `NaN` for `na`). Leading absence —
/// the warmup prefix — stays absent; nothing is resurrected.
#[must_use]
pub fn fixnan(series: &[Option<f64>]) -> Vec<Option<f64>> {
    let mut last: Option<f64> = None;
    series
        .iter()
        .map(|slot| match slot {
            Some(v) if !v.is_nan() => {
                last = Some(*v);
                Some(*v)
            }
            _ => last,
        })
        .collect()
}

impl fmt::Display for PineValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Float(v) => write!(f, "{v}"),
            Self::Int(v) => write!(f, "{v}"),
            Self::Bool(v) => write!(f, "{v}"),
            Self::Str(v) => write!(f, "{v}"),
            Self::ColorHandle(v) => write!(f, "color({})", v.raw()),
            Self::EnumHandle(v) => write!(f, "{v}"),
            Self::Object(instance) => write!(f, "{} instance", instance.type_name),
            Self::Drawing(handle) => write!(f, "{}", handle.key()),
            Self::Matrix(grid) => write!(f, "matrix[{}x{}]", grid.rows, grid.cols),
            Self::Array(items) => {
                let mut first = true;
                write!(f, "[")?;
                for item in items {
                    if !first {
                        write!(f, ", ")?;
                    }
                    first = false;
                    match item {
                        Some(value) => write!(f, "{value}")?,
                        None => write!(f, "na")?,
                    }
                }
                write!(f, "]")
            }
            Self::IntMap(entries) => {
                let mut keys: Vec<i64> = entries.keys().copied().collect();
                keys.sort_unstable();
                let mut first = true;
                write!(f, "{{")?;
                for key in keys {
                    if !first {
                        write!(f, ", ")?;
                    }
                    first = false;
                    write!(f, "{key}: {}", entries[&key])?;
                }
                write!(f, "}}")
            }
            Self::Map(entries) => {
                let mut first = true;
                write!(f, "{{")?;
                for (key, (_, value)) in entries {
                    if !first {
                        write!(f, ", ")?;
                    }
                    first = false;
                    match value {
                        Some(stored) => write!(f, "{key}: {stored}")?,
                        None => write!(f, "{key}: na")?,
                    }
                }
                write!(f, "}}")
            }
        }
    }
}

/// One native method-call outcome: decline to Pine or serve with the call
/// value. `Declined` means the engine runs the Pine body with fields
/// untouched; `Served(value)` means the table is already updated and
/// `value` is the call value (`None` is Pine `na`). The two axes —
/// handled-vs-declined and value-vs-absence — never share one option.
#[derive(Debug, Clone, PartialEq)]
pub enum NativeCall {
    /// Not handled: fields untouched, the engine runs the Pine body.
    Declined,
    /// Handled: table already updated, carrying the call value.
    Served(Option<PineValue>),
}

impl NativeCall {
    /// Whether this outcome declines to the Pine body.
    #[must_use]
    pub fn is_declined(&self) -> bool {
        matches!(self, Self::Declined)
    }

    /// The served call value, or `None` for declines and served absence.
    #[must_use]
    pub fn into_value(self) -> Option<PineValue> {
        match self {
            Self::Declined => None,
            Self::Served(value) => value,
        }
    }
}

/// Pine object field table shared with the engine bridge: field name to
/// Pine value (`None` is Pine `na`). A cheap-to-clone handle over one
/// resident map: cloning shares the table, and the first mutation after a
/// share copies it (copy-on-write). So a native bridge that returns `this`
/// hands back the resident table by handle instead of copying it — and
/// never copies the arrays it holds — while an unmapped field still passes
/// through byte-identical without native code ever seeing it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FieldTable(Arc<HashMap<String, Option<PineValue>>>);

impl FieldTable {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether two handles share one resident table (no copy has separated
    /// them yet). A native bridge that returns `this` hands back a handle to
    /// the receiver's table, so the call copies no table — and none of the
    /// arrays it holds.
    #[must_use]
    pub fn shares(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl From<HashMap<String, Option<PineValue>>> for FieldTable {
    fn from(fields: HashMap<String, Option<PineValue>>) -> Self {
        Self(Arc::new(fields))
    }
}

impl FromIterator<(String, Option<PineValue>)> for FieldTable {
    fn from_iter<T: IntoIterator<Item = (String, Option<PineValue>)>>(iter: T) -> Self {
        Self(Arc::new(iter.into_iter().collect()))
    }
}

impl std::ops::Deref for FieldTable {
    type Target = HashMap<String, Option<PineValue>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for FieldTable {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.0)
    }
}

impl<'a> IntoIterator for &'a FieldTable {
    type Item = (&'a String, &'a Option<PineValue>);
    type IntoIter = std::collections::hash_map::Iter<'a, String, Option<PineValue>>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'a> IntoIterator for &'a mut FieldTable {
    type Item = (&'a String, &'a mut Option<PineValue>);
    type IntoIter = std::collections::hash_map::IterMut<'a, String, Option<PineValue>>;

    fn into_iter(self) -> Self::IntoIter {
        Arc::make_mut(&mut self.0).iter_mut()
    }
}

#[cfg(test)]
#[path = "../tests/value.rs"]
mod tests;
