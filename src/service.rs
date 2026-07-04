//! Self-installing platform service (Task 7): `tephra service
//! install|uninstall|status`.
//!
//! See `docs/DESIGN.md` §Service management: both platforms schedule
//! `tephra bridge --once <vault>` on a periodic timer -- `--watch` is a
//! foreground/debugging mode and is never what the installed service runs.
//!
//! - **macOS**: `~/Library/LaunchAgents/com.tephra.<vault>.plist`,
//!   `StartInterval` 120s, logs to `~/Library/Logs/tephra-<vault>.log`,
//!   loaded via `launchctl bootout` (tolerated failure) + `bootstrap` with a
//!   bounded retry (bootout->bootstrap of a still-live service races).
//! - **Linux**: `$XDG_CONFIG_HOME/systemd/user/` (or `~/.config/systemd/user/`)
//!   `tephra-<vault>.service` (oneshot) + `tephra-<vault>.timer`
//!   (`OnUnitActiveSec=2min`), enabled via `systemctl --user enable --now`.
//!
//! Unit generation is pure (`generate_*`, taking the exe path, vault name,
//! and any host paths as plain arguments) so it's testable without touching
//! the filesystem or a real platform service manager -- see
//! `tests/service.rs`'s golden-file tests. `install`/`uninstall`/`detect`
//! are the impure, platform-dispatching half, implemented per-`target_os` in
//! a private `imp` module (mirroring `notify.rs`'s pattern).

use std::path::{Path, PathBuf};

use anyhow::Result;

/// `com.tephra.<vault>`, the launchd label (and systemd unit basename
/// prefix's logical equivalent).
pub fn launchd_label(vault: &str) -> String {
    format!("com.tephra.{vault}")
}

/// `com.tephra.<vault>.plist`, the launchd plist's file name.
pub fn launchd_plist_filename(vault: &str) -> String {
    format!("{}.plist", launchd_label(vault))
}

/// `tephra-<vault>.service`, the systemd oneshot unit's file name.
pub fn systemd_service_name(vault: &str) -> String {
    format!("tephra-{vault}.service")
}

/// `tephra-<vault>.timer`, the systemd timer unit's file name.
pub fn systemd_timer_name(vault: &str) -> String {
    format!("tephra-{vault}.timer")
}

/// `tephra-<vault>.log`, the shared stdout/stderr log file name (macOS
/// launchd only; systemd's oneshot service logs to the user journal).
pub fn log_filename(vault: &str) -> String {
    format!("tephra-{vault}.log")
}

/// Escape `&`, `<`, `>`, `"`, and `'` for safe interpolation into a plist
/// XML string/attribute value. Vault names and exe paths tephra generates
/// itself are never adversarial, but a stray `&` in a path (rare, but legal
/// on both platforms) would otherwise produce invalid XML.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Generate the launchd plist for `vault`: runs `<exe> bridge --once
/// <vault>` every 120s (`StartInterval`) plus once immediately on load
/// (`RunAtLoad`), logging both streams to `log_path`.
pub fn generate_launchd_plist(exe: &Path, vault: &str, log_path: &Path) -> String {
    let label = xml_escape(&launchd_label(vault));
    let exe = xml_escape(&exe.display().to_string());
    let vault = xml_escape(vault);
    let log = xml_escape(&log_path.display().to_string());

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key><array>
    <string>{exe}</string>
    <string>bridge</string>
    <string>--once</string>
    <string>{vault}</string>
  </array>
  <key>StartInterval</key><integer>120</integer>
  <key>RunAtLoad</key><true/>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
  <key>EnvironmentVariables</key><dict>
    <key>PATH</key><string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>
  </dict>
</dict></plist>
"#
    )
}

/// Generate the systemd oneshot service unit: a single `<exe> bridge --once
/// <vault>` invocation, triggered by the matching `.timer` unit (see
/// [`generate_systemd_timer`]) rather than run continuously.
pub fn generate_systemd_service(exe: &Path, vault: &str) -> String {
    let exe = exe.display();
    format!(
            "[Unit]\nDescription=tephra bridge cycle for vault {vault}\n\n[Service]\nType=oneshot\nExecStart={exe} bridge --once {vault}\n"
        )
}

/// Generate the systemd timer unit paired with [`generate_systemd_service`]:
/// runs 2 minutes after boot and every 2 minutes thereafter, persistent
/// across suspend/reboot (a missed run fires as soon as the user session is
/// back).
pub fn generate_systemd_timer(vault: &str) -> String {
    let _ = vault; // vault-independent content; kept for signature symmetry.
    "[Timer]\nOnBootSec=2min\nOnUnitActiveSec=2min\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n"
        .to_string()
}

fn home_dir_error() -> anyhow::Error {
    anyhow::anyhow!("could not determine home directory")
}

fn launch_agents_dir_from(home: Option<&Path>) -> Result<PathBuf> {
    Ok(home
        .ok_or_else(home_dir_error)?
        .join("Library")
        .join("LaunchAgents"))
}

/// `~/Library/LaunchAgents`.
pub fn launch_agents_dir() -> Result<PathBuf> {
    launch_agents_dir_from(dirs::home_dir().as_deref())
}

fn logs_dir_from(home: Option<&Path>) -> Result<PathBuf> {
    Ok(home
        .ok_or_else(home_dir_error)?
        .join("Library")
        .join("Logs"))
}

/// `~/Library/Logs`.
pub fn logs_dir() -> Result<PathBuf> {
    logs_dir_from(dirs::home_dir().as_deref())
}

fn systemd_user_dir_from(xdg_config_home: Option<&Path>, home: Option<&Path>) -> Result<PathBuf> {
    if let Some(xdg) = xdg_config_home {
        return Ok(xdg.join("systemd").join("user"));
    }
    Ok(home
        .ok_or_else(home_dir_error)?
        .join(".config")
        .join("systemd")
        .join("user"))
}

/// `$XDG_CONFIG_HOME/systemd/user` if `XDG_CONFIG_HOME` is set, else
/// `~/.config/systemd/user`.
pub fn systemd_user_dir() -> Result<PathBuf> {
    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    systemd_user_dir_from(xdg.as_deref(), dirs::home_dir().as_deref())
}

/// `~/Library/Logs/tephra-<vault>.log`.
pub fn log_path(vault: &str) -> Result<PathBuf> {
    Ok(logs_dir()?.join(log_filename(vault)))
}

/// `~/Library/LaunchAgents/com.tephra.<vault>.plist`.
pub fn launchd_plist_path(vault: &str) -> Result<PathBuf> {
    Ok(launch_agents_dir()?.join(launchd_plist_filename(vault)))
}

/// `<systemd user dir>/tephra-<vault>.service`.
pub fn systemd_service_path(vault: &str) -> Result<PathBuf> {
    Ok(systemd_user_dir()?.join(systemd_service_name(vault)))
}

/// `<systemd user dir>/tephra-<vault>.timer`.
pub fn systemd_timer_path(vault: &str) -> Result<PathBuf> {
    Ok(systemd_user_dir()?.join(systemd_timer_name(vault)))
}

/// Loaded/running state of the installed service for a vault, as reported
/// by [`detect`]. `Unknown` covers both "wrong platform" and "platform tool
/// (launchctl/systemctl) unavailable" -- e.g. a container without a user
/// systemd/launchd instance -- so best-effort callers like
/// `agent::status`'s JSON `service` field never hard-fail on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Loaded,
    NotLoaded,
    Unknown,
}

impl ServiceState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ServiceState::Loaded => "loaded",
            ServiceState::NotLoaded => "not-loaded",
            ServiceState::Unknown => "unknown",
        }
    }
}

/// A clear, actionable error for platforms other than macOS and Linux.
/// Defined outside the `imp` cfg-gating (rather than inline in the
/// non-mac/non-linux `imp` module) so it's exercised by a unit test on
/// every platform, not just the ones where it's actually reachable at
/// runtime.
#[allow(dead_code)] // reachable via imp::install/uninstall only on other target_os; see the module doc comment.
fn unsupported_platform_error() -> anyhow::Error {
    anyhow::anyhow!(
        "tephra service management is only supported on macOS and Linux (detected: {})",
        std::env::consts::OS
    )
}

/// Write and load the platform service for `vault`, pointing at the
/// currently running `tephra` binary. Idempotent: re-running replaces the
/// unit file(s) and reloads.
pub fn install(vault: &str) -> Result<()> {
    imp::install(vault)
}

/// Unload and remove the platform service for `vault`. Idempotent: a second
/// call finds no unit file(s), prints a "not installed" note, and returns
/// `Ok(())`.
pub fn uninstall(vault: &str) -> Result<()> {
    imp::uninstall(vault)
}

/// Best-effort loaded/running query -- never errors, see [`ServiceState`].
pub fn detect(vault: &str) -> ServiceState {
    imp::detect(vault)
}

/// `tephra service status`'s CLI-facing report: prints the state, and
/// exits non-`Ok` (domain error, exit 1) unless it's confirmed [`Loaded`](ServiceState::Loaded).
pub fn status(name: &str) -> Result<()> {
    let state = detect(name);
    println!("service {name}: {}", state.as_str());
    if state == ServiceState::Loaded {
        Ok(())
    } else {
        anyhow::bail!("service for vault '{name}' is not loaded")
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::path::Path;
    use std::process::Command;
    use std::time::Duration;

    use anyhow::{Context, Result};

    use super::ServiceState;

    /// `launchctl bootstrap` attempts before giving up: a `bootout` of a
    /// still-live service is asynchronous, so an immediate `bootstrap` can
    /// race it (DESIGN.md §Service management).
    const BOOTSTRAP_ATTEMPTS: u32 = 3;
    const BOOTSTRAP_RETRY_DELAY: Duration = Duration::from_secs(1);

    pub fn install(vault: &str) -> Result<()> {
        let exe = std::env::current_exe().context("resolving the tephra executable path")?;
        let plist_path = super::launchd_plist_path(vault)?;
        let log_path = super::log_path(vault)?;

        if let Some(parent) = plist_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }

        let contents = super::generate_launchd_plist(&exe, vault, &log_path);
        std::fs::write(&plist_path, contents)
            .with_context(|| format!("writing {}", plist_path.display()))?;

        let label = super::launchd_label(vault);
        bootout_ignore_failure(&label)?;
        bootstrap_with_retry(&plist_path)?;

        println!(
            "installed and loaded service for vault '{vault}':\n  unit: {}\n  log:  {}",
            plist_path.display(),
            log_path.display()
        );
        Ok(())
    }

    pub fn uninstall(vault: &str) -> Result<()> {
        let plist_path = super::launchd_plist_path(vault)?;

        if !plist_path.exists() {
            println!("service for vault '{vault}' is not installed");
            return Ok(());
        }

        bootout_ignore_failure(&super::launchd_label(vault))?;
        std::fs::remove_file(&plist_path)
            .with_context(|| format!("removing {}", plist_path.display()))?;

        println!(
            "uninstalled service for vault '{vault}': removed {}",
            plist_path.display()
        );
        Ok(())
    }

    pub fn detect(vault: &str) -> ServiceState {
        let label = super::launchd_label(vault);
        let uid = match current_uid() {
            Ok(uid) => uid,
            Err(_) => return ServiceState::Unknown,
        };
        let target = format!("gui/{uid}/{label}");
        match Command::new("launchctl").arg("print").arg(&target).output() {
            Ok(output) if output.status.success() => ServiceState::Loaded,
            Ok(_) => ServiceState::NotLoaded,
            Err(_) => ServiceState::Unknown,
        }
    }

    fn current_uid() -> Result<String> {
        let output = Command::new("id")
            .arg("-u")
            .output()
            .context("failed to run `id -u`")?;
        if !output.status.success() {
            anyhow::bail!(
                "`id -u` failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// `launchctl bootout gui/<uid>/<label>`, tolerating failure (the
    /// service simply wasn't loaded yet -- true on first install, and on
    /// every subsequent one after the matching `bootout` two lines up
    /// already succeeded).
    fn bootout_ignore_failure(label: &str) -> Result<()> {
        let uid = current_uid()?;
        let target = format!("gui/{uid}/{label}");
        let _ = Command::new("launchctl")
            .arg("bootout")
            .arg(target)
            .output();
        Ok(())
    }

    fn bootstrap_with_retry(plist_path: &Path) -> Result<()> {
        let uid = current_uid()?;
        let domain = format!("gui/{uid}");
        let mut last_stderr = String::new();
        for attempt in 1..=BOOTSTRAP_ATTEMPTS {
            let output = Command::new("launchctl")
                .arg("bootstrap")
                .arg(&domain)
                .arg(plist_path)
                .output()
                .context("failed to run `launchctl bootstrap`")?;
            if output.status.success() {
                return Ok(());
            }
            last_stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if attempt < BOOTSTRAP_ATTEMPTS {
                std::thread::sleep(BOOTSTRAP_RETRY_DELAY);
            }
        }
        anyhow::bail!(
            "`launchctl bootstrap {domain} {}` failed after {BOOTSTRAP_ATTEMPTS} attempts: {last_stderr}",
            plist_path.display()
        );
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::process::Command;

    use anyhow::{Context, Result};

    use super::ServiceState;

    pub fn install(vault: &str) -> Result<()> {
        let exe = std::env::current_exe().context("resolving the tephra executable path")?;
        let service_path = super::systemd_service_path(vault)?;
        let timer_path = super::systemd_timer_path(vault)?;

        if let Some(dir) = service_path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }

        std::fs::write(&service_path, super::generate_systemd_service(&exe, vault))
            .with_context(|| format!("writing {}", service_path.display()))?;
        std::fs::write(&timer_path, super::generate_systemd_timer(vault))
            .with_context(|| format!("writing {}", timer_path.display()))?;

        systemctl(&["daemon-reload"])?;
        systemctl(&["enable", "--now", &super::systemd_timer_name(vault)])?;

        println!(
            "installed and enabled service for vault '{vault}':\n  service: {}\n  timer:   {}",
            service_path.display(),
            timer_path.display()
        );
        Ok(())
    }

    pub fn uninstall(vault: &str) -> Result<()> {
        let service_path = super::systemd_service_path(vault)?;
        let timer_path = super::systemd_timer_path(vault)?;

        if !service_path.exists() && !timer_path.exists() {
            println!("service for vault '{vault}' is not installed");
            return Ok(());
        }

        let _ = Command::new("systemctl")
            .arg("--user")
            .arg("disable")
            .arg("--now")
            .arg(super::systemd_timer_name(vault))
            .output();

        for path in [&service_path, &timer_path] {
            if path.exists() {
                std::fs::remove_file(path)
                    .with_context(|| format!("removing {}", path.display()))?;
            }
        }
        let _ = systemctl(&["daemon-reload"]);

        println!(
            "uninstalled service for vault '{vault}': removed {} and {}",
            service_path.display(),
            timer_path.display()
        );
        Ok(())
    }

    pub fn detect(vault: &str) -> ServiceState {
        let timer = super::systemd_timer_name(vault);
        let active = match Command::new("systemctl")
            .arg("--user")
            .arg("is-active")
            .arg(&timer)
            .output()
        {
            Ok(output) => output,
            Err(_) => return ServiceState::Unknown,
        };
        let enabled = match Command::new("systemctl")
            .arg("--user")
            .arg("is-enabled")
            .arg(&timer)
            .output()
        {
            Ok(output) => output,
            Err(_) => return ServiceState::Unknown,
        };

        let is_active = String::from_utf8_lossy(&active.stdout).trim() == "active";
        let is_enabled = String::from_utf8_lossy(&enabled.stdout).trim() == "enabled";
        if is_active && is_enabled {
            ServiceState::Loaded
        } else {
            ServiceState::NotLoaded
        }
    }

    fn systemctl(args: &[&str]) -> Result<()> {
        let output = Command::new("systemctl")
            .arg("--user")
            .args(args)
            .output()
            .context("failed to run `systemctl --user`")?;
        if !output.status.success() {
            anyhow::bail!(
                "`systemctl --user {}` failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use anyhow::Result;

    use super::ServiceState;

    pub fn install(_vault: &str) -> Result<()> {
        Err(super::unsupported_platform_error())
    }

    pub fn uninstall(_vault: &str) -> Result<()> {
        Err(super::unsupported_platform_error())
    }

    pub fn detect(_vault: &str) -> ServiceState {
        ServiceState::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launchd_label_formats_as_com_tephra_vault() {
        assert_eq!(launchd_label("personal"), "com.tephra.personal");
    }

    #[test]
    fn launchd_plist_filename_appends_dot_plist() {
        assert_eq!(
            launchd_plist_filename("personal"),
            "com.tephra.personal.plist"
        );
    }

    #[test]
    fn systemd_names_format_as_tephra_vault_dot_unit() {
        assert_eq!(systemd_service_name("personal"), "tephra-personal.service");
        assert_eq!(systemd_timer_name("personal"), "tephra-personal.timer");
    }

    #[test]
    fn log_filename_formats_as_tephra_vault_dot_log() {
        assert_eq!(log_filename("personal"), "tephra-personal.log");
    }

    #[test]
    fn xml_escape_escapes_the_five_special_characters() {
        assert_eq!(
            xml_escape(r#"a & b < c > d " e ' f"#),
            "a &amp; b &lt; c &gt; d &quot; e &apos; f"
        );
    }

    #[test]
    fn xml_escape_leaves_plain_text_untouched() {
        assert_eq!(
            xml_escape("/opt/homebrew/bin/tephra"),
            "/opt/homebrew/bin/tephra"
        );
    }

    #[test]
    fn systemd_user_dir_from_respects_xdg_override() {
        let got = systemd_user_dir_from(
            Some(Path::new("/custom/xdg")),
            Some(Path::new("/home/someone")),
        )
        .unwrap();
        assert_eq!(got, PathBuf::from("/custom/xdg/systemd/user"));
    }

    #[test]
    fn systemd_user_dir_from_falls_back_to_home_dot_config() {
        let got = systemd_user_dir_from(None, Some(Path::new("/home/someone"))).unwrap();
        assert_eq!(got, PathBuf::from("/home/someone/.config/systemd/user"));
    }

    #[test]
    fn systemd_user_dir_from_errors_without_home_or_xdg() {
        let err = systemd_user_dir_from(None, None).unwrap_err();
        assert!(err.to_string().contains("home directory"));
    }

    #[test]
    fn launch_agents_dir_from_uses_library_launchagents() {
        let got = launch_agents_dir_from(Some(Path::new("/home/someone"))).unwrap();
        assert_eq!(got, PathBuf::from("/home/someone/Library/LaunchAgents"));
    }

    #[test]
    fn launch_agents_dir_from_errors_without_home() {
        assert!(launch_agents_dir_from(None).is_err());
    }

    #[test]
    fn logs_dir_from_uses_library_logs() {
        let got = logs_dir_from(Some(Path::new("/home/someone"))).unwrap();
        assert_eq!(got, PathBuf::from("/home/someone/Library/Logs"));
    }

    #[test]
    fn unsupported_platform_error_names_the_detected_os() {
        let err = unsupported_platform_error();
        let msg = err.to_string();
        assert!(
            msg.contains(std::env::consts::OS),
            "error should name the detected OS, got: {msg}"
        );
        assert!(
            msg.contains("macOS") && msg.contains("Linux"),
            "error should name the supported platforms, got: {msg}"
        );
    }

    #[test]
    fn service_state_as_str_matches_json_contract() {
        assert_eq!(ServiceState::Loaded.as_str(), "loaded");
        assert_eq!(ServiceState::NotLoaded.as_str(), "not-loaded");
        assert_eq!(ServiceState::Unknown.as_str(), "unknown");
    }
}
