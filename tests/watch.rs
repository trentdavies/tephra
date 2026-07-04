//! Integration tests for `tephra bridge --watch` (Task 6): the
//! spawn-and-SIGTERM round trip proving clean shutdown (a real cycle ran,
//! lock released, exit 0), the `--interval` clamp's logged warning, and the
//! missing-bridge-dir guard rail (hard error, no loop).
//!
//! Uses `std::process::Command` (via `Fixture::tephra_command`) rather than
//! `assert_cmd::Command` for the spawn/signal tests: `assert_cmd::Command`
//! only exposes blocking `.output()`/`.assert()`, not `.spawn()`, and these
//! tests need a live `Child` to send a signal to and bound how long they
//! wait for exit. Signals are sent via the `kill` binary (`Command::new`)
//! rather than adding a `nix` dev-dependency for a single syscall.

mod common;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::Fixture;

fn append_line(path: &Path, line: &str) {
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap_or_else(|e| panic!("open {path:?} for append: {e}"));
    writeln!(f, "{line}").unwrap();
}

fn remote_head_subject(fx: &Fixture) -> String {
    let output = fx.git(&fx.remote, &["log", "-1", "--format=%s"]);
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Poll `condition` until it's true or `timeout` elapses; panics with `msg`
/// on timeout rather than a fixed long sleep, so the happy path is fast and
/// a genuine hang still fails instead of blocking the suite forever.
fn wait_until(timeout: Duration, msg: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    loop {
        if condition() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("{msg}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Poll `child.try_wait()` until it exits or `timeout` elapses; force-kills
/// on timeout so a failing assertion never leaves an orphan process behind.
fn wait_for_exit(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Send SIGTERM to `pid` via the `kill` binary.
fn sigterm(pid: u32) {
    let status = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()
        .expect("spawn `kill`");
    assert!(status.success(), "kill -TERM {pid} did not report success");
}

/// Drain a child pipe on a background thread into a channel, so the pipe
/// can never back-pressure (and hang) the child regardless of whether, or
/// how quickly, the test reads lines back out.
fn drain_lines<R: Read + Send + 'static>(reader: R) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// Everything already buffered (or that arrives within a short grace
/// window of the last line), without blocking indefinitely if the process
/// producing them has already exited and closed the pipe.
fn collect_available(rx: &mpsc::Receiver<String>) -> Vec<String> {
    let mut lines = Vec::new();
    while let Ok(line) = rx.recv_timeout(Duration::from_millis(300)) {
        lines.push(line);
    }
    lines
}

#[test]
fn watch_runs_a_cycle_and_exits_cleanly_on_sigterm() {
    let fx = Fixture::new("testvault");
    // Written before spawn: --watch's first cycle runs immediately at
    // startup (no wait for the interval), so this edit is committed and
    // pushed within the first seconds rather than needing a full interval
    // to elapse first.
    append_line(&fx.bridge.join("Home.md"), "human line via watch");

    let mut child = fx
        .tephra_command()
        .arg("bridge")
        .arg("--watch")
        .arg("--interval")
        .arg("10")
        .arg(&fx.name)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `tephra bridge --watch`");

    let stdout_lines = drain_lines(child.stdout.take().expect("stdout should be piped"));
    let stderr_lines = drain_lines(child.stderr.take().expect("stderr should be piped"));

    wait_until(
        Duration::from_secs(10),
        "the first --watch cycle should commit and push the human edit within 10s",
        || remote_head_subject(&fx) == "vault: human edits",
    );

    sigterm(child.id());

    let status = wait_for_exit(&mut child, Duration::from_secs(3))
        .unwrap_or_else(|| panic!("tephra bridge --watch did not exit within 3s of SIGTERM"));

    let stdout: Vec<String> = collect_available(&stdout_lines);
    let stderr: Vec<String> = collect_available(&stderr_lines);
    assert_eq!(
        status.code(),
        Some(0),
        "a signal-initiated shutdown should exit 0; stdout: {stdout:?}, stderr: {stderr:?}"
    );
    assert!(
        stdout.iter().any(|l| l.contains("shut")),
        "watch should log a shutdown line on signal exit, got: {stdout:?}"
    );

    let lock = fx.bridge.join(".git").join("tephra-bridge.lock");
    assert!(
        !lock.exists(),
        "the per-cycle lock should be released after shutdown"
    );
}

#[test]
fn watch_clamps_interval_below_minimum_and_logs_a_warning() {
    let fx = Fixture::new("testvault");

    let mut child = fx
        .tephra_command()
        .arg("bridge")
        .arg("--watch")
        .arg("--interval")
        .arg("5")
        .arg(&fx.name)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn `tephra bridge --watch`");

    let stdout_lines = drain_lines(child.stdout.take().expect("stdout should be piped"));
    let _stderr_lines = drain_lines(child.stderr.take().expect("stderr should be piped"));

    // The clamp warning and the "watch: interval Ns" startup line are both
    // logged before the first cycle starts (see `bridge::watch`), so two
    // lines is enough -- no need to wait for a full cycle to complete.
    let mut lines = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while lines.len() < 2 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match stdout_lines.recv_timeout(remaining) {
            Ok(line) => lines.push(line),
            Err(_) => break,
        }
    }

    let _ = child.kill();
    let _ = child.wait();

    assert!(
        lines.len() >= 2,
        "expected a clamp warning line and a startup line, got: {lines:?}"
    );
    let joined = lines.join("\n");
    assert!(
        joined.contains("interval 10s"),
        "startup log should report the clamped interval (10s), got: {joined:?}"
    );
    assert!(
        joined.to_lowercase().contains("clamp"),
        "output should warn that the requested interval was clamped, got: {joined:?}"
    );
}

#[test]
fn watch_with_missing_bridge_dir_exits_1_promptly_without_looping() {
    let fx = Fixture::new("testvault");
    fs::remove_dir_all(&fx.bridge).expect("remove bridge dir to simulate misconfiguration");

    let start = Instant::now();
    fx.tephra_cmd()
        .arg("bridge")
        .arg("--watch")
        .arg("--interval")
        .arg("10")
        .arg(&fx.name)
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains("bridge directory does not exist"));

    assert!(
        start.elapsed() < Duration::from_secs(3),
        "a missing bridge dir should fail immediately, not after looping or sleeping"
    );
}
