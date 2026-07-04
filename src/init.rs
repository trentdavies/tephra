//! `tephra init`: register a vault, writing or merging `config.toml`.
//!
//! See `docs/DESIGN.md` §Command surface and §Configuration.
//!
//! Two write paths, deliberately different tools:
//!
//! - **Fresh file** (`config.toml` doesn't exist yet): built from
//!   `config::Config`/`config::Vault` (now `Serialize`, alongside their
//!   existing `Deserialize`) and rendered with `toml::to_string_pretty`.
//!   There's nothing to preserve, so the plain, non-format-preserving
//!   serializer is fine.
//! - **Existing file**: parsed and edited with `toml_edit`, which preserves
//!   every byte this command doesn't touch -- other `[vaults.*]` tables,
//!   blank lines, and comments a user hand-wrote all survive untouched. The
//!   `toml` crate's serializer cannot do this: round-tripping through it
//!   would silently drop comments and reformat the whole file.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::{self, UsageError};

/// Parsed `tephra init` CLI flags, straight from `main.rs`'s clap
/// definition. Every value field is optional except `force`/`yes`: under
/// `--yes` all four (name/bridge/work/url) are required outright; otherwise
/// whichever are missing are filled in by an interactive prompt.
pub struct InitArgs {
    pub name: Option<String>,
    pub bridge: Option<String>,
    pub work: Option<String>,
    pub url: Option<String>,
    pub branch: Option<String>,
    pub force: bool,
    pub yes: bool,
}

/// A fully resolved vault, ready to be written into `config.toml`. Paths are
/// kept as the caller/typist gave them (e.g. `~/dev/memory/bridge-personal`
/// unexpanded) -- `config::load_from` tilde-expands at load time, and
/// leaving `~` in the file is far more readable than an absolute path baked
/// in for one particular machine.
#[derive(Debug)]
struct Resolved {
    name: String,
    bridge: String,
    work: String,
    url: String,
    branch: String,
}

/// `tephra init`'s entry point.
pub fn run(args: InitArgs) -> Result<()> {
    let force = args.force;
    // Resolve the target config path up front: name validation (which
    // names the file in its error message) happens inside `resolve`,
    // immediately after the name is obtained -- a bad name must fail
    // before any further interactive prompting, not after the user has
    // typed four more answers.
    let path = config::resolve_config_path()?;
    let resolved = resolve(args, &path)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    if path.exists() {
        merge_into_existing(&path, &resolved, force)?;
    } else {
        write_fresh(&path, &resolved)?;
    }

    print_success(&resolved, &path);
    Ok(())
}

// --------------------------------------------------------------------
// resolving flags/prompts into a `Resolved` vault
// --------------------------------------------------------------------

/// `config_path` is only used to name the config file in a bad-name error
/// message (see `config::validate_vault_name`).
fn resolve(args: InitArgs, config_path: &Path) -> Result<Resolved> {
    if args.yes {
        resolve_non_interactive(args, config_path)
    } else {
        resolve_interactive(args, config_path)
    }
}

/// `--yes`: pure non-interactive. name/bridge/work/url are all required;
/// branch defaults to "main". Never touches stdin.
fn resolve_non_interactive(args: InitArgs, config_path: &Path) -> Result<Resolved> {
    let name = require_flag(args.name, "--name")?;
    config::validate_vault_name(&name, config_path)?;
    Ok(Resolved {
        name,
        bridge: require_flag(args.bridge, "--bridge")?,
        work: require_flag(args.work, "--work")?,
        url: require_flag(args.url, "--url")?,
        branch: args.branch.unwrap_or_else(|| "main".to_string()),
    })
}

fn require_flag(value: Option<String>, flag: &str) -> Result<String> {
    value.ok_or_else(|| {
        anyhow::Error::new(UsageError(format!(
            "{flag} is required with --yes (non-interactive init)"
        )))
    })
}

/// Without `--yes`: whatever flags were passed win outright; anything
/// missing is prompted for interactively, in name -> bridge -> work -> url
/// -> branch order (bridge/work's defaults depend on the just-entered name,
/// so name must resolve first). The name is validated the moment it's
/// obtained -- flag or prompt -- so a bad name errors out before the user
/// is asked for anything else.
fn resolve_interactive(args: InitArgs, config_path: &Path) -> Result<Resolved> {
    let name = match args.name {
        Some(n) => n,
        None => prompt("vault name", None)?,
    };
    config::validate_vault_name(&name, config_path)?;
    let bridge_default = format!("~/dev/memory/bridge-{name}");
    let bridge = match args.bridge {
        Some(b) => b,
        None => prompt("bridge path", Some(&bridge_default))?,
    };
    let work_default = format!("~/dev/memory/work-{name}");
    let work = match args.work {
        Some(w) => w,
        None => prompt("work path", Some(&work_default))?,
    };
    let url = match args.url {
        Some(u) => u,
        None => prompt("remote url", None)?,
    };
    let branch = match args.branch {
        Some(b) => b,
        None => prompt("branch", Some("main"))?,
    };
    Ok(Resolved {
        name,
        bridge,
        work,
        url,
        branch,
    })
}

/// Prompt on stdout (`<label> [<default>]: `, or `<label>: ` with no
/// default), then read one line from stdin. Loops until a usable answer
/// arrives: an empty line uses the default when there is one, or is
/// re-prompted when there isn't (a required field can't silently become
/// empty). stdin reaching EOF before that happens is a usage error naming
/// the escape hatch -- this is what makes a closed/non-interactive stdin
/// fail clearly instead of hanging or silently accepting an empty answer,
/// while still letting a script pipe real answers over stdin (see
/// `tests/init.rs`'s interactive-prompt test, which does exactly that).
fn prompt(label: &str, default: Option<&str>) -> Result<String> {
    loop {
        match default {
            Some(d) => print!("{label} [{d}]: "),
            None => print!("{label}: "),
        }
        io::stdout().flush().ok();

        let mut line = String::new();
        let n = io::stdin()
            .read_line(&mut line)
            .context("reading from stdin")?;
        let raw = if n == 0 { None } else { Some(line.as_str()) };

        if let Some(value) = resolve_prompt_line(raw, default, label)? {
            return Ok(value);
        }
        // Empty line, no default: ask again.
    }
}

/// The pure decision behind one prompt round-trip, factored out so it's
/// unit-testable without touching real stdin. `line` is `None` for EOF,
/// `Some(raw)` (not yet trimmed) otherwise. Returns `Ok(Some(value))` when
/// an answer was obtained (typed or defaulted), `Ok(None)` to mean "empty
/// input and no default -- ask again", and `Err` (a `UsageError`) for EOF.
fn resolve_prompt_line(
    line: Option<&str>,
    default: Option<&str>,
    label: &str,
) -> Result<Option<String>> {
    let Some(line) = line else {
        return Err(anyhow::Error::new(UsageError(format!(
            "stdin closed before a value for '{label}' was provided; pass \
             --name/--bridge/--work/--url (and --branch/--force) explicitly, \
             or --yes for a fully non-interactive run"
        ))));
    };
    let trimmed = line.trim();
    if !trimmed.is_empty() {
        return Ok(Some(trimmed.to_string()));
    }
    Ok(default.map(str::to_string))
}

// --------------------------------------------------------------------
// writing config.toml
// --------------------------------------------------------------------

/// Brand-new `config.toml`: build the canonical `Config`/`Vault` shape and
/// serialize it directly. See the module doc comment for why this path
/// (unlike the merge path) doesn't need `toml_edit`.
fn write_fresh(path: &Path, resolved: &Resolved) -> Result<()> {
    let mut vaults = std::collections::HashMap::new();
    vaults.insert(
        resolved.name.clone(),
        config::Vault {
            bridge: PathBuf::from(&resolved.bridge),
            work: PathBuf::from(&resolved.work),
            url: resolved.url.clone(),
            branch: resolved.branch.clone(),
        },
    );
    let cfg = config::Config { vaults };
    let contents = toml::to_string_pretty(&cfg).context("serializing new config.toml")?;
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

/// Merge into an existing `config.toml` via `toml_edit`, so anything this
/// command doesn't touch (other `[vaults.*]` tables, blank lines, hand-
/// written comments) survives byte-for-byte. Refuses a duplicate vault name
/// unless `force` is set.
fn merge_into_existing(path: &Path, resolved: &Resolved, force: bool) -> Result<()> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading existing config {}", path.display()))?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("parsing existing config {} as TOML", path.display()))?;

    if doc.get("vaults").is_none() {
        let mut vaults_table = toml_edit::Table::new();
        vaults_table.set_implicit(true);
        doc["vaults"] = toml_edit::Item::Table(vaults_table);
    }
    let vaults = doc["vaults"]
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("`vaults` in {} is not a table", path.display()))?;

    if vaults.contains_key(&resolved.name) && !force {
        return Err(anyhow::Error::new(UsageError(format!(
            "vault '{}' already exists in {}; use --force to replace it",
            resolved.name,
            path.display()
        ))));
    }

    // On a `--force` replace, mutate the existing table's values in place
    // rather than inserting a fresh `Item`: `Table::insert` on an occupied
    // key resets the key's decor, which would eat any comment block sitting
    // above the `[vaults.<name>]` header (including a file-header comment,
    // which toml_edit models as the FIRST table's leading decor).
    if let Some(existing) = vaults
        .get_mut(&resolved.name)
        .and_then(toml_edit::Item::as_table_mut)
    {
        existing["bridge"] = toml_edit::value(resolved.bridge.as_str());
        existing["work"] = toml_edit::value(resolved.work.as_str());
        existing["url"] = toml_edit::value(resolved.url.as_str());
        existing["branch"] = toml_edit::value(resolved.branch.as_str());
    } else {
        let mut vault_table = toml_edit::Table::new();
        vault_table["bridge"] = toml_edit::value(resolved.bridge.as_str());
        vault_table["work"] = toml_edit::value(resolved.work.as_str());
        vault_table["url"] = toml_edit::value(resolved.url.as_str());
        vault_table["branch"] = toml_edit::value(resolved.branch.as_str());
        vaults.insert(&resolved.name, toml_edit::Item::Table(vault_table));
    }

    std::fs::write(path, doc.to_string()).with_context(|| format!("writing {}", path.display()))
}

fn print_success(resolved: &Resolved, path: &Path) {
    println!(
        "wrote vault '{}' to {}:\n  bridge = \"{}\"\n  work   = \"{}\"\n  url    = \"{}\"\n  branch = \"{}\"",
        resolved.name,
        path.display(),
        resolved.bridge,
        resolved.work,
        resolved.url,
        resolved.branch,
    );
    println!();
    println!("next steps:");
    println!(
        "  1. clone the bridge checkout (skip if your sync app already manages that folder):\n       git clone {} {}",
        resolved.url, resolved.bridge
    );
    println!(
        "  2. install the background service:\n       tephra service install {}",
        resolved.name
    );
    println!(
        "  3. clone your agent work copy:\n       tephra clone {}",
        resolved.name
    );
    println!(
        "  4. scaffold agent instructions:\n       tephra agent init {}",
        resolved.name
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- resolve_prompt_line ---

    #[test]
    fn resolve_prompt_line_uses_input_when_present() {
        assert_eq!(
            resolve_prompt_line(Some("hello\n"), Some("default"), "x").unwrap(),
            Some("hello".to_string())
        );
    }

    #[test]
    fn resolve_prompt_line_falls_back_to_default_on_empty_input() {
        assert_eq!(
            resolve_prompt_line(Some("\n"), Some("default"), "x").unwrap(),
            Some("default".to_string())
        );
        assert_eq!(
            resolve_prompt_line(Some(""), Some("default"), "x").unwrap(),
            Some("default".to_string())
        );
    }

    #[test]
    fn resolve_prompt_line_none_when_empty_and_no_default() {
        assert_eq!(resolve_prompt_line(Some("\n"), None, "x").unwrap(), None);
    }

    #[test]
    fn resolve_prompt_line_trims_surrounding_whitespace() {
        assert_eq!(
            resolve_prompt_line(Some("  value  \n"), None, "x").unwrap(),
            Some("value".to_string())
        );
    }

    #[test]
    fn resolve_prompt_line_errors_on_eof_naming_the_label_and_flags() {
        let err = resolve_prompt_line(None, Some("default"), "remote url").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("stdin closed"), "got: {msg}");
        assert!(msg.contains("remote url"), "got: {msg}");
        assert!(msg.contains("--yes"), "got: {msg}");
        assert!(
            err.chain()
                .any(|c| c.downcast_ref::<UsageError>().is_some()),
            "EOF during a prompt should be a usage error"
        );
    }

    // --- require_flag ---

    #[test]
    fn require_flag_ok_when_present() {
        assert_eq!(
            require_flag(Some("x".to_string()), "--name").unwrap(),
            "x".to_string()
        );
    }

    #[test]
    fn require_flag_errors_naming_the_flag() {
        let err = require_flag(None, "--name").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("--name"), "got: {msg}");
        assert!(msg.contains("--yes"), "got: {msg}");
        assert!(err
            .chain()
            .any(|c| c.downcast_ref::<UsageError>().is_some()));
    }

    // --- resolve_non_interactive ---

    #[test]
    fn resolve_non_interactive_defaults_branch_to_main() {
        let resolved = resolve_non_interactive(
            InitArgs {
                name: Some("personal".to_string()),
                bridge: Some("/tmp/bridge".to_string()),
                work: Some("/tmp/work".to_string()),
                url: Some("tailgit:x".to_string()),
                branch: None,
                force: false,
                yes: true,
            },
            Path::new("/fake/config.toml"),
        )
        .unwrap();
        assert_eq!(resolved.branch, "main");
    }

    #[test]
    fn resolve_non_interactive_missing_field_errors() {
        let err = resolve_non_interactive(
            InitArgs {
                name: Some("personal".to_string()),
                bridge: None,
                work: Some("/tmp/work".to_string()),
                url: Some("tailgit:x".to_string()),
                branch: None,
                force: false,
                yes: true,
            },
            Path::new("/fake/config.toml"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("--bridge"));
    }

    #[test]
    fn resolve_non_interactive_rejects_a_bad_name_naming_the_charset() {
        let err = resolve_non_interactive(
            InitArgs {
                name: Some("bad name".to_string()),
                bridge: Some("/tmp/bridge".to_string()),
                work: Some("/tmp/work".to_string()),
                url: Some("tailgit:x".to_string()),
                branch: None,
                force: false,
                yes: true,
            },
            Path::new("/fake/config.toml"),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("bad name"), "got: {msg}");
        assert!(
            msg.contains("ASCII letters, digits, '-', '_', and '.'"),
            "got: {msg}"
        );
    }
}
