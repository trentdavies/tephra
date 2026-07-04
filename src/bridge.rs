//! Bridge merge cycle — the daemon's core loop.
//!
//! Ports `docs/reference/prototype/memory-bridge.sh` step-for-step; see
//! `docs/DESIGN.md` §Bridge cycle semantics for why the step order (abort a
//! stale merge *before* committing human edits) is load-bearing: committing
//! a dirty tree while `MERGE_HEAD` exists would bake leftover conflict
//! markers into notes as a "human edits" commit.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use chrono::Local;

use crate::config::Vault;
use crate::gitx;
use crate::notify;

/// Lock dir name, under `<bridge>/.git/`. `mkdir` is atomic on every
/// platform tephra targets (unlike `flock`, which isn't stock on macOS).
const LOCK_DIR_NAME: &str = "tephra-bridge.lock";
/// A lock dir older than this is assumed abandoned by a crashed run.
const LOCK_STALE_AFTER: Duration = Duration::from_secs(30 * 60);
/// Failure-counter file name, under `<bridge>/.git/`.
const FAILCOUNT_FILE_NAME: &str = "tephra-bridge.failcount";
/// Consecutive remote failures before notifying the desktop (~30 min at the
/// service's 2-minute cycle interval).
const NOTIFY_AFTER: u32 = 15;

/// Run one bridge merge cycle for `vault` (`name` is the configured vault
/// name, used only in log lines). Step order matches
/// `docs/DESIGN.md` §Bridge cycle semantics exactly.
pub fn run_once(name: &str, vault: &Vault) -> Result<()> {
    let bridge = vault.bridge.as_path();
    validate_bridge_dir(bridge)?;

    let _lock = match BridgeLock::acquire(bridge, name)? {
        Some(lock) => lock,
        None => return Ok(()),
    };

    // 1. Abort any half-finished merge from a crashed prior run. Must run
    // before committing human edits (see module docs). If the abort itself
    // fails and MERGE_HEAD is still there, committing would bake conflict
    // markers into notes as "human edits" — the exact hazard this ordering
    // exists to prevent — so bail out instead of proceeding.
    if gitx::merge_in_progress(bridge)? {
        let abort = gitx::run(bridge, &["merge", "--abort"])?;
        if !abort.status.success() && gitx::merge_in_progress(bridge)? {
            anyhow::bail!(
                "could not abort the in-progress merge in {}; refusing to commit human \
                 edits on top of a conflicted tree: {}",
                bridge.display(),
                String::from_utf8_lossy(&abort.stderr).trim()
            );
        }
    }

    // 2. Commit anything the sync app delivered.
    let porcelain = gitx::status_porcelain(bridge)?;
    if !porcelain.trim().is_empty() {
        gitx::run_ok(bridge, &["add", "-A"])?;
        gitx::run_ok(bridge, &["commit", "-q", "-m", "vault: human edits"])?;
        log(name, "committed human edits");
    }

    // 3. Resolve upstream. No remote-name assumption (DESIGN.md): the
    // bash prototype's hardcoded remote name was a real footgun.
    let (remote, remote_branch) = match gitx::upstream(bridge, &vault.branch)? {
        Some(pair) => pair,
        None => resolve_fallback_remote(bridge, name, &vault.branch)?,
    };
    let remote_ref = format!("{remote}/{remote_branch}");

    // 4. Fetch.
    let fetch = gitx::run(bridge, &["fetch", "-q", &remote])?;
    if !fetch.status.success() {
        return remote_failed(bridge, name, &remote);
    }
    remote_ok(bridge)?;

    // 5. Merge.
    let merge = gitx::run(bridge, &["merge", "-q", "--no-edit", &remote_ref])?;
    if !merge.status.success() && !resolve_conflicts_and_commit(bridge, name)? {
        // Resolution commit failed: merge already aborted and logged;
        // tree is clean, skip the push, next cycle retries.
        return Ok(());
    }

    // 6. Push, one bounded retry after a re-fetch/merge (a push race).
    // Never `--force`. The refspec pushes the local branch to the SAME
    // remote branch the merge just consumed: pushing bare `vault.branch`
    // would silently split brain whenever the local branch tracks a
    // differently-named remote branch (e.g. local main -> origin/master).
    let refspec = format!("{}:{}", vault.branch, remote_branch);
    let push = gitx::run(bridge, &["push", "-q", &remote, &refspec])?;
    if !push.status.success() {
        let fetch2 = gitx::run(bridge, &["fetch", "-q", &remote])?;
        if !fetch2.status.success() {
            return remote_failed(bridge, name, &remote);
        }
        let merge2 = gitx::run(bridge, &["merge", "-q", "--no-edit", &remote_ref])?;
        if !merge2.status.success() {
            let _ = gitx::run(bridge, &["merge", "--abort"]);
            return remote_failed(bridge, name, &remote);
        }
        let push2 = gitx::run(bridge, &["push", "-q", &remote, &refspec])?;
        if !push2.status.success() {
            return remote_failed(bridge, name, &remote);
        }
    }

    log(name, "cycle complete");
    Ok(())
}

/// The bridge dir must exist and be a git working tree; anything else is a
/// clear, immediate (exit 1) error rather than a confusing git failure
/// three steps later.
fn validate_bridge_dir(bridge: &Path) -> Result<()> {
    if !bridge.is_dir() {
        anyhow::bail!("bridge directory does not exist: {}", bridge.display());
    }
    let check = gitx::run(bridge, &["rev-parse", "--is-inside-work-tree"])?;
    if !check.status.success() {
        anyhow::bail!(
            "bridge directory is not a git repository: {}",
            bridge.display()
        );
    }
    Ok(())
}

/// When the branch has no upstream configured, fall back to the checkout's
/// sole remote. Zero or multiple remotes is a configuration error naming
/// the fix, since tephra can't guess which one is authoritative.
fn resolve_fallback_remote(bridge: &Path, name: &str, branch: &str) -> Result<(String, String)> {
    let remotes = gitx::remotes(bridge)?;
    match remotes.as_slice() {
        [only] => {
            log(
                name,
                &format!("no upstream configured for '{branch}'; using sole remote '{only}'"),
            );
            Ok((only.clone(), branch.to_string()))
        }
        [] => anyhow::bail!(
            "branch '{branch}' has no upstream and {} has no remotes configured; \
             add one, then run `git -C {} branch --set-upstream-to=<remote>/{branch} {branch}`",
            bridge.display(),
            bridge.display()
        ),
        many => anyhow::bail!(
            "branch '{branch}' has no upstream and {} has multiple remotes ({}); \
             run `git -C {} branch --set-upstream-to=<remote>/{branch} {branch}`",
            bridge.display(),
            many.join(", "),
            bridge.display()
        ),
    }
}

/// Handle a failed `git merge`: for each conflicted path, preserve the
/// human version in place and the agent's version as a sibling copy, then
/// commit the resolution. Returns `Ok(true)` if the cycle should continue
/// to the push step, `Ok(false)` if the resolution commit itself failed
/// (the merge is aborted and logged; the caller must stop here without
/// pushing).
fn resolve_conflicts_and_commit(bridge: &Path, name: &str) -> Result<bool> {
    let conflicts = gitx::conflicted_paths(bridge)?;
    let stamp = Local::now().format("%Y-%m-%d").to_string();

    for path in &conflicts {
        resolve_one_conflict(bridge, path, &stamp, name);
    }

    let commit_msg = format!(
        "merge: agent changes ({} conflict(s) preserved alongside)",
        conflicts.len()
    );
    let commit = gitx::run(bridge, &["commit", "-q", "--no-edit", "-m", &commit_msg])?;
    if !commit.status.success() {
        let _ = gitx::run(bridge, &["merge", "--abort"]);
        let stderr = String::from_utf8_lossy(&commit.stderr);
        log(name, &format!("conflict merge FAILED: {}", stderr.trim()));
        return Ok(false);
    }
    log(
        name,
        &format!("merged with {} preserved conflict(s)", conflicts.len()),
    );
    Ok(true)
}

/// Resolve one conflicted path: preserve the agent's version (git stage 3,
/// "theirs") as a sibling conflict-copy file, then keep the human version
/// in place (`checkout --ours`, falling back to `rm` for a both-deleted-ish
/// conflict). Every step is best-effort, exactly like the bash prototype:
/// if something goes fundamentally wrong here, the path is left unresolved
/// and the caller's subsequent `git commit` fails, which aborts the whole
/// conflicted merge for a retry next cycle rather than committing something
/// half-fixed.
fn resolve_one_conflict(bridge: &Path, path: &Path, stamp: &str, name: &str) {
    let Some(path_str) = path.to_str() else {
        log(
            name,
            &format!(
                "skipping conflict resolution for non-UTF-8 path {path:?}; \
                 the merge commit will fail and this cycle retries next time"
            ),
        );
        return;
    };

    // Preserve the agent's version as raw bytes -- never through `String`,
    // since note content isn't guaranteed to be valid UTF-8. Absent (agent
    // deleted the file, so stage 3 doesn't exist): no copy, per contract.
    let mut copy_rel: Option<PathBuf> = None;
    if let Ok(show) = gitx::run(bridge, &["show", &format!(":3:{path_str}")]) {
        if show.status.success() {
            let rel = conflict_copy_relpath(bridge, path, stamp);
            match fs::write(bridge.join(&rel), &show.stdout) {
                Ok(()) => copy_rel = Some(rel),
                Err(e) => log(
                    name,
                    &format!(
                        "failed to write conflict copy {}: {e}; the agent version is \
                         still recoverable from the merge's second parent",
                        rel.display()
                    ),
                ),
            }
        }
    }

    // Human wins in place.
    let checkout_ok = matches!(
        gitx::run(bridge, &["checkout", "--ours", "--", path_str]),
        Ok(o) if o.status.success()
    );
    if !checkout_ok {
        let _ = gitx::run(bridge, &["rm", "-q", "--", path_str]);
    }
    let _ = gitx::run(bridge, &["add", "--", path_str]);
    if let Some(copy_str) = copy_rel.as_ref().and_then(|r| r.to_str()) {
        let _ = gitx::run(bridge, &["add", "--", copy_str]);
    }
}

/// Build the sibling conflict-copy path for a conflicted repo-relative
/// path, uniquified against files already on disk in the bridge: a second
/// conflict on the same file on the same day must not clobber the first
/// preserved copy, so taken names get a ` (2)`, ` (3)`, ... counter.
fn conflict_copy_relpath(bridge: &Path, path: &Path, stamp: &str) -> PathBuf {
    (1u32..)
        .map(|n| conflict_copy_candidate(path, stamp, n))
        .find(|rel| !bridge.join(rel).exists())
        .expect("some conflict-copy counter value is always free")
}

/// The `n`-th candidate name for a conflict copy:
/// `<stem> (agent conflict YYYY-MM-DD).md` for `.md` files,
/// `<full-name>.agent-conflict-YYYY-MM-DD` otherwise; `n > 1` appends a
/// ` (n)` counter before the `.md` extension (or at the end for non-`.md`
/// files). Any directory prefix is preserved.
fn conflict_copy_candidate(path: &Path, stamp: &str, n: u32) -> PathBuf {
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let counter = if n <= 1 {
        String::new()
    } else {
        format!(" ({n})")
    };
    let copy_name = match file_name.strip_suffix(".md") {
        Some(stem) => format!("{stem} (agent conflict {stamp}){counter}.md"),
        None => format!("{file_name}.agent-conflict-{stamp}{counter}"),
    };
    match path.parent() {
        Some(parent) if parent != Path::new("") => parent.join(copy_name),
        _ => PathBuf::from(copy_name),
    }
}

fn failcount_path(bridge: &Path) -> PathBuf {
    bridge.join(".git").join(FAILCOUNT_FILE_NAME)
}

/// Increment the on-disk failure counter, log it, and notify the desktop
/// once the threshold is hit. Always returns `Ok(())`: a remote failure
/// queues commits locally for retry next cycle, and is never treated as a
/// hard (nonzero-exit) error.
fn remote_failed(bridge: &Path, name: &str, remote: &str) -> Result<()> {
    let path = failcount_path(bridge);
    let n = fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0u32)
        .saturating_add(1);
    fs::write(&path, n.to_string()).with_context(|| format!("writing {}", path.display()))?;
    log(name, &format!("remote unreachable (attempt {n})"));
    if n == NOTIFY_AFTER {
        notify::notify(
            &format!("tephra-bridge {name}"),
            &format!("{remote} unreachable; vault commits queuing locally"),
        );
    }
    Ok(())
}

/// Clear the failure counter after a successful fetch.
fn remote_ok(bridge: &Path) -> Result<()> {
    let path = failcount_path(bridge);
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(())
}

/// RAII guard for the mkdir-based single-instance lock at
/// `<bridge>/.git/tephra-bridge.lock`. Released on every exit path via
/// `Drop`.
struct BridgeLock {
    path: PathBuf,
}

impl BridgeLock {
    /// Try to acquire the lock. `Ok(None)` means another run currently
    /// holds it (the caller should skip this cycle, exit 0 without acting);
    /// a lock dir older than 30 minutes is assumed abandoned by a crashed
    /// run and is removed and retaken instead.
    fn acquire(bridge: &Path, name: &str) -> Result<Option<BridgeLock>> {
        let path = bridge.join(".git").join(LOCK_DIR_NAME);
        match fs::create_dir(&path) {
            Ok(()) => Ok(Some(BridgeLock { path })),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if lock_is_stale(&path)? {
                    let _ = fs::remove_dir(&path);
                    match fs::create_dir(&path) {
                        Ok(()) => Ok(Some(BridgeLock { path })),
                        Err(_) => {
                            log(
                                name,
                                "lock contended immediately after stale removal; \
                                 skipping this cycle",
                            );
                            Ok(None)
                        }
                    }
                } else {
                    log(
                        name,
                        "bridge already running (lock held); skipping this cycle",
                    );
                    Ok(None)
                }
            }
            Err(e) => {
                Err(e).with_context(|| format!("failed to create lock dir {}", path.display()))
            }
        }
    }
}

impl Drop for BridgeLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.path);
    }
}

fn lock_is_stale(path: &Path) -> Result<bool> {
    let modified = fs::metadata(path)
        .with_context(|| format!("stat lock dir {}", path.display()))?
        .modified()
        .with_context(|| format!("mtime of lock dir {}", path.display()))?;
    Ok(SystemTime::now()
        .duration_since(modified)
        .unwrap_or_default()
        > LOCK_STALE_AFTER)
}

/// `[tephra-bridge/<name>] <YYYY-MM-DD HH:MM:SS> <msg>` -- the date-inclusive
/// format the prototype's dateless logs were flagged for in review.
fn log(name: &str, msg: &str) {
    let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    println!("{}", format_log_line(name, &now, msg));
}

fn format_log_line(name: &str, timestamp: &str, msg: &str) -> String {
    format!("[tephra-bridge/{name}] {timestamp} {msg}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_copy_candidate_for_md_file() {
        let got = conflict_copy_candidate(Path::new("Home.md"), "2026-07-03", 1);
        assert_eq!(got, PathBuf::from("Home (agent conflict 2026-07-03).md"));
    }

    #[test]
    fn conflict_copy_candidate_for_non_md_file() {
        let got = conflict_copy_candidate(Path::new("notes.txt"), "2026-07-03", 1);
        assert_eq!(got, PathBuf::from("notes.txt.agent-conflict-2026-07-03"));
    }

    #[test]
    fn conflict_copy_candidate_preserves_directory_prefix() {
        let got = conflict_copy_candidate(Path::new("agents/log.md"), "2026-07-03", 1);
        assert_eq!(
            got,
            PathBuf::from("agents/log (agent conflict 2026-07-03).md")
        );
    }

    #[test]
    fn conflict_copy_candidate_handles_unicode_stem() {
        let got = conflict_copy_candidate(Path::new("Café ☕.md"), "2026-07-03", 1);
        assert_eq!(got, PathBuf::from("Café ☕ (agent conflict 2026-07-03).md"));
    }

    #[test]
    fn conflict_copy_candidate_counter_goes_before_md_extension() {
        let got = conflict_copy_candidate(Path::new("Home.md"), "2026-07-03", 2);
        assert_eq!(
            got,
            PathBuf::from("Home (agent conflict 2026-07-03) (2).md")
        );
    }

    #[test]
    fn conflict_copy_candidate_counter_appends_for_non_md() {
        let got = conflict_copy_candidate(Path::new("notes.txt"), "2026-07-03", 3);
        assert_eq!(
            got,
            PathBuf::from("notes.txt.agent-conflict-2026-07-03 (3)")
        );
    }

    #[test]
    fn conflict_copy_relpath_uniquifies_against_existing_copies() {
        let dir = tempfile::tempdir().unwrap();
        let first = conflict_copy_relpath(dir.path(), Path::new("Home.md"), "2026-07-03");
        assert_eq!(first, PathBuf::from("Home (agent conflict 2026-07-03).md"));

        fs::write(dir.path().join(&first), "occupied").unwrap();
        let second = conflict_copy_relpath(dir.path(), Path::new("Home.md"), "2026-07-03");
        assert_eq!(
            second,
            PathBuf::from("Home (agent conflict 2026-07-03) (2).md")
        );

        fs::write(dir.path().join(&second), "occupied").unwrap();
        let third = conflict_copy_relpath(dir.path(), Path::new("Home.md"), "2026-07-03");
        assert_eq!(
            third,
            PathBuf::from("Home (agent conflict 2026-07-03) (3).md")
        );
    }

    /// A bridge-shaped tempdir (has a `.git` subdir) for exercising the
    /// failcount paths without a full git fixture.
    fn fake_bridge() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        dir
    }

    #[test]
    fn remote_failed_saturates_at_u32_max_instead_of_panicking() {
        let dir = fake_bridge();
        fs::write(failcount_path(dir.path()), u32::MAX.to_string()).unwrap();

        remote_failed(dir.path(), "t", "origin").unwrap();

        assert_eq!(
            fs::read_to_string(failcount_path(dir.path())).unwrap(),
            u32::MAX.to_string(),
            "the counter must saturate, not overflow"
        );
    }

    #[test]
    fn remote_failed_treats_corrupt_failcount_as_zero() {
        let dir = fake_bridge();
        fs::write(failcount_path(dir.path()), "not a number\n").unwrap();

        remote_failed(dir.path(), "t", "origin").unwrap();

        assert_eq!(
            fs::read_to_string(failcount_path(dir.path())).unwrap(),
            "1",
            "a corrupt counter file should restart the count at 1"
        );
    }

    #[test]
    fn remote_failed_increments_existing_count() {
        let dir = fake_bridge();
        fs::write(failcount_path(dir.path()), "3").unwrap();

        remote_failed(dir.path(), "t", "origin").unwrap();

        assert_eq!(fs::read_to_string(failcount_path(dir.path())).unwrap(), "4");
    }

    #[test]
    fn format_log_line_matches_expected_shape() {
        let line = format_log_line("personal", "2026-07-03 12:00:00", "cycle complete");
        assert_eq!(
            line,
            "[tephra-bridge/personal] 2026-07-03 12:00:00 cycle complete"
        );
    }

    #[test]
    fn lock_is_stale_false_for_freshly_created_dir() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("lock");
        fs::create_dir(&lock).unwrap();
        assert!(!lock_is_stale(&lock).unwrap());
    }

    #[test]
    fn lock_is_stale_true_for_old_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("lock");
        fs::create_dir(&lock).unwrap();
        // Backdate via `touch -t` (unix); skip rather than flake on
        // environments without it.
        let status = std::process::Command::new("touch")
            .arg("-t")
            .arg("202001010000")
            .arg(&lock)
            .status();
        if !matches!(status, Ok(s) if s.success()) {
            return;
        }
        assert!(lock_is_stale(&lock).unwrap());
    }
}
