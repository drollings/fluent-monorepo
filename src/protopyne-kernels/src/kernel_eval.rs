use std::sync::Arc;

use fluent_wvr::ArcIntern;

use crate::indicator::{Indicator, IndicatorResult};

pub(crate) fn kernel_depends(indicator: &Arc<dyn Indicator>) -> Vec<ArcIntern<str>> {
    indicator
        .spec()
        .depends
        .iter()
        .map(|d| ArcIntern::from(d.as_str()))
        .collect()
}

pub(crate) fn kernel_provides(indicator: &Arc<dyn Indicator>) -> Vec<ArcIntern<str>> {
    vec![ArcIntern::from(indicator.spec().id.as_str())]
}

pub(crate) fn kernel_message(id: &str, len: usize, warmed: usize) -> String {
    format!("{id} evaluated {len} bars ({warmed} warmed)")
}

pub(crate) fn warmed_count(result: &IndicatorResult) -> usize {
    result.values.iter().flatten().count()
}

#[cfg(test)]
#[path = "../tests/kernel_eval.rs"]
mod tests;
