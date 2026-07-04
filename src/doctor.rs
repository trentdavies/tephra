//! `tephra doctor [VAULT]`: environment/health checks for an already-
//! resolved vault.
//!
//! See `docs/DESIGN.md` §Command surface. House style (`ok:`/`warn:`/
//! `FAIL:` + an indented remediation line under any `FAIL:`, exit 1 on any
//! FAIL) matches `src/obsidian.rs`'s `doctor` exactly.
//!
//! Config-loading and vault-resolution failures are usage errors and are
//! never printed as a check line here: `main.rs::cmd_doctor` resolves the
//! vault via `config::load`/`config::resolve_vault` *before* calling into
//! this module, so any config problem propagates as its own top-level
//! error (exit 2) instead of being folded into this module's ok/warn/FAIL
//! report.

use std::path::Path;
use std::process::Command;

use anyhow::Result;

use crate::agent;
use crate::bridge;
use crate::config::Vault;
use crate::gitx;

/// git ≥ this is required (`docs/DESIGN.md` §Core decision).
const MIN_GIT_VERSION: (u32, u32) = (2, 36);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CheckOutcome {
    status: Status,
    message: String,
    remediation: Option<String>,
}

impl CheckOutcome {
    fn ok(message: impl Into<String>) -> Self {
        CheckOutcome {
            status: Status::Ok,
            message: message.into(),
            remediation: None,
        }
    }

    fn warn(message: impl Into<String>) -> Self {
        CheckOutcome {
            status: Status::Warn,
            message: message.into(),
            remediation: None,
        }
    }

    fn fail(message: impl Into<String>, remediation: impl Into<String>) -> Self {
        CheckOutcome {
            status: Status::Fail,
            message: message.into(),
            remediation: Some(remediation.into()),
        }
    }

    fn is_fail(&self) -> bool {
        self.status == Status::Fail
    }

    fn print(&self) {
        let prefix = match self.status {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "FAIL",
        };
        println!("{prefix}: {}", self.message);
        if let Some(remediation) = &self.remediation {
            println!("  {remediation}");
        }
    }
}

/// `tephra doctor [VAULT]`'s checks, run in `docs/DESIGN.md` order for an
/// already-resolved vault:
///
/// 1. git present and `>= 2.36`.
/// 2. (handled by the caller, see the module doc comment -- not printed
///    here.)
/// 3. bridge dir exists and is a git repository.
/// 4. git identity resolves in the bridge.
/// 5. upstream configured (or a usable sole-remote fallback).
/// 6. the resolved remote is reachable.
/// 7. stale-state report: lock / failcount / heartbeat (informational --
///    `ok`/`warn` only, never `FAIL`).
/// 8. work clone exists.
///
/// Checks 4-7 are skipped (with a `warn:` line explaining why) when check 3
/// itself fails -- there's nothing more to learn from running git commands
/// against a bridge directory that doesn't exist or isn't a repo.
///
/// Returns an error (exit 1, a domain failure -- see `config::UsageError`'s
/// doc comment for why this isn't a usage error) iff any check `FAIL`ed.
pub fn doctor(name: &str, vault: &Vault) -> Result<()> {
    let mut any_fail = false;

    let git_check = check_git_version();
    any_fail |= git_check.is_fail();
    git_check.print();

    let bridge_dir = vault.bridge.as_path();
    let bridge_check = check_bridge_dir(bridge_dir, &vault.url);
    any_fail |= bridge_check.is_fail();
    let bridge_available = !bridge_check.is_fail();
    bridge_check.print();

    if bridge_available {
        let identity_check = check_identity(bridge_dir);
        any_fail |= identity_check.is_fail();
        identity_check.print();

        let (upstream_check, remote) = check_upstream(bridge_dir, &vault.branch);
        any_fail |= upstream_check.is_fail();
        upstream_check.print();

        let reachable_check = check_remote_reachable(bridge_dir, remote.as_deref());
        any_fail |= reachable_check.is_fail();
        reachable_check.print();

        check_lock_state(bridge_dir).print();
        check_failcount(bridge_dir).print();
        check_heartbeat(bridge_dir).print();
    } else {
        println!("warn: skipping remaining bridge checks (bridge unavailable)");
    }

    check_work_clone(name, &vault.work).print();

    if any_fail {
        anyhow::bail!("tephra doctor found problems");
    }
    Ok(())
}

// --------------------------------------------------------------------
// 1. git present + version
// --------------------------------------------------------------------

fn check_git_version() -> CheckOutcome {
    const INSTALL_REMEDIATION: &str = "install git >= 2.36 and ensure it's on PATH";
    match Command::new("git").arg("version").output() {
        Err(e) => CheckOutcome::fail(
            format!("could not run `git version`: {e}"),
            INSTALL_REMEDIATION,
        ),
        Ok(output) if !output.status.success() => CheckOutcome::fail(
            format!(
                "`git version` exited with an error: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            INSTALL_REMEDIATION,
        ),
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            match parse_git_version(&stdout) {
                Some(v) if version_at_least(v, MIN_GIT_VERSION) => {
                    CheckOutcome::ok(format!("git {}.{}.{} (>= 2.36 required)", v.0, v.1, v.2))
                }
                Some(v) => CheckOutcome::fail(
                    format!(
                        "git {}.{}.{} is older than the required 2.36",
                        v.0, v.1, v.2
                    ),
                    "upgrade git to >= 2.36",
                ),
                None => {
                    CheckOutcome::warn(format!("could not parse `git version` output: {stdout:?}"))
                }
            }
        }
    }
}

/// Parse `git version`'s stdout -- e.g. `git version 2.50.1 (Apple
/// Git-155)` or a plain `git version 2.36` with no trailing parenthetical --
/// into `(major, minor, patch)`. `patch` defaults to 0 when the version
/// string omits it.
fn parse_git_version(stdout: &str) -> Option<(u32, u32, u32)> {
    let rest = stdout.trim().strip_prefix("git version ")?;
    let version = rest.split_whitespace().next()?;
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts
        .next()
        .and_then(|p| p.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    Some((major, minor, patch))
}

fn version_at_least(have: (u32, u32, u32), min: (u32, u32)) -> bool {
    (have.0, have.1) >= min
}

// --------------------------------------------------------------------
// 3. bridge dir exists + is a git repo
// --------------------------------------------------------------------

fn check_bridge_dir(bridge: &Path, url: &str) -> CheckOutcome {
    let clone_remediation = format!("clone it: git clone {url} {}", bridge.display());
    if !bridge.is_dir() {
        return CheckOutcome::fail(
            format!("bridge directory does not exist: {}", bridge.display()),
            clone_remediation,
        );
    }
    match gitx::run(bridge, &["rev-parse", "--is-inside-work-tree"]) {
        Ok(output) if output.status.success() => CheckOutcome::ok(format!(
            "bridge exists and is a git repository ({})",
            bridge.display()
        )),
        Ok(_) => CheckOutcome::fail(
            format!(
                "bridge directory is not a git repository: {}",
                bridge.display()
            ),
            clone_remediation,
        ),
        Err(e) => CheckOutcome::fail(
            format!("could not check bridge directory {}: {e}", bridge.display()),
            clone_remediation,
        ),
    }
}

// --------------------------------------------------------------------
// 4. git identity resolves in the bridge
// --------------------------------------------------------------------

fn check_identity(bridge: &Path) -> CheckOutcome {
    match gitx::run(bridge, &["var", "GIT_COMMITTER_IDENT"]) {
        Ok(output) if output.status.success() => {
            let ident = String::from_utf8_lossy(&output.stdout);
            CheckOutcome::ok(format!(
                "git identity resolves ({})",
                strip_ident_timestamp(ident.trim())
            ))
        }
        Ok(output) => CheckOutcome::fail(
            format!(
                "git identity does not resolve in {}: {}",
                bridge.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            format!(
                "set an identity for this checkout, e.g. `git -C {} config user.email \
                 you@example.com` (or check for an includeIf in ~/.gitconfig that isn't \
                 matching this path)",
                bridge.display()
            ),
        ),
        Err(e) => CheckOutcome::fail(
            format!("could not run `git var GIT_COMMITTER_IDENT`: {e}"),
            "ensure git is installed and on PATH",
        ),
    }
}

/// `"Name <email> 1700000000 +0000"` -> `"Name <email>"`: drop
/// `GIT_COMMITTER_IDENT`'s trailing unix-timestamp-and-timezone, which is
/// just clutter in a doctor `ok:` line.
fn strip_ident_timestamp(ident: &str) -> &str {
    match ident.rfind('>') {
        Some(idx) => &ident[..=idx],
        None => ident,
    }
}

// --------------------------------------------------------------------
// 5. upstream configured
// --------------------------------------------------------------------

/// Returns the check outcome, plus the remote name resolved for check 6
/// (`None` when no remote could be determined at all -- check 6 then skips
/// itself with a `warn:`).
fn check_upstream(bridge: &Path, branch: &str) -> (CheckOutcome, Option<String>) {
    match gitx::upstream(bridge, branch) {
        Ok(Some((remote, remote_branch))) => (
            CheckOutcome::ok(format!("upstream configured ({remote}/{remote_branch})")),
            Some(remote),
        ),
        Ok(None) => match gitx::remotes(bridge) {
            Ok(remotes) => match remotes.as_slice() {
                [only] => (
                    CheckOutcome::warn(format!(
                        "no upstream configured for '{branch}'; will fall back to sole remote '{only}'"
                    )),
                    Some(only.clone()),
                ),
                [] => (
                    CheckOutcome::fail(
                        format!(
                            "branch '{branch}' has no upstream and {} has no remotes configured",
                            bridge.display()
                        ),
                        set_upstream_remediation(bridge, branch),
                    ),
                    None,
                ),
                many => (
                    CheckOutcome::fail(
                        format!(
                            "branch '{branch}' has no upstream and {} has multiple remotes ({})",
                            bridge.display(),
                            many.join(", ")
                        ),
                        set_upstream_remediation(bridge, branch),
                    ),
                    None,
                ),
            },
            Err(e) => (
                CheckOutcome::warn(format!(
                    "could not list remotes in {}: {e}",
                    bridge.display()
                )),
                None,
            ),
        },
        Err(e) => (
            CheckOutcome::warn(format!(
                "could not determine upstream in {}: {e}",
                bridge.display()
            )),
            None,
        ),
    }
}

fn set_upstream_remediation(bridge: &Path, branch: &str) -> String {
    format!(
        "run: git -C {} branch --set-upstream-to=<remote>/{branch} {branch}",
        bridge.display()
    )
}

// --------------------------------------------------------------------
// 6. remote reachable
// --------------------------------------------------------------------

/// `git -C <bridge> ls-remote --heads <remote>`, with a bounded SSH connect
/// timeout so an unreachable host can't hang `doctor` forever.
///
/// This does NOT go through `gitx::run`: that helper's own env shaping
/// (`LC_ALL=C`, `GIT_TERMINAL_PROMPT=0`, a prompt-proofing
/// `GIT_SSH_COMMAND` default) only applies when tephra's own process
/// doesn't already set those variables, and there's no way to also layer
/// `ConnectTimeout=10` onto its `GIT_SSH_COMMAND` without either mutating
/// process-global env (affecting every other git call this process makes)
/// or changing `gitx`'s shared behavior for everyone. So this check builds
/// its own `Command` with the same prompt-proofing plus the timeout,
/// applied only here.
fn check_remote_reachable(bridge: &Path, remote: Option<&str>) -> CheckOutcome {
    let Some(remote) = remote else {
        return CheckOutcome::warn("skipping remote reachability check (no remote resolved)");
    };

    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(bridge)
        .arg("ls-remote")
        .arg("--heads")
        .arg(remote);
    cmd.env("LC_ALL", "C").env("LANGUAGE", "C");
    if std::env::var_os("GIT_TERMINAL_PROMPT").is_none() {
        cmd.env("GIT_TERMINAL_PROMPT", "0");
    }
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        cmd.env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o ConnectTimeout=10",
        );
    }

    match cmd.output() {
        Ok(output) if output.status.success() => {
            CheckOutcome::ok(format!("remote '{remote}' reachable"))
        }
        Ok(output) => CheckOutcome::fail(
            format!(
                "remote '{remote}' unreachable: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            format!("check connectivity/credentials for '{remote}', then re-run tephra doctor"),
        ),
        Err(e) => CheckOutcome::fail(
            format!("could not run `git ls-remote --heads {remote}`: {e}"),
            "ensure git is installed and on PATH",
        ),
    }
}

// --------------------------------------------------------------------
// 7. stale-state report: lock / failcount / heartbeat -- informational,
// `ok`/`warn` only, never `FAIL` (bridge.rs already treats all of these as
// recoverable, not hard failures).
// --------------------------------------------------------------------

fn check_lock_state(bridge: &Path) -> CheckOutcome {
    let lock_path = bridge.join(".git").join(bridge::LOCK_DIR_NAME);
    let Ok(metadata) = std::fs::metadata(&lock_path) else {
        return CheckOutcome::ok("lock free");
    };
    let age = metadata
        .modified()
        .ok()
        .and_then(|m| std::time::SystemTime::now().duration_since(m).ok());
    match age {
        Some(age) if age > bridge::LOCK_STALE_AFTER => CheckOutcome::warn(format!(
            "lock held, {} old -- looks stale; the next bridge cycle will reclaim it",
            format_duration(age)
        )),
        Some(age) => CheckOutcome::warn(format!("lock held, {} old", format_duration(age))),
        None => CheckOutcome::warn("lock held (age unknown)"),
    }
}

fn format_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn check_failcount(bridge: &Path) -> CheckOutcome {
    match agent::failcount(bridge) {
        Some(n) if n > 0 => CheckOutcome::warn(format!(
            "failcount {n} (remote fetch has failed {n} consecutive time(s))"
        )),
        _ => CheckOutcome::ok("failcount 0"),
    }
}

fn check_heartbeat(bridge: &Path) -> CheckOutcome {
    match agent::last_cycle(bridge) {
        (Some(at), Some(outcome)) => {
            CheckOutcome::ok(format!("last bridge cycle: {at} ({outcome})"))
        }
        (Some(at), None) => CheckOutcome::ok(format!("last bridge cycle: {at}")),
        (None, _) => CheckOutcome::ok("no completed bridge cycle recorded yet"),
    }
}

// --------------------------------------------------------------------
// 8. work clone exists
// --------------------------------------------------------------------

fn check_work_clone(name: &str, work: &Path) -> CheckOutcome {
    if work.join(".git").exists() {
        CheckOutcome::ok(format!("work clone exists ({})", work.display()))
    } else {
        CheckOutcome::warn(format!(
            "work clone missing ({}); agents need it: tephra clone {name}",
            work.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_git_version / version_at_least ---

    #[test]
    fn parse_git_version_apple_git_format() {
        assert_eq!(
            parse_git_version("git version 2.50.1 (Apple Git-155)"),
            Some((2, 50, 1))
        );
    }

    #[test]
    fn parse_git_version_plain_format() {
        assert_eq!(parse_git_version("git version 2.36.0"), Some((2, 36, 0)));
    }

    #[test]
    fn parse_git_version_without_patch_component() {
        assert_eq!(parse_git_version("git version 2.36"), Some((2, 36, 0)));
    }

    #[test]
    fn parse_git_version_rejects_unrelated_text() {
        assert_eq!(parse_git_version("not git at all"), None);
    }

    #[test]
    fn version_at_least_true_at_and_above_the_2_36_boundary() {
        assert!(version_at_least((2, 36, 0), MIN_GIT_VERSION));
        assert!(version_at_least((2, 36, 5), MIN_GIT_VERSION));
        assert!(version_at_least((2, 50, 1), MIN_GIT_VERSION));
        assert!(version_at_least((3, 0, 0), MIN_GIT_VERSION));
    }

    #[test]
    fn version_at_least_false_below_the_2_36_boundary() {
        assert!(!version_at_least((2, 35, 99), MIN_GIT_VERSION));
        assert!(!version_at_least((1, 99, 0), MIN_GIT_VERSION));
    }

    // --- strip_ident_timestamp ---

    #[test]
    fn strip_ident_timestamp_removes_trailing_epoch_and_timezone() {
        assert_eq!(
            strip_ident_timestamp("Test User <test@example.com> 1700000000 +0000"),
            "Test User <test@example.com>"
        );
    }

    #[test]
    fn strip_ident_timestamp_passes_through_without_angle_bracket() {
        assert_eq!(strip_ident_timestamp("garbage"), "garbage");
    }

    // --- format_duration ---

    #[test]
    fn format_duration_shapes_seconds_minutes_and_hours() {
        assert_eq!(format_duration(std::time::Duration::from_secs(5)), "5s");
        assert_eq!(format_duration(std::time::Duration::from_secs(125)), "2m");
        assert_eq!(
            format_duration(std::time::Duration::from_secs(3725)),
            "1h2m"
        );
    }

    // --- CheckOutcome ---

    #[test]
    fn check_outcome_fail_is_fail_ok_and_warn_are_not() {
        assert!(CheckOutcome::fail("x", "y").is_fail());
        assert!(!CheckOutcome::ok("x").is_fail());
        assert!(!CheckOutcome::warn("x").is_fail());
    }
}
