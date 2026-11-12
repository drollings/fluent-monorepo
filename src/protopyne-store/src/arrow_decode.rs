//! One Arrow decode layer for both Parquet readers.
//!
//! The market ingest reader and the sealed-snapshot reader used to carry
//! their own copies of the same mechanics — column downcasts, schema
//! lookup, decimal-scale checking, and the string-offset variants — and the
//! copies had already diverged. This module is the single home for that
//! mechanics. Each reader keeps its distinct contract on top (tolerance,
//! ticker filter, restatement merge, pinned schema, digest checks) and its
//! own error taxonomy: every function here returns plain [`DecodeError`]
//! facts, and the readers map those into `StoreError` with their own words.
//!
//! Value conversion keeps the readers' rules verbatim: timestamps floor
//! sub-millisecond remainders toward the earlier millisecond, second stamps
//! that would overflow refuse, non-finite floats refuse, volumes must read
//! as whole share counts, and a decimal scale that cannot be named as a
//! `Decimal` scale refuses.

use arrow::array::{
    Array, Decimal128Array, Float32Array, Float64Array, Int32Array, Int64Array, LargeStringArray,
    StringArray, TimestampMicrosecondArray, TimestampMillisecondArray, TimestampNanosecondArray,
    TimestampSecondArray,
};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;
use rust_decimal::prelude::{FromPrimitive as _, ToPrimitive as _};
use rust_decimal::Decimal;

/// What went wrong resolving an Arrow column. Structured facts, not words:
/// the readers format these into their own taxonomies.
#[derive(Debug, Clone, PartialEq, Eq)]
/// What went wrong resolving an Arrow column. Structured facts, not words:
/// the readers format these into their own taxonomies.
pub(crate) enum DecodeError {
    /// A column resolved by position or name has an unexpected Arrow type.
    UnexpectedType {
        /// The column's actual type, as spelled by Arrow.
        actual: String,
    },
    /// A decimal scale cannot be named as a `Decimal` scale.
    BadScale {
        /// The column carrying the scale.
        column: String,
        /// The offending scale.
        scale: i8,
    },
}

/// The row's timestamp column, resolved once per file into its concrete
/// array type. Every variant reads to epoch milliseconds.
pub(crate) enum TimeColumn<'a> {
    Nanos(&'a TimestampNanosecondArray),
    Micros(&'a TimestampMicrosecondArray),
    Millis(&'a TimestampMillisecondArray),
    Seconds(&'a TimestampSecondArray),
    EpochMillis(&'a Int64Array),
}

impl TimeColumn<'_> {
    /// Whether the row holds no stamp.
    #[must_use]
    pub(crate) fn is_null(&self, row: usize) -> bool {
        match self {
            Self::Nanos(v) => v.is_null(row),
            Self::Micros(v) => v.is_null(row),
            Self::Millis(v) => v.is_null(row),
            Self::Seconds(v) => v.is_null(row),
            Self::EpochMillis(v) => v.is_null(row),
        }
    }

    /// The row's stamp in epoch milliseconds. A sub-millisecond stamp
    /// floors toward the earlier millisecond; a second stamp that would
    /// overflow refuses.
    pub(crate) fn millis(&self, row: usize) -> Result<i64, String> {
        match self {
            Self::Nanos(v) => Ok(v.value(row).div_euclid(1_000_000)),
            Self::Micros(v) => Ok(v.value(row).div_euclid(1_000)),
            Self::Millis(v) => Ok(v.value(row)),
            Self::Seconds(v) => v
                .value(row)
                .checked_mul(1_000)
                .ok_or_else(|| format!("timestamp {}s overflows milliseconds", v.value(row))),
            Self::EpochMillis(v) => Ok(v.value(row)),
        }
    }
}

/// The row's price column: float or decimal, resolved once.
pub(crate) enum PriceColumn<'a> {
    F64(&'a Float64Array),
    F32(&'a Float32Array),
    Decimal(&'a Decimal128Array, u32),
}

impl PriceColumn<'_> {
    /// Whether the row holds no price.
    #[must_use]
    pub(crate) fn is_null(&self, row: usize) -> bool {
        match self {
            Self::F64(v) => v.is_null(row),
            Self::F32(v) => v.is_null(row),
            Self::Decimal(v, _) => v.is_null(row),
        }
    }

    /// The row's price as an exact `Decimal`. A NaN or an infinity has no
    /// decimal form and refuses rather than silently becoming zero.
    pub(crate) fn decimal(&self, row: usize) -> Result<Decimal, String> {
        match self {
            Self::F64(v) => Decimal::from_f64(v.value(row))
                .ok_or_else(|| format!("price is not finite: {}", v.value(row))),
            Self::F32(v) => Decimal::from_f32(v.value(row))
                .ok_or_else(|| format!("price is not finite: {}", v.value(row))),
            Self::Decimal(v, scale) => Ok(Decimal::from_i128_with_scale(v.value(row), *scale)),
        }
    }
}

/// The row's volume column: any numeric encoding, resolved once.
pub(crate) enum VolumeColumn<'a> {
    F64(&'a Float64Array),
    F32(&'a Float32Array),
    I64(&'a Int64Array),
    I32(&'a Int32Array),
    Decimal(&'a Decimal128Array, u32),
}

impl VolumeColumn<'_> {
    /// Whether the row holds no volume.
    #[must_use]
    pub(crate) fn is_null(&self, row: usize) -> bool {
        match self {
            Self::F64(v) => v.is_null(row),
            Self::F32(v) => v.is_null(row),
            Self::I64(v) => v.is_null(row),
            Self::I32(v) => v.is_null(row),
            Self::Decimal(v, _) => v.is_null(row),
        }
    }

    /// The row's volume as whole shares. Vendor float volumes that land
    /// between integers round to the nearest share (ties away from zero):
    /// a fraction is vendor apportionment noise on a share count, not a
    /// distinct quantity, so refusing the row would strand the bar over
    /// dust. Non-finite values still refuse, and a negative or
    /// unrepresentable count is not servable, so those refuse as before.
    pub(crate) fn whole(&self, row: usize) -> Result<u64, String> {
        let value = match self {
            Self::F64(v) => Decimal::from_f64(v.value(row))
                .ok_or_else(|| format!("volume is not finite: {}", v.value(row)))?,
            Self::F32(v) => Decimal::from_f32(v.value(row))
                .ok_or_else(|| format!("volume is not finite: {}", v.value(row)))?,
            Self::I64(v) => Decimal::from(v.value(row)),
            Self::I32(v) => Decimal::from(v.value(row)),
            Self::Decimal(v, scale) => Decimal::from_i128_with_scale(v.value(row), *scale),
        };
        value
            .round()
            .to_u64()
            .ok_or_else(|| format!("volume {value} is negative or out of range"))
    }
}

/// The row's string column: 32- or 64-bit offsets (polars emits
/// `LargeUtf8`), resolved once.
#[derive(Clone, Copy)]
/// The row's string column: 32- or 64-bit offsets (polars emits
/// `LargeUtf8`), resolved once.
pub(crate) enum StringColumn<'a> {
    Utf8(&'a StringArray),
    Large(&'a LargeStringArray),
}

impl StringColumn<'_> {
    /// Whether the row holds no string.
    #[must_use]
    pub(crate) fn is_null(&self, row: usize) -> bool {
        match self {
            Self::Utf8(v) => v.is_null(row),
            Self::Large(v) => v.is_null(row),
        }
    }

    /// The row's string.
    #[must_use]
    pub(crate) fn value(&self, row: usize) -> &str {
        match self {
            Self::Utf8(v) => v.value(row),
            Self::Large(v) => v.value(row),
        }
    }
}

/// Find one column by any of its accepted spellings, case-insensitively.
#[must_use]
/// Find one column by any of its accepted spellings, case-insensitively.
pub(crate) fn find(schema: &Schema, names: &[&str]) -> Option<usize> {
    schema.fields().iter().position(|field| {
        let lowered = field.name().to_lowercase();
        names.iter().any(|name| lowered == *name)
    })
}

/// A decimal scale that cannot be named as a `Decimal` scale refuses.
pub(crate) fn checked_scale(scale: i8, column: &str) -> Result<u32, DecodeError> {
    u32::try_from(scale).map_err(|_| DecodeError::BadScale {
        column: column.to_owned(),
        scale,
    })
}

/// Read a named column's decimal scale from the schema: the field must
/// carry `Decimal128`, and its scale must check.
pub(crate) fn decimal_scale_of(schema: &Schema, column: &str) -> Result<u32, DecodeError> {
    match schema.field_with_name(column).map(Field::data_type) {
        Ok(DataType::Decimal128(_, scale)) => checked_scale(*scale, column),
        Ok(data_type) => Err(DecodeError::UnexpectedType {
            actual: data_type.to_string(),
        }),
        Err(_) => Err(DecodeError::UnexpectedType {
            actual: "absent".to_owned(),
        }),
    }
}

/// Downcast the positional column to the reader's pinned array type.
pub(crate) fn column_at<T: 'static>(batch: &RecordBatch, index: usize) -> Result<&T, DecodeError> {
    batch
        .column(index)
        .as_any()
        .downcast_ref::<T>()
        .ok_or_else(|| DecodeError::UnexpectedType {
            actual: batch.column(index).data_type().to_string(),
        })
}

/// Resolve the timestamp column into its concrete array type.
pub(crate) fn time_column_of(
    batch: &RecordBatch,
    index: usize,
) -> Result<TimeColumn<'_>, DecodeError> {
    let column = batch.column(index);
    let refuse = || DecodeError::UnexpectedType {
        actual: column.data_type().to_string(),
    };
    match column.data_type() {
        DataType::Timestamp(TimeUnit::Nanosecond, _) => column
            .as_any()
            .downcast_ref::<TimestampNanosecondArray>()
            .map(TimeColumn::Nanos)
            .ok_or_else(refuse),
        DataType::Timestamp(TimeUnit::Microsecond, _) => column
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .map(TimeColumn::Micros)
            .ok_or_else(refuse),
        DataType::Timestamp(TimeUnit::Millisecond, _) => column
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .map(TimeColumn::Millis)
            .ok_or_else(refuse),
        DataType::Timestamp(TimeUnit::Second, _) => column
            .as_any()
            .downcast_ref::<TimestampSecondArray>()
            .map(TimeColumn::Seconds)
            .ok_or_else(refuse),
        DataType::Int64 => column
            .as_any()
            .downcast_ref::<Int64Array>()
            .map(TimeColumn::EpochMillis)
            .ok_or_else(refuse),
        _ => Err(refuse()),
    }
}

/// Resolve a price column into its concrete array type. `name` rides the
/// scale refusal so the reader can name its column.
pub(crate) fn price_column_of<'a>(
    batch: &'a RecordBatch,
    index: usize,
    name: &str,
) -> Result<PriceColumn<'a>, DecodeError> {
    let column = batch.column(index);
    let refuse = || DecodeError::UnexpectedType {
        actual: column.data_type().to_string(),
    };
    match column.data_type() {
        DataType::Float64 => column
            .as_any()
            .downcast_ref::<Float64Array>()
            .map(PriceColumn::F64)
            .ok_or_else(refuse),
        DataType::Float32 => column
            .as_any()
            .downcast_ref::<Float32Array>()
            .map(PriceColumn::F32)
            .ok_or_else(refuse),
        DataType::Decimal128(_, scale) => {
            let scale = checked_scale(*scale, name)?;
            column
                .as_any()
                .downcast_ref::<Decimal128Array>()
                .map(|array| PriceColumn::Decimal(array, scale))
                .ok_or_else(refuse)
        }
        _ => Err(refuse()),
    }
}

/// Resolve the volume column into its concrete array type. `name` rides
/// the scale refusal so the reader can name its column.
pub(crate) fn volume_column_of<'a>(
    batch: &'a RecordBatch,
    index: usize,
    name: &str,
) -> Result<VolumeColumn<'a>, DecodeError> {
    let column = batch.column(index);
    let refuse = || DecodeError::UnexpectedType {
        actual: column.data_type().to_string(),
    };
    match column.data_type() {
        DataType::Float64 => column
            .as_any()
            .downcast_ref::<Float64Array>()
            .map(VolumeColumn::F64)
            .ok_or_else(refuse),
        DataType::Float32 => column
            .as_any()
            .downcast_ref::<Float32Array>()
            .map(VolumeColumn::F32)
            .ok_or_else(refuse),
        DataType::Int64 => column
            .as_any()
            .downcast_ref::<Int64Array>()
            .map(VolumeColumn::I64)
            .ok_or_else(refuse),
        DataType::Int32 => column
            .as_any()
            .downcast_ref::<Int32Array>()
            .map(VolumeColumn::I32)
            .ok_or_else(refuse),
        DataType::Decimal128(_, scale) => {
            let scale = checked_scale(*scale, name)?;
            column
                .as_any()
                .downcast_ref::<Decimal128Array>()
                .map(|array| VolumeColumn::Decimal(array, scale))
                .ok_or_else(refuse)
        }
        _ => Err(refuse()),
    }
}

/// Resolve a string column (32- or 64-bit offsets) into its concrete type.
pub(crate) fn string_column_of(
    batch: &RecordBatch,
    index: usize,
) -> Result<StringColumn<'_>, DecodeError> {
    let column = batch.column(index);
    let refuse = || DecodeError::UnexpectedType {
        actual: column.data_type().to_string(),
    };
    match column.data_type() {
        DataType::Utf8 => column
            .as_any()
            .downcast_ref::<StringArray>()
            .map(StringColumn::Utf8)
            .ok_or_else(refuse),
        DataType::LargeUtf8 => column
            .as_any()
            .downcast_ref::<LargeStringArray>()
            .map(StringColumn::Large)
            .ok_or_else(refuse),
        _ => Err(refuse()),
    }
}