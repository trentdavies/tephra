//! Obsidian Sync pairing (Task 9): `tephra obsidian doctor|service`.
//!
//! Wraps Obsidian's official `obsidian-headless` beta CLI (`ob`, a Node
//! shebang script). See `docs/DESIGN.md` §Obsidian pairing + §Service
//! management.
//!
//! ## Calibration notes (real `ob` on this machine, `obsidian-headless`
//! 0.0.12 under Node v26.4.0, one bound vault at `~/dev/memory/bridge-personal`)
//!
//! These are the exact shapes `doctor`'s parsing keys off of. Success always
//! writes to stdout with exit 0; failure always writes to stderr with a
//! nonzero exit -- confirmed for every subcommand below by redirecting
//! stdout/stderr separately, not just eyeballing combined output.
//!
//! - `ob --version` -> stdout: bare `0.0.12` (no `v` prefix, no other text).
//! - `ob sync-list-remote` (logged in) -> stdout:
//!   ```text
//!   Fetching vaults...
//!
//!   Vaults:
//!     <id>  "<name>"  (<region>)
//!   ```
//!   one line per remote vault. Logged-out failure text (per the task spec
//!   this was ported from; not independently reproduced here since logging
//!   out this machine's real, in-use `ob` login would be destructive):
//!   `No account logged in. Run "ob login" first.` on stderr.
//! - `ob sync-list-local` -> stdout:
//!   ```text
//!   Configured vaults:
//!     <id>
//!       Path: <path>
//!       Host: <host>
//!   ```
//!   A native-module (`better-sqlite3`) load failure -- e.g. after a `brew
//!   upgrade node` invalidates the prebuilt binding's `NODE_MODULE_VERSION`
//!   -- surfaces as a nonzero exit with `ERR_DLOPEN_FAILED` and/or
//!   `NODE_MODULE_VERSION` somewhere in stderr's Node stack trace.
//! - `ob sync-status --path <path>` -> stdout when bound:
//!   ```text
//!   Sync Configuration:
//!     Vault: <name> (<id>)
//!     Location: <path>
//!     ...
//!   ```
//!   stderr when not bound (exit 3, observed): `No sync configuration found
//!   for <path>`.
//!
//! ## Design decisions
//!
//! - If `ob` isn't found on `PATH` at all (check 1), checks 2-4 are skipped
//!   rather than each independently failing to spawn `ob` and repeating the
//!   same remediation -- there's nothing more to learn from them.
//! - The `ob(args) -> Result<Output>` helper below inherits the calling
//!   process's environment untouched (no locale pinning, no PATH
//!   overriding). That's deliberately the caller's problem: an interactive
//!   human's shell already has `/opt/homebrew/bin` (or wherever `ob` lives)
//!   on `PATH`, so `tephra obsidian doctor` run by hand just works. The
//!   long-running sync SERVICE (`obsidian service install`, below) sidesteps
//!   the issue entirely by resolving `ob`'s (and optionally `node`'s)
//!   absolute path once at install time and baking that absolute path into
//!   the generated launchd/systemd unit -- launchd/systemd's own minimal
//!   environment never has to resolve a bare `ob` via `PATH` at run time.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result};

use crate::config::Vault;
use crate::service;

// --------------------------------------------------------------------
// `ob` command runner + PATH resolution
// --------------------------------------------------------------------

/// Run `ob <args>`, inheriting this process's environment untouched. See
/// the module doc comment's "Design decisions" section for why this
/// deliberately doesn't pin PATH/locale itself.
pub fn ob(args: &[&str]) -> Result<Output> {
    Command::new("ob").args(args).output().with_context(|| {
        format!(
            "failed to run `ob {}` (is obsidian-headless installed and on PATH? \
             install: npm install -g obsidian-headless)",
            args.join(" ")
        )
    })
}

/// Locate `ob` on `PATH`, the way a shell's `command -v ob` would, without
/// shelling out to any external `which`/`command` utility. Returns the
/// first `PATH` entry containing an existing file named `ob` that's
/// executable (unix: any of the executable bits set; other platforms: just
/// existence, since there's no exec-bit concept there).
pub fn which_ob() -> Option<PathBuf> {
    which_ob_from(std::env::var_os("PATH").as_deref())
}

fn which_ob_from(path_var: Option<&OsStr>) -> Option<PathBuf> {
    let path_var = path_var?;
    std::env::split_paths(path_var).find_map(|dir| {
        let candidate = dir.join("ob");
        is_executable_file(&candidate).then_some(candidate)
    })
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

fn stderr_trimmed(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

// --------------------------------------------------------------------
// doctor
// --------------------------------------------------------------------

const INSTALL_REMEDIATION: &str = "install: npm install -g obsidian-headless (Node 22+)";
const LOGIN_REMEDIATION: &str = "run: ob login";
const NATIVE_BINDING_REMEDIATION: &str = "reinstall obsidian-headless under the node your service uses; if npm blocked build scripts, run: npm approve-scripts better-sqlite3";

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

/// Check 1: `ob` resolvable on `PATH`, and `ob --version` runs.
fn check_ob_on_path() -> CheckOutcome {
    match which_ob() {
        None => CheckOutcome::fail("ob not found on PATH", INSTALL_REMEDIATION),
        Some(path) => match ob(&["--version"]) {
            Ok(output) if output.status.success() => {
                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                CheckOutcome::ok(format!(
                    "ob found on PATH ({}), version {version}",
                    path.display()
                ))
            }
            Ok(output) => CheckOutcome::fail(
                format!("`ob --version` failed: {}", stderr_trimmed(&output)),
                INSTALL_REMEDIATION,
            ),
            Err(err) => CheckOutcome::fail(
                format!("failed to run `ob --version`: {err}"),
                INSTALL_REMEDIATION,
            ),
        },
    }
}

/// Number of remote vault lines under `sync-list-remote`'s `Vaults:`
/// header, purely for a friendlier `ok:` message -- not otherwise load
/// bearing.
fn count_listed_vaults(stdout: &str) -> usize {
    let mut counting = false;
    let mut n = 0;
    for line in stdout.lines() {
        if line.trim() == "Vaults:" {
            counting = true;
            continue;
        }
        if counting && !line.trim().is_empty() {
            n += 1;
        }
    }
    n
}

/// Check 2: logged in to Obsidian Sync (`ob sync-list-remote` succeeds).
fn check_logged_in() -> CheckOutcome {
    match ob(&["sync-list-remote"]) {
        Err(err) => CheckOutcome::fail(
            format!("could not run `ob sync-list-remote`: {err}"),
            LOGIN_REMEDIATION,
        ),
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let n = count_listed_vaults(&stdout);
            CheckOutcome::ok(format!(
                "logged in to Obsidian Sync ({n} remote vault(s) visible)"
            ))
        }
        Ok(output) => {
            let stderr = stderr_trimmed(&output);
            if stderr.contains("No account logged in") {
                CheckOutcome::fail("not logged in to Obsidian Sync", LOGIN_REMEDIATION)
            } else {
                CheckOutcome::warn(format!("`ob sync-list-remote` failed: {stderr}"))
            }
        }
    }
}

/// Check 3: native module (better-sqlite3) binding loads, smoke-tested via
/// the cheapest sqlite-touching command, `ob sync-list-local`.
fn check_native_binding() -> CheckOutcome {
    match ob(&["sync-list-local"]) {
        Err(err) => CheckOutcome::fail(
            format!("could not run `ob sync-list-local`: {err}"),
            NATIVE_BINDING_REMEDIATION,
        ),
        Ok(output) => {
            let stderr = stderr_trimmed(&output);
            if stderr.contains("ERR_DLOPEN_FAILED") || stderr.contains("NODE_MODULE_VERSION") {
                CheckOutcome::fail(
                    format!("native module binding failed to load: {stderr}"),
                    NATIVE_BINDING_REMEDIATION,
                )
            } else if output.status.success() {
                CheckOutcome::ok("native module binding loads (`ob sync-list-local` ran cleanly)")
            } else {
                CheckOutcome::warn(format!("`ob sync-list-local` failed: {stderr}"))
            }
        }
    }
}

/// The trimmed `Vault: <name> (<id>)` line from a successful `ob
/// sync-status` stdout, if present.
fn extract_vault_line(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("Vault:"))
        .map(str::to_string)
}

/// Check 4: the bridge path is bound to a synced remote vault (`ob
/// sync-status --path <bridge>`).
fn check_bridge_bound(vault_name: &str, bridge: &Path) -> CheckOutcome {
    let bridge_str = bridge.display().to_string();
    let remediation = format!(
        "run: cd {bridge_str} && ob sync-setup --vault {vault_name} \
         (prompts for the E2E password -- interactive, yours to type)"
    );
    match ob(&["sync-status", "--path", &bridge_str]) {
        Err(err) => CheckOutcome::fail(
            format!("could not run `ob sync-status`: {err}"),
            remediation,
        ),
        Ok(output) => {
            let stderr = stderr_trimmed(&output);
            if stderr.contains("No sync configuration") {
                CheckOutcome::fail(
                    format!("bridge not bound to a synced vault ({bridge_str})"),
                    remediation,
                )
            } else if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                match extract_vault_line(&stdout) {
                    Some(line) => CheckOutcome::ok(format!("bridge bound ({line})")),
                    None => CheckOutcome::ok("bridge bound"),
                }
            } else {
                CheckOutcome::warn(format!("`ob sync-status` failed: {stderr}"))
            }
        }
    }
}

/// `tephra obsidian doctor [VAULT]`: four sequential checks (ob on PATH,
/// logged in, native-binding smoke test, bridge bound), each printing an
/// `ok:`/`warn:`/`FAIL:` line (`FAIL:` lines get an indented remediation
/// line beneath). Returns an error (exit 1, a domain failure -- see
/// `config::UsageError`'s doc comment for why this isn't a usage error) iff
/// any check `FAIL`ed.
pub fn doctor(vault_name: &str, vault: &Vault) -> Result<()> {
    let mut any_fail = false;

    let ob_check = check_ob_on_path();
    any_fail |= ob_check.is_fail();
    let ob_available = !ob_check.is_fail();
    ob_check.print();

    if ob_available {
        for check in [
            check_logged_in(),
            check_native_binding(),
            check_bridge_bound(vault_name, &vault.bridge),
        ] {
            any_fail |= check.is_fail();
            check.print();
        }
    } else {
        println!("warn: skipping remaining checks (ob unavailable)");
    }

    if any_fail {
        anyhow::bail!("obsidian doctor found problems");
    }
    Ok(())
}

// --------------------------------------------------------------------
// service install/uninstall: the `ob sync --continuous` KeepAlive /
// Restart=always platform service
// --------------------------------------------------------------------

/// `com.tephra.obsidian-sync.<vault>`, the launchd label.
pub fn obsidian_launchd_label(vault: &str) -> String {
    format!("com.tephra.obsidian-sync.{vault}")
}

/// `com.tephra.obsidian-sync.<vault>.plist`, the launchd plist's file name.
pub fn obsidian_launchd_plist_filename(vault: &str) -> String {
    format!("{}.plist", obsidian_launchd_label(vault))
}

/// `tephra-obsidian-<vault>.service`, the systemd unit's file name.
pub fn obsidian_systemd_service_name(vault: &str) -> String {
    format!("tephra-obsidian-{vault}.service")
}

/// `tephra-obsidian-<vault>.log`, the launchd stdout/stderr log file name
/// (systemd logs to the user journal instead, matching `service.rs`'s own
/// bridge-service convention).
pub fn obsidian_log_filename(vault: &str) -> String {
    format!("tephra-obsidian-{vault}.log")
}

/// `~/Library/LaunchAgents/com.tephra.obsidian-sync.<vault>.plist`.
pub fn obsidian_launchd_plist_path(vault: &str) -> Result<PathBuf> {
    Ok(service::launch_agents_dir()?.join(obsidian_launchd_plist_filename(vault)))
}

/// `~/Library/Logs/tephra-obsidian-<vault>.log`.
pub fn obsidian_log_path(vault: &str) -> Result<PathBuf> {
    Ok(service::logs_dir()?.join(obsidian_log_filename(vault)))
}

/// `<systemd user dir>/tephra-obsidian-<vault>.service`.
pub fn obsidian_systemd_service_path(vault: &str) -> Result<PathBuf> {
    Ok(service::systemd_user_dir()?.join(obsidian_systemd_service_name(vault)))
}

/// The already-resolved program invocation for `ob sync --continuous`:
/// either `ob` directly, or `<node> <script>` when `--node` pins the
/// interpreter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObInvocation {
    Direct(PathBuf),
    Pinned { node: PathBuf, script: PathBuf },
}

impl ObInvocation {
    /// The `ProgramArguments`/`ExecStart` prefix, before the trailing
    /// `sync --continuous` tokens every invocation shares.
    fn program_args(&self) -> Vec<PathBuf> {
        match self {
            ObInvocation::Direct(ob) => vec![ob.clone()],
            ObInvocation::Pinned { node, script } => vec![node.clone(), script.clone()],
        }
    }
}

/// Lexically collapse `.`/`..` components without touching the filesystem
/// (a real `fs::canonicalize` would also resolve further symlinks along the
/// way, which isn't what we want here -- `ob_path`'s single symlink hop is
/// already fully accounted for by the caller). `obsidian-headless` installs
/// `ob` as a relative symlink (e.g. `../lib/node_modules/obsidian-headless/
/// cli.js`), so joining it onto `ob_path`'s parent dir needs this to produce
/// a clean, human-readable absolute path rather than one littered with
/// `foo/../`.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Resolve `--node <node>`'s invocation: reads `ob_path`'s symlink target
/// (obsidian-headless installs `ob` as a symlink to its `cli.js`) and, if it
/// looks like a `cli.js` entrypoint, invokes `<node> <cli.js>` directly --
/// bypassing `ob`'s own `#!/usr/bin/env node` shebang, which a service
/// manager's minimal, non-interactive-shell environment may resolve to a
/// different (or no) `node` than the one the caller explicitly pinned.
/// Falls back to `<node> <ob_path>` -- noting why, via the returned second
/// element -- if `ob_path` isn't a symlink, or its target's file name isn't
/// `cli.js`.
pub fn resolve_node_pin(ob_path: &Path, node: &Path) -> (ObInvocation, Option<String>) {
    let link_target = std::fs::read_link(ob_path).ok();
    match link_target {
        Some(target) => {
            let joined = if target.is_absolute() {
                target
            } else {
                ob_path
                    .parent()
                    .unwrap_or_else(|| Path::new(""))
                    .join(target)
            };
            let resolved = normalize_lexically(&joined);
            if resolved.file_name().and_then(OsStr::to_str) == Some("cli.js") {
                (
                    ObInvocation::Pinned {
                        node: node.to_path_buf(),
                        script: resolved,
                    },
                    None,
                )
            } else {
                let note = format!(
                    "{}'s symlink target ({}) doesn't look like a cli.js entrypoint; \
                     invoking {} directly under --node instead",
                    ob_path.display(),
                    resolved.display(),
                    ob_path.display()
                );
                (
                    ObInvocation::Pinned {
                        node: node.to_path_buf(),
                        script: ob_path.to_path_buf(),
                    },
                    Some(note),
                )
            }
        }
        None => {
            let note = format!(
                "{} is not a symlink; invoking it directly under --node instead of \
                 resolving a cli.js entrypoint",
                ob_path.display()
            );
            (
                ObInvocation::Pinned {
                    node: node.to_path_buf(),
                    script: ob_path.to_path_buf(),
                },
                Some(note),
            )
        }
    }
}

/// Generate the launchd plist for the `ob sync --continuous` KeepAlive
/// service. `invocation` is the already-resolved `ProgramArguments` prefix
/// (see [`ObInvocation`]) -- this function is pure and takes no filesystem
/// action, matching `service.rs`'s `generate_launchd_plist` convention (see
/// `tests/obsidian.rs`'s golden-file tests, and `tests/golden/*`).
pub fn generate_obsidian_launchd_plist(
    invocation: &ObInvocation,
    vault: &str,
    bridge: &Path,
    log_path: &Path,
) -> String {
    let label = service::xml_escape(&obsidian_launchd_label(vault));
    let bridge_esc = service::xml_escape(&bridge.display().to_string());
    let log = service::xml_escape(&log_path.display().to_string());

    let program_strs: Vec<String> = invocation
        .program_args()
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    let mut program_refs: Vec<&str> = program_strs.iter().map(String::as_str).collect();
    program_refs.push("sync");
    program_refs.push("--continuous");
    let program = service::plist_string_array(&program_refs);

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key><array>
{program}  </array>
  <key>WorkingDirectory</key><string>{bridge_esc}</string>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>30</integer>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
  <key>EnvironmentVariables</key><dict>
    <key>PATH</key><string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
  </dict>
</dict></plist>
"#
    )
}

/// Generate the systemd unit for the `ob sync --continuous` service:
/// `Type=simple`, `Restart=always`/`RestartSec=30`, enabled via
/// `WantedBy=default.target` (unlike `service.rs`'s bridge unit, this one
/// runs continuously itself -- there's no separate `.timer`). `ExecStart`'s
/// variable path tokens are double-quoted (the fixed `sync --continuous`
/// tokens are not, matching `service.rs::generate_systemd_service`'s
/// convention of only quoting tokens that could plausibly contain spaces).
pub fn generate_obsidian_systemd_service(
    invocation: &ObInvocation,
    vault: &str,
    bridge: &Path,
) -> String {
    let program_quoted = invocation
        .program_args()
        .iter()
        .map(|p| format!("\"{}\"", p.display()))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "[Unit]\nDescription=tephra obsidian sync for vault {vault}\n\n\
         [Service]\nType=simple\nExecStart={program_quoted} sync --continuous\n\
         WorkingDirectory=\"{}\"\nRestart=always\nRestartSec=30\n\n\
         [Install]\nWantedBy=default.target\n",
        bridge.display()
    )
}

fn ob_path_or_install_error() -> Result<PathBuf> {
    which_ob().ok_or_else(|| anyhow::anyhow!("{INSTALL_REMEDIATION}"))
}

/// `tephra obsidian service install [VAULT] [--node <path>]`.
pub fn service_install(vault_name: &str, vault: &Vault, node: Option<&Path>) -> Result<()> {
    imp::install(vault_name, vault, node)
}

/// `tephra obsidian service uninstall [VAULT]`.
pub fn service_uninstall(vault_name: &str) -> Result<()> {
    imp::uninstall(vault_name)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::path::Path;

    use anyhow::{Context, Result};

    use crate::config::Vault;
    use crate::service;

    pub fn install(vault_name: &str, vault: &Vault, node: Option<&Path>) -> Result<()> {
        let ob_path = super::ob_path_or_install_error()?;
        let invocation = match node {
            Some(node) => {
                let (invocation, note) = super::resolve_node_pin(&ob_path, node);
                if let Some(note) = note {
                    println!("note: {note}");
                }
                invocation
            }
            None => super::ObInvocation::Direct(ob_path),
        };

        let plist_path = super::obsidian_launchd_plist_path(vault_name)?;
        let log_path = super::obsidian_log_path(vault_name)?;

        if let Some(parent) = plist_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }

        let contents = super::generate_obsidian_launchd_plist(
            &invocation,
            vault_name,
            &vault.bridge,
            &log_path,
        );
        std::fs::write(&plist_path, contents)
            .with_context(|| format!("writing {}", plist_path.display()))?;

        let label = super::obsidian_launchd_label(vault_name);
        service::launchctl_bootout_ignore_failure(&label)?;
        service::launchctl_bootstrap_with_retry(&plist_path)?;

        println!(
            "installed and loaded obsidian sync service for vault '{vault_name}':\n  unit: {}\n  log:  {}",
            plist_path.display(),
            log_path.display()
        );
        Ok(())
    }

    pub fn uninstall(vault_name: &str) -> Result<()> {
        let plist_path = super::obsidian_launchd_plist_path(vault_name)?;

        if !plist_path.exists() {
            println!("obsidian sync service for vault '{vault_name}' is not installed");
            return Ok(());
        }

        service::launchctl_bootout_ignore_failure(&super::obsidian_launchd_label(vault_name))?;
        std::fs::remove_file(&plist_path)
            .with_context(|| format!("removing {}", plist_path.display()))?;

        let _ = std::fs::remove_file(super::obsidian_log_path(vault_name)?);

        println!(
            "uninstalled obsidian sync service for vault '{vault_name}': removed {}",
            plist_path.display()
        );
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::path::Path;
    use std::process::Command;

    use anyhow::{Context, Result};

    use crate::config::Vault;
    use crate::service;

    pub fn install(vault_name: &str, vault: &Vault, node: Option<&Path>) -> Result<()> {
        let ob_path = super::ob_path_or_install_error()?;
        let invocation = match node {
            Some(node) => {
                let (invocation, note) = super::resolve_node_pin(&ob_path, node);
                if let Some(note) = note {
                    println!("note: {note}");
                }
                invocation
            }
            None => super::ObInvocation::Direct(ob_path),
        };

        let service_path = super::obsidian_systemd_service_path(vault_name)?;
        if let Some(dir) = service_path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }

        let contents =
            super::generate_obsidian_systemd_service(&invocation, vault_name, &vault.bridge);
        std::fs::write(&service_path, contents)
            .with_context(|| format!("writing {}", service_path.display()))?;

        service::systemctl_user(&["daemon-reload"])?;
        service::systemctl_user(&[
            "enable",
            "--now",
            &super::obsidian_systemd_service_name(vault_name),
        ])?;

        println!(
            "installed and enabled obsidian sync service for vault '{vault_name}':\n  unit: {}",
            service_path.display()
        );
        Ok(())
    }

    pub fn uninstall(vault_name: &str) -> Result<()> {
        let service_path = super::obsidian_systemd_service_path(vault_name)?;

        if !service_path.exists() {
            println!("obsidian sync service for vault '{vault_name}' is not installed");
            return Ok(());
        }

        let _ = Command::new("systemctl")
            .arg("--user")
            .arg("disable")
            .arg("--now")
            .arg(super::obsidian_systemd_service_name(vault_name))
            .output();

        std::fs::remove_file(&service_path)
            .with_context(|| format!("removing {}", service_path.display()))?;
        let _ = service::systemctl_user(&["daemon-reload"]);

        println!(
            "uninstalled obsidian sync service for vault '{vault_name}': removed {}",
            service_path.display()
        );
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use std::path::Path;

    use anyhow::Result;

    use crate::config::Vault;

    pub fn install(_vault_name: &str, _vault: &Vault, _node: Option<&Path>) -> Result<()> {
        Err(crate::service::unsupported_platform_error())
    }

    pub fn uninstall(_vault_name: &str) -> Result<()> {
        Err(crate::service::unsupported_platform_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- which_ob_from ---

    #[test]
    fn which_ob_from_finds_executable_in_path_dir() {
        let dir = tempfile::tempdir().unwrap();
        let ob_path = dir.path().join("ob");
        std::fs::write(&ob_path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&ob_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let path_var = std::ffi::OsString::from(dir.path());
        assert_eq!(which_ob_from(Some(&path_var)), Some(ob_path));
    }

    #[test]
    fn which_ob_from_returns_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path_var = std::ffi::OsString::from(dir.path());
        assert_eq!(which_ob_from(Some(&path_var)), None);
    }

    #[cfg(unix)]
    #[test]
    fn which_ob_from_skips_non_executable_file() {
        let dir = tempfile::tempdir().unwrap();
        let ob_path = dir.path().join("ob");
        std::fs::write(&ob_path, "not executable").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ob_path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let path_var = std::ffi::OsString::from(dir.path());
        assert_eq!(which_ob_from(Some(&path_var)), None);
    }

    #[test]
    fn which_ob_from_returns_first_match_across_multiple_dirs() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let first_ob = first.path().join("ob");
        let second_ob = second.path().join("ob");
        for p in [&first_ob, &second_ob] {
            std::fs::write(p, "#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        let joined = std::env::join_paths([first.path(), second.path()]).unwrap();
        assert_eq!(which_ob_from(Some(&joined)), Some(first_ob));
    }

    #[test]
    fn which_ob_from_none_path_returns_none() {
        assert_eq!(which_ob_from(None), None);
    }

    // --- count_listed_vaults / extract_vault_line ---

    #[test]
    fn count_listed_vaults_counts_real_sync_list_remote_shape() {
        let stdout = "Fetching vaults...\n\nVaults:\n  \
             b16329ed3abc6885ae482c0c3299d23f  \"tbd\"  (North America)\n  \
             5ce1671db83546d1e56c2a25acaa1e6b  \"Personal-Notes\"  (North America)\n  \
             61901c20e85487f30ff10ae406143505  \"personal\"  (North America)\n";
        assert_eq!(count_listed_vaults(stdout), 3);
    }

    #[test]
    fn count_listed_vaults_zero_without_vaults_header() {
        assert_eq!(count_listed_vaults("nothing here\n"), 0);
    }

    #[test]
    fn extract_vault_line_finds_real_sync_status_shape() {
        let stdout = "Sync Configuration:\n  \
             Vault: personal (61901c20e85487f30ff10ae406143505)\n  \
             Location: /Users/trent/dev/memory/bridge-personal\n";
        assert_eq!(
            extract_vault_line(stdout),
            Some("Vault: personal (61901c20e85487f30ff10ae406143505)".to_string())
        );
    }

    #[test]
    fn extract_vault_line_none_when_absent() {
        assert_eq!(extract_vault_line("nothing here\n"), None);
    }

    // --- path/label helpers ---

    #[test]
    fn obsidian_launchd_label_formats_with_sync_infix() {
        assert_eq!(
            obsidian_launchd_label("personal"),
            "com.tephra.obsidian-sync.personal"
        );
    }

    #[test]
    fn obsidian_launchd_plist_filename_appends_dot_plist() {
        assert_eq!(
            obsidian_launchd_plist_filename("personal"),
            "com.tephra.obsidian-sync.personal.plist"
        );
    }

    #[test]
    fn obsidian_systemd_service_name_formats_as_tephra_obsidian_vault() {
        assert_eq!(
            obsidian_systemd_service_name("personal"),
            "tephra-obsidian-personal.service"
        );
    }

    #[test]
    fn obsidian_log_filename_formats_as_tephra_obsidian_vault_dot_log() {
        assert_eq!(
            obsidian_log_filename("personal"),
            "tephra-obsidian-personal.log"
        );
    }

    // --- resolve_node_pin ---

    #[cfg(unix)]
    #[test]
    fn resolve_node_pin_follows_symlink_to_cli_js() {
        let dir = tempfile::tempdir().unwrap();
        let lib_dir = dir.path().join("lib/node_modules/obsidian-headless");
        std::fs::create_dir_all(&lib_dir).unwrap();
        let cli_js = lib_dir.join("cli.js");
        std::fs::write(&cli_js, "// fake cli.js\n").unwrap();

        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let ob_path = bin_dir.join("ob");
        std::os::unix::fs::symlink("../lib/node_modules/obsidian-headless/cli.js", &ob_path)
            .unwrap();

        let node = PathBuf::from("/fake/bin/node");
        let (invocation, note) = resolve_node_pin(&ob_path, &node);

        assert_eq!(
            invocation,
            ObInvocation::Pinned {
                node: node.clone(),
                script: cli_js,
            }
        );
        assert_eq!(note, None);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_node_pin_falls_back_and_notes_when_symlink_target_is_not_cli_js() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("something-else.js");
        std::fs::write(&target, "// not a cli.js\n").unwrap();
        let ob_path = dir.path().join("ob");
        std::os::unix::fs::symlink(&target, &ob_path).unwrap();

        let node = PathBuf::from("/fake/bin/node");
        let (invocation, note) = resolve_node_pin(&ob_path, &node);

        assert_eq!(
            invocation,
            ObInvocation::Pinned {
                node,
                script: ob_path.clone(),
            }
        );
        assert!(note.is_some(), "expected a fallback note, got None");
    }

    #[test]
    fn resolve_node_pin_falls_back_and_notes_when_not_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let ob_path = dir.path().join("ob");
        std::fs::write(&ob_path, "#!/usr/bin/env node\n").unwrap();

        let node = PathBuf::from("/fake/bin/node");
        let (invocation, note) = resolve_node_pin(&ob_path, &node);

        assert_eq!(
            invocation,
            ObInvocation::Pinned {
                node,
                script: ob_path,
            }
        );
        assert!(note.is_some(), "expected a fallback note, got None");
    }

    // --- generate_obsidian_launchd_plist / generate_obsidian_systemd_service ---
    // (golden-file coverage lives in tests/obsidian.rs; these are quick
    // shape assertions, not full byte comparisons.)

    #[test]
    fn generate_obsidian_launchd_plist_direct_contains_expected_keys() {
        let invocation = ObInvocation::Direct(PathBuf::from("/fake/bin/ob"));
        let got = generate_obsidian_launchd_plist(
            &invocation,
            "goldenvault",
            Path::new("/fake/home/bridge-goldenvault"),
            Path::new("/fake/home/Library/Logs/tephra-obsidian-goldenvault.log"),
        );
        assert!(got.contains("<string>/fake/bin/ob</string>"));
        assert!(got.contains("<string>sync</string>"));
        assert!(got.contains("<string>--continuous</string>"));
        assert!(got.contains("<key>KeepAlive</key><true/>"));
        assert!(got.contains("<key>ThrottleInterval</key><integer>30</integer>"));
        assert!(
            got.contains("<key>Label</key><string>com.tephra.obsidian-sync.goldenvault</string>")
        );
    }

    #[test]
    fn generate_obsidian_launchd_plist_pinned_lists_node_then_script() {
        let invocation = ObInvocation::Pinned {
            node: PathBuf::from("/fake/node/bin/node"),
            script: PathBuf::from("/fake/lib/node_modules/obsidian-headless/cli.js"),
        };
        let got = generate_obsidian_launchd_plist(
            &invocation,
            "goldenvault",
            Path::new("/fake/home/bridge-goldenvault"),
            Path::new("/fake/home/Library/Logs/tephra-obsidian-goldenvault.log"),
        );
        let node_pos = got.find("<string>/fake/node/bin/node</string>").unwrap();
        let script_pos = got
            .find("<string>/fake/lib/node_modules/obsidian-headless/cli.js</string>")
            .unwrap();
        let sync_pos = got.find("<string>sync</string>").unwrap();
        assert!(node_pos < script_pos && script_pos < sync_pos);
    }

    #[test]
    fn generate_obsidian_systemd_service_direct_quotes_ob_path() {
        let invocation = ObInvocation::Direct(PathBuf::from("/fake/bin/ob"));
        let got = generate_obsidian_systemd_service(
            &invocation,
            "goldenvault",
            Path::new("/fake/home/bridge-goldenvault"),
        );
        assert!(got.contains("ExecStart=\"/fake/bin/ob\" sync --continuous"));
        assert!(got.contains("Restart=always"));
        assert!(got.contains("RestartSec=30"));
        assert!(got.contains("Type=simple"));
        assert!(got.contains("WantedBy=default.target"));
    }

    #[test]
    fn generate_obsidian_systemd_service_pinned_quotes_both_tokens() {
        let invocation = ObInvocation::Pinned {
            node: PathBuf::from("/fake/node/bin/node"),
            script: PathBuf::from("/fake/lib/node_modules/obsidian-headless/cli.js"),
        };
        let got = generate_obsidian_systemd_service(
            &invocation,
            "goldenvault",
            Path::new("/fake/home/bridge-goldenvault"),
        );
        assert!(got.contains(
            "ExecStart=\"/fake/node/bin/node\" \"/fake/lib/node_modules/obsidian-headless/cli.js\" sync --continuous"
        ));
    }

    // --- CheckOutcome ---

    #[test]
    fn check_outcome_fail_is_fail_ok_and_warn_are_not() {
        assert!(CheckOutcome::fail("x", "y").is_fail());
        assert!(!CheckOutcome::ok("x").is_fail());
        assert!(!CheckOutcome::warn("x").is_fail());
    }
}
