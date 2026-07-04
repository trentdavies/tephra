//! Agent-facing commands: `clone`, `sync`, `status`.
//!
//! Ports `docs/reference/prototype/mem.sh` (see its header comment for the
//! original `mem clone|sync|status` contract) to the tephra CLI. See
//! `docs/DESIGN.md` §Command surface and §Bridge cycle semantics' last
//! paragraph for `sync`'s rebase-conflict "wedge rule": a conflicted rebase
//! must always be aborted, never left half-finished, because the next sync
//! would otherwise stage conflict markers and commit them on a detached
//! HEAD.
//!
//! `sync`'s rebase-conflict error is a *domain* failure (exit 1), not a
//! `config::UsageError` -- see that type's doc comment in `src/config.rs`.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::bridge::{FAILCOUNT_FILE_NAME, LASTCYCLE_FILE_NAME, LOCK_DIR_NAME};
use crate::config::Vault;
use crate::gitx;

const DEFAULT_SYNC_MESSAGE: &str = "memory: agent update";

/// `tephra clone`: idempotent clone of `vault.url` into `vault.work`.
pub fn clone(vault: &Vault) -> Result<()> {
    if vault.work.join(".git").exists() {
        println!("already cloned: {}", vault.work.display());
        return Ok(());
    }

    if let Some(parent) = vault.work.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent directory {}", parent.display()))?;
    }

    gitx::clone(&vault.url, &vault.work)?;
    println!("cloned: {}", vault.work.display());
    Ok(())
}

/// `tephra sync`: port of `mem sync` -- commit-all (if dirty), pull --rebase
/// --autostash, push with one bounded pull-rebase-then-push retry on
/// rejection. A rebase conflict is always aborted and reported as a plain
/// (exit 1) error naming the clone; it is never left in progress.
pub fn sync(name: &str, vault: &Vault, message: Option<&str>) -> Result<()> {
    let dir = vault.work.as_path();
    if !dir.join(".git").exists() {
        anyhow::bail!("not cloned; run: tephra clone {name}");
    }

    let msg = message.unwrap_or(DEFAULT_SYNC_MESSAGE);
    let committed = commit_all_if_dirty(dir, msg)?;

    pull_rebase(dir)?;

    let mut retried = false;
    let push = gitx::run(dir, &["push", "-q"])?;
    if !push.status.success() {
        retried = true;
        pull_rebase(dir)?;
        gitx::run_ok(dir, &["push", "-q"])?;
    }

    let commit_word = if committed {
        "committed"
    } else {
        "nothing to commit"
    };
    let push_word = if retried {
        "pushed (after retry)"
    } else {
        "pushed"
    };
    println!("sync: {commit_word}; pulled; {push_word}");
    Ok(())
}

/// Commit all changes in `dir` under `msg` if the tree is dirty. Returns
/// whether a commit was made.
fn commit_all_if_dirty(dir: &Path, msg: &str) -> Result<bool> {
    let porcelain = gitx::status_porcelain(dir)?;
    if porcelain.trim().is_empty() {
        return Ok(false);
    }
    gitx::run_ok(dir, &["add", "-A"])?;
    gitx::run_ok(dir, &["commit", "-q", "-m", msg])?;
    Ok(true)
}

/// `git pull --rebase --autostash`, with faithful failure diagnosis. If the
/// failure left a rebase in progress (a genuine conflict), abort it
/// (tolerating the abort's own failure) and report the wedge-rule message
/// -- this is the load-bearing "never leave a rebase in progress" rule from
/// `docs/DESIGN.md`. Any other pull failure (detached HEAD, unreachable or
/// deleted remote, ...) is NOT a rebase conflict: there is nothing to
/// abort, and "resolve manually" would be wrong advice, so git's own
/// diagnosis is surfaced instead. Both are plain domain errors (exit 1).
fn pull_rebase(dir: &Path) -> Result<()> {
    let pull = gitx::run(dir, &["pull", "-q", "--rebase", "--autostash"])?;
    if pull.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&pull.stderr);
    let stderr = stderr.trim();
    if gitx::rebase_in_progress(dir)? {
        let _ = gitx::run(dir, &["rebase", "--abort"]);
        anyhow::bail!(
            "rebase conflict in {} — resolve manually (local commit kept): {stderr}",
            dir.display()
        );
    }
    anyhow::bail!(
        "`git pull --rebase --autostash` failed in {}: {stderr}",
        dir.display()
    );
}

/// `tephra status`: best-effort snapshot of the work clone and the bridge
/// checkout. Every field is gathered independently and is `null`/absent
/// when ungatherable -- status reports, it doesn't judge, so this command
/// exits 0 even when the work clone or bridge doesn't exist. No network
/// calls are made: ahead/behind counts come from the last-known
/// remote-tracking refs (i.e. whatever the last `fetch`/`pull`/`clone` left
/// behind), not a fresh fetch.
pub fn status(name: &str, vault: &Vault, json: bool) -> Result<()> {
    let work = GitSnapshot::gather(&vault.work);
    let bridge = BridgeSnapshot::gather(&vault.bridge);
    let report = StatusReport {
        vault: name.to_string(),
        work,
        bridge,
        service: "unknown".to_string(),
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human(&report);
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct StatusReport {
    vault: String,
    work: GitSnapshot,
    bridge: BridgeSnapshot,
    service: String,
}

#[derive(Debug, Serialize)]
struct GitSnapshot {
    exists: bool,
    branch: Option<String>,
    dirty: Option<usize>,
    ahead: Option<usize>,
    behind: Option<usize>,
}

impl GitSnapshot {
    fn gather(dir: &Path) -> GitSnapshot {
        if !dir.join(".git").exists() {
            return GitSnapshot {
                exists: false,
                branch: None,
                dirty: None,
                ahead: None,
                behind: None,
            };
        }

        let branch = current_branch(dir);
        let dirty = dirty_count(dir);
        let (ahead, behind) = ahead_behind(dir, branch.as_deref());
        GitSnapshot {
            exists: true,
            branch,
            dirty,
            ahead,
            behind,
        }
    }
}

#[derive(Debug, Serialize)]
struct BridgeSnapshot {
    #[serde(flatten)]
    git: GitSnapshot,
    /// Consecutive remote-failure count from the bridge's counter file.
    /// `null` means the file is absent, i.e. either no consecutive failures
    /// (the bridge deletes the file on success and never writes 0) or no
    /// bridge checkout at all.
    failcount: Option<u32>,
    lock: bool,
    last_commit: Option<String>,
    /// RFC3339 UTC timestamp of the bridge's last completed cycle. `null`
    /// (together with `last_cycle_outcome`) means the heartbeat file is
    /// absent: the bridge has never completed a cycle.
    last_cycle_at: Option<String>,
    /// Outcome of the last completed cycle:
    /// `ok` | `remote-failure` | `conflict-abort`.
    last_cycle_outcome: Option<String>,
}

impl BridgeSnapshot {
    fn gather(dir: &Path) -> BridgeSnapshot {
        let git = GitSnapshot::gather(dir);
        let last_commit = if git.exists {
            last_commit_subject(dir)
        } else {
            None
        };
        let (last_cycle_at, last_cycle_outcome) = last_cycle(dir);
        BridgeSnapshot {
            git,
            failcount: failcount(dir),
            lock: lock_present(dir),
            last_commit,
            last_cycle_at,
            last_cycle_outcome,
        }
    }
}

fn current_branch(dir: &Path) -> Option<String> {
    let output = gitx::run(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).ok()?;
    if !output.status.success() {
        return None;
    }
    non_empty(String::from_utf8_lossy(&output.stdout).trim())
}

fn dirty_count(dir: &Path) -> Option<usize> {
    let porcelain = gitx::status_porcelain(dir).ok()?;
    Some(porcelain.lines().filter(|l| !l.is_empty()).count())
}

/// Ahead/behind vs. the branch's upstream, without any network access:
/// `@{upstream}` resolves to the local remote-tracking ref as of the last
/// fetch/pull/clone, so this is exactly the "last-known" state, not live
/// remote state. `None`/`None` when there's no upstream configured or the
/// count can't be gathered.
fn ahead_behind(dir: &Path, branch: Option<&str>) -> (Option<usize>, Option<usize>) {
    let Some(branch) = branch else {
        return (None, None);
    };
    let range = format!("{branch}...{branch}@{{upstream}}");
    let output = match gitx::run(dir, &["rev-list", "--left-right", "--count", &range]) {
        Ok(o) if o.status.success() => o,
        _ => return (None, None),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut parts = stdout.split_whitespace();
    let ahead = parts.next().and_then(|s| s.parse().ok());
    let behind = parts.next().and_then(|s| s.parse().ok());
    (ahead, behind)
}

fn last_commit_subject(dir: &Path) -> Option<String> {
    let output = gitx::run(dir, &["log", "-1", "--format=%s"]).ok()?;
    if !output.status.success() {
        return None;
    }
    non_empty(String::from_utf8_lossy(&output.stdout).trim())
}

fn failcount(bridge: &Path) -> Option<u32> {
    let path = bridge.join(".git").join(FAILCOUNT_FILE_NAME);
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

fn lock_present(bridge: &Path) -> bool {
    bridge.join(".git").join(LOCK_DIR_NAME).is_dir()
}

/// Parse the bridge's heartbeat file (`<RFC3339 UTC> <outcome>`) into its
/// `(at, outcome)` halves. `(None, None)` when the file is absent (the
/// bridge has never completed a cycle) or unreadable.
fn last_cycle(bridge: &Path) -> (Option<String>, Option<String>) {
    let path = bridge.join(".git").join(LASTCYCLE_FILE_NAME);
    let Ok(text) = std::fs::read_to_string(path) else {
        return (None, None);
    };
    match text.trim().split_once(' ') {
        Some((at, outcome)) => (non_empty(at), non_empty(outcome)),
        None => (non_empty(text.trim()), None),
    }
}

fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn print_human(report: &StatusReport) {
    println!("vault: {}", report.vault);
    println!();
    print_git_section("work", &report.work);
    println!();
    print_git_section("bridge", &report.bridge.git);
    println!("  failcount:    {}", opt_display(&report.bridge.failcount));
    let lock = match (report.bridge.git.exists, report.bridge.lock) {
        // No bridge checkout: lock state is unknowable, not "free".
        (false, _) => "-",
        (true, true) => "held",
        (true, false) => "free",
    };
    println!("  lock:         {lock}");
    println!("  last commit:  {}", opt_str(&report.bridge.last_commit));
    let last_cycle = match (
        &report.bridge.last_cycle_at,
        &report.bridge.last_cycle_outcome,
    ) {
        (Some(at), Some(outcome)) => format!("{at} ({outcome})"),
        (Some(at), None) => at.clone(),
        _ => "-".to_string(),
    };
    println!("  last cycle:   {last_cycle}");
    println!();
    println!("service: {}", report.service);
}

fn print_git_section(label: &str, g: &GitSnapshot) {
    println!("{label}:");
    println!("  exists:       {}", g.exists);
    println!("  branch:       {}", opt_str(&g.branch));
    println!("  dirty:        {}", opt_display(&g.dirty));
    println!(
        "  ahead/behind: {}/{}",
        opt_display(&g.ahead),
        opt_display(&g.behind)
    );
}

fn opt_str(o: &Option<String>) -> &str {
    o.as_deref().unwrap_or("-")
}

fn opt_display<T: std::fmt::Display>(o: &Option<T>) -> String {
    o.as_ref()
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_empty_treats_empty_string_as_none() {
        assert_eq!(non_empty(""), None);
        assert_eq!(non_empty("main"), Some("main".to_string()));
    }

    #[test]
    fn opt_str_defaults_to_dash() {
        assert_eq!(opt_str(&None), "-");
        assert_eq!(opt_str(&Some("main".to_string())), "main");
    }

    #[test]
    fn opt_display_defaults_to_dash() {
        assert_eq!(opt_display::<usize>(&None), "-");
        assert_eq!(opt_display(&Some(3usize)), "3");
    }

    #[test]
    fn git_snapshot_absent_when_no_git_dir() {
        let dir = tempfile::tempdir().unwrap();
        let snap = GitSnapshot::gather(dir.path());
        assert!(!snap.exists);
        assert_eq!(snap.branch, None);
        assert_eq!(snap.dirty, None);
        assert_eq!(snap.ahead, None);
        assert_eq!(snap.behind, None);
    }

    #[test]
    fn failcount_absent_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        assert_eq!(failcount(dir.path()), None);
    }

    #[test]
    fn failcount_parses_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git").join("tephra-bridge.failcount"), "5").unwrap();
        assert_eq!(failcount(dir.path()), Some(5));
    }

    #[test]
    fn last_cycle_absent_file_is_none_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        assert_eq!(last_cycle(dir.path()), (None, None));
    }

    #[test]
    fn last_cycle_parses_timestamp_and_outcome() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(
            dir.path().join(".git").join(LASTCYCLE_FILE_NAME),
            "2026-07-03T12:00:00Z ok\n",
        )
        .unwrap();
        assert_eq!(
            last_cycle(dir.path()),
            (
                Some("2026-07-03T12:00:00Z".to_string()),
                Some("ok".to_string())
            )
        );
    }

    #[test]
    fn lock_present_false_when_absent_true_when_dir_exists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        assert!(!lock_present(dir.path()));
        std::fs::create_dir(dir.path().join(".git").join("tephra-bridge.lock")).unwrap();
        assert!(lock_present(dir.path()));
    }
}
