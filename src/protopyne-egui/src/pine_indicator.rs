//! Pine-script indicator plugin: a catalogued `.pine` script executed by the
//! embedded Pine interpreter, one `IndicatorSeries` per captured plot.
//!
//! This is the testing/porting surface for Pine parity — explicitly NOT the
//! research or production path. The simple backtester, paper, and live all
//! run native kernels; only a slot the operator fills from a `pine:` catalog
//! id evaluates here, and only in the chart pane. A script that refuses to
//! link, parse, or run refuses the slot with the engine's reason instead of
//! painting invented values. Manifest parity is NOT a gate on this path:
//! it guards silent native substitution (which still applies first),
//! while this plugin always runs the actual script text.
//!
//! Linking mirrors the strategy harness loader: the indicator file plus
//! every library its imports name, keyed by the import declaration's own
//! publisher/name/version. Plots publish under `{id}#p{i}` nodes (index
//! order is the engine's first-seen order, stable per script) while the plan
//! registers the single plugin id — consumer chaining into Pine plots is
//! out of scope, Pine outputs feed paint and readouts. Warmup-skipped bars
//! pad with holes so stored series keep full bar length.

use std::collections::HashMap;

use fluent_egui::{cache_key, EguiError, SlotAttachment};
use fluent_wvr::component_downcast_ref;
use protopyne_chart_model::ChartRow;
use protopyne_kernels::{
    parse_pine_source, parse_program, resolve_imports, LibraryId, LinkedModule, ModuleInput,
    PineEngine, PineRuntimeConfig, PineValue,
};
use protopyne_types::Bar;

use crate::indicators::{wrong_config, ChartIndicator, IndicatorEvaluation, IndicatorSeries};
use crate::topology::SeriesCatalog as ChartSeriesCatalog;

/// Per-plot theme style keys, cycled so multi-plot scripts paint distinct
/// lines. Every key names a real theme field (see `ThemeConfig`), so an
/// unknown key can never refuse a paint.
const PLOT_STYLE_KEYS: [&str; 10] = [
    "fast", "slow", "bull", "bear", "rsi", "cci", "mfi", "stoch", "aroon", "adx",
];

/// One linked Pine script, ready to evaluate over chart bars.
#[derive(Debug)]
pub struct PineIndicatorPlugin {
    /// Catalog id (`pine:<stem>`).
    id: String,
    /// Display title.
    title: String,
    /// Where the plots paint (from the script's `overlay` header flag).
    attachment: SlotAttachment,
    /// Linked modules in dependency order.
    modules: Vec<LinkedModule>,
}

impl PineIndicatorPlugin {
    /// Link `script_text` (the indicator file) against `libraries`
    /// (`relpath` → text). Import declarations name their own
    /// publisher/name/version; a library resolves by filename stem.
    pub fn link(
        id: &str,
        title: &str,
        script_text: &str,
        libraries: &HashMap<String, String>,
    ) -> Result<Self, EguiError> {
        let source = parse_pine_source(id, script_text);
        let program = parse_program(script_text).map_err(|error| EguiError::InvalidParam {
            what: "pine indicator".to_owned(),
            reason: format!("{id} does not parse: {error}"),
        })?;
        let indicator = ModuleInput::new(
            LibraryId {
                publisher: "local".to_owned(),
                name: stem_of(id).to_owned(),
                version: source
                    .version
                    .unwrap_or(protopyne_kernels::SUPPORTED_PINE_VERSION),
            },
            source,
            program,
        );
        let mut inputs = vec![indicator];
        // Transitive closure over library imports: a library's own imports
        // resolve the same way the indicator's do, keyed by the full
        // publisher/name/version the import declaration names (one file may
        // serve several declared versions as separate namespaces).
        let mut seen = std::collections::HashSet::new();
        let mut queue: Vec<LibraryId> = parse_pine_source(id, script_text)
            .imports
            .iter()
            .map(|import| import.library.clone())
            .collect();
        while let Some(decl) = queue.pop() {
            if !seen.insert(decl.to_string()) {
                continue;
            }
            let file = libraries
                .iter()
                .find(|(path, _)| library_stem(path) == decl.name)
                .ok_or_else(|| EguiError::InvalidParam {
                    what: "pine indicator".to_owned(),
                    reason: format!("{id} imports unknown library '{}'", decl.name),
                })?;
            let library_source = parse_pine_source(file.0, file.1);
            let program = parse_program(file.1).map_err(|error| EguiError::InvalidParam {
                what: "pine indicator".to_owned(),
                reason: format!("library {} does not parse: {error}", decl.name),
            })?;
            queue.extend(
                library_source
                    .imports
                    .iter()
                    .map(|import| import.library.clone()),
            );
            inputs.push(ModuleInput::new(decl, library_source, program));
        }
        let graph = resolve_imports(inputs).map_err(|error| EguiError::InvalidParam {
            what: "pine indicator".to_owned(),
            reason: format!("{id} imports do not link: {error}"),
        })?;
        let modules = graph
            .order()
            .iter()
            .map(|mid| {
                let entry = graph.get(mid).expect("ordered module resolves");
                let aliases = graph
                    .alias_table(mid)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(alias, target)| (alias, target.to_string()))
                    .collect();
                LinkedModule::new(&mid.to_string(), entry.program.clone(), aliases)
            })
            .collect();
        Ok(Self {
            id: id.to_owned(),
            title: title.to_owned(),
            attachment: if pine_overlay_of(script_text) {
                SlotAttachment::MainOverlay
            } else {
                SlotAttachment::SubPane
            },
            modules,
        })
    }
}

/// Stem behind a `pine:<stem>` catalog id.
fn stem_of(id: &str) -> &str {
    id.strip_prefix("pine:").unwrap_or(id)
}

/// Locate the Pine indicator tree for a history path: the first ancestor
/// (the path itself when it is a directory, else its parent) containing an
/// `indicators/` subdirectory, searched upward. `None` means no Pine
/// catalog — the terminal simply offers no Pine scripts.
#[must_use]
pub fn pine_data_root(history: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut dir = if history.is_dir() {
        history.to_path_buf()
    } else {
        history.parent()?.to_path_buf()
    };
    for _ in 0..3 {
        if dir.join("indicators").is_dir() {
            return Some(dir);
        }
        dir = dir.parent()?.to_path_buf();
    }
    None
}

/// Stem behind an indicator file path (`indicators/_Foo.pine` serves
/// `_Foo`, the catalog stem — the underscore is kept, unlike libraries).
#[must_use]
pub(crate) fn pine_stem_of(relpath: &str) -> &str {
    let file = relpath.rsplit('/').next().unwrap_or(relpath);
    file.strip_suffix(".pine").unwrap_or(file)
}

/// Library stem behind a `libraries/<name>.pine` path: one leading
/// underscore is the on-disk convention (`_libUtil.pine` serves `libUtil`).
fn library_stem(relpath: &str) -> &str {
    let file = relpath.rsplit('/').next().unwrap_or(relpath);
    let stem = file.strip_suffix(".pine").unwrap_or(file);
    stem.strip_prefix('_').unwrap_or(stem)
}

/// Whether the script's `indicator(...)` header requests the main chart.
/// A named `overlay = true/false` wins; else a positional third `true`/`false`
/// literal wins (the reference scripts use both spellings); absent or
/// unparseable means the Pine default, overlay off. Legacy `study(...)`
/// headers read the same way.
#[must_use]
pub fn pine_overlay_of(text: &str) -> bool {
    let header = text
        .find("indicator(")
        .map(|index| (index, "indicator(".len()))
        .into_iter()
        .chain(text.find("study(").map(|index| (index, "study(".len())))
        .min_by_key(|(index, _)| *index);
    let Some((start, len)) = header else {
        return false;
    };
    let mut depth = 0_usize;
    let mut current = String::new();
    let mut args: Vec<String> = Vec::new();
    let mut in_string: Option<char> = None;
    let mut escaped = false;
    for char in text[start + len..].chars() {
        if let Some(quote) = in_string {
            current.push(char);
            if escaped {
                escaped = false;
            } else if char == '\\' {
                escaped = true;
            } else if char == quote {
                in_string = None;
            }
            continue;
        }
        match char {
            '"' | '\'' => {
                in_string = Some(char);
                current.push(char);
            }
            '(' => {
                depth += 1;
                current.push(char);
            }
            ')' => {
                if depth == 0 {
                    args.push(current);
                    break;
                }
                depth -= 1;
                current.push(char);
            }
            ',' if depth == 0 => {
                args.push(std::mem::take(&mut current));
            }
            _ => current.push(char),
        }
    }
    for (index, arg) in args.iter().enumerate() {
        let arg = arg.trim();
        if let Some((name, value)) = arg.split_once('=') {
            if name.trim() == "overlay" {
                return value.trim().starts_with("true");
            }
            continue;
        }
        // Positional `indicator(title, shorttitle, overlay, ...)`: the
        // third slot carries the flag when it is a bare literal.
        if index == 2 {
            if arg == "true" {
                return true;
            }
            if arg == "false" {
                return false;
            }
        }
    }
    false
}

/// Chart rows as kernel bars for the engine: fail-closed on the first
/// non-finite leg or unrepresentable timestamp, naming the bar.
fn rows_to_bars(rows: &[ChartRow]) -> Result<Vec<Bar>, EguiError> {
    // Display floats cross into kernel bars through the one sanctioned
    // helper — render code never names `Decimal` itself (budget pin).
    let display: Vec<protopyne_kernels::DisplayBar> = rows
        .iter()
        .map(|row| protopyne_kernels::DisplayBar {
            time: row.time,
            open: row.open,
            high: row.high,
            low: row.low,
            close: row.close,
            volume: row.volume,
        })
        .collect();
    protopyne_kernels::bars_from_display(&display).map_err(|error| EguiError::InvalidParam {
        what: "pine indicator".to_owned(),
        reason: error.to_string(),
    })
}

/// A captured plot value as a display float: numbers pass, everything else
/// (including Pine `na`) is a hole.
fn plot_number(value: Option<&PineValue>) -> Option<f64> {
    match value {
        Some(PineValue::Float(number)) => Some(*number),
        Some(PineValue::Int(number)) => Some(*number as f64),
        Some(PineValue::Bool(flag)) => Some(if *flag { 1.0 } else { 0.0 }),
        _ => None,
    }
}

impl ChartIndicator for PineIndicatorPlugin {
    fn id(&self) -> &str {
        &self.id
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn attachment(&self) -> SlotAttachment {
        self.attachment
    }

    fn default_config(&self) -> Box<dyn fluent_wvr::Component> {
        Box::new(PineRuntimeConfig::new().build())
    }

    fn plan_nodes(&self, _config: &dyn fluent_wvr::Component) -> Result<Vec<String>, EguiError> {
        // One stable node for the slot: per-plot series publish under
        // `{id}#p{i}` at evaluate time. Consumer chaining into Pine plots is
        // out of scope — Pine outputs feed paint and readouts.
        Ok(vec![self.id.clone()])
    }

    fn config_key(&self, config: &dyn fluent_wvr::Component) -> Result<u64, EguiError> {
        let config = component_downcast_ref::<PineRuntimeConfig>(config)
            .ok_or_else(|| wrong_config(&self.id, "PineRuntimeConfig"))?;
        Ok(common_core::hash::fingerprint_json(config))
    }

    fn evaluate(
        &self,
        rows: &[ChartRow],
        config: &dyn fluent_wvr::Component,
        _series: &ChartSeriesCatalog,
    ) -> Result<crate::indicators::IndicatorEvaluation, EguiError> {
        let config = component_downcast_ref::<PineRuntimeConfig>(config)
            .ok_or_else(|| wrong_config(&self.id, "PineRuntimeConfig"))?;
        let bars = rows_to_bars(rows)?;
        let mut engine = PineEngine::new(config.clone());
        let outcome = engine
            .run_linked(&self.modules, &bars, protopyne_kernels::InputSet::new())
            .map_err(|error| EguiError::InvalidParam {
                what: "pine indicator".to_owned(),
                reason: format!("{} refused to run: {error}", self.id),
            })?;
        let fingerprint = common_core::hash::fingerprint_json(config);
        // Values cover the evaluated tail; leading warmup-skipped bars pad
        // with holes so stored series keep full bar length.
        let pad = bars.len().saturating_sub(outcome.evaluated);
        let series = outcome
            .plots
            .iter()
            .enumerate()
            .filter(|(_, plot)| plot.display != Some(PineValue::Int(0)))
            .map(|(index, plot)| {
                let mut values = vec![None; pad];
                values.extend(plot.values.iter().map(|value| plot_number(value.as_ref())));
                let node = format!("{}#p{index}", self.id);
                IndicatorSeries {
                    node: node.clone(),
                    name: if plot.title.trim().is_empty() {
                        format!("plot {index}")
                    } else {
                        plot.title.clone()
                    },
                    values: values.clone(),
                    display_values: values,
                    display_mapping: None,
                    style: PLOT_STYLE_KEYS[index % PLOT_STYLE_KEYS.len()].to_owned(),
                    attachment: self.attachment,
                    key: cache_key(&node, fingerprint),
                    pane: None,
                }
            })
            .collect();
        // Non-series visuals ride the shared overlay projection: levels,
        // markers, labels, and areas paint beside the series, while a
        // refusal (plotcandle, barcolor, fill, an unparseable drawing)
        // refuses the whole overlay and surfaces as an on-pane chip.
        let overlay = protopyne_kernels::project(&outcome).map(|projection| {
            projection
                .primitives
                .into_iter()
                .filter(|primitive| {
                    !matches!(primitive, protopyne_kernels::PlotPrimitive::Series { .. })
                })
                .collect()
        });
        Ok(IndicatorEvaluation { series, overlay })
    }

    fn paint_main_overlay(
        &self,
        painter: &egui::Painter,
        evaluation: &crate::indicators::IndicatorEvaluation,
        viewport: &fluent_egui::Viewport,
        origin: egui::Pos2,
        base: usize,
        width_px: f32,
        theme: &fluent_egui::ThemeConfig,
        _config: &dyn fluent_wvr::Component,
        style: &fluent_egui::IndicatorStyle,
    ) -> Result<(), EguiError> {
        crate::indicators::paint_series(
            painter, evaluation, viewport, origin, base, width_px, theme, style,
        )?;
        Self::paint_pine_overlay(painter, evaluation, viewport, origin, width_px, theme)
    }

    fn paint_sub_pane(
        &self,
        painter: &egui::Painter,
        evaluation: &crate::indicators::IndicatorEvaluation,
        viewport: &fluent_egui::Viewport,
        origin: egui::Pos2,
        base: usize,
        width_px: f32,
        theme: &fluent_egui::ThemeConfig,
        _config: &dyn fluent_wvr::Component,
        style: &fluent_egui::IndicatorStyle,
    ) -> Result<(), EguiError> {
        crate::indicators::paint_series(
            painter, evaluation, viewport, origin, base, width_px, theme, style,
        )?;
        Self::paint_pine_overlay(painter, evaluation, viewport, origin, width_px, theme)
    }
}

impl PineIndicatorPlugin {
    /// Paint the projected drawings beside the series: levels, markers,
    /// labels, and areas through the shared overlay blitter; a refused
    /// projection renders its reason as red on-pane text instead of silent
    /// absence.
    fn paint_pine_overlay(
        painter: &egui::Painter,
        evaluation: &crate::indicators::IndicatorEvaluation,
        viewport: &fluent_egui::Viewport,
        origin: egui::Pos2,
        width_px: f32,
        theme: &fluent_egui::ThemeConfig,
    ) -> Result<(), EguiError> {
        match &evaluation.overlay {
            Ok(primitives) if !primitives.is_empty() => {
                let geoms = crate::pine_overlay::translate_overlay(
                    crate::pine_overlay::overlay_geoms(primitives, viewport),
                    (origin.x, origin.y),
                );
                crate::pine_overlay::paint_overlay(painter, &geoms, theme, width_px)
            }
            Err(refusal) => {
                painter.text(
                    origin,
                    egui::Align2::LEFT_TOP,
                    format!("pine overlay unsupported: {refusal}"),
                    egui::FontId::proportional(11.0),
                    egui::Color32::RED,
                );
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
#[path = "../tests/pine_indicator.rs"]
mod tests;
