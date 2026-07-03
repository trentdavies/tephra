use clap::{Args, Parser, Subcommand};

use tephra::config;

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
    Init,

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

#[derive(Args)]
#[group(required = true, multiple = false)]
struct BridgeMode {
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
    /// KeepAlive service for `ob sync --continuous`
    Install {
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
        Commands::Init => cmd_init(),
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
            AgentAction::Init { vault } => cmd_agent_init(vault),
        },
        Commands::Obsidian { action } => match action {
            ObsidianAction::Doctor { vault } => cmd_obsidian_doctor(vault),
            ObsidianAction::Service { action } => match action {
                ObsidianServiceAction::Install { vault } => cmd_obsidian_service_install(vault),
            },
        },
        Commands::Doctor { vault } => cmd_doctor(vault),
    }
}

fn cmd_init() -> anyhow::Result<()> {
    anyhow::bail!("not implemented")
}

fn cmd_bridge(_mode: BridgeMode, _vault: Option<String>) -> anyhow::Result<()> {
    anyhow::bail!("not implemented")
}

fn cmd_clone(_vault: Option<String>) -> anyhow::Result<()> {
    anyhow::bail!("not implemented")
}

fn cmd_sync(_vault: Option<String>, _message: Option<String>) -> anyhow::Result<()> {
    anyhow::bail!("not implemented")
}

fn cmd_status(_vault: Option<String>, _json: bool) -> anyhow::Result<()> {
    anyhow::bail!("not implemented")
}

fn cmd_service_install(_vault: Option<String>) -> anyhow::Result<()> {
    anyhow::bail!("not implemented")
}

fn cmd_service_uninstall(_vault: Option<String>) -> anyhow::Result<()> {
    anyhow::bail!("not implemented")
}

fn cmd_service_status(_vault: Option<String>) -> anyhow::Result<()> {
    anyhow::bail!("not implemented")
}

fn cmd_agent_init(_vault: Option<String>) -> anyhow::Result<()> {
    anyhow::bail!("not implemented")
}

fn cmd_obsidian_doctor(_vault: Option<String>) -> anyhow::Result<()> {
    anyhow::bail!("not implemented")
}

fn cmd_obsidian_service_install(_vault: Option<String>) -> anyhow::Result<()> {
    anyhow::bail!("not implemented")
}

fn cmd_doctor(_vault: Option<String>) -> anyhow::Result<()> {
    anyhow::bail!("not implemented")
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
