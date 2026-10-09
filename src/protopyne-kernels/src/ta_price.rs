//! Price-transform remainder: average, median, typical, and weighted-close price.
//!
//! Each kernel is a row-wise map over the legs it names — no warmup beyond
//! the first bar, no registry of hidden state. The arithmetic runs directly
//! on the `f64` legs in the reference's association order rather than
//! round-tripping through `Decimal` per bar: a per-bar `f64` → `Decimal` →
//! `f64` detour would fork rounding away from the `f64` reference the parity
//! suites check against, for no gain (the leg combinations are standard and
//! stated on each kernel). Missing legs refuse via [`Ohlcv::require`];
//! `na` legs poison only their own row.

use crate::error::KernelError;
use crate::indicator::{
    fin_or_none, BarTick, Indicator, IndicatorResult, IndicatorSpec, IndicatorState, Ohlcv, ParamSet,
};

/// Average price (`(open + high + low + close) / 4`).
pub struct AvgPriceIndicator {
    spec: IndicatorSpec,
}

impl AvgPriceIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "avgprice";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Average Price".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }
}

impl Default for AvgPriceIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for AvgPriceIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, _params: &ParamSet) -> Result<usize, KernelError> {
        Ok(1)
    }

    fn batch(&self, data: &Ohlcv<'_>, _params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let (open, high, low) = (
            data.require("open", data.open)?,
            data.require("high", data.high)?,
            data.require("low", data.low)?,
        );
        let values = open
            .iter()
            .zip(high.iter())
            .zip(low.iter())
            .zip(data.close.iter())
            .map(|(((&o, &h), &l), &c)| fin_or_none((o + h + l + c) / 4.0))
            .collect();
        Ok(IndicatorResult::new(Self::ID.to_owned(), values))
    }

    fn update(
        &self,
        _state: &mut IndicatorState,
        bar: BarTick,
        _params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        Ok(fin_or_none(
            (bar.open + bar.high + bar.low + bar.close) / 4.0,
        ))
    }
}

/// Median price (`(high + low) / 2`).
pub struct MedPriceIndicator {
    spec: IndicatorSpec,
}

impl MedPriceIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "medprice";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Median Price".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }
}

impl Default for MedPriceIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for MedPriceIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, _params: &ParamSet) -> Result<usize, KernelError> {
        Ok(1)
    }

    fn batch(&self, data: &Ohlcv<'_>, _params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let (high, low) = (
            data.require("high", data.high)?,
            data.require("low", data.low)?,
        );
        let values = high
            .iter()
            .zip(low.iter())
            .map(|(&h, &l)| fin_or_none(crate::math::midpoint_of(h, l)))
            .collect();
        Ok(IndicatorResult::new(Self::ID.to_owned(), values))
    }

    fn update(
        &self,
        _state: &mut IndicatorState,
        bar: BarTick,
        _params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        Ok(fin_or_none(crate::math::midpoint_of(bar.high, bar.low)))
    }
}

/// Typical price (`(high + low + close) / 3`).
pub struct TypPriceIndicator {
    spec: IndicatorSpec,
}

impl TypPriceIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "typprice";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Typical Price".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }
}

impl Default for TypPriceIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for TypPriceIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, _params: &ParamSet) -> Result<usize, KernelError> {
        Ok(1)
    }

    fn batch(&self, data: &Ohlcv<'_>, _params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let (high, low) = (
            data.require("high", data.high)?,
            data.require("low", data.low)?,
        );
        let values = high
            .iter()
            .zip(low.iter())
            .zip(data.close.iter())
            .map(|((&h, &l), &c)| fin_or_none((h + l + c) / 3.0))
            .collect();
        Ok(IndicatorResult::new(Self::ID.to_owned(), values))
    }

    fn update(
        &self,
        _state: &mut IndicatorState,
        bar: BarTick,
        _params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        Ok(fin_or_none((bar.high + bar.low + bar.close) / 3.0))
    }
}

/// Weighted close (`(high + low + close * 2) / 4`).
pub struct WclPriceIndicator {
    spec: IndicatorSpec,
}

impl WclPriceIndicator {
    /// Stable registry id.
    pub const ID: &'static str = "wclprice";

    /// Describe the kernel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: IndicatorSpec::new()
                .id(Self::ID.to_owned())
                .display_name("Weighted Close".to_owned())
                .warmup_bars(1_usize)
                .build(),
        }
    }
}

impl Default for WclPriceIndicator {
    fn default() -> Self {
        Self::new()
    }
}

impl Indicator for WclPriceIndicator {
    fn spec(&self) -> &IndicatorSpec {
        &self.spec
    }

    fn warmup(&self, _params: &ParamSet) -> Result<usize, KernelError> {
        Ok(1)
    }

    fn batch(&self, data: &Ohlcv<'_>, _params: &ParamSet) -> Result<IndicatorResult, KernelError> {
        let (high, low) = (
            data.require("high", data.high)?,
            data.require("low", data.low)?,
        );
        let values = high
            .iter()
            .zip(low.iter())
            .zip(data.close.iter())
            .map(|((&h, &l), &c)| fin_or_none((h + l + c * 2.0) / 4.0))
            .collect();
        Ok(IndicatorResult::new(Self::ID.to_owned(), values))
    }

    fn update(
        &self,
        _state: &mut IndicatorState,
        bar: BarTick,
        _params: &ParamSet,
    ) -> Result<Option<f64>, KernelError> {
        Ok(fin_or_none((bar.high + bar.low + bar.close * 2.0) / 4.0))
    }
}

#[cfg(test)]
#[path = "../tests/ta_price.rs"]
mod tests;
