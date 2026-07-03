//! Git subprocess runner.
//!
//! See `docs/DESIGN.md` §Core decision: tephra shells out to the system
//! `git` binary rather than using libgit2, so users' identity
//! (`includeIf`), SSH config, commit signing, and credential helpers all
//! resolve exactly as they would on the command line.
//!
//! This module's public API isn't called from `main.rs` yet (that lands
//! starting with the `bridge`/`agent` commands in later tasks), so the
//! non-test build has no live root reaching it. Silence dead_code until
//! then rather than wire it in prematurely.
#![allow(dead_code)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result};

/// Run `git -C <dir> <args...>`, capturing stdout/stderr. Never errors on a
/// nonzero exit — callers that care about success should inspect
/// `output.status`, or use [`run_ok`].
pub fn run(dir: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .with_context(|| format!("failed to execute `{}`", command_line(dir, args)))
}

/// Like [`run`], but a nonzero exit becomes an `Err` whose message includes
/// the full command line and trimmed stderr.
pub fn run_ok(dir: &Path, args: &[&str]) -> Result<Output> {
    let output = run(dir, args)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "`{}` failed ({}): {}",
            command_line(dir, args),
            output.status,
            stderr.trim()
        );
    }
    Ok(output)
}

fn command_line(dir: &Path, args: &[&str]) -> String {
    let mut parts = vec![
        "git".to_string(),
        "-C".to_string(),
        dir.display().to_string(),
    ];
    parts.extend(args.iter().map(|s| s.to_string()));
    parts.join(" ")
}

/// `git status --porcelain` output, verbatim.
pub fn status_porcelain(dir: &Path) -> Result<String> {
    let output = run_ok(dir, &["status", "--porcelain"])?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Paths with unmerged (conflicted) entries, via
/// `git diff --name-only -z --diff-filter=U`. NUL-delimited so unicode
/// filenames (routine in note vaults) round-trip correctly and are never
/// quoted/escaped the way plain `--name-only` output can be.
pub fn conflicted_paths(dir: &Path) -> Result<Vec<PathBuf>> {
    let output = run_ok(dir, &["diff", "--name-only", "-z", "--diff-filter=U"])?;
    Ok(parse_nul_paths(&output.stdout))
}

fn parse_nul_paths(bytes: &[u8]) -> Vec<PathBuf> {
    bytes
        .split(|&b| b == 0)
        .filter(|segment| !segment.is_empty())
        .map(|segment| PathBuf::from(os_string_from_bytes(segment)))
        .collect()
}

#[cfg(unix)]
fn os_string_from_bytes(bytes: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::OsStr::from_bytes(bytes).to_os_string()
}

#[cfg(not(unix))]
fn os_string_from_bytes(bytes: &[u8]) -> OsString {
    OsString::from(String::from_utf8_lossy(bytes).into_owned())
}

/// Resolve `<branch>@{upstream}` to `(remote, remote_branch)`, split on the
/// first `/` (so branch names containing `/` are handled correctly). `None`
/// when git reports no upstream is configured; other failures are `Err`.
pub fn upstream(dir: &Path, branch: &str) -> Result<Option<(String, String)>> {
    let arg = format!("{branch}@{{upstream}}");
    let args = ["rev-parse", "--abbrev-ref", "--symbolic-full-name", &arg];
    let output = run(dir, &args)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.to_lowercase().contains("no upstream") {
            return Ok(None);
        }
        anyhow::bail!(
            "`{}` failed ({}): {}",
            command_line(dir, &args),
            output.status,
            stderr.trim()
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    match split_upstream(trimmed) {
        Some(parsed) => Ok(Some(parsed)),
        None => anyhow::bail!("unexpected upstream format: {trimmed:?}"),
    }
}

/// Split `<remote>/<remote_branch>` on the first `/`.
fn split_upstream(s: &str) -> Option<(String, String)> {
    let (remote, branch) = s.split_once('/')?;
    if remote.is_empty() || branch.is_empty() {
        return None;
    }
    Some((remote.to_string(), branch.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // --- split_upstream / upstream parsing edges ---

    #[test]
    fn split_upstream_basic() {
        assert_eq!(
            split_upstream("origin/main"),
            Some(("origin".to_string(), "main".to_string()))
        );
    }

    #[test]
    fn split_upstream_splits_on_first_slash_only() {
        // Branch names containing '/' (e.g. "feature/foo") must not get
        // truncated: only the remote name is split off.
        assert_eq!(
            split_upstream("origin/feature/foo"),
            Some(("origin".to_string(), "feature/foo".to_string()))
        );
    }

    #[test]
    fn split_upstream_rejects_missing_slash() {
        assert_eq!(split_upstream("origin"), None);
    }

    #[test]
    fn split_upstream_rejects_empty_remote_or_branch() {
        assert_eq!(split_upstream("/main"), None);
        assert_eq!(split_upstream("origin/"), None);
    }

    // --- conflicted_paths / NUL parsing edges ---

    #[test]
    fn parse_nul_paths_on_empty_input() {
        assert_eq!(parse_nul_paths(b""), Vec::<PathBuf>::new());
    }

    #[test]
    fn parse_nul_paths_splits_on_nul_and_drops_trailing_empty() {
        let input = b"a.md\0Caf\xc3\xa9 \xe2\x98\x95.md\0";
        assert_eq!(
            parse_nul_paths(input),
            vec![
                PathBuf::from("a.md"),
                PathBuf::from("Caf\u{e9} \u{2615}.md"),
            ]
        );
    }

    // --- run / run_ok against a real git binary ---

    #[test]
    fn run_never_errors_on_nonzero_exit() {
        let dir = tempdir().unwrap();
        // Not a git repo: `git status` exits nonzero.
        let output = run(dir.path(), &["status"]).unwrap();
        assert!(!output.status.success());
    }

    #[test]
    fn run_ok_error_includes_command_and_stderr() {
        let dir = tempdir().unwrap();
        let err = run_ok(dir.path(), &["status"]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("git -C"), "should include command line: {msg}");
        assert!(msg.contains("status"), "should include command line: {msg}");
        assert!(
            !msg.trim().is_empty(),
            "should include (trimmed) stderr: {msg}"
        );
    }

    #[test]
    fn status_porcelain_reports_untracked_file() {
        let dir = tempdir().unwrap();
        run_ok(dir.path(), &["init", "--quiet"]).unwrap();
        assert_eq!(status_porcelain(dir.path()).unwrap(), "");

        std::fs::write(dir.path().join("new.md"), "hi").unwrap();
        let porcelain = status_porcelain(dir.path()).unwrap();
        assert!(porcelain.contains("new.md"), "got: {porcelain:?}");
    }

    #[test]
    fn upstream_is_none_when_branch_has_no_upstream_configured() {
        let dir = tempdir().unwrap();
        run_ok(dir.path(), &["init", "--quiet", "-b", "main"]).unwrap();
        run_ok(
            dir.path(),
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "init",
            ],
        )
        .unwrap();
        assert_eq!(upstream(dir.path(), "main").unwrap(), None);
    }
}
