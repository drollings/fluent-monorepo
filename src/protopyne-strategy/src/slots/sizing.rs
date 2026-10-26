//! Sizer slots: the existing sizing policies under one registry.
//!
//! The registry is a catalog over where the policies already live; it does
//! not re-implement a fraction, a floor, or a Kelly cap. A factory parses the
//! parameter bag and calls the policy's own constructor, so an invalid
//! fraction still refuses exactly where it always did.

#![allow(clippy::unnecessary_wraps)] // uniform factory signature
use std::sync::{Arc, OnceLock};

use protopyne_types::Money;
use rust_decimal::Decimal;

#[cfg(feature = "research")]
use crate::elements::FractionalKelly;
use crate::elements::{BuyingPower, FixedContracts, PercentNetLiq, Sizing, VolatilityScaled};
use crate::equity::EquitySizer;
use crate::error::StrategyError;
use crate::slots::{FnFactory, SlotError, SlotRegistry};

fn money(params: &serde_json::Value, key: &str, id: &'static str) -> Result<Money, SlotError> {
    let value = params
        .get(key)
        .ok_or_else(|| SlotError::invalid(id, format!("{key} is required")))?;
    let decimal = match value {
        serde_json::Value::Number(number) => number
            .as_f64()
            .and_then(Decimal::from_f64_retain)
            .ok_or_else(|| SlotError::invalid(id, format!("{key} is not a decimal")))?,
        serde_json::Value::String(text) => text
            .parse::<Decimal>()
            .map_err(|_| SlotError::invalid(id, format!("{key} is not a decimal")))?,
        _ => return Err(SlotError::invalid(id, format!("{key} is not a decimal"))),
    };
    Ok(Money::new(decimal))
}

fn number(params: &serde_json::Value, key: &str, id: &'static str) -> Result<f64, SlotError> {
    params
        .get(key)
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| SlotError::invalid(id, format!("{key} must be a number")))
}

fn count(params: &serde_json::Value, key: &str, id: &'static str) -> Result<u32, SlotError> {
    params
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| SlotError::invalid(id, format!("{key} must be a u32")))
}

#[allow(clippy::needless_pass_by_value)] // owned from map_err; the message is all that survives
fn translate(id: &'static str, error: StrategyError) -> SlotError {
    SlotError::invalid(id, error.to_string())
}

fn sizing_registry() -> &'static SlotRegistry<Arc<dyn Sizing>> {
    static REGISTRY: OnceLock<SlotRegistry<Arc<dyn Sizing>>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let mut registry = SlotRegistry::new();
        for (id, build) in [
            (
                "fixed",
                fixed_factory as fn(&serde_json::Value) -> Result<Arc<dyn Sizing>, SlotError>,
            ),
            ("percent_net_liq", percent_net_liq_factory),
            ("volatility_scaled", volatility_factory),
            ("buying_power", buying_power_factory),
            ("equity", equity_factory),
        ] {
            registry
                .register(Arc::new(FnFactory::new(id, build)))
                .expect("unique sizing id");
        }
        #[cfg(feature = "research")]
        registry
            .register(Arc::new(FnFactory::new(
                "fractional_kelly",
                fractional_kelly_factory,
            )))
            .expect("unique sizing id");
        registry
    })
}

fn fixed_factory(params: &serde_json::Value) -> Result<Arc<dyn Sizing>, SlotError> {
    Ok(Arc::new(FixedContracts {
        contracts: count(params, "contracts", "fixed")?,
    }))
}

fn percent_net_liq_factory(params: &serde_json::Value) -> Result<Arc<dyn Sizing>, SlotError> {
    Ok(Arc::new(
        PercentNetLiq::new(
            number(params, "fraction", "percent_net_liq")?,
            money(params, "per_unit_max_loss", "percent_net_liq")?,
        )
        .map_err(|error| translate("percent_net_liq", error))?,
    ))
}

fn volatility_factory(params: &serde_json::Value) -> Result<Arc<dyn Sizing>, SlotError> {
    Ok(Arc::new(VolatilityScaled {
        base: count(params, "base", "volatility_scaled")?,
        target_vol: number(params, "target_vol", "volatility_scaled")?,
        vol_floor: number(params, "vol_floor", "volatility_scaled")?,
    }))
}

fn buying_power_factory(params: &serde_json::Value) -> Result<Arc<dyn Sizing>, SlotError> {
    Ok(Arc::new(
        BuyingPower::new(
            number(params, "fraction", "buying_power")?,
            money(params, "per_unit_price", "buying_power")?,
        )
        .map_err(|error| translate("buying_power", error))?,
    ))
}

fn equity_factory(params: &serde_json::Value) -> Result<Arc<dyn Sizing>, SlotError> {
    let mode = params
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| SlotError::invalid("equity", "mode is required"))?;
    Ok(Arc::new(
        EquitySizer::new(
            mode,
            number(params, "risk_pct", "equity")?,
            number(params, "size_pct", "equity")?,
            number(params, "fixed_qty", "equity")?,
            number(params, "max_notional_pct", "equity")?,
        )
        .map_err(|error| translate("equity", error))?,
    ))
}

#[cfg(feature = "research")]
fn fractional_kelly_factory(params: &serde_json::Value) -> Result<Arc<dyn Sizing>, SlotError> {
    Ok(Arc::new(
        FractionalKelly::new(
            count(params, "base", "fractional_kelly")?,
            number(params, "fraction", "fractional_kelly")?,
            number(params, "cap", "fractional_kelly")?,
        )
        .map_err(|error| translate("fractional_kelly", error))?,
    ))
}

/// The sizer catalog.
#[must_use]
pub fn sizers() -> &'static SlotRegistry<Arc<dyn Sizing>> {
    sizing_registry()
}

/// The catalog ids, in schema order.
#[must_use]
pub fn sizing_ids() -> Vec<&'static str> {
    sizers().ids()
}
