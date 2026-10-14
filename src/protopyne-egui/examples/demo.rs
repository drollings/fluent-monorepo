//! Native terminal demo shell: a thin composition over [`ChartWorkspace`].
//!
//! Loads SPY history from the native CSV fixture, seeds the shared workspace,
//! and renders the workspace canvas (top bar, candles, hot-slot legend, and
//! the four-oscillator + delta sub-pane). No chart-rendering or indicator
//! math lives here: every pane comes from `paint_workspace` and every number
//! from the shared kernels, so the demo and the gateway-backed terminal draw
//! the same terminal.
//!
//! Usage: `cargo run -p protopyne-egui --features demo --example demo --
//! [HISTORY.csv]`, or pass `--check` to run the headless data path without
//! opening a window and print a summary.

use std::path::{Path, PathBuf};

use protopyne_chart_model::{ChartRow, rows_of};
use protopyne_egui::{ChartWorkspace, TimeframeKey, paint_workspace};
use protopyne_types::Bar;

fn history_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs/historical")
}

fn default_m15() -> PathBuf {
    history_dir().join("SPY_15M.csv")
}

fn load_bars(path: &Path) -> Result<Vec<Bar>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let bars = protopyne_server::parse_bars_csv(&text)
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

/// Native slot files the demo seeds, so `1M`/`5M`/`15M` switch the actual
/// loaded candles (`1H`/`4H` resample the `15M` slot inside the workspace).
fn native_slots(path: &Path) -> [(TimeframeKey, PathBuf); 3] {
    let dir = history_dir();
    [
        (TimeframeKey::M1, dir.join("SPY_1M.csv")),
        (TimeframeKey::M5, dir.join("SPY_5M.csv")),
        (TimeframeKey::M15, path.to_path_buf()),
    ]
}

/// The window shell: owns the workspace and paints one frame per pass.
struct DemoApp {
    workspace: ChartWorkspace,
}

impl DemoApp {
    fn new(path: &Path) -> Result<Self, String> {
        let mut workspace =
            ChartWorkspace::new().map_err(|e| format!("workspace refused: {e}"))?;
        for (timeframe, file) in native_slots(path) {
            let rows = load_rows(&file)?;
            workspace
                .seed_history(timeframe, rows)
                .map_err(|e| format!("seed {}: {e}", timeframe.label()))?;
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
    format!(
        "symbol={} timeframe={} bars={bars} slots=[{slots}] cached={}",
        workspace.active_symbol(),
        workspace.timeframe().label(),
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
