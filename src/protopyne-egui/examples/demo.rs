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

use std::path::PathBuf;

use protopyne_chart_model::rows_of;
use protopyne_egui::{ChartWorkspace, paint_workspace};
use protopyne_types::Bar;

fn history_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs/historical")
}

fn default_m15() -> PathBuf {
    history_dir().join("SPY_15M.csv")
}

fn load_bars(path: &PathBuf) -> Result<Vec<Bar>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let bars = protopyne_server::parse_bars_csv(&text)
        .map_err(|e| format!("parse {}: {e}", path.display()))?;
    if bars.is_empty() {
        return Err(format!("no bars in {}", path.display()));
    }
    Ok(bars)
}

/// The window shell: owns the workspace and paints one frame per pass.
struct DemoApp {
    workspace: ChartWorkspace,
}

impl DemoApp {
    fn new(path: &PathBuf) -> Result<Self, String> {
        let bars = load_bars(path)?;
        let converted = rows_of(&bars);
        let workspace = ChartWorkspace::with_rows(converted.rows)
            .map_err(|e| format!("workspace refused: {e}"))?;
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
