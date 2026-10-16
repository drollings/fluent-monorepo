//! Shared canonical-golden dumper for the headless Pine proof suites.
//!
//! One home for the golden dialect (`dump_value`'s `na` encoding, key order,
//! float text) so the IntegrationLite suite and the task-indicator suite
//! cannot drift into incompatible formats. Both suites call these helpers;
//! neither keeps a private copy. Output is byte-identical across reruns of
//! identical inputs; checked-in files compare textually (exact float text,
//! hence within kernel tolerance by construction).

use std::path::Path;

use protopyne_kernels::{BarSnapshot, EnumHandle, PineValue, RunOutcome};

/// Deterministic JSON for one script value (`na` and non-finite floats
/// both read `null`; maps and objects sort by key). Takes the reference
/// form because every call site holds a borrowed slot.
#[allow(clippy::ref_option)]
pub fn dump_value(value: &Option<PineValue>) -> serde_json::Value {
    match value {
        None => serde_json::Value::Null,
        Some(PineValue::Float(v)) => serde_json::Number::from_f64(*v)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Some(PineValue::Int(v)) => serde_json::Value::Number((*v).into()),
        Some(PineValue::Bool(v)) => serde_json::Value::Bool(*v),
        Some(PineValue::Str(v)) => serde_json::Value::String(v.clone()),
        Some(PineValue::ColorHandle(v)) => serde_json::Value::Number(v.raw().into()),
        Some(PineValue::EnumHandle(EnumHandle { enum_name, member })) => {
            serde_json::Value::String(format!("{enum_name}.{member}"))
        }
        Some(PineValue::Array(items)) => {
            serde_json::Value::Array(items.iter().map(dump_value).collect())
        }
        Some(PineValue::IntMap(entries)) => {
            let mut keys: Vec<i64> = entries.keys().copied().collect();
            keys.sort_unstable();
            serde_json::Value::Object(
                keys.into_iter()
                    .map(|k| (k.to_string(), serde_json::Value::Number(entries[&k].into())))
                    .collect(),
            )
        }
        Some(PineValue::Map(entries)) => serde_json::Value::Object(
            entries
                .iter()
                .map(|(key, (_, value))| (key.clone(), dump_value(value)))
                .collect(),
        ),
        Some(PineValue::Object(instance)) => {
            let mut keys: Vec<&String> = instance.fields.keys().collect();
            keys.sort_unstable();
            serde_json::Value::Object(
                keys.into_iter()
                    .map(|k| (k.clone(), dump_value(&instance.fields[k])))
                    .collect(),
            )
        }
        Some(PineValue::Drawing(handle)) => {
            serde_json::Value::String(format!("{:?}#{}", handle.kind, handle.id))
        }
        Some(PineValue::Matrix(grid)) => serde_json::json!({
            "rows": grid.rows,
            "cols": grid.cols,
            "cells": grid.cells.iter().map(dump_value).collect::<Vec<_>>(),
        }),
    }
}

/// Canonical dump of snapshots plus every capture: key-sorted JSON whose
/// text form is byte-identical across reruns of identical inputs.
pub fn dump_canonical(outcome: &RunOutcome) -> String {
    let snapshots: Vec<serde_json::Value> = outcome
        .snapshots
        .iter()
        .map(|BarSnapshot { bar, values }| {
            let mut keys: Vec<&String> = values.keys().collect();
            keys.sort_unstable();
            serde_json::json!({
                "bar": bar,
                "values": keys
                    .into_iter()
                    .map(|k| (k.clone(), dump_value(&values[k])))
                    .collect::<serde_json::Map<_, _>>(),
            })
        })
        .collect();
    let plots: Vec<serde_json::Value> = outcome
        .plots
        .iter()
        .map(|plot| {
            serde_json::json!({
                "title": plot.title,
                "display": dump_value(&plot.display),
                "values": plot.values.iter().map(dump_value).collect::<Vec<_>>(),
            })
        })
        .collect();
    let candles: Vec<serde_json::Value> = outcome
        .plotcandles
        .iter()
        .map(|candle| {
            serde_json::json!({
                "title": candle.title,
                "display": dump_value(&candle.display),
                "color": dump_value(&candle.color),
                "wickcolor": dump_value(&candle.wickcolor),
                "bordercolor": dump_value(&candle.bordercolor),
                "opens": candle.opens.iter().map(dump_value).collect::<Vec<_>>(),
                "highs": candle.highs.iter().map(dump_value).collect::<Vec<_>>(),
                "lows": candle.lows.iter().map(dump_value).collect::<Vec<_>>(),
                "closes": candle.closes.iter().map(dump_value).collect::<Vec<_>>(),
            })
        })
        .collect();
    let barcolors: Vec<serde_json::Value> = outcome
        .barcolors
        .iter()
        .map(|paint| {
            serde_json::json!({
                "title": paint.title,
                "values": paint.values.iter().map(dump_value).collect::<Vec<_>>(),
            })
        })
        .collect();
    let fills: Vec<serde_json::Value> = outcome
        .fills
        .iter()
        .map(|fill| {
            serde_json::json!({
                "title": fill.title,
                "first": fill.first,
                "second": fill.second,
                "first_title": fill.first_title,
                "second_title": fill.second_title,
                "color": dump_value(&fill.color),
                "values": fill.values.iter().map(dump_value).collect::<Vec<_>>(),
            })
        })
        .collect();
    let events: Vec<serde_json::Value> = outcome
        .draw_events
        .iter()
        .map(|event| {
            serde_json::json!({
                "bar": event.bar,
                "kind": format!("{:?}", event.kind),
                "object": event.object,
                "key": event.key,
            })
        })
        .collect();
    serde_json::json!({
        "evaluated": outcome.evaluated,
        "snapshots": snapshots,
        "plots": plots,
        "plotcandles": candles,
        "barcolors": barcolors,
        "fills": fills,
        "draw_events": events,
    })
    .to_string()
}

/// Slim golden dump: the caller's pinned series over the full run plus
/// every capture. Snapshots ride the in-memory determinism check instead —
/// they carry whole engine state (bars, contexts, scheduler maps) that
/// would bloat a checked-in file without adding proof value.
pub fn dump_slim(outcome: &RunOutcome, pinned: &[&str]) -> String {
    let series: serde_json::Map<String, serde_json::Value> = pinned
        .iter()
        .map(|name| {
            (
                name.to_string(),
                serde_json::Value::Array(
                    outcome
                        .snapshots
                        .iter()
                        .map(|snapshot| {
                            snapshot
                                .values
                                .get(*name)
                                .map_or(serde_json::Value::Null, dump_value)
                        })
                        .collect(),
                ),
            )
        })
        .collect();
    let plots: Vec<serde_json::Value> = outcome
        .plots
        .iter()
        .map(|plot| {
            serde_json::json!({
                "title": plot.title,
                "display": dump_value(&plot.display),
                "values": plot.values.iter().map(dump_value).collect::<Vec<_>>(),
            })
        })
        .collect();
    let candles: Vec<serde_json::Value> = outcome
        .plotcandles
        .iter()
        .map(|candle| {
            serde_json::json!({
                "title": candle.title,
                "display": dump_value(&candle.display),
                "color": dump_value(&candle.color),
                "wickcolor": dump_value(&candle.wickcolor),
                "bordercolor": dump_value(&candle.bordercolor),
                "opens": candle.opens.iter().map(dump_value).collect::<Vec<_>>(),
                "highs": candle.highs.iter().map(dump_value).collect::<Vec<_>>(),
                "lows": candle.lows.iter().map(dump_value).collect::<Vec<_>>(),
                "closes": candle.closes.iter().map(dump_value).collect::<Vec<_>>(),
            })
        })
        .collect();
    let barcolors: Vec<serde_json::Value> = outcome
        .barcolors
        .iter()
        .map(|paint| {
            serde_json::json!({
                "title": paint.title,
                "values": paint.values.iter().map(dump_value).collect::<Vec<_>>(),
            })
        })
        .collect();
    let fills: Vec<serde_json::Value> = outcome
        .fills
        .iter()
        .map(|fill| {
            serde_json::json!({
                "title": fill.title,
                "first": fill.first,
                "second": fill.second,
                "first_title": fill.first_title,
                "second_title": fill.second_title,
                "color": dump_value(&fill.color),
                "values": fill.values.iter().map(dump_value).collect::<Vec<_>>(),
            })
        })
        .collect();
    let events: Vec<serde_json::Value> = outcome
        .draw_events
        .iter()
        .map(|event| {
            serde_json::json!({
                "bar": event.bar,
                "kind": format!("{:?}", event.kind),
                "object": event.object,
                "key": event.key,
            })
        })
        .collect();
    serde_json::json!({
        "evaluated": outcome.evaluated,
        "series": series,
        "plots": plots,
        "plotcandles": candles,
        "barcolors": barcolors,
        "fills": fills,
        "draw_events": events,
    })
    .to_string()
}

/// Record or compare the checked-in golden at `path` for one run. With
/// `PROTOPYNE_RECORD=1` the file regenerates and the test passes
/// trivially; otherwise the slim dump must match textually (exact
/// float text, hence within kernel tolerance by construction). A missing
/// file fails proving nothing: the suite records first, never asserts
/// against absence.
pub fn check_golden(path: &Path, outcome: &RunOutcome, pinned: &[&str]) {
    let dump = dump_slim(outcome, pinned);
    if std::env::var("PROTOPYNE_RECORD").is_ok() {
        std::fs::write(path, &dump).expect("golden records");
        return;
    }
    let recorded = std::fs::read_to_string(path)
        .unwrap_or_else(|_| panic!("golden {} records first", path.display()));
    assert_eq!(dump, recorded, "rerun matches the checked-in golden");
}
