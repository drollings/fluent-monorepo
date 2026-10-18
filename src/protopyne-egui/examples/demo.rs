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
use protopyne_chart_model::{rows_of, ChartRow};
use protopyne_egui::{paint_workspace, prefs_path, ChartWorkspace, TerminalPrefs, TimeframeKey};
use protopyne_server::{HistoryCache, HistoryRoot, TickerLoad};
use protopyne_types::{Bar, FoldEdges, Timeframe};

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

/// The toolbar key behind a store-grid timeframe.
fn key_of(timeframe: Timeframe) -> Option<TimeframeKey> {
    TimeframeKey::all()
        .into_iter()
        .find(|key| key.timeframe() == timeframe)
}

/// Fold one 1M series into every toolbar frame the base reaches: the base is
/// taken as-is and every whole-multiple coarser target is tallied through
/// the shared bucket-anchored fold. Frames finer than the base stay absent
/// (they cannot be synthesized); `1D` folds from a one-minute base.
fn frames_of(bars: &[Bar]) -> Result<Vec<(TimeframeKey, Vec<ChartRow>)>, String> {
    let base = protopyne_server::HISTORY_ROOT_BASE;
    let targets: Vec<Timeframe> = TimeframeKey::all()
        .into_iter()
        .map(TimeframeKey::timeframe)
        .filter(|target| target.is_whole_multiple_of(base))
        .collect();
    // Sub-daily frames fold complete-only (torn intraday buckets are omitted);
    // day-and-coarser frames fold keep-partial, because a trading session
    // cannot tile a 24-hour UTC day, so a complete-only daily fold emits
    // nothing and the 1D chart would go dark. This mirrors the workspace's
    // display fold, so a seeded 1D frame and an on-demand resample agree.
    let (intraday, daily): (Vec<Timeframe>, Vec<Timeframe>) = targets
        .into_iter()
        .partition(|target| target.duration_ms() < Timeframe::D1.duration_ms());
    let mut seeded = Vec::new();
    for (group, edges) in [
        (intraday, FoldEdges::CompleteOnly),
        (daily, FoldEdges::KeepPartial),
    ] {
        let frames = protopyne_server::fold_frames(bars, base, &group, edges)
            .map_err(|e| format!("tally 1M series: {e}"))?;
        for (timeframe, frame) in frames {
            let key =
                key_of(timeframe).ok_or_else(|| format!("no toolbar slot for {timeframe:?}"))?;
            seeded.push((key, rows_of(&frame).rows));
        }
    }
    Ok(seeded)
}

/// Empty frames under every key: the fail-closed seed for a ticker that
/// refuses to load — its label over no rows, never another ticker's.
fn empty_frames() -> Vec<(TimeframeKey, Vec<ChartRow>)> {
    TimeframeKey::all()
        .into_iter()
        .map(|key| (key, Vec::new()))
        .collect()
}

/// A load notice for the status line: excluded rows, excluded months, and
/// superseded rows are observations about the source tree, rendered, never
/// dropped.
fn load_notice(ticker: &str, load: &TickerLoad) -> Option<Notice> {
    if load.skipped.is_empty() && load.superseded == 0 && load.skipped_rows == 0 {
        return None;
    }
    let mut parts = Vec::new();
    if load.skipped_rows > 0 {
        parts.push(format!(
            "{} row(s) excluded as unparseable",
            load.skipped_rows
        ));
    }
    if !load.skipped.is_empty() {
        let names: Vec<String> = load
            .skipped
            .iter()
            .filter_map(|skip| {
                skip.path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .collect();
        parts.push(format!(
            "{} month file(s) excluded [{}]",
            load.skipped.len(),
            names.join(", ")
        ));
    }
    if load.superseded > 0 {
        parts.push(format!(
            "{} parquet row(s) superseded by CSV",
            load.superseded
        ));
    }
    Some(Notice::warning(format!(
        "{ticker}: {} (a corrected month in CSV is the fix path)",
        parts.join(", ")
    )))
}

/// A status-line notice: warnings disclose holes in served data, errors
/// refuse the requested ticker outright.
enum Notice {
    Warning(String),
    Error(String),
}

impl Notice {
    fn warning(text: String) -> Self {
        Self::Warning(text)
    }

    fn error(text: String) -> Self {
        Self::Error(text)
    }

    fn text(&self) -> &str {
        match self {
            Self::Warning(text) | Self::Error(text) => text,
        }
    }

    fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }
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
    /// favorites, loadout, session selection, and chart settings, then
    /// re-select the persisted timeframe over the seeded frames so the label
    /// and the rows agree. Recorded as the baseline so the first write only
    /// lands on a real change.
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
        let picked = self.workspace.active_symbol().to_owned();
        if self.loaded_symbol.as_deref() == Some(picked.as_str()) {
            return;
        }
        match cache.load_for(&picked) {
            Ok(load) => {
                let notice = load_notice(&picked, &load);
                match frames_of(&load.bars).and_then(|seeded| {
                    self.workspace
                        .switch_symbol_seeded(&picked, seeded)
                        .map_err(|e| format!("seed {picked}: {e}"))
                }) {
                    Ok(()) => {
                        self.loaded_symbol = Some(picked);
                        self.notice = notice;
                    }
                    Err(reason) => {
                        self.notice = Some(Notice::error(reason));
                    }
                }
            }
            Err(error) => {
                let _ = self.workspace.switch_symbol_seeded(&picked, empty_frames());
                self.loaded_symbol = Some(picked.clone());
                self.notice = Some(Notice::error(format!("load {picked}: {error}")));
            }
        }
    }
}

impl DemoApp {
    /// Persist the favorites, loadout, and settings when they change: the
    /// snapshot is compared against the last write so the file is touched only
    /// on a real edit.
    fn persist_prefs(&mut self) {
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
        Box::new(|_cc| Ok::<_, Box<dyn std::error::Error + Send + Sync>>(Box::new(app))),
    );
    if let Err(e) = run {
        eprintln!("protopyne-gui: window refused: {e}");
        std::process::exit(1);
    }
}
