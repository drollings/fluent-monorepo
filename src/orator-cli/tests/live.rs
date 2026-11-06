//! Live barge/quit harness: a scripted pty drives the real TUI
//! against the real stack (daemon + supervised inference + capture),
//! then asserts the cut was named, the client exited cleanly, the
//! daemon still serves, and no inference child was orphaned.
//!
//! Skips cleanly without the stack: no opt-in, no pty helper, no
//! daemon, no inference, or no capture device each skip loudly
//! instead of failing. Run via the live-ai target, never CI.

use std::path::PathBuf;
use std::process::Stdio;

fn cli_binary() -> PathBuf {
    std::env::var("CARGO_BIN_EXE_orator_cli").map_or_else(
        |_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/orator-cli"),
        PathBuf::from,
    )
}

/// Whether captured TUI output reports an environment limitation
/// (no capture device, no terminal) rather than a product failure.
/// The TUI's own stderr flows into the pty transcript, so this scans
/// the stripped transcript — excluding script(1)'s own header/footer,
/// which mention "terminal" for its own bookkeeping.
fn unavailable_in(text: &str) -> bool {
    const NEEDLES: [&str; 8] = [
        "capture",
        "ALSA",
        "device",
        "JACK",
        "terminal",
        "TTY",
        "audio stream",
        "input config",
    ];
    text.lines()
        .filter(|line| !(line.starts_with("Script started") || line.starts_with("Script done")))
        .any(|line| NEEDLES.iter().any(|needle| line.contains(needle)))
}

/// Pids whose cmdline names an inference server.
fn inference_pids() -> std::collections::HashSet<u32> {
    let mut pids = std::collections::HashSet::new();
    let Ok(proc) = std::fs::read_dir("/proc") else {
        return pids;
    };
    for entry in proc.filter_map(Result::ok) {
        let name = entry.file_name();
        let Some(pid): Option<u32> = name.to_str().and_then(|text| text.parse().ok()) else {
            continue;
        };
        let cmdline = std::fs::read_to_string(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        if cmdline.contains("llama-server") {
            pids.insert(pid);
        }
    }
    pids
}

/// Parent pid and state letter for `pid`, if still alive.
fn parent_and_state(pid: u32) -> Option<(u32, char)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rfind(')')?;
    let mut fields = stat[after_comm + 1..].split_whitespace();
    let state = fields.next()?.chars().next()?;
    let ppid = fields.next()?.parse().ok()?;
    Some((ppid, state))
}

/// A live child process group, killed (gracefully, then forcefully)
/// when dropped. Panics and timeouts must never orphan a pty session:
/// the group dies with the test, not after it.
struct ChildGroup {
    child: std::process::Child,
}

impl ChildGroup {
    fn spawn(mut command: std::process::Command) -> std::io::Result<Self> {
        use std::os::unix::process::CommandExt as _;
        // Own process group: the pty child and everything it spawns
        // share one group the test can signal outright. (No setsid:
        // the child keeps its controlling terminal.)
        command.process_group(0);
        Ok(Self {
            child: command.spawn()?,
        })
    }

    fn stdin(&mut self) -> Option<std::process::ChildStdin> {
        self.child.stdin.take()
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait()
    }
}

impl Drop for ChildGroup {
    fn drop(&mut self) {
        // Negative pid = the whole group. Graceful first (the TUI
        // quits cleanly on SIGHUP/HUP of the pty), then forceful.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGHUP);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        unsafe {
                            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
                        }
                        let _ = self.child.wait();
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
    }
}

/// Strip terminal escapes and control bytes (keeping newlines) so
/// transcript assertions read the screen text, not the protocol.
fn strip_terminal(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(char) = chars.next() {
        if char == '\x1b' {
            for char in chars.by_ref() {
                if char.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if !char.is_control() || char == '\n' {
            out.push(char);
        }
    }
    out
}

/// Live barge/quit: cut mid-reply, quit mid-session, assert the cut
/// was named, the exit was clean, the daemon still serves, and no
/// inference child outlived its supervisor.
#[ignore = "needs a pty, daemon, inference, and capture; run via the live-ai target"]
#[test]
fn tui_barge_and_quit_leave_no_orphans() {
    if std::env::var("ORATOR_TUI_LIVE").as_deref() != Ok("1") {
        eprintln!("live: ORATOR_TUI_LIVE != 1, skipping");
        return;
    }
    if std::process::Command::new("script")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("live: no script(1) pty helper, skipping");
        return;
    }
    // Readiness probes through the product itself: the daemon serves,
    // and it owns a voice server (inference fleet up — the TUI turn
    // takes the daemon Ask path, never the CLI direct-LLM path).
    let status = std::process::Command::new(cli_binary())
        .arg("status")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    let status = match status {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).into_owned()
        }
        _ => {
            eprintln!("live: no serving daemon (`orator status` fails), skipping");
            return;
        }
    };
    if !status.contains("\"voice_server\":true") {
        eprintln!("live: daemon owns no voice server (status: {status}), skipping");
        return;
    }
    let supervised_before = inference_pids();
    // Drive the real TUI under a sized pty, capturing to a typescript
    // file the test polls: long question, cut once the reply text
    // lands (voicing just starting), quit. Stdin pipes into the pty
    // master. Event-driven (reply marker), never fixed sleeps past
    // startup: slow CPU inference just lengthens the wait.
    let typescript =
        std::env::temp_dir().join(format!("orator-tui-live-{}.typescript", std::process::id()));
    let _ = std::fs::remove_file(&typescript);
    let mut command = std::process::Command::new("script");
    command
        .args([
            "-qec",
            &format!("{} tui", cli_binary().display()),
            &typescript.display().to_string(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = ChildGroup::spawn(command).expect("script(1) spawns");
    let mut stdin = child.stdin().expect("pty stdin");
    use std::io::Write as _;
    let mut feed = |bytes: &[u8], wait_ms: u64| {
        stdin.write_all(bytes).expect("pty feed");
        stdin.flush().expect("pty flush");
        std::thread::sleep(std::time::Duration::from_millis(wait_ms));
    };
    // Startup: terminal init plus capture open. Note the TUI's own
    // stderr flows into the typescript (pty), not script's stderr.
    // Keystrokes fed after startup can vanish under automation, so
    // trickle the question from spawn (one char at a time): whatever
    // lands forms the turn, and the echo check below confirms it.
    // The assertions never depend on the question text.
    if child.try_wait().expect("pollable").is_some() {
        let _ = child.wait();
        let early = std::fs::read(&typescript).unwrap_or_default();
        let early_text = strip_terminal(&early);
        if unavailable_in(&early_text) {
            eprintln!("live: TUI cannot start here, skipping");
            return;
        }
        panic!("live: TUI exited during startup:\n{early_text}");
    }
    let mut ready = false;
    for _ in 0..3 {
        for char in "say the word done".chars() {
            if child.try_wait().expect("pollable").is_some() {
                break;
            }
            feed(&[char as u8], 700);
        }
        feed(b"\n", 2_000);
        let bytes = std::fs::read(&typescript).unwrap_or_default();
        if strip_terminal(&bytes).contains("word done") {
            ready = true;
            break;
        }
        if child.try_wait().expect("pollable").is_some() {
            break;
        }
    }
    if !ready {
        let bytes = std::fs::read(&typescript).unwrap_or_default();
        let text = strip_terminal(&bytes);
        if unavailable_in(&text) {
            eprintln!("live: TUI cannot start here, skipping");
            return;
        }
        panic!("live: typed question never echoed");
    }
    // Wait for the reply text to land (voicing starts): poll the
    // typescript, bounded. Slow CPU inference just lengthens the
    // wait; the question asks for one word so the turn completes
    // instead of racing the budget.
    let mut heard = false;
    for _ in 0..240 {
        let bytes = std::fs::read(&typescript).unwrap_or_default();
        if strip_terminal(&bytes).contains("orator: ") {
            heard = true;
            break;
        }
        if child.try_wait().expect("pollable").is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    if !heard {
        let bytes = std::fs::read(&typescript).unwrap_or_default();
        let text = strip_terminal(&bytes);
        if unavailable_in(&text) {
            eprintln!("live: TUI lost its environment mid-run, skipping");
            return;
        }
        panic!("live: reply text never landed in 8 min");
    }
    // Cut mid-reply: Ctrl-B is the barge key. A few seconds of
    // voicing precede it on a long reply.
    std::thread::sleep(std::time::Duration::from_secs(3));
    feed(b"\x02", 5_000);
    feed(b"/quit\n", 0);
    drop(stdin);
    let mut waited = 0;
    let exit = loop {
        if let Some(status) = child.try_wait().expect("pollable") {
            break status;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
        waited += 1;
        if waited >= 60 {
            // No manual kill: dropping the group reaps the pty
            // session on every exit path, including this panic.
            panic!("live: TUI did not quit within 60 s");
        }
    };
    let bytes = std::fs::read(&typescript).unwrap_or_default();
    let _ = std::fs::remove_file(&typescript);
    let screen = strip_terminal(&bytes);
    assert!(
        screen.contains("barge: cut the in-flight turn"),
        "live: barge named its cut:\n{screen}"
    );
    assert!(exit.success(), "live: clean quit, no hang: {exit}");
    // No orphaned inference children: any server pid that appeared
    // during the run must still have a live supervisor (or be gone —
    // reaped is fine). Adopted (ppid 1) or zombie servers fail.
    std::thread::sleep(std::time::Duration::from_secs(5));
    let mut orphans = Vec::new();
    for pid in inference_pids().difference(&supervised_before) {
        if let Some((ppid, state)) = parent_and_state(*pid) {
            if ppid == 1 || state == 'Z' {
                orphans.push((*pid, ppid, state));
            }
        }
    }
    assert!(
        orphans.is_empty(),
        "live: orphaned inference children: {orphans:?}"
    );
    // The serving loop survived the barge and the quit.
    let status = std::process::Command::new(cli_binary())
        .arg("status")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("status runs");
    assert!(status.success(), "live: daemon still serves after quit");
}
