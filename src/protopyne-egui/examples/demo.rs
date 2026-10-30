//! Chart-shell example (demo): a thin composition over [`ChartWorkspace`].
//!
//! This is the chart shell, not the trading demo: it paints candles,
//! indicators, and the oscillator pane over seeded history, with no gateway,
//! no portfolio, and no arming form. For the trading demo (gateway-backed
//! views + workspace canvas + grant form + replay, no server boot), run the
//! `terminal` example with `--mock` instead.
//!
//!
//! History arrives as one CLI argument with two shapes: a directory opens as
//! a history root (`TICKER/YYYY/MM/*.parquet` months with `*.csv` supplements
//! beside them, merged per ticker on demand through the shared
//! [`HistoryCache`]); a `.csv`/`.parquet` file keeps the legacy single-file
//! load. With no argument the shell opens the shipped
//! `data/protopyne-rs/historical` root. `--symbol` names the first ticker
//! (`SPY` when present, else the first in sorted order).
//!
//! No chart-rendering or indicator math lives here: every pane comes from
//! `paint_workspace` and every number from the shared kernels, so the demo
//! and the gateway-backed terminal draw the same terminal. Symbol picks in
//! the modal load that ticker's months on the next frame and seed every
//! toolbar frame through the shared load-time tally; a ticker that refuses
//! paints its own label over empty rows (never stale rows) with the reason
//! on screen. Display stays on the optimized path throughout: one cached
//! series per ticker, revision-skipped recomputes, and trailing-window,
//! culled, column-aggregated paint.
//!
//! Usage: `cargo run -p protopyne-egui --features demo --example demo --
//! [DIR|HISTORY.csv|HISTORY.parquet] [--symbol TICKER]`, or pass `--check`
//! to run the headless data path without opening a window and print a
//! summary. `--check` output is pinned in `tests/data/demo_check*.txt`: the
//! `protopyne-demo` make target reproduces it byte-for-byte.

use std::path::{Path, PathBuf};

use common_core::config::load_json_or_default;
use common_core::io::write_atomic;
use protopyne_chart_model::rows_of;
use protopyne_egui::{paint_workspace, prefs_path, ChartWorkspace, TerminalPrefs, TimeframeKey};
use protopyne_server::{HistoryCache, HistoryRoot};
use protopyne_types::Bar;

#[path = "shared/seed.rs"]
mod seed;

use seed::{frames_of, load_notice, Notice};

fn history_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/protopyne-rs/historical")
}

fn is_parquet(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("parquet")
}

/// Load a history file through the server's one entry point (`.parquet` or
/// `.csv` by extension, anything else refused). An empty file is still the
/// demo's own refusal: the loader answers empty, the shell decides that
/// means nothing to draw.
fn load_bars(path: &Path) -> Result<Vec<Bar>, String> {
    let bars = protopyne_server::load_bars_file(path)
        .map_err(|e| format!("parse {}: {e}", path.display()))?;
    if bars.is_empty() {
        return Err(format!("no bars in {}", path.display()));
    }
    Ok(bars)
}

/// The window shell: owns the workspace and paints one frame per pass. In
/// directory mode it also owns the history cache and the ticker whose
/// frames are seeded, so a symbol pick loads on the next frame.
struct DemoApp {
    workspace: ChartWorkspace,
    cache: Option<HistoryCache>,
    loaded_symbol: Option<String>,
    notice: Option<Notice>,
    /// Where the favorites/loadout/settings document lives (`protopyne.json`
    /// beside the historical root).
    prefs_path: PathBuf,
    /// Last persisted snapshot, so the frame writes only on a real change.
    saved: TerminalPrefs,
}

impl DemoApp {
    /// Seed a workspace from `path` under the built-in defaults. The persisted
    /// document is applied later, by [`load_prefs`](Self::load_prefs), so the
    /// headless `--check` path stays deterministic and independent of any
    /// on-disk `protopyne.json`.
    fn new(path: &Path, symbol: Option<&str>) -> Result<Self, String> {
        let mut workspace = ChartWorkspace::new().map_err(|e| format!("workspace refused: {e}"))?;
        if path.is_dir() {
            let root = HistoryRoot::open(path)
                .map_err(|e| format!("history root {}: {e}", path.display()))?;
            let mut cache = HistoryCache::new(root);
            let tickers = cache
                .tickers()
                .map_err(|e| format!("list {}: {e}", path.display()))?;
            let first = symbol
                .map(str::to_owned)
                .or_else(|| {
                    tickers
                        .iter()
                        .find(|ticker| *ticker == protopyne_server::DEFAULT_TICKER)
                        .cloned()
                        .or_else(|| tickers.first().cloned())
                })
                .ok_or_else(|| format!("no tickers under {}", path.display()))?;
            let load = cache
                .load_for(&first)
                .map_err(|e| format!("load {first}: {e}"))?;
            let notice = load_notice(&first, &load);
            let seeded = frames_of(&load.bars)?;
            workspace
                .switch_symbol_seeded(&first, seeded)
                .map_err(|e| format!("seed {first}: {e}"))?;
            return Ok(Self::assemble(
                workspace,
                path,
                Some(cache),
                Some(first),
                notice,
            ));
        }
        if is_parquet(path) {
            let bars = load_bars(path)?;
            let seeded = frames_of(&bars)?;
            // The file names no ticker; the workspace keeps its default
            // label over the folded frames, as before.
            let label = workspace.active_symbol().to_owned();
            workspace
                .switch_symbol_seeded(&label, seeded)
                .map_err(|e| format!("seed {}: {e}", path.display()))?;
        } else {
            // Legacy single-file load, unified: the argument file seeds the
            // `15M` slot while the shipped `1M`/`5M` trims seed the finer
            // slots (`1H`/`4H`/`1D` resample the `15M` slot inside the
            // workspace). Every file goes through the server's one entry
            // point, so the rows are byte-identical to the old slot map
            // (pinned in `tests/demo_shell.rs`).
            let dir = history_dir();
            for (timeframe, file) in [
                (TimeframeKey::M1, dir.join("SPY_1M.csv")),
                (TimeframeKey::M5, dir.join("SPY_5M.csv")),
                (TimeframeKey::M15, path.to_path_buf()),
            ] {
                let rows = rows_of(&load_bars(&file)?).rows;
                workspace
                    .seed_history(timeframe, rows)
                    .map_err(|e| format!("seed {}: {e}", timeframe.label()))?;
            }
        }
        Ok(Self::assemble(workspace, path, None, None, None))
    }

    /// Seed a workspace with the prefs path and an empty snapshot. The
    /// document is applied only when [`load_prefs`](Self::load_prefs) runs.
    fn assemble(
        workspace: ChartWorkspace,
        path: &Path,
        cache: Option<HistoryCache>,
        loaded_symbol: Option<String>,
        notice: Option<Notice>,
    ) -> Self {
        Self {
            workspace,
            cache,
            loaded_symbol,
            notice,
            prefs_path: prefs_path(path),
            saved: TerminalPrefs::default(),
        }
    }

    /// Apply the persisted document over the seeded workspace: restore the
    /// favorites, bookmarks, session selection, and the active ticker's
    /// layout, then re-select the active timeframe over the seeded frames so
    /// the label and the rows agree. Recorded as the baseline so a Save
    /// Layout write only lands on a real change.
    fn load_prefs(&mut self, path: &Path) {
        let prefs = load_json_or_default::<TerminalPrefs>(&prefs_path(path));
        if let Err(error) = self.workspace.apply_prefs(&prefs) {
            eprintln!("protopyne-gui: prefs ignored: {error}");
            return;
        }
        let _ = self.workspace.select_timeframe(self.workspace.timeframe());
        self.saved = prefs;
    }

    /// On-demand ticker switch: when the modal picked a symbol the seeded
    /// frames do not hold, load its months through the cache and reseed.
    /// Runs ahead of the paint so the frame draws the picked ticker; a
    /// refusal seeds empty frames under the picked label and reports the
    /// reason instead of painting stale rows.
    fn maybe_switch_symbol(&mut self) {
        let Some(cache) = self.cache.as_mut() else {
            return;
        };
        seed::sync_symbol(
            &mut self.workspace,
            cache,
            &mut self.loaded_symbol,
            &mut self.notice,
        );
    }
}

impl DemoApp {
    /// Persist the active ticker's layout on Save Layout only: nothing
    /// writes `protopyne.json` unless the menu-bar button requested it. The
    /// snapshot is compared against the last write so the file is touched
    /// only on a real change.
    fn persist_prefs(&mut self) {
        if !self.workspace.take_save_request() {
            return;
        }
        let prefs = self.workspace.prefs();
        if prefs == self.saved {
            return;
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(&prefs) {
            let _ = write_atomic(&self.prefs_path, &bytes);
        }
        self.saved = prefs;
    }
}

impl eframe::App for DemoApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.maybe_switch_symbol();
        if let Err(error) = paint_workspace(ui, &mut self.workspace, None) {
            ui.colored_label(egui::Color32::RED, format!("workspace refused: {error}"));
        }
        self.persist_prefs();
        if let Some(notice) = &self.notice {
            let ink = if notice.is_error() {
                egui::Color32::RED
            } else {
                egui::Color32::YELLOW
            };
            ui.colored_label(ink, notice.text());
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
    let mut symbol: Option<String> = None;
    let mut check = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--check" => check = true,
            "--symbol" => {
                symbol = Some(args.next().unwrap_or_else(|| {
                    eprintln!("protopyne-gui: --symbol needs a ticker");
                    std::process::exit(1);
                }));
            }
            _ => path = Some(PathBuf::from(arg)),
        }
    }
    let path = path.unwrap_or_else(history_dir);
    if check {
        let app = match DemoApp::new(&path, symbol.as_deref()) {
            Ok(app) => app,
            Err(e) => {
                eprintln!("protopyne-gui: {e}");
                std::process::exit(1);
            }
        };
        println!("protopyne-gui check: {}", summary(&app.workspace));
        if let Some(cache) = app.cache.as_ref() {
            match cache.root().tickers() {
                Ok(tickers) => println!("protopyne-gui check: tickers=[{}]", tickers.join(",")),
                Err(e) => println!("protopyne-gui check: tickers refused: {e}"),
            }
        }
        match &app.notice {
            Some(notice) => println!("protopyne-gui check: notice: {}", notice.text()),
            None => println!("protopyne-gui check: notice: none"),
        }
        return;
    }
    let mut app = match DemoApp::new(&path, symbol.as_deref()) {
        Ok(app) => app,
        Err(e) => {
            eprintln!("protopyne-gui: {e}");
            std::process::exit(1);
        }
    };
    app.load_prefs(&path);
    let options = eframe::NativeOptions::default();
    let run = eframe::run_native(
        "protopyne chart shell",
        options,
        Box::new(|cc| {
            protopyne_egui::install_noto_sans(
                &cc.egui_ctx,
                include_bytes!("../fonts/NotoSans-Regular.ttf"),
                include_bytes!("../fonts/NotoSansMono-Regular.ttf"),
            )?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(Box::new(app))
        }),
    );
    if let Err(e) = run {
        eprintln!("protopyne-gui: window refused: {e}");
        std::process::exit(1);
    }
}
