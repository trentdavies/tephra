//! Shared integration-test fixture builder.
//!
//! Ported from `docs/reference/prototype/test-harness.sh`'s setup: a bare
//! "remote" repo, seeded with a `Home.md` note, plus bridge and agent
//! clones of it, plus a tephra `config.toml` wired to point at them.
//!
//! Not every helper here is exercised by every test file that includes this
//! module (e.g. `tephra_cmd` isn't needed until the bridge/agent commands
//! land in later tasks), so unused-function warnings are silenced until
//! then, matching the pattern `src/config.rs`/`src/gitx.rs` already use.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::{tempdir, TempDir};

/// Git identity/config env vars applied to every fixture git invocation, so
/// tests are independent of (and can't corrupt) the host's `~/.gitconfig`:
/// a fixed author/committer identity, and `GIT_CONFIG_COUNT`-based
/// overrides disabling commit signing and `user.useConfigOnly` (either of
/// which could otherwise make commits behave differently, or fail outright,
/// on a locked-down dev machine).
const GIT_ENV: &[(&str, &str)] = &[
    ("GIT_AUTHOR_NAME", "test"),
    ("GIT_AUTHOR_EMAIL", "test@example.com"),
    ("GIT_COMMITTER_NAME", "test"),
    ("GIT_COMMITTER_EMAIL", "test@example.com"),
    ("GIT_CONFIG_COUNT", "2"),
    ("GIT_CONFIG_KEY_0", "commit.gpgsign"),
    ("GIT_CONFIG_VALUE_0", "false"),
    ("GIT_CONFIG_KEY_1", "user.useConfigOnly"),
    ("GIT_CONFIG_VALUE_1", "false"),
];

fn run_git(dir: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .envs(GIT_ENV.iter().copied())
        .output()
        .expect("failed to spawn git")
}

/// Like `run_git`, but panics with the command, dir, and stderr if git
/// exits nonzero. Used for fixture setup steps that must succeed.
fn run_git_ok(dir: &Path, args: &[&str]) -> Output {
    let output = run_git(dir, args);
    assert!(
        output.status.success(),
        "git -C {} {} failed: {}",
        dir.display(),
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// A self-contained test fixture: a bare "remote" repo plus bridge and
/// agent checkouts, seeded with a `Home.md` note, and a generated
/// `config.toml` naming the vault.
pub struct Fixture {
    /// Keeps the tempdir (and everything under it) alive for the fixture's
    /// lifetime; the whole tree is removed when the fixture is dropped.
    pub root: TempDir,
    /// Bare "remote" repo path (`<root>/remote.git`).
    pub remote: PathBuf,
    /// Bridge checkout path (`<root>/bridge-<name>`) — the daemon's
    /// working tree.
    pub bridge: PathBuf,
    /// Agent checkout path (`<root>/agent`) — also the vault's `work` path
    /// in the generated config.
    pub agent: PathBuf,
    /// Path to the generated `config.toml` (set as `TEPHRA_CONFIG` by
    /// [`Fixture::tephra_cmd`]).
    pub config: PathBuf,
    /// The configured vault name, as passed to [`Fixture::new`].
    pub name: String,
}

impl Fixture {
    /// Build a fixture for a vault named `vault_name`: a bare remote seeded
    /// with `Home.md` on `main`, plus bridge and agent clones of it, plus a
    /// `config.toml` registering the vault.
    pub fn new(vault_name: &str) -> Fixture {
        let root = tempdir().expect("create fixture tempdir");
        let remote = root.path().join("remote.git");
        let seed = root.path().join("seed");
        let bridge = root.path().join(format!("bridge-{vault_name}"));
        let agent = root.path().join("agent");

        let remote_str = remote.to_str().expect("remote path is valid UTF-8");
        let seed_str = seed.to_str().expect("seed path is valid UTF-8");
        let bridge_str = bridge.to_str().expect("bridge path is valid UTF-8");
        let agent_str = agent.to_str().expect("agent path is valid UTF-8");

        run_git_ok(root.path(), &["init", "--quiet", "--bare", remote_str]);

        run_git_ok(root.path(), &["clone", "--quiet", remote_str, seed_str]);
        std::fs::write(seed.join("Home.md"), "# Home\n").expect("write seed Home.md");
        run_git_ok(&seed, &["add", "-A"]);
        run_git_ok(&seed, &["commit", "--quiet", "-m", "init"]);
        run_git_ok(&seed, &["branch", "-M", "main"]);
        run_git_ok(&seed, &["push", "--quiet", "origin", "main"]);

        run_git_ok(root.path(), &["clone", "--quiet", remote_str, bridge_str]);
        run_git_ok(root.path(), &["clone", "--quiet", remote_str, agent_str]);

        let config = root.path().join("config.toml");
        let contents = format!(
            "[vaults.{vault_name}]\n\
             bridge = \"{bridge}\"\n\
             work = \"{work}\"\n\
             url = \"{url}\"\n\
             branch = \"main\"\n",
            vault_name = vault_name,
            bridge = bridge.display(),
            work = agent.display(),
            url = remote.display(),
        );
        std::fs::write(&config, contents).expect("write fixture config.toml");

        Fixture {
            root,
            remote,
            bridge,
            agent,
            config,
            name: vault_name.to_string(),
        }
    }

    /// Run `git -C <dir> <args>` with the fixture's isolation env, without
    /// checking the exit status — mirrors `gitx::run`'s "never errors on
    /// nonzero exit" contract, since some fixture-manipulation steps (e.g.
    /// provoking a merge conflict) expect failure.
    pub fn git(&self, dir: &Path, args: &[&str]) -> Output {
        run_git(dir, args)
    }

    /// A `tephra` binary invocation preconfigured with `TEPHRA_CONFIG`
    /// pointing at this fixture's config, and the same git isolation env
    /// (inherited by the binary's own child `git` processes).
    pub fn tephra_cmd(&self) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::cargo_bin("tephra").expect("find tephra binary");
        cmd.env("TEPHRA_CONFIG", &self.config);
        for (key, value) in GIT_ENV {
            cmd.env(key, value);
        }
        cmd
    }

    /// A `tephra bridge --once <name>` invocation against this fixture.
    pub fn bridge_once(&self) -> assert_cmd::Command {
        let mut cmd = self.tephra_cmd();
        cmd.arg("bridge").arg("--once").arg(&self.name);
        cmd
    }

    /// A raw `std::process::Command` for the `tephra` binary, with the same
    /// `TEPHRA_CONFIG` and git-isolation env as [`Fixture::tephra_cmd`].
    /// `assert_cmd::Command` only offers blocking `.output()`/`.assert()`;
    /// tests that need a live `Child` (to stream output, send it a signal,
    /// or bound how long they wait for exit) spawn this instead.
    pub fn tephra_command(&self) -> Command {
        let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("tephra"));
        cmd.env("TEPHRA_CONFIG", &self.config);
        for (key, value) in GIT_ENV {
            cmd.env(key, value);
        }
        cmd
    }
}
