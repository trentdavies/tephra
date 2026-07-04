use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use tephra::{agent, bridge, config, doctor, init, obsidian, service};

/// tephra: layered memory for humans and their agents.
#[derive(Parser)]
#[command(name = "tephra", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Register a vault (writes config.toml)
    Init {
        /// Vault name (prompted if omitted, unless --yes)
        #[arg(long)]
        name: Option<String>,

        /// Bridge checkout path (prompted if omitted, unless --yes; default
        /// ~/dev/memory/bridge-<name>)
        #[arg(long)]
        bridge: Option<String>,

        /// Agent work-clone path (prompted if omitted, unless --yes;
        /// default ~/dev/memory/work-<name>)
        #[arg(long)]
        work: Option<String>,

        /// Remote URL (prompted if omitted, unless --yes)
        #[arg(long)]
        url: Option<String>,

        /// Branch to track (default "main")
        #[arg(long)]
        branch: Option<String>,

        /// Replace an existing vault of the same name
        #[arg(long)]
        force: bool,

        /// Fully non-interactive: require --name/--bridge/--work/--url,
        /// never prompt
        #[arg(long)]
        yes: bool,
    },

    /// Run the bridge merge cycle
    Bridge {
        #[command(flatten)]
        mode: BridgeMode,

        /// Vault name (defaults to the sole configured vault)
        vault: Option<String>,
    },

    /// Clone the vault repo to the work path
    Clone {
        /// Vault name (defaults to the sole configured vault)
        vault: Option<String>,
    },

    /// commit-all -> pull --rebase -> push (agent entry point)
    Sync {
        /// Vault name (defaults to the sole configured vault)
        vault: Option<String>,

        /// Commit message
        #[arg(short = 'm', long = "message")]
        message: Option<String>,
    },

    /// Work clone + bridge + service + remote state
    Status {
        /// Vault name (defaults to the sole configured vault)
        vault: Option<String>,

        /// Emit machine-readable JSON
        #[arg(long)]
        json: bool,
    },

    /// Manage the platform service (launchd / systemd) that runs the bridge
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },

    /// Agent-facing scaffolding commands
    Agent {
        #[command(subcommand)]
        action: AgentAction,
    },

    /// Obsidian Sync pairing commands
    Obsidian {
        #[command(subcommand)]
        action: ObsidianAction,
    },

    /// Identity resolves, remote reachable, stale locks, ...
    Doctor {
        /// Vault name (defaults to the sole configured vault)
        vault: Option<String>,
    },
}

// `once`/`watch` live in their own nested `Args` struct so the mutual-
// exclusion group they form (via `#[group(...)]`, which otherwise sweeps in
// every field of the annotated struct into the group) doesn't also swallow
// `interval`.
//
// `interval` is `Option<u64>` (no `default_value_t`) rather than using
// clap's declarative `requires = "watch"`: empirically, `requires` pointing
// at an arg that's a member of a `#[group(required, multiple = false)]` in
// a sibling flattened struct is silently not enforced (reproduced in
// isolation against clap 4.6.1 outside this codebase) — `--once --interval
// 10` parses successfully instead of erroring. `None` vs `Some` lets
// `cmd_bridge` tell "flag omitted" from "flag explicitly passed" and check
// it against `--watch` itself in plain Rust, which isn't subject to that
// gap.
#[derive(Args)]
struct BridgeMode {
    #[command(flatten)]
    which: BridgeWhich,

    /// Seconds between cycles in --watch mode (default 120, clamped to a
    /// minimum of 10); only valid together with --watch
    #[arg(long)]
    interval: Option<u64>,
}

#[derive(Args)]
#[group(required = true, multiple = false)]
struct BridgeWhich {
    /// Run a single merge cycle (what the service invokes)
    #[arg(long)]
    once: bool,

    /// Run a foreground loop (systemd / debugging)
    #[arg(long)]
    watch: bool,
}

#[derive(Subcommand)]
enum ServiceAction {
    /// Write + load launchd plist / systemd user units
    Install {
        /// Vault name (defaults to the sole configured vault)
        vault: Option<String>,
    },

    /// Unload + remove the platform service units
    Uninstall {
        /// Vault name (defaults to the sole configured vault)
        vault: Option<String>,
    },

    /// Report whether the platform service is loaded
    Status {
        /// Vault name (defaults to the sole configured vault)
        vault: Option<String>,
    },
}

#[derive(Subcommand)]
enum AgentAction {
    /// Scaffold AGENTS.md + CLAUDE.md from embedded template
    Init {
        /// Vault name (defaults to the sole configured vault)
        vault: Option<String>,

        /// Overwrite AGENTS.md/CLAUDE.md if they already exist
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum ObsidianAction {
    /// ob CLI present, logged in, vault bound, binding loads
    Doctor {
        /// Vault name (defaults to the sole configured vault)
        vault: Option<String>,
    },

    /// Manage the `ob sync --continuous` platform service
    Service {
        #[command(subcommand)]
        action: ObsidianServiceAction,
    },
}

#[derive(Subcommand)]
enum ObsidianServiceAction {
    /// KeepAlive/Restart=always service for `ob sync --continuous`
    Install {
        /// Vault name (defaults to the sole configured vault)
        vault: Option<String>,

        /// Pin the node interpreter (writes `<node> <resolved cli.js>`
        /// instead of bare `ob`); use when the service's node differs from
        /// the shebang `ob` would otherwise resolve under launchd/systemd's
        /// minimal environment
        #[arg(long)]
        node: Option<PathBuf>,
    },

    /// Unload + remove the obsidian sync platform service
    Uninstall {
        /// Vault name (defaults to the sole configured vault)
        vault: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();
    if let Err(err) = run(cli) {
        eprintln!("tephra: {err:#}");
        std::process::exit(exit_code_for(&err));
    }
}

/// Exit-code contract (DESIGN.md §Agent awareness): 0 ok, 1 domain error,
/// 2 configuration/usage error. Usage errors are identified by
/// `config::UsageError` anywhere in the error's chain (context wrapping
/// shouldn't hide it).
fn exit_code_for(err: &anyhow::Error) -> i32 {
    let is_usage = err
        .chain()
        .any(|cause| cause.downcast_ref::<config::UsageError>().is_some());
    if is_usage {
        2
    } else {
        1
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Commands::Init {
            name,
            bridge,
            work,
            url,
            branch,
            force,
            yes,
        } => cmd_init(name, bridge, work, url, branch, force, yes),
        Commands::Bridge { mode, vault } => cmd_bridge(mode, vault),
        Commands::Clone { vault } => cmd_clone(vault),
        Commands::Sync { vault, message } => cmd_sync(vault, message),
        Commands::Status { vault, json } => cmd_status(vault, json),
        Commands::Service { action } => match action {
            ServiceAction::Install { vault } => cmd_service_install(vault),
            ServiceAction::Uninstall { vault } => cmd_service_uninstall(vault),
            ServiceAction::Status { vault } => cmd_service_status(vault),
        },
        Commands::Agent { action } => match action {
            AgentAction::Init { vault, force } => cmd_agent_init(vault, force),
        },
        Commands::Obsidian { action } => match action {
            ObsidianAction::Doctor { vault } => cmd_obsidian_doctor(vault),
            ObsidianAction::Service { action } => match action {
                ObsidianServiceAction::Install { vault, node } => {
                    cmd_obsidian_service_install(vault, node)
                }
                ObsidianServiceAction::Uninstall { vault } => cmd_obsidian_service_uninstall(vault),
            },
        },
        Commands::Doctor { vault } => cmd_doctor(vault),
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_init(
    name: Option<String>,
    bridge: Option<String>,
    work: Option<String>,
    url: Option<String>,
    branch: Option<String>,
    force: bool,
    yes: bool,
) -> anyhow::Result<()> {
    init::run(init::InitArgs {
        name,
        bridge,
        work,
        url,
        branch,
        force,
        yes,
    })
}

fn cmd_bridge(mode: BridgeMode, vault: Option<String>) -> anyhow::Result<()> {
    if mode.interval.is_some() && !mode.which.watch {
        return Err(config::UsageError(
            "--interval is only valid together with --watch".to_string(),
        )
        .into());
    }
    let cfg = config::load()?;
    let resolved = config::resolve_vault(&cfg, vault.as_deref())?;
    if mode.which.watch {
        let interval = mode.interval.unwrap_or(bridge::DEFAULT_INTERVAL_SECS);
        bridge::watch(resolved.name, resolved.vault, interval)
    } else {
        bridge::run_once(resolved.name, resolved.vault)
    }
}

fn cmd_clone(vault: Option<String>) -> anyhow::Result<()> {
    let cfg = config::load()?;
    let resolved = config::resolve_vault(&cfg, vault.as_deref())?;
    agent::clone(resolved.vault)
}

fn cmd_sync(vault: Option<String>, message: Option<String>) -> anyhow::Result<()> {
    let cfg = config::load()?;
    let resolved = config::resolve_vault(&cfg, vault.as_deref())?;
    agent::sync(resolved.name, resolved.vault, message.as_deref())
}

fn cmd_status(vault: Option<String>, json: bool) -> anyhow::Result<()> {
    let cfg = config::load()?;
    let resolved = config::resolve_vault(&cfg, vault.as_deref())?;
    agent::status(resolved.name, resolved.vault, json)
}

fn cmd_service_install(vault: Option<String>) -> anyhow::Result<()> {
    let cfg = config::load()?;
    let resolved = config::resolve_vault(&cfg, vault.as_deref())?;
    service::install(resolved.name)
}

fn cmd_service_uninstall(vault: Option<String>) -> anyhow::Result<()> {
    let cfg = config::load()?;
    let resolved = config::resolve_vault(&cfg, vault.as_deref())?;
    service::uninstall(resolved.name)
}

fn cmd_service_status(vault: Option<String>) -> anyhow::Result<()> {
    let cfg = config::load()?;
    let resolved = config::resolve_vault(&cfg, vault.as_deref())?;
    service::status(resolved.name)
}

fn cmd_agent_init(vault: Option<String>, force: bool) -> anyhow::Result<()> {
    let cfg = config::load()?;
    let resolved = config::resolve_vault(&cfg, vault.as_deref())?;
    agent::init(resolved.name, resolved.vault, force)
}

fn cmd_obsidian_doctor(vault: Option<String>) -> anyhow::Result<()> {
    let cfg = config::load()?;
    let resolved = config::resolve_vault(&cfg, vault.as_deref())?;
    obsidian::doctor(resolved.name, resolved.vault)
}

fn cmd_obsidian_service_install(
    vault: Option<String>,
    node: Option<PathBuf>,
) -> anyhow::Result<()> {
    let cfg = config::load()?;
    let resolved = config::resolve_vault(&cfg, vault.as_deref())?;
    obsidian::service_install(resolved.name, resolved.vault, node.as_deref())
}

fn cmd_obsidian_service_uninstall(vault: Option<String>) -> anyhow::Result<()> {
    let cfg = config::load()?;
    let resolved = config::resolve_vault(&cfg, vault.as_deref())?;
    obsidian::service_uninstall(resolved.name)
}

fn cmd_doctor(vault: Option<String>) -> anyhow::Result<()> {
    let cfg = config::load()?;
    let resolved = config::resolve_vault(&cfg, vault.as_deref())?;
    doctor::doctor(resolved.name, resolved.vault)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    #[test]
    fn plain_error_exits_1() {
        let err = anyhow::anyhow!("boom");
        assert_eq!(exit_code_for(&err), 1);
    }

    #[test]
    fn usage_error_exits_2() {
        let err: anyhow::Error = config::UsageError("bad config".to_string()).into();
        assert_eq!(exit_code_for(&err), 2);
    }

    #[test]
    fn usage_error_wrapped_in_context_still_exits_2() {
        let err: anyhow::Error =
            Err::<(), anyhow::Error>(config::UsageError("bad config".to_string()).into())
                .context("while loading config")
                .unwrap_err();
        assert_eq!(exit_code_for(&err), 2);
    }
}
