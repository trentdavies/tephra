//! Integration tests for the agent-facing commands: `clone`, `sync`,
//! `status [--json]`. See `docs/DESIGN.md` §Command surface and §Bridge
//! cycle semantics (the sync wedge rule) and
//! `docs/reference/prototype/mem.sh` (the porting contract for clone/sync).

mod common;

use std::fs;
use std::path::Path;

use common::Fixture;

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

fn last_commit_subject(fx: &Fixture, dir: &Path) -> String {
    let output = fx.git(dir, &["log", "-1", "--format=%s"]);
    assert!(
        output.status.success(),
        "git log failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn commit_count_all(fx: &Fixture, dir: &Path) -> u32 {
    let output = fx.git(dir, &["rev-list", "--all", "--count"]);
    assert!(
        output.status.success(),
        "rev-list --all --count failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("rev-list --count should print an integer")
}

fn is_clean(fx: &Fixture, dir: &Path) -> bool {
    let output = fx.git(dir, &["status", "--porcelain"]);
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().is_empty()
}

// --- 1. clone -------------------------------------------------------------

#[test]
fn clone_fresh_creates_the_work_checkout() {
    let fx = Fixture::new("testvault");
    // The fixture pre-clones `agent` for the bridge/sync tests; remove it so
    // `tephra clone` has to do the actual cloning.
    fs::remove_dir_all(&fx.agent).unwrap();

    fx.tephra_cmd()
        .arg("clone")
        .arg(&fx.name)
        .assert()
        .success()
        .stdout(predicates::str::contains("cloned:"));

    assert!(fx.agent.join(".git").exists(), "clone should create .git");
    assert!(
        fx.agent.join("Home.md").exists(),
        "clone should check out the seeded content"
    );
}

#[test]
fn clone_twice_is_idempotent() {
    let fx = Fixture::new("testvault");
    fs::remove_dir_all(&fx.agent).unwrap();

    fx.tephra_cmd()
        .arg("clone")
        .arg(&fx.name)
        .assert()
        .success();

    fx.tephra_cmd()
        .arg("clone")
        .arg(&fx.name)
        .assert()
        .success()
        .stdout(predicates::str::contains("already cloned"));
}

// --- 2. sync clean tree ----------------------------------------------------

#[test]
fn sync_clean_tree_is_a_noop_and_exits_zero() {
    let fx = Fixture::new("testvault");

    fx.tephra_cmd().arg("sync").arg(&fx.name).assert().success();

    assert!(is_clean(&fx, &fx.agent));
}

// --- 3. sync dirty tree: default commit message -----------------------------

#[test]
fn sync_dirty_tree_commits_with_default_message_and_pushes() {
    let fx = Fixture::new("testvault");
    fs::write(fx.agent.join("Notes.md"), "agent wrote this\n").unwrap();

    fx.tephra_cmd().arg("sync").arg(&fx.name).assert().success();

    assert!(is_clean(&fx, &fx.agent));
    assert_eq!(last_commit_subject(&fx, &fx.agent), "memory: agent update");
    assert_eq!(
        last_commit_subject(&fx, &fx.remote),
        "memory: agent update",
        "the commit should be pushed to the remote"
    );
}

// --- 4. sync -m custom message ----------------------------------------------

#[test]
fn sync_dirty_tree_honors_custom_message() {
    let fx = Fixture::new("testvault");
    fs::write(fx.agent.join("Notes.md"), "agent wrote this\n").unwrap();

    fx.tephra_cmd()
        .arg("sync")
        .arg(&fx.name)
        .arg("-m")
        .arg("memory: custom note")
        .assert()
        .success();

    assert_eq!(last_commit_subject(&fx, &fx.remote), "memory: custom note");
}

// --- 5. the wedge drill: rebase conflict must never wedge the clone -------

#[test]
fn wedge_drill_rebase_conflict_leaves_a_clean_recoverable_clone() {
    let fx = Fixture::new("testvault");

    // Someone else (the bridge, in production) pushes a conflicting edit to
    // the same file via a separate clone.
    let racer = fx.root.path().join("racer");
    git_ok(
        &fx,
        fx.root.path(),
        &[
            "clone",
            "--quiet",
            fx.remote.to_str().unwrap(),
            racer.to_str().unwrap(),
        ],
    );
    fs::write(racer.join("Home.md"), "REMOTE VERSION\n").unwrap();
    git_ok(&fx, &racer, &["add", "-A"]);
    git_ok(
        &fx,
        &racer,
        &["commit", "--quiet", "-m", "remote: conflicting edit"],
    );
    git_ok(&fx, &racer, &["push", "--quiet", "origin", "main"]);

    // The agent independently edits the same lines.
    fs::write(fx.agent.join("Home.md"), "LOCAL VERSION\n").unwrap();

    fx.tephra_cmd()
        .arg("sync")
        .arg(&fx.name)
        .assert()
        .failure()
        .stderr(predicates::str::contains("rebase conflict"));

    // Work tree clean, on a branch, no rebase in progress.
    assert!(is_clean(&fx, &fx.agent), "tree must be clean after abort");
    let branch = fx.git(&fx.agent, &["symbolic-ref", "-q", "--short", "HEAD"]);
    assert!(
        branch.status.success(),
        "must be on a branch, not detached HEAD"
    );
    assert!(!fx.agent.join(".git/rebase-merge").exists());
    assert!(!fx.agent.join(".git/rebase-apply").exists());

    // The local commit (the agent's conflicting edit) is preserved.
    assert_eq!(
        last_commit_subject(&fx, &fx.agent),
        "memory: agent update",
        "the local commit must be kept, not discarded"
    );

    let commit_count_after_first = commit_count_all(&fx, &fx.agent);

    // Running again must fail identically -- no marker commits, no
    // detached HEAD, no drift in the repo's commit graph.
    fx.tephra_cmd()
        .arg("sync")
        .arg(&fx.name)
        .assert()
        .failure()
        .stderr(predicates::str::contains("rebase conflict"));

    assert!(is_clean(&fx, &fx.agent));
    assert!(!fx.agent.join(".git/rebase-merge").exists());
    assert!(!fx.agent.join(".git/rebase-apply").exists());

    let commit_count_after_second = commit_count_all(&fx, &fx.agent);
    assert_eq!(
        commit_count_after_first, commit_count_after_second,
        "the second wedged run must not add or lose any commits"
    );
}

// --- 6. push race: remote advances between the local commit and the push -

#[test]
fn sync_retries_and_succeeds_when_the_remote_advances_mid_push() {
    let fx = Fixture::new("testvault");

    // A racer clone the pre-push hook will use to advance the remote out
    // from under the agent's first push attempt, forcing a genuine
    // non-fast-forward rejection that `sync`'s bounded retry must recover
    // from.
    let racer = fx.root.path().join("racer");
    git_ok(
        &fx,
        fx.root.path(),
        &[
            "clone",
            "--quiet",
            fx.remote.to_str().unwrap(),
            racer.to_str().unwrap(),
        ],
    );

    let hook_path = fx.agent.join(".git/hooks/pre-push");
    let marker = fx.agent.join(".git/tephra-test-race-fired");
    let hook = format!(
        "#!/bin/sh\n\
         set -e\n\
         if [ ! -f '{marker}' ]; then\n\
         \x20\x20touch '{marker}'\n\
         \x20\x20echo 'race edit' >> '{racer}/Home.md'\n\
         \x20\x20git -C '{racer}' add -A\n\
         \x20\x20git -C '{racer}' commit --quiet -m 'race: concurrent push'\n\
         \x20\x20git -C '{racer}' push --quiet origin main\n\
         fi\n\
         exit 0\n",
        marker = marker.display(),
        racer = racer.display(),
    );
    fs::write(&hook_path, hook).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook_path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fs::write(fx.agent.join("Notes.md"), "agent wrote this\n").unwrap();

    fx.tephra_cmd().arg("sync").arg(&fx.name).assert().success();

    assert!(marker.exists(), "the pre-push hook should have fired once");
    assert!(is_clean(&fx, &fx.agent));

    // Both the racer's commit and the agent's commit reached the remote.
    let remote_log = fx.git(&fx.remote, &["log", "--format=%s", "main"]);
    let subjects = String::from_utf8_lossy(&remote_log.stdout);
    assert!(
        subjects.contains("race: concurrent push"),
        "remote log should include the racing commit, got: {subjects}"
    );
    assert!(
        subjects.contains("memory: agent update"),
        "remote log should include the agent's commit, got: {subjects}"
    );
}

// --- 7. status --json --------------------------------------------------

#[test]
fn status_json_has_stable_keys_and_correct_dirty_counts() {
    let fx = Fixture::new("testvault");

    fs::write(fx.agent.join("Untracked.md"), "dirty\n").unwrap();
    fs::write(fx.bridge.join(".git/tephra-bridge.failcount"), "3").unwrap();

    let output = fx
        .tephra_cmd()
        .arg("status")
        .arg("--json")
        .arg(&fx.name)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let value: serde_json::Value =
        serde_json::from_slice(&output).expect("status --json should emit valid JSON");

    assert_eq!(value["vault"], "testvault");
    assert_eq!(value["service"], "unknown");

    let work = &value["work"];
    assert_eq!(work["exists"], true);
    assert_eq!(work["dirty"], 1);
    assert!(work.get("branch").is_some());
    assert!(work.get("ahead").is_some());
    assert!(work.get("behind").is_some());

    let bridge = &value["bridge"];
    assert_eq!(bridge["exists"], true);
    assert_eq!(bridge["dirty"], 0);
    assert_eq!(bridge["failcount"], 3);
    assert_eq!(bridge["lock"], false);
    assert!(bridge.get("last_commit").is_some());
}

#[test]
fn status_human_mode_exits_zero_on_the_same_state() {
    let fx = Fixture::new("testvault");
    fs::write(fx.agent.join("Untracked.md"), "dirty\n").unwrap();
    fs::write(fx.bridge.join(".git/tephra-bridge.failcount"), "3").unwrap();

    fx.tephra_cmd()
        .arg("status")
        .arg(&fx.name)
        .assert()
        .success();
}

// --- 8. status/sync against a missing clone --------------------------------

#[test]
fn sync_on_missing_clone_errors_and_names_the_clone_command() {
    let fx = Fixture::new("testvault");
    fs::remove_dir_all(&fx.agent).unwrap();

    fx.tephra_cmd()
        .arg("sync")
        .arg(&fx.name)
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains("not cloned; run: tephra clone"));
}

#[test]
fn status_on_missing_clone_exits_zero_and_reports_work_absent() {
    let fx = Fixture::new("testvault");
    fs::remove_dir_all(&fx.agent).unwrap();

    let output = fx
        .tephra_cmd()
        .arg("status")
        .arg("--json")
        .arg(&fx.name)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let value: serde_json::Value =
        serde_json::from_slice(&output).expect("status --json should emit valid JSON");
    assert_eq!(value["work"]["exists"], false);
}
