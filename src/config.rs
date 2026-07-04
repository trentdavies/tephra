//! Config loading and vault resolution.
//!
//! See `docs/DESIGN.md` §Configuration for the authoritative schema and load
//! order.
//!
//! This module's public API isn't called from `main.rs` yet (that lands
//! starting with the `gitx`/`agent`/`doctor` commands in later tasks), so
//! the non-test build has no live root reaching it. Silence dead_code until
//! then rather than wire it in prematurely.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Deserialize;

/// One configured vault: a bridge checkout + agent work-clone pairing.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vault {
    /// Bridge checkout path (where the daemon operates).
    pub bridge: PathBuf,
    /// Default agent clone location.
    pub work: PathBuf,
    /// Remote URL used by `tephra clone` and bridge re-clone advice.
    pub url: String,
    /// Branch tracked by the bridge and agent clones. Defaults to "main".
    #[serde(default = "default_branch")]
    pub branch: String,
}

fn default_branch() -> String {
    "main".to_string()
}

/// Top-level config: `[vaults.<name>]` tables.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub vaults: HashMap<String, Vault>,
}

/// Marker for configuration/usage errors (vs. domain/runtime errors).
///
/// `main` downcasts through the error chain for this type to decide the
/// process exit code: 2 for usage/config errors, 1 otherwise, per
/// `docs/DESIGN.md`'s exit-code contract.
///
/// Use this ONLY for CLI-usage and config-resolution failures (bad vault
/// name, missing/unparseable config, ambiguous vault selection). Domain
/// failures — e.g. a sync rebase conflict, a failed push, an unreachable
/// remote — must remain plain anyhow errors so they exit 1, not 2.
#[derive(Debug)]
pub struct UsageError(pub String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UsageError {}

fn usage_err<T>(msg: impl Into<String>) -> Result<T> {
    Err(anyhow::Error::new(UsageError(msg.into())))
}

/// Expand a leading `~` or `~/...` to the user's home directory. Paths that
/// don't start with `~` are returned unchanged. If the home directory can't
/// be determined, the path is also returned unchanged.
pub fn expand_tilde(path: &Path) -> PathBuf {
    let Some(s) = path.to_str() else {
        return path.to_path_buf();
    };
    if s == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    } else if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    path.to_path_buf()
}

/// Parse a config file at an exact path. Tilde-expands `bridge` and `work`
/// on every vault.
pub fn load_from(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        let msg = if e.kind() == std::io::ErrorKind::NotFound {
            format!(
                "config file not found: {} (run `tephra init` to create one)",
                path.display()
            )
        } else {
            format!("could not read config file {}: {e}", path.display())
        };
        anyhow::Error::new(UsageError(msg))
    })?;
    let mut cfg: Config = toml::from_str(&text).map_err(|e| {
        anyhow::Error::new(UsageError(format!(
            "failed to parse config file {}: {e}",
            path.display()
        )))
    })?;
    for name in cfg.vaults.keys() {
        validate_vault_name(name, path)?;
    }
    for vault in cfg.vaults.values_mut() {
        vault.bridge = expand_tilde(&vault.bridge);
        vault.work = expand_tilde(&vault.work);
    }
    Ok(cfg)
}

/// Vault names are interpolated into service unit names/labels
/// (`com.tephra.<vault>`, `tephra-<vault>.service`), file paths
/// (`tephra-<vault>.log`), and shell-adjacent contexts by the `service` and
/// `obsidian` modules, so they're validated once here at the config
/// boundary: non-empty, ASCII alphanumeric plus `-`, `_`, and `.` only.
/// Everything downstream can then trust a loaded config's names (a `/`
/// would otherwise traverse directories; a space would split tokens).
fn validate_vault_name(name: &str, config_path: &Path) -> Result<()> {
    if name.is_empty() {
        return usage_err(format!(
            "empty vault name in {}: vault names must be non-empty",
            config_path.display()
        ));
    }
    let valid = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !valid {
        return usage_err(format!(
            "invalid vault name '{name}' in {}: vault names may only contain \
             ASCII letters, digits, '-', '_', and '.'",
            config_path.display()
        ));
    }
    Ok(())
}

/// Resolve which config file to load, per `docs/DESIGN.md`'s load order:
/// `$TEPHRA_CONFIG` (exact file) if set; else
/// `$XDG_CONFIG_HOME/tephra/config.toml` if `XDG_CONFIG_HOME` is set; else
/// `~/.config/tephra/config.toml`.
fn resolve_config_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("TEPHRA_CONFIG") {
        return Ok(PathBuf::from(p));
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg).join("tephra").join("config.toml"));
    }
    let home = dirs::home_dir().ok_or_else(|| {
        anyhow::Error::new(UsageError("could not determine home directory".to_string()))
    })?;
    Ok(home.join(".config").join("tephra").join("config.toml"))
}

/// Load config using the environment-driven load order. See
/// `resolve_config_path` for the exact rules.
pub fn load() -> Result<Config> {
    let path = resolve_config_path()?;
    load_from(&path)
}

/// A vault picked out of the config, together with its configured name.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedVault<'a> {
    pub name: &'a str,
    pub vault: &'a Vault,
}

/// Resolve a vault by name, or the sole configured vault if `name` is
/// `None`. Errors are usage errors (see `UsageError`): unknown name lists
/// available vaults; `None` with zero or more than one configured vault
/// lists the available choices (or notes there are none).
pub fn resolve_vault<'a>(cfg: &'a Config, name: Option<&str>) -> Result<ResolvedVault<'a>> {
    match name {
        Some(n) => cfg
            .vaults
            .get_key_value(n)
            .map(|(name, vault)| ResolvedVault {
                name: name.as_str(),
                vault,
            })
            .map_or_else(
                || {
                    usage_err(format!(
                        "no vault named '{n}'; available vaults: {}",
                        sorted_names(cfg)
                    ))
                },
                Ok,
            ),
        None => {
            let mut iter = cfg.vaults.iter();
            match (iter.next(), iter.next()) {
                (Some((name, vault)), None) => Ok(ResolvedVault {
                    name: name.as_str(),
                    vault,
                }),
                (None, None) => {
                    usage_err("no vaults configured; run `tephra init` to add one".to_string())
                }
                _ => usage_err(format!(
                    "multiple vaults configured; specify one: {}",
                    sorted_names(cfg)
                )),
            }
        }
    }
}

fn sorted_names(cfg: &Config) -> String {
    let mut names: Vec<&str> = cfg.vaults.keys().map(|s| s.as_str()).collect();
    names.sort();
    names.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::tempdir;

    /// Guards tests that read or write process-global env vars
    /// (`TEPHRA_CONFIG`, `XDG_CONFIG_HOME`, `HOME`) so they can't race each
    /// other across parallel test threads.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn write_config(dir: &Path, contents: &str) -> PathBuf {
        let path = dir.join("config.toml");
        std::fs::write(&path, contents).unwrap();
        path
    }

    // --- parsing (load_from) ---

    #[test]
    fn minimal_config_parses_with_default_branch() {
        let dir = tempdir().unwrap();
        let path = write_config(
            dir.path(),
            r#"
            [vaults.personal]
            bridge = "/tmp/bridge-personal"
            work = "/tmp/work-personal"
            url = "tailgit:obsidian-personal"
            "#,
        );

        let cfg = load_from(&path).unwrap();

        assert_eq!(cfg.vaults.len(), 1);
        let vault = &cfg.vaults["personal"];
        assert_eq!(vault.bridge, PathBuf::from("/tmp/bridge-personal"));
        assert_eq!(vault.work, PathBuf::from("/tmp/work-personal"));
        assert_eq!(vault.url, "tailgit:obsidian-personal");
        assert_eq!(vault.branch, "main");
    }

    #[test]
    fn full_config_parses_two_vaults_with_explicit_branch() {
        let dir = tempdir().unwrap();
        let path = write_config(
            dir.path(),
            r#"
            [vaults.personal]
            bridge = "/tmp/bridge-personal"
            work = "/tmp/work-personal"
            url = "tailgit:obsidian-personal"
            branch = "trunk"

            [vaults.work]
            bridge = "/tmp/bridge-work"
            work = "/tmp/work-work"
            url = "tailgit:obsidian-work"
            "#,
        );

        let cfg = load_from(&path).unwrap();

        assert_eq!(cfg.vaults.len(), 2);
        assert_eq!(cfg.vaults["personal"].branch, "trunk");
        assert_eq!(cfg.vaults["work"].branch, "main");
        assert_eq!(cfg.vaults["work"].url, "tailgit:obsidian-work");
    }

    #[test]
    fn tilde_is_expanded_on_bridge_and_work() {
        let _guard = ENV_LOCK.lock().unwrap();

        let dir = tempdir().unwrap();
        let path = write_config(
            dir.path(),
            r#"
            [vaults.personal]
            bridge = "~/dev/memory/bridge-personal"
            work = "~/dev/memory/work-personal"
            url = "tailgit:obsidian-personal"
            "#,
        );

        let cfg = load_from(&path).unwrap();
        let home = dirs::home_dir().unwrap();
        let vault = &cfg.vaults["personal"];
        assert_eq!(vault.bridge, home.join("dev/memory/bridge-personal"));
        assert_eq!(vault.work, home.join("dev/memory/work-personal"));
    }

    #[test]
    fn unknown_vault_field_is_a_parse_error_naming_the_field() {
        let dir = tempdir().unwrap();
        let path = write_config(
            dir.path(),
            r#"
            [vaults.personal]
            bridge = "/tmp/bridge-personal"
            work = "/tmp/work-personal"
            url = "tailgit:obsidian-personal"
            brnach = "trunk"
            "#,
        );

        let err = load_from(&path).unwrap_err();
        let msg = err.to_string();

        assert!(
            msg.contains("brnach"),
            "error should name the unknown field, got: {msg}"
        );
        assert!(
            msg.contains(&path.display().to_string()),
            "error should name the file, got: {msg}"
        );
    }

    #[test]
    fn unknown_top_level_table_is_a_parse_error_naming_the_table() {
        let dir = tempdir().unwrap();
        let path = write_config(
            dir.path(),
            r#"
            [vaulst.oops]
            bridge = "/tmp/bridge"
            work = "/tmp/work"
            url = "tailgit:x"
            "#,
        );

        let err = load_from(&path).unwrap_err();
        let msg = err.to_string();

        assert!(
            msg.contains("vaulst"),
            "error should name the unknown table, got: {msg}"
        );
    }

    // --- vault-name validation ---

    #[test]
    fn vault_name_with_space_is_rejected_naming_vault_and_charset() {
        let dir = tempdir().unwrap();
        let path = write_config(
            dir.path(),
            r#"
            [vaults."bad name"]
            bridge = "/tmp/bridge"
            work = "/tmp/work"
            url = "tailgit:x"
            "#,
        );

        let err = load_from(&path).unwrap_err();
        let msg = err.to_string();

        assert!(
            msg.contains("bad name"),
            "error should name the offending vault, got: {msg}"
        );
        assert!(
            msg.contains("ASCII letters, digits, '-', '_', and '.'"),
            "error should name the allowed charset, got: {msg}"
        );
        assert!(
            err.chain()
                .any(|c| c.downcast_ref::<UsageError>().is_some()),
            "invalid vault names are config (usage) errors"
        );
    }

    #[test]
    fn vault_name_with_slash_is_rejected() {
        // `/` in a vault name would traverse directories when interpolated
        // into unit-file and log paths (`com.tephra.<vault>.plist`,
        // `tephra-<vault>.log`, ...).
        let dir = tempdir().unwrap();
        let path = write_config(
            dir.path(),
            r#"
            [vaults."../evil"]
            bridge = "/tmp/bridge"
            work = "/tmp/work"
            url = "tailgit:x"
            "#,
        );

        let err = load_from(&path).unwrap_err();
        assert!(
            err.to_string().contains("../evil"),
            "error should name the offending vault, got: {err}"
        );
    }

    #[test]
    fn empty_vault_name_is_rejected() {
        let dir = tempdir().unwrap();
        let path = write_config(
            dir.path(),
            r#"
            [vaults.""]
            bridge = "/tmp/bridge"
            work = "/tmp/work"
            url = "tailgit:x"
            "#,
        );

        let err = load_from(&path).unwrap_err();
        assert!(
            err.to_string().contains("empty"),
            "error should say the name is empty, got: {err}"
        );
    }

    #[test]
    fn vault_names_with_dots_dashes_underscores_digits_are_valid() {
        let dir = tempdir().unwrap();
        let path = write_config(
            dir.path(),
            r#"
            [vaults."my-vault_2.beta"]
            bridge = "/tmp/bridge"
            work = "/tmp/work"
            url = "tailgit:x"
            "#,
        );

        let cfg = load_from(&path).unwrap();
        assert!(cfg.vaults.contains_key("my-vault_2.beta"));
    }

    #[test]
    fn empty_file_parses_to_zero_vaults() {
        let dir = tempdir().unwrap();
        let path = write_config(dir.path(), "");

        let cfg = load_from(&path).unwrap();

        assert!(cfg.vaults.is_empty());
    }

    #[test]
    fn empty_vaults_table_parses_to_zero_vaults() {
        let dir = tempdir().unwrap();
        let path = write_config(dir.path(), "[vaults]\n");

        let cfg = load_from(&path).unwrap();

        assert!(cfg.vaults.is_empty());
    }

    #[test]
    fn missing_file_error_names_the_path_and_suggests_init() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.toml");

        let err = load_from(&path).unwrap_err();
        let msg = err.to_string();

        assert!(
            msg.contains(&path.display().to_string()),
            "error should name the missing path, got: {msg}"
        );
        assert!(
            msg.contains("run `tephra init` to create one"),
            "error should suggest tephra init, got: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_file_error_says_could_not_read() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let path = write_config(dir.path(), "[vaults]\n");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();

        // Running as root makes chmod 000 ineffective; skip in that case.
        if std::fs::read_to_string(&path).is_ok() {
            return;
        }

        let err = load_from(&path).unwrap_err();
        let msg = err.to_string();

        assert!(
            msg.starts_with("could not read config file"),
            "error should use the 'could not read' wording, got: {msg}"
        );
        assert!(
            msg.contains(&path.display().to_string()),
            "error should name the file, got: {msg}"
        );
        assert!(
            !msg.contains("not found"),
            "permission errors must not claim the file is missing, got: {msg}"
        );
    }

    #[test]
    fn malformed_toml_error_names_the_file() {
        let dir = tempdir().unwrap();
        let path = write_config(dir.path(), "this is not valid toml [[[");

        let err = load_from(&path).unwrap_err();
        let msg = err.to_string();

        assert!(
            msg.contains(&path.display().to_string()),
            "error should name the file, got: {msg}"
        );
    }

    // --- resolve_vault ---

    fn two_vault_config() -> Config {
        let mut vaults = HashMap::new();
        vaults.insert(
            "personal".to_string(),
            Vault {
                bridge: PathBuf::from("/tmp/bridge-personal"),
                work: PathBuf::from("/tmp/work-personal"),
                url: "tailgit:obsidian-personal".to_string(),
                branch: "main".to_string(),
            },
        );
        vaults.insert(
            "work".to_string(),
            Vault {
                bridge: PathBuf::from("/tmp/bridge-work"),
                work: PathBuf::from("/tmp/work-work"),
                url: "tailgit:obsidian-work".to_string(),
                branch: "main".to_string(),
            },
        );
        Config { vaults }
    }

    fn one_vault_config() -> Config {
        let mut vaults = HashMap::new();
        vaults.insert(
            "personal".to_string(),
            Vault {
                bridge: PathBuf::from("/tmp/bridge-personal"),
                work: PathBuf::from("/tmp/work-personal"),
                url: "tailgit:obsidian-personal".to_string(),
                branch: "main".to_string(),
            },
        );
        Config { vaults }
    }

    #[test]
    fn resolve_vault_explicit_name_hit() {
        let cfg = two_vault_config();
        let resolved = resolve_vault(&cfg, Some("work")).unwrap();
        assert_eq!(resolved.name, "work");
        assert_eq!(resolved.vault.url, "tailgit:obsidian-work");
    }

    #[test]
    fn resolve_vault_explicit_name_miss_lists_names() {
        let cfg = two_vault_config();
        let err = resolve_vault(&cfg, Some("nope")).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("nope"),
            "error should mention the requested name, got: {msg}"
        );
        assert!(
            msg.contains("personal"),
            "error should list available vaults, got: {msg}"
        );
        assert!(
            msg.contains("work"),
            "error should list available vaults, got: {msg}"
        );
    }

    #[test]
    fn resolve_vault_none_with_one_configured() {
        let cfg = one_vault_config();
        let resolved = resolve_vault(&cfg, None).unwrap();
        assert_eq!(resolved.name, "personal");
        assert_eq!(resolved.vault.url, "tailgit:obsidian-personal");
    }

    #[test]
    fn resolve_vault_none_with_two_configured_lists_choices() {
        let cfg = two_vault_config();
        let err = resolve_vault(&cfg, None).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("personal"),
            "error should list choices, got: {msg}"
        );
        assert!(
            msg.contains("work"),
            "error should list choices, got: {msg}"
        );
    }

    #[test]
    fn resolve_vault_none_with_zero_configured_suggests_init() {
        let cfg = Config::default();
        let err = resolve_vault(&cfg, None).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no vaults configured; run `tephra init` to add one"),
            "error should suggest tephra init, got: {msg}"
        );
    }

    #[test]
    fn resolve_vault_errors_are_usage_errors() {
        let cfg = Config::default();
        let err = resolve_vault(&cfg, None).unwrap_err();
        assert!(err
            .chain()
            .any(|c| c.downcast_ref::<UsageError>().is_some()));
    }

    // --- load order (env-driven) ---

    #[test]
    fn tephra_config_env_wins() {
        let _guard = ENV_LOCK.lock().unwrap();

        let dir = tempdir().unwrap();
        let path = write_config(
            dir.path(),
            r#"
            [vaults.fromenv]
            bridge = "/tmp/bridge"
            work = "/tmp/work"
            url = "tailgit:x"
            "#,
        );

        std::env::set_var("TEPHRA_CONFIG", &path);
        std::env::remove_var("XDG_CONFIG_HOME");

        let result = load();

        std::env::remove_var("TEPHRA_CONFIG");

        let cfg = result.unwrap();
        assert!(cfg.vaults.contains_key("fromenv"));
    }

    #[test]
    fn xdg_config_home_is_respected() {
        let _guard = ENV_LOCK.lock().unwrap();

        let dir = tempdir().unwrap();
        let xdg = dir.path().join("xdg");
        std::fs::create_dir_all(xdg.join("tephra")).unwrap();
        write_config(
            &xdg.join("tephra"),
            r#"
            [vaults.fromxdg]
            bridge = "/tmp/bridge"
            work = "/tmp/work"
            url = "tailgit:x"
            "#,
        );

        std::env::remove_var("TEPHRA_CONFIG");
        std::env::set_var("XDG_CONFIG_HOME", &xdg);

        let result = load();

        std::env::remove_var("XDG_CONFIG_HOME");

        let cfg = result.unwrap();
        assert!(cfg.vaults.contains_key("fromxdg"));
    }

    #[test]
    fn tephra_config_pointing_at_missing_file_errors_despite_valid_xdg_fallback() {
        let _guard = ENV_LOCK.lock().unwrap();

        // A perfectly valid XDG config exists...
        let dir = tempdir().unwrap();
        let xdg = dir.path().join("xdg");
        std::fs::create_dir_all(xdg.join("tephra")).unwrap();
        write_config(
            &xdg.join("tephra"),
            r#"
            [vaults.fromxdg]
            bridge = "/tmp/bridge"
            work = "/tmp/work"
            url = "tailgit:x"
            "#,
        );

        // ...but TEPHRA_CONFIG points at a missing file. An explicit
        // override must hard-error, never silently fall back.
        let missing = dir.path().join("missing.toml");
        std::env::set_var("TEPHRA_CONFIG", &missing);
        std::env::set_var("XDG_CONFIG_HOME", &xdg);

        let result = load();

        std::env::remove_var("TEPHRA_CONFIG");
        std::env::remove_var("XDG_CONFIG_HOME");

        let err = result.unwrap_err();
        assert!(
            err.to_string().contains(&missing.display().to_string()),
            "error should name the TEPHRA_CONFIG path, got: {err}"
        );
    }

    #[test]
    fn falls_back_to_home_dot_config() {
        let _guard = ENV_LOCK.lock().unwrap();

        let dir = tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join(".config").join("tephra")).unwrap();
        write_config(
            &home.join(".config").join("tephra"),
            r#"
            [vaults.fromhome]
            bridge = "/tmp/bridge"
            work = "/tmp/work"
            url = "tailgit:x"
            "#,
        );

        let prior_home = std::env::var("HOME").ok();
        std::env::remove_var("TEPHRA_CONFIG");
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::set_var("HOME", &home);

        let result = load();

        match prior_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }

        let cfg = result.unwrap();
        assert!(cfg.vaults.contains_key("fromhome"));
    }
}
