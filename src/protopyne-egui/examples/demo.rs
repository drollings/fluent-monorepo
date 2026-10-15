//! Native terminal demo shell: a thin composition over [`ChartWorkspace`].
//!
//! Loads history from the native fixtures and seeds the shared workspace: a
//! CSV set (per-timeframe files), or one Parquet file read as-is and tallied
//! into every toolbar frame on load. No chart-rendering or indicator math
//! lives here: every pane comes from `paint_workspace` and every number from
//! the shared kernels, so the demo and the gateway-backed terminal draw the
//! same terminal.
//!
//! Usage: `cargo run -p protopyne-egui --features demo --example demo --
//! [HISTORY.csv|HISTORY.parquet]`, or pass `--check` to run the headless
//! data path without opening a window and print a summary.

use std::path::{Path, PathBuf};

use protopyne_chart_model::{ChartRow, rows_of};
use protopyne_egui::{ChartWorkspace, TimeframeKey, paint_workspace};
use protopyne_types::{Bar, Timeframe};

fn history_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs/historical")
}

fn default_m15() -> PathBuf {
    history_dir().join("SPY_15M.csv")
}

fn is_parquet(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("parquet")
}

/// Load a history file through the server's one entry point (`.parquet` or
/// `.csv` by extension, anything else refused). An empty file is still the
/// demo's own refusal: the loader answers empty, the shell decides that
/// means nothing to draw.
fn load_bars(path: &Path) -> Result<Vec<Bar>, String> {
    let bars = protopyne_server::load_bars_any(path)
        .map_err(|e| format!("parse {}: {e}", path.display()))?;
    if bars.is_empty() {
        return Err(format!("no bars in {}", path.display()));
    }
    Ok(bars)
}

/// Load one CSV as display rows (the workspace's native currency).
fn load_rows(path: &Path) -> Result<Vec<ChartRow>, String> {
    Ok(rows_of(&load_bars(path)?).rows)
}

/// Native slot files the demo seeds from CSV, so `1M`/`5M`/`15M` switch the
/// actual loaded candles (`1H`/`4H`/`1D` resample the `15M` slot inside the
/// workspace).
fn native_slots(path: &Path) -> [(TimeframeKey, PathBuf); 3] {
    let dir = history_dir();
    [
        (TimeframeKey::M1, dir.join("SPY_1M.csv")),
        (TimeframeKey::M5, dir.join("SPY_5M.csv")),
        (TimeframeKey::M15, path.to_path_buf()),
    ]
}

/// The toolbar key behind a store-grid timeframe.
fn key_of(timeframe: Timeframe) -> Option<TimeframeKey> {
    TimeframeKey::all()
        .into_iter()
        .find(|key| key.timeframe() == timeframe)
}

/// Seed every frame the base can reach from one file: the base is read as-is
/// and every whole-multiple coarser target is tallied on load through the
/// shared bucket-anchored fold. Frames finer than the base stay unseeded
/// (they cannot be synthesized); `1D` is seeded for a one-minute base.
fn seed_base_frames(
    workspace: &mut ChartWorkspace,
    base: TimeframeKey,
    path: &Path,
) -> Result<(), String> {
    let bars = load_bars(path)?;
    let base_grid = base.timeframe();
    let targets: Vec<Timeframe> = TimeframeKey::all()
        .into_iter()
        .map(TimeframeKey::timeframe)
        .filter(|target| {
            *target == base_grid || target.duration_ms() % base_grid.duration_ms() == 0
        })
        .collect();
    let frames = protopyne_server::fold_frames(&bars, base_grid, &targets)
        .map_err(|e| format!("tally {}: {e}", path.display()))?;
    for (timeframe, frame) in frames {
        let key =
            key_of(timeframe).ok_or_else(|| format!("no toolbar slot for {timeframe:?}"))?;
        let rows = rows_of(&frame).rows;
        workspace
            .seed_history(key, rows)
            .map_err(|e| format!("seed {}: {e}", key.label()))?;
    }
    Ok(())
}

/// The window shell: owns the workspace and paints one frame per pass.
struct DemoApp {
    workspace: ChartWorkspace,
}

impl DemoApp {
    fn new(path: &Path) -> Result<Self, String> {
        let mut workspace =
            ChartWorkspace::new().map_err(|e| format!("workspace refused: {e}"))?;
        if is_parquet(path) {
            seed_base_frames(&mut workspace, TimeframeKey::M1, path)?;
        } else {
            for (timeframe, file) in native_slots(path) {
                let rows = load_rows(&file)?;
                workspace
                    .seed_history(timeframe, rows)
                    .map_err(|e| format!("seed {}: {e}", timeframe.label()))?;
            }
        }
        Ok(Self { workspace })
    }
}

impl eframe::App for DemoApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Err(error) = paint_workspace(ui, &mut self.workspace, None) {
            ui.colored_label(egui::Color32::RED, format!("workspace refused: {error}"));
        }
    }
}

fn summary(workspace: &ChartWorkspace) -> String {
    let bars = workspace.rows().len();
    let slots = workspace.slot_ids().join(",");
    let frames: Vec<&str> = workspace
        .loaded_timeframes()
        .iter()
        .map(|key| key.label())
        .collect();
    format!(
        "symbol={} timeframe={} bars={bars} frames=[{}] slots=[{slots}] cached={}",
        workspace.active_symbol(),
        workspace.timeframe().label(),
        frames.join(","),
        workspace.cached_series(),
    )
}

fn main() {
    let mut path: Option<PathBuf> = None;
    let mut check = false;
    for arg in std::env::args().skip(1) {
        if arg == "--check" {
            check = true;
        } else {
            path = Some(PathBuf::from(arg));
        }
    }
    let path = path.unwrap_or_else(default_m15);
    if check {
        let app = match DemoApp::new(&path) {
            Ok(app) => app,
            Err(e) => {
                eprintln!("protopyne-gui: {e}");
                std::process::exit(1);
            }
        };
        println!("protopyne-gui check: {}", summary(&app.workspace));
        return;
    }
    let app = match DemoApp::new(&path) {
        Ok(app) => app,
        Err(e) => {
            eprintln!("protopyne-gui: {e}");
            std::process::exit(1);
        }
    };
    let options = eframe::NativeOptions::default();
    let run = eframe::run_native(
        "protopyne terminal",
        options,
        Box::new(|_cc| Ok::<_, Box<dyn std::error::Error + Send + Sync>>(Box::new(app))),
    );
    if let Err(e) = run {
        eprintln!("protopyne-gui: window refused: {e}");
        std::process::exit(1);
    }
}
