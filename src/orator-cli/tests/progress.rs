//! Long-op progress goldens: the real binary narrates on stderr while
//! stdout contracts stay byte-identical. Hermetic: temp HOME, dead
//! filesystem paths, seeded byte-small files — no models, no network,
//! no daemon, no audio hardware.

use std::path::PathBuf;

/// Process env is shared across test threads: every test below holds
/// this while it moves HOME and spawns the binary.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn cli_binary() -> PathBuf {
    std::env::var("CARGO_BIN_EXE_orator_cli").map_or_else(
        |_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/orator-cli"),
        PathBuf::from,
    )
}

struct TestHome {
    dir: tempfile::TempDir,
    runtime: tempfile::TempDir,
    cwd: tempfile::TempDir,
    prior_home: Option<std::ffi::OsString>,
    prior_data_home: Option<std::ffi::OsString>,
    prior_runtime: Option<std::ffi::OsString>,
}

impl TestHome {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = tempfile::tempdir().expect("tempdir");
        let cwd = tempfile::tempdir().expect("tempdir");
        let prior_home = std::env::var_os("HOME");
        let prior_data_home = std::env::var_os("XDG_DATA_HOME");
        let prior_runtime = std::env::var_os("XDG_RUNTIME_DIR");
        std::env::set_var("HOME", dir.path());
        std::env::remove_var("XDG_DATA_HOME");
        std::env::set_var("XDG_RUNTIME_DIR", runtime.path());
        Self {
            dir,
            runtime,
            cwd,
            prior_home,
            prior_data_home,
            prior_runtime,
        }
    }

    fn home(&self) -> &std::path::Path {
        self.dir.path()
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = std::process::Command::new(cli_binary());
        cmd.args(args);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.env("HOME", self.home());
        cmd.env("XDG_RUNTIME_DIR", self.runtime.path());
        cmd.env_remove("XDG_DATA_HOME");
        cmd.current_dir(self.cwd.path());
        cmd.output().expect("cli runs")
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        match &self.prior_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match &self.prior_data_home {
            Some(value) => std::env::set_var("XDG_DATA_HOME", value),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
        match &self.prior_runtime {
            Some(value) => std::env::set_var("XDG_RUNTIME_DIR", value),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
    }
}

/// Seed one byte-small present file for the pinned `tiny` entry: size
/// mismatches, so verification fails fast on the size stat without
/// hashing megabytes.
fn seed_tiny_present(home: &TestHome) -> String {
    let entry = orator_models::find_entry(orator_models::ModelKind::Whisper, "tiny")
        .expect("tiny entry exists");
    assert!(entry.pinned(), "tiny stays pinned (test seeds a mismatch)");
    let dest = orator_models::path_for(&entry).expect("managed dir resolves");
    assert!(
        dest.starts_with(home.home()),
        "seed lands under the temp HOME: {}",
        dest.display()
    );
    std::fs::create_dir_all(dest.parent().expect("parent")).expect("mkdir");
    std::fs::write(&dest, b"x").expect("seed writes");
    entry.name
}

fn lines(text: &str) -> Vec<&str> {
    text.lines().collect()
}

#[test]
fn verify_all_narrates_each_hashed_entry_on_stderr() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let home = TestHome::new();
    let name = seed_tiny_present(&home);
    let output = home.run(&["models", "verify"]);
    assert_eq!(output.status.code(), Some(1), "a MISMATCH fails the run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains(&format!("{name}: MISMATCH")),
        "stdout keeps the report: {stdout}"
    );
    assert!(
        stdout.lines().count() >= 8,
        "stdout keeps one line per entry: {stdout}"
    );
    assert!(
        stderr.contains(&format!("orator: verify {name} ... hashing")),
        "hashing start narrates: {stderr}"
    );
    assert!(
        stderr.contains(&format!("orator: verify {name}: MISMATCH")),
        "hashing outcome narrates: {stderr}"
    );
}

#[test]
fn verify_named_model_keeps_the_stdout_contract() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let home = TestHome::new();
    let name = seed_tiny_present(&home);
    let output = home.run(&["models", "verify", &name]);
    assert_eq!(output.status.code(), Some(1), "named mismatch fails");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("{name}: MISMATCH\n"),
        "named stdout is exactly the report line"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("orator: verify {name} ... hashing"))
            && stderr.contains(&format!("orator: verify {name}: MISMATCH")),
        "named run narrates start and outcome: {stderr}"
    );
}

#[test]
fn stats_missing_file_keeps_envelope_and_empty_stdout() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let home = TestHome::new();
    let missing = home.home().join("no-such-stat.jsonl");
    let output = home.run(&["stats", &missing.display().to_string()]);
    assert_eq!(output.status.code(), Some(1), "missing stat file fails");
    assert!(output.stdout.is_empty(), "stdout stays empty on failure");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("orator: read stat file"), "{stderr}");
    assert!(stderr.contains("check the path"), "{stderr}");
}

/// Minimal 16-bit mono 16 kHz WAV writer for gate fixtures.
fn write_wav(path: &std::path::Path, samples: &[i16]) {
    let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + samples.len() as u32 * 2).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&16_000u32.to_le_bytes());
    bytes.extend_from_slice(&32_000u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(samples.len() as u32 * 2).to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    std::fs::write(path, bytes).expect("wav writes");
}

#[test]
fn transcribe_tone_is_empty_with_note() {
    let _guard = common_core::sync::lock(&ENV_LOCK);
    let home = TestHome::new();
    let wav = home.home().join("tone.wav");
    let tone: Vec<i16> = (0..16_000)
        .map(|index| {
            (10_000.0 * (2.0 * std::f32::consts::PI * 440.0 * index as f32 / 16_000.0).sin()) as i16
        })
        .collect();
    write_wav(&wav, &tone);
    let output = home.run(&["transcribe", &wav.display().to_string()]);
    assert_eq!(output.status.code(), Some(0), "held gate is success");
    assert!(output.stdout.is_empty(), "held gate prints no transcript");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no speech detected"), "{stderr}");
    assert!(
        lines(&stderr)
            .iter()
            .all(|line| line.starts_with("orator:")),
        "{stderr}"
    );
}
