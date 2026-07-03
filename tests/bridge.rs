//! Integration tests for `tephra bridge --once`, porting all 12 assertions
//! from `docs/reference/prototype/test-harness.sh` (grouped into functions
//! mirroring harness tests 1-5), plus two new scenarios exercised only by
//! the Rust port: stale-`MERGE_HEAD` recovery and lock exclusion (see
//! `docs/plans/2026-07-03-v1-implementation.md` Task 4).

mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use common::Fixture;
use tephra::gitx;

fn git_ok(fx: &Fixture, dir: &Path, args: &[&str]) {
    let output = fx.git(dir, args);
    assert!(
        output.status.success(),
        "git -C {} {} failed: {}",
        dir.display(),
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn append_line(path: &Path, line: &str) {
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap_or_else(|e| panic!("open {path:?} for append: {e}"));
    writeln!(f, "{line}").unwrap();
}

fn last_commit_subject(fx: &Fixture, dir: &Path) -> String {
    let output = fx.git(dir, &["log", "-1", "--format=%s"]);
    assert!(
        output.status.success(),
        "git log failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn rev_parse(fx: &Fixture, dir: &Path, rev: &str) -> String {
    let output = fx.git(dir, &["rev-parse", rev]);
    assert!(
        output.status.success(),
        "rev-parse {rev} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Find a conflict-copy file created alongside a conflicted note, whatever
/// today's date stamp happens to be.
fn find_conflict_copy(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.contains("agent conflict") || n.contains("agent-conflict"))
                .unwrap_or(false)
        })
}

fn failcount_path(fx: &Fixture) -> PathBuf {
    fx.bridge.join(".git").join("tephra-bridge.failcount")
}

fn lock_path(fx: &Fixture) -> PathBuf {
    fx.bridge.join(".git").join("tephra-bridge.lock")
}

// --- harness test 1: human edit propagated to the agent clone -----------

#[test]
fn human_edit_in_bridge_propagates_to_agent() {
    let fx = Fixture::new("testvault");

    append_line(&fx.bridge.join("Home.md"), "human line");

    fx.bridge_once().assert().success();

    git_ok(&fx, &fx.agent, &["pull", "--quiet"]);
    let content = fs::read_to_string(fx.agent.join("Home.md")).unwrap();
    assert!(
        content.contains("human line"),
        "harness test 1: human edit should propagate to the agent clone, got: {content:?}"
    );
}

// --- harness test 2: agent push merged into the bridge -------------------

#[test]
fn agent_push_merges_into_bridge() {
    let fx = Fixture::new("testvault");

    fs::write(fx.agent.join("Agent.md"), "agent note\n").unwrap();
    git_ok(&fx, &fx.agent, &["add", "-A"]);
    git_ok(&fx, &fx.agent, &["commit", "--quiet", "-m", "memory: note"]);
    git_ok(&fx, &fx.agent, &["push", "--quiet", "origin", "main"]);

    fx.bridge_once().assert().success();

    assert!(
        fx.bridge.join("Agent.md").exists(),
        "harness test 2: agent push should merge into the bridge checkout"
    );
}

// --- harness test 3 (+3b): same-file / unicode-filename conflict --------
// Human wins in place; agent's version is preserved as a sibling copy.

#[test]
fn same_file_conflict_human_wins_agent_copy_preserved() {
    let fx = Fixture::new("testvault");

    fs::write(fx.agent.join("Home.md"), "AGENT VERSION\n").unwrap();
    git_ok(&fx, &fx.agent, &["add", "-A"]);
    git_ok(
        &fx,
        &fx.agent,
        &["commit", "--quiet", "-m", "memory: edit home"],
    );
    git_ok(&fx, &fx.agent, &["push", "--quiet", "origin", "main"]);

    fs::write(fx.bridge.join("Home.md"), "HUMAN VERSION\n").unwrap();

    fx.bridge_once().assert().success();

    let content = fs::read_to_string(fx.bridge.join("Home.md")).unwrap();
    assert_eq!(
        content.trim(),
        "HUMAN VERSION",
        "harness test 3: human version should win in place"
    );

    let copy = find_conflict_copy(&fx.bridge);
    assert!(
        copy.is_some(),
        "harness test 3: an agent-conflict copy should be created"
    );
    let copy_content = fs::read_to_string(copy.unwrap()).unwrap();
    assert!(
        copy_content.contains("AGENT VERSION"),
        "harness test 3: agent-conflict copy should contain the agent's version, got: {copy_content:?}"
    );
}

#[test]
fn unicode_filename_conflict_same_policy() {
    let fx = Fixture::new("testvault");
    let filename = "Café ☕.md";

    fs::write(fx.agent.join(filename), "AGENT CAFE\n").unwrap();
    git_ok(&fx, &fx.agent, &["add", "-A"]);
    git_ok(&fx, &fx.agent, &["commit", "--quiet", "-m", "memory: cafe"]);
    git_ok(&fx, &fx.agent, &["push", "--quiet", "origin", "main"]);

    fs::write(fx.bridge.join(filename), "HUMAN CAFE\n").unwrap();

    fx.bridge_once().assert().success();

    let content = fs::read_to_string(fx.bridge.join(filename)).unwrap();
    assert_eq!(
        content.trim(),
        "HUMAN CAFE",
        "harness test 3b: human version should win in place for unicode filenames"
    );

    let copy = find_conflict_copy(&fx.bridge);
    assert!(
        copy.is_some(),
        "harness test 3b: an agent-conflict copy should be created for unicode filenames"
    );
    let copy_content = fs::read_to_string(copy.unwrap()).unwrap();
    assert!(
        copy_content.contains("AGENT CAFE"),
        "harness test 3b: agent-conflict copy should contain the agent's version"
    );
}

// --- extra: conflict copies preserve non-UTF-8 note content byte-for-byte
// (the spec is explicit that `git show :3:<path>` output must reach disk
// as raw bytes, never through `String`, since note content isn't
// guaranteed to be valid UTF-8).

#[test]
fn conflict_copy_preserves_non_utf8_note_content_as_raw_bytes() {
    let fx = Fixture::new("testvault");
    let agent_content: &[u8] = b"AGENT \xff\xfe BYTES\n";

    fs::write(fx.agent.join("Home.md"), agent_content).unwrap();
    git_ok(&fx, &fx.agent, &["add", "-A"]);
    git_ok(
        &fx,
        &fx.agent,
        &["commit", "--quiet", "-m", "memory: binary edit"],
    );
    git_ok(&fx, &fx.agent, &["push", "--quiet", "origin", "main"]);

    fs::write(fx.bridge.join("Home.md"), "HUMAN VERSION\n").unwrap();

    fx.bridge_once().assert().success();

    let copy = find_conflict_copy(&fx.bridge).expect("conflict copy should exist");
    let copy_bytes = fs::read(&copy).unwrap();
    assert_eq!(
        copy_bytes, agent_content,
        "conflict copy should preserve the agent's non-UTF-8 content byte-for-byte"
    );
}

// --- harness tests 4-5: offline queues locally, then recovers -----------
// (kept as one test: recovery inherently depends on the offline state the
// same run built, exactly like the bash harness's linear script.)

#[test]
fn offline_remote_queues_commit_and_bumps_failcount_then_recovers_on_reconnect() {
    let fx = Fixture::new("testvault");
    let gone = fx.root.path().join("remote.gone");

    fs::rename(&fx.remote, &gone).expect("rename remote.git away to simulate an outage");
    append_line(&fx.bridge.join("Home.md"), "offline edit");

    fx.bridge_once().assert().success(); // harness test 4a: offline run exits 0

    assert_eq!(
        last_commit_subject(&fx, &fx.bridge),
        "vault: human edits",
        "harness test 4b: the offline edit should still be committed locally"
    );
    assert!(
        failcount_path(&fx).exists(),
        "harness test 4c: a failure counter file should exist after an unreachable remote"
    );

    fs::rename(&gone, &fx.remote).expect("restore remote.git");

    fx.bridge_once().assert().success(); // harness test 5: recovery run

    assert!(
        !failcount_path(&fx).exists(),
        "harness test 5a: the failure counter should be cleared after the remote recovers"
    );

    git_ok(&fx, &fx.agent, &["pull", "--quiet"]);
    let content = fs::read_to_string(fx.agent.join("Home.md")).unwrap();
    assert!(
        content.contains("offline edit"),
        "harness test 5b: the queued offline commit should reach the remote"
    );
}

// --- new scenario 13: stale MERGE_HEAD is recovered before committing ---

#[test]
fn stale_merge_head_is_aborted_before_human_edits_are_committed() {
    let fx = Fixture::new("testvault");

    // Agent edits and pushes Home.md.
    fs::write(fx.agent.join("Home.md"), "AGENT VERSION\n").unwrap();
    git_ok(&fx, &fx.agent, &["add", "-A"]);
    git_ok(
        &fx,
        &fx.agent,
        &["commit", "--quiet", "-m", "memory: edit home"],
    );
    git_ok(&fx, &fx.agent, &["push", "--quiet", "origin", "main"]);

    // Bridge independently commits a conflicting human edit, then we
    // hand-craft a crashed-mid-merge state: fetch + merge conflicts, and
    // MERGE_HEAD is left in place (as if a prior run died mid-cycle).
    fs::write(fx.bridge.join("Home.md"), "HUMAN VERSION\n").unwrap();
    git_ok(&fx, &fx.bridge, &["add", "-A"]);
    git_ok(
        &fx,
        &fx.bridge,
        &["commit", "--quiet", "-m", "vault: human edits"],
    );
    git_ok(&fx, &fx.bridge, &["fetch", "--quiet", "origin"]);
    let merge = fx.git(&fx.bridge, &["merge", "--no-edit", "origin/main"]);
    assert!(
        !merge.status.success(),
        "setup: expected a conflicting merge"
    );
    assert!(
        gitx::merge_in_progress(&fx.bridge).unwrap(),
        "setup: MERGE_HEAD should be present before running the bridge"
    );

    fx.bridge_once().assert().success();

    // No conflict markers anywhere in HEAD's tree.
    let grep = fx.git(&fx.bridge, &["grep", "-I", "-l", "<<<<<<<", "HEAD"]);
    let hits = String::from_utf8_lossy(&grep.stdout);
    assert!(
        hits.trim().is_empty(),
        "scenario 13: HEAD should contain no conflict markers, found in: {hits:?}"
    );

    let content = fs::read_to_string(fx.bridge.join("Home.md")).unwrap();
    assert_eq!(
        content.trim(),
        "HUMAN VERSION",
        "scenario 13: human content should be in place after recovery"
    );

    let copy = find_conflict_copy(&fx.bridge);
    assert!(
        copy.is_some(),
        "scenario 13: an agent-conflict copy should exist after recovery"
    );

    assert_eq!(
        rev_parse(&fx, &fx.bridge, "HEAD"),
        rev_parse(&fx, &fx.remote, "main"),
        "scenario 13: the recovered merge should be pushed to the remote"
    );
}

// --- new scenario 14: lock exclusion, then stale-lock recovery ----------

#[test]
fn lock_excludes_concurrent_runs_and_recovers_once_stale() {
    let fx = Fixture::new("testvault");
    let lock = lock_path(&fx);

    fs::create_dir(&lock).expect("pre-create a fresh lock dir");
    append_line(
        &fx.bridge.join("Home.md"),
        "should not be committed while locked",
    );

    fx.bridge_once().assert().success();

    assert_ne!(
        last_commit_subject(&fx, &fx.bridge),
        "vault: human edits",
        "scenario 14a: a fresh lock should block the cycle entirely"
    );
    assert!(
        lock.exists(),
        "scenario 14a: the lock dir should be left untouched"
    );

    // Backdate the lock dir's mtime well past the 30-minute staleness
    // threshold via `touch -t` (unix; mirrors the bash prototype's own
    // staleness check via `find -mmin +30`).
    let touch = std::process::Command::new("touch")
        .arg("-t")
        .arg("202001010000")
        .arg(&lock)
        .status()
        .expect("spawn touch");
    assert!(touch.success(), "failed to backdate the lock dir's mtime");

    fx.bridge_once().assert().success();

    assert_eq!(
        last_commit_subject(&fx, &fx.bridge),
        "vault: human edits",
        "scenario 14b: a stale lock should be reclaimed and the cycle should proceed"
    );
    assert!(
        !lock.exists(),
        "scenario 14b: the lock should be released after a completed cycle"
    );
}
