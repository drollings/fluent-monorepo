//! History-load economics: wall time, CPU time, and peak RSS per storage
//! arm, measured in fresh processes so allocator peaks do not bleed between
//! arms.
//!
//! The driver prepares both derived fixtures from the Parquet file — a CSV
//! rendering and a seeded SQLite tape — outside any measured phase, then
//! re-execs itself once per arm. Each arm reports the minimum wall time over
//! `--repeat` runs, the total CPU time of the process (user + system), and
//! the process's peak resident set size.
//!
//! Run (release profile matters):
//!
//! ```text
//! cargo run --release -p protopyne-server --example history_econ -- --all
//! ```
//!
//! Options: `--file <history.parquet>` (defaults to the committed SPY
//! month), `--repeat <n>` (default 5), `--dir <workdir>` (defaults to a
//! fresh directory under the system temp dir). Single arms can be re-run
//! with `--phase parquet|csv|sql_read --dir <workdir>`.
//!
//! CPU and RSS sampling read `/proc/self` on Linux; on other platforms the
//! harness still reports wall time and marks the rest unavailable.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use protopyne_server::{fold_frames, load_bars_csv, load_bars_parquet};
use protopyne_store::{
    AssetClass, CandleStore as _, FoldPolicy, PriceScale, ReadRequest as TapeReadRequest,
    SqliteTape, StoreStorage, TapeKey, TimeRange, WritePolicy,
};
use protopyne_types::{Bar, Timeframe};

const VENUE: &str = "econ-venue";
const TARGETS: [Timeframe; 5] = [
    Timeframe::M5,
    Timeframe::M15,
    Timeframe::H1,
    Timeframe::H4,
    Timeframe::D1,
];

fn default_file() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/protopyne-rs/historical/SPY-2026-03-1M.parquet")
}

fn render_csv(bars: &[Bar]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bars.len() * 48);
    out.push_str("time,open,high,low,close,volume\n");
    for bar in bars {
        writeln!(
            out,
            "{},{},{},{},{},{}",
            bar.ts.timestamp(),
            bar.o.normalize(),
            bar.h.normalize(),
            bar.l.normalize(),
            bar.c.normalize(),
            bar.volume,
        )
        .expect("writing to a String cannot fail");
    }
    out
}

/// The committed series with every missing minute filled flat (previous
/// close, zero volume): the tape a month of committed 1M bars actually
/// holds. The store's read planner refuses torn windows by design, so a
/// gap-filled tape is what its month-scale read is measured over.
fn gap_filled(bars: &[Bar]) -> Vec<Bar> {
    let step = 60_000_i64;
    let mut out: Vec<Bar> = Vec::new();
    let mut cursor = bars.first().map_or(0, |bar| bar.ts.timestamp_millis());
    for bar in bars {
        let ts = bar.ts.timestamp_millis();
        while cursor < ts {
            let previous = out.last().expect("the first bar seeds the grid").c;
            out.push(
                Bar::new()
                    .ts(chrono::DateTime::from_timestamp_millis(cursor).expect("in range"))
                    .o(previous)
                    .h(previous)
                    .l(previous)
                    .c(previous)
                    .volume(0)
                    .maybe_adj(None)
                    .build(),
            );
            cursor += step;
        }
        out.push(bar.clone());
        cursor = ts + step;
    }
    out
}

/// CPU time consumed by this process, in milliseconds: user + system ticks
/// from `/proc/self/stat`, converted at the conventional 100 Hz. `None`
/// off Linux.
fn cpu_ms() -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        // Field 2 is the comm in parentheses and may contain spaces: split
        // after the last ')' so field 14/15 line up.
        let rest = stat.rsplit_once(')')?.1;
        let fields: Vec<&str> = rest.split_whitespace().collect();
        // After the comm field, utime is field 14 overall => index 11 here.
        let utime: f64 = fields.get(11)?.parse().ok()?;
        let stime: f64 = fields.get(12)?.parse().ok()?;
        Some((utime + stime) * 10.0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Peak resident set size in KiB from `/proc/self/status` (`VmHWM`).
fn peak_rss_kib() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmHWM:") {
                return rest.split_whitespace().next()?.parse().ok();
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

struct Args {
    all: bool,
    phase: Option<String>,
    file: PathBuf,
    dir: Option<PathBuf>,
    repeat: usize,
}

fn parse_args() -> Args {
    let mut args = Args {
        all: false,
        phase: None,
        file: default_file(),
        dir: None,
        repeat: 5,
    };
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--all" => args.all = true,
            "--phase" => args.phase = iter.next(),
            "--file" => {
                if let Some(path) = iter.next() {
                    args.file = PathBuf::from(path);
                }
            }
            "--dir" => args.dir = iter.next().map(PathBuf::from),
            "--repeat" => {
                if let Some(n) = iter.next().and_then(|n| n.parse::<usize>().ok()) {
                    args.repeat = n.max(1);
                }
            }
            other => eprintln!("history_econ: ignoring unknown argument {other:?}"),
        }
    }
    args
}

fn print_row(phase: &str, min_ms: f64, cpu_ms: Option<f64>, rss: Option<u64>, detail: &str) {
    println!(
        "{phase:<12} min_wall_ms={min_ms:>9.2}  cpu_ms={:>9}  peak_rss_kib={:>9}  {detail}",
        cpu_ms.map_or_else(|| "n/a".to_owned(), |v| format!("{v:.1}")),
        rss.map_or_else(|| "n/a".to_owned(), |v| v.to_string()),
    );
}

/// One measured arm inside a fresh process.
fn run_phase(phase: &str, dir: &Path, file: &Path, repeat: usize) -> Result<(), String> {
    let cpu_before = cpu_ms();
    let mut best = f64::MAX;
    let mut detail = String::new();
    for _ in 0..repeat {
        let started = Instant::now();
        match phase {
            "parquet" => {
                let bars = load_bars_parquet(file).map_err(|e| format!("parquet load: {e}"))?;
                let frames = fold_frames(&bars, Timeframe::M1, &TARGETS)
                    .map_err(|e| format!("frames: {e}"))?;
                detail = format!(
                    "{} bars -> {} frames ({} bytes)",
                    bars.len(),
                    frames.len(),
                    std::fs::metadata(file).map_or(0, |m| m.len())
                );
            }
            "csv" => {
                let csv = dir.join("SPY-2026-03-1M.csv");
                let bars = load_bars_csv(&csv).map_err(|e| format!("csv load: {e}"))?;
                let frames = fold_frames(&bars, Timeframe::M1, &TARGETS)
                    .map_err(|e| format!("frames: {e}"))?;
                detail = format!("{} bars -> {} frames", bars.len(), frames.len());
            }
            "sql_read" => {
                let tape_path = dir.join("tape.sqlite");
                let tape = SqliteTape::open(&tape_path).map_err(|e| format!("open: {e}"))?;
                let caps =
                    fluent_wvr::CapabilitySet::new().with(StoreStorage::new(tape_path.clone()));
                let request = TapeReadRequest {
                    symbol: "SPY".to_owned(),
                    venue: VENUE.to_owned(),
                    timeframe: Timeframe::M1,
                    range: TimeRange {
                        start_ms: None,
                        end_ms: None,
                    },
                    limit: Some(1_000_000),
                    policy: FoldPolicy::ExtendToFullBuckets,
                };
                let series = fluent_wvr::CURRENT_CAPS
                    .sync_scope(caps, || tape.read(&request))
                    .map_err(|e| format!("read: {e}"))?;
                detail = format!("{} bars", series.bars.len());
            }
            other => return Err(format!("unknown phase {other:?}")),
        }
        best = best.min(started.elapsed().as_secs_f64() * 1_000.0);
    }
    let cpu = match (cpu_before, cpu_ms()) {
        (Some(before), Some(after)) => Some(after - before),
        _ => None,
    };
    print_row(phase, best, cpu, peak_rss_kib(), &detail);
    Ok(())
}

/// Prepare the derived fixtures from the Parquet file, timing each once:
/// the CSV rendering and the seeded SQLite tape both copy the identical
/// series, so every arm loads the same bars.
fn prepare(dir: &Path, file: &Path) -> Result<Vec<Bar>, String> {
    let parquet_started = Instant::now();
    let bars = load_bars_parquet(file).map_err(|e| format!("parquet load: {e}"))?;
    let parquet_ms = parquet_started.elapsed().as_secs_f64() * 1_000.0;
    let bytes = std::fs::metadata(file).map_or(0, |m| m.len());
    print_row(
        "parquet*",
        parquet_ms,
        None,
        None,
        &format!("{} bars ({bytes} bytes, driver's own read)", bars.len()),
    );

    let csv_started = Instant::now();
    let csv_path = dir.join("SPY-2026-03-1M.csv");
    std::fs::write(&csv_path, render_csv(&bars)).map_err(|e| format!("csv write: {e}"))?;
    let csv_ms = csv_started.elapsed().as_secs_f64() * 1_000.0;
    let csv_bytes = std::fs::metadata(&csv_path).map_or(0, |m| m.len());
    print_row(
        "csv*",
        csv_ms,
        None,
        None,
        &format!("render+write ({csv_bytes} bytes)"),
    );

    let sql_started = Instant::now();
    let tape_path = dir.join("tape.sqlite");
    let tape = SqliteTape::open(&tape_path).map_err(|e| format!("tape open: {e}"))?;
    let caps = fluent_wvr::CapabilitySet::new().with(StoreStorage::new(tape_path.clone()));
    let filled = gap_filled(&bars);
    let filled_len = filled.len();
    fluent_wvr::CURRENT_CAPS.sync_scope(caps, || {
        let symbol_id = tape
            .register(
                "SPY",
                VENUE,
                AssetClass::Stock,
                PriceScale::of(3).expect("scale"),
                "UTC",
                Timeframe::M1,
            )
            .expect("register");
        let key = TapeKey {
            symbol_id,
            timeframe: Timeframe::M1,
        };
        let outcome = tape
            .append(key, &filled, WritePolicy::InsertIfAbsent)
            .expect("append");
        assert_eq!(
            outcome.rejected, 0,
            "the fixture scale represents every price"
        );
        assert_eq!(outcome.inserted, filled.len(), "every bar lands");
    });
    drop(tape);
    let sql_ms = sql_started.elapsed().as_secs_f64() * 1_000.0;
    // WAL mode keeps fresh writes in the `-wal` sibling until a checkpoint:
    // the committed bytes are the pair.
    let sql_bytes = std::fs::metadata(&tape_path).map_or(0, |m| m.len())
        + std::fs::metadata(tape_path.with_extension("sqlite-wal"))
            .map_or(0, |m| m.len());
    print_row(
        "sqlite*",
        sql_ms,
        None,
        None,
        &format!("register+append ({filled_len} gap-filled bars, {sql_bytes} bytes)"),
    );
    Ok(bars)
}

fn driver(args: &Args) -> Result<(), String> {
    let dir = match &args.dir {
        Some(dir) => dir.clone(),
        None => std::env::temp_dir().join(format!("protopyne-econ-{}", std::process::id())),
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("workdir {}: {e}", dir.display()))?;
    println!(
        "history_econ: file={} repeat={}",
        args.file.display(),
        args.repeat
    );
    println!("history_econ: workdir={}", dir.display());
    let bars = prepare(&dir, &args.file)?;
    println!(
        "history_econ: {} bars, {} five targets tallied per arm",
        bars.len(),
        TARGETS.len()
    );
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let mut failures = 0;
    for phase in ["parquet", "csv", "sql_read"] {
        let output = Command::new(&exe)
            .args([
                "--phase",
                phase,
                "--dir",
                &dir.display().to_string(),
                "--file",
                &args.file.display().to_string(),
                "--repeat",
                &args.repeat.to_string(),
            ])
            .output()
            .map_err(|e| format!("spawn {phase}: {e}"))?;
        if output.status.success() {
            print!("{}", String::from_utf8_lossy(&output.stdout));
        } else {
            failures += 1;
            eprintln!(
                "history_econ: phase {phase} failed:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
    if failures > 0 {
        return Err(format!("{failures} phase(s) failed"));
    }
    Ok(())
}

fn main() {
    let args = parse_args();
    let result = match (&args.phase, args.all) {
        (Some(phase), false) => {
            let dir = args
                .dir
                .clone()
                .unwrap_or_else(|| std::env::temp_dir().join("protopyne-econ"));
            run_phase(phase, &dir, &args.file, args.repeat)
        }
        _ => driver(&args),
    };
    if let Err(error) = result {
        eprintln!("history_econ: {error}");
        std::process::exit(2);
    }
}
