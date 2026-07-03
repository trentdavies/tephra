# tephra v1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship tephra 0.1.0 — a Rust CLI implementing `docs/DESIGN.md`: git bridge daemon + agent commands + self-installing launchd/systemd services + Obsidian pairing.

**Architecture:** Single binary (clap subcommands). Modules: `config`, `gitx` (git subprocess runner), `bridge`, `agent` (clone/sync/status), `service` (launchd/systemd), `obsidian`, `doctor`, `notify`. All git operations shell out to system git. Integration tests drive the compiled binary against temp bare-repo fixtures.

**Tech Stack:** Rust 2021, clap (derive), serde+toml, anyhow, dirs, tempfile+assert_cmd+predicates (dev). No libgit2, no async runtime.

**Porting contract:** `docs/reference/prototype/{memory-bridge.sh,mem.sh,test-harness.sh}` are the drilled, production-verified semantics. When this plan says "port," the bash file is the byte-level behavioral spec — ordering of operations included (the merge-abort-before-commit ordering is load-bearing; see DESIGN.md).

**Commit rule:** plain `git commit -m` messages, conventional-commits style (`feat:`, `fix:`, `test:`, `docs:`, `chore:`). **No AI attribution/Co-Authored-By trailers, ever.**

**Repo:** `~/dev/wip/tephra`, branch `main` (pre-0.1 — direct commits to main are fine; no force-push).

**Verification-as-artifact rule:** anything verified during development must be committed as a repeatable check — unit test, integration test, golden file, or e2e assertion. Manual-only verification is a plan violation. The e2e layer (Task 12) senses system state exclusively through tephra's own `status --json` / `doctor` output plus git plumbing — the CLI's observability surface IS the sensor contract, and e2e failures that require richer sensing mean the CLI needs a better sensor, not the test a workaround.

---

### Task 1: Cargo scaffold + CI

**Files:** `Cargo.toml`, `src/main.rs` (clap skeleton: all subcommands declared, each returning "not implemented" error, exit 2), `rustfmt.toml` (default, empty ok), `.github/workflows/ci.yml` (macos-latest + ubuntu-latest: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`), `CHANGELOG.md` (keep-a-changelog header, Unreleased section).

- [ ] `cargo init --name tephra`; deps: `clap = { version = "4", features = ["derive"] }`, `serde = { version = "1", features = ["derive"] }`, `toml = "0.8"`, `anyhow = "1"`, `dirs = "5"`; dev-deps: `assert_cmd = "2"`, `predicates = "3"`, `tempfile = "3"`.
- [ ] Clap skeleton with the exact command surface from DESIGN.md §Command surface (including flags: `bridge --once/--watch`, `sync -m`, `status --json`, `service install|uninstall|status`, `agent init`, `obsidian doctor|service install`, `doctor`). `--version` works.
- [ ] Exit-code contract in `main`: Ok=0, domain error=1, usage/config error=2 (clap's own usage errors already exit 2).
- [ ] `cargo build && cargo clippy --all-targets -- -D warnings && cargo fmt --check` green.
- [ ] Commit `chore: cargo scaffold, CLI skeleton, CI`.

### Task 2: config module

**Files:** `src/config.rs`, unit tests inline.

- [ ] Schema per DESIGN.md §Configuration: `HashMap<String, Vault>` under `[vaults.*]`; `Vault { bridge: PathBuf, work: PathBuf, url: String, branch: String (default "main") }`. Tilde-expansion on paths.
- [ ] Load order: `$TEPHRA_CONFIG` file override, else `~/.config/tephra/config.toml` (via `dirs`, but honor `$XDG_CONFIG_HOME` on both platforms — resolve manually: `$XDG_CONFIG_HOME` if set, else `~/.config`).
- [ ] `resolve_vault(name: Option<&str>) -> Result<(String, Vault)>`: explicit name → lookup or error listing known vaults; None → sole vault, else error listing choices (exit 2 path).
- [ ] Tests: parse minimal + full config; default branch; tilde expansion; resolve_vault all three branches; missing file error message names the expected path.
- [ ] Commit `feat: config loading and vault resolution`.

### Task 3: gitx runner + test fixtures

**Files:** `src/gitx.rs`, `tests/common/mod.rs` (fixture builder).

- [ ] `gitx::run(dir, args) -> Result<Output>` and `run_ok` variant; captures stdout/stderr; error context includes the full command and stderr. `gitx::status_porcelain(dir) -> Result<String>`, `gitx::upstream(dir, branch) -> Result<(remote, ref)>` (via `git rev-parse --abbrev-ref branch@{upstream}`), `gitx::conflicted_paths(dir) -> Result<Vec<PathBuf>>` using `git diff --name-only -z --diff-filter=U` split on NUL.
- [ ] Fixture builder (ported from test-harness.sh setup): temp dir with bare `remote.git`, `bridge-<name>` clone, `agent` clone, seeded `Home.md`; sets fixture env on every git call (`GIT_AUTHOR_*`, `GIT_COMMITTER_*`, `GIT_CONFIG_COUNT=2` disabling `commit.gpgsign` and `user.useConfigOnly`) so tests are host-gitconfig-independent; writes a tephra config.toml pointing at the fixture and returns the env for invoking the binary (`TEPHRA_CONFIG`).
- [ ] Tests: run captures stderr on failure; conflicted_paths handles `Café ☕.md`; upstream detection.
- [ ] Commit `feat: git subprocess runner and test fixtures`.

### Task 4: bridge --once (the core port)

**Files:** `src/bridge.rs`, `src/notify.rs`, `tests/bridge.rs`.

- [ ] Port `docs/reference/prototype/memory-bridge.sh` step-for-step per DESIGN.md §Bridge cycle semantics. Differences from bash, by design: remote/branch come from config + `gitx::upstream` (no remote-name assumption; if the branch has no upstream, fall back to the sole remote of the clone, else error); lock dir `.git/tephra-bridge.lock` (mkdir-based, stale >30 min); failure counter `.git/tephra-bridge.failcount`; notification via `notify.rs` (osascript / notify-send / no-op, threshold 15).
- [ ] Conflict copies: `<stem> (agent conflict YYYY-MM-DD).md` for `.md`, `<name>.agent-conflict-YYYY-MM-DD` otherwise — byte-compatible with prototype naming.
- [ ] Integration tests: port ALL 12 harness assertions from `test-harness.sh` (human propagation; agent merge; conflict human-wins + copy exists + copy content; unicode conflict both assertions; offline exit-0 + committed + failcount; recovery clears failcount + queued commit reaches remote), plus: stale-MERGE_HEAD recovery (plant a conflicted merge state, run once, assert no conflict markers in HEAD), and lock exclusion (lock dir present → second run exits 0 without acting).
- [ ] Commit `feat: bridge merge cycle with conflict preservation`.

### Task 5: agent commands — clone, sync, status

**Files:** `src/agent.rs`, `tests/agent.rs`.

- [ ] `clone`: idempotent ("already cloned" exit 0), parent dirs created, clones `vault.url` → `vault.work`.
- [ ] `sync`: port `mem.sh` semantics — commit-all (message from `-m`, default `memory: agent update`), `pull --rebase --autostash`; on rebase conflict: `git rebase --abort`, stderr message naming the dir, exit 1; push with one bounded pull+retry.
- [ ] `status [--json]`: work clone (branch, ahead/behind, dirty count), bridge (same + failcount + lock present), last bridge commit subject, service loaded state (best-effort: launchctl/systemctl query, "unknown" off-platform). `--json`: serde_json — add dep `serde_json = "1"`.
- [ ] Tests: clone idempotency; sync clean/dirty/no-op; the wedge drill (conflicting histories → sync exits 1, tree clean, no rebase in progress, second sync identical, no marker commits — port of the mem.sh review drill); push-race retry (advance remote between operations via the fixture's second clone); status --json parses and has stable keys.
- [ ] Commit `feat: agent clone/sync/status`.

### Task 6: bridge --watch

**Files:** `src/bridge.rs` (extend).

- [ ] Loop: run cycle, sleep 120 s (`--interval <secs>` flag, default 120, min 10), SIGINT/SIGTERM exit cleanly releasing the lock (`ctrlc = "3"` dep or manual signal handling via `signal-hook`; choose `ctrlc`, simplest).
- [ ] Test: `--watch --interval 10` under `assert_cmd` with a kill after ~2 cycles — assert ≥2 cycles logged and lock dir absent after exit. (Time-based; keep tolerant.)
- [ ] Commit `feat: bridge watch mode`.

### Task 7: service install/uninstall/status

**Files:** `src/service.rs`, `tests/service.rs` (golden files under `tests/golden/`).

- [ ] Unit-file generation from templates embedded via `format!`: launchd plist (label `com.tephra.<vault>`, ProgramArguments `[current_exe, "bridge", "--once", vault]`, StartInterval 120, logs `~/Library/Logs/tephra-<vault>.log`, PATH env including `/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin`) and systemd user pair (`tephra-<vault>.service` Type=oneshot + `.timer` OnUnitActiveSec=2min, Persistent=true).
- [ ] `install`: write file(s), then platform load — macOS `launchctl bootout` (ignore failure) + `bootstrap` with 3-attempt retry; Linux `systemctl --user daemon-reload && enable --now tephra-<vault>.timer`. `uninstall`: bootout / `disable --now` + remove files. `status`: query + report, exit 0 loaded / 1 not.
- [ ] Golden tests for both generated formats (normalize the exe path and $HOME in comparison). Runtime install/uninstall/status test gated `#[cfg(target_os = ...)]` + env `TEPHRA_TEST_SERVICES=1` (CI skips; documented in test comment).
- [ ] Commit `feat: self-installing launchd/systemd services`.

### Task 8: agent init scaffolding

**Files:** `src/agent.rs` (extend), `templates/AGENTS.md` (embedded via `include_str!`), `tests/agent_init.rs`.

- [ ] Template: generalized from the prototype vault's AGENTS.md — mechanics (`tephra clone/sync`), commit prefix `memory:`, never rewrite history, conflict-copy semantics ("files matching `* (agent conflict *)` are preserved merge losers — reconcile, don't delete unprompted"), agent-owned subtree convention (`agents/`), placeholder `{vault_url}` filled from config.
- [ ] `agent init [VAULT]`: writes `AGENTS.md` + byte-identical `CLAUDE.md` at the work-clone root; refuses to overwrite unless `--force`; reminds to commit via `tephra sync`.
- [ ] Tests: scaffold content contains url; identical files; no-overwrite without --force; --force overwrites.
- [ ] Commit `feat: agent init scaffolding`.

### Task 9: obsidian pairing

**Files:** `src/obsidian.rs`, `tests/obsidian.rs` (doctor logic tests with fake `ob` on PATH).

- [ ] `obsidian doctor [VAULT]`: checks, each with pass/warn/fail line and remediation text: `ob` on PATH (+ version); `ob login` status (parse `ob login` output — it prints status when logged in); vault bound (`ob sync-status --path <bridge>` exit code); **native-binding smoke**: run `ob sync-list-local` (or cheapest ob command touching sqlite) and grep stderr for `ERR_DLOPEN_FAILED`/`NODE_MODULE_VERSION` → fail with "reinstall obsidian-headless under the node your service uses; check npm approve-scripts". Exit 1 if any check fails.
- [ ] `obsidian service install [VAULT]`: platform unit running `ob sync --continuous` in the bridge (macOS KeepAlive + ThrottleInterval 30; Linux `Restart=always`, `RestartSec=30`); `--node <path>` flag to pin the interpreter (writes `<node> <resolved ob cli.js path>` instead of bare `ob` — resolve via `command -v ob` + readlink). Golden tests for both formats.
- [ ] Tests: doctor with fake `ob` shims (happy, not-logged-in, dlopen-failure stderr); golden units incl. `--node` pin.
- [ ] Commit `feat: obsidian doctor and sync service`.

### Task 10: doctor + init

**Files:** `src/doctor.rs`, `src/init.rs`, `tests/doctor.rs`.

- [ ] `tephra init`: interactive prompts (vault name, bridge path, work path, url, branch) with flag overrides for scripting (`--name --bridge --work --url --branch --yes`); writes/merges config.toml (refuses dup name without `--force`); prints next steps (clone bridge, service install, agent init).
- [ ] `tephra doctor [VAULT]`: git ≥ 2.36; config parses; bridge exists + is a git repo + upstream resolves; `git -C bridge var GIT_COMMITTER_IDENT` resolves (catches the useConfigOnly/identity gap); remote reachable (`git ls-remote --heads`, 10 s timeout via `GIT_SSH_COMMAND=ssh -o ConnectTimeout=10`); stale lock/failcount report. Each check pass/warn/fail + remediation; exit 1 on any fail.
- [ ] Tests: init non-interactive writes valid config, merge/dup behavior; doctor against fixture (healthy) and with upstream removed (fails with named check).
- [ ] Commit `feat: init and doctor`.

### Task 12: docker clean-room e2e

**Files:** `e2e/Dockerfile.remote` (sshd + git, a throwaway "tailgit"), `e2e/Dockerfile.tephra` (rust build stage → runtime with git+ssh client), `e2e/compose.yml`, `e2e/scenario.sh` (orchestrator), `e2e/human-sim.sh` (the external-synchronizer simulator), `.github/workflows/e2e.yml` (ubuntu only; docker unavailable on macOS runners).

The point: prove the whole system converges in a clean room no host state can contaminate — git-over-ssh remote like production, a **human/sync simulator** writing and deleting files in the bridge directory out-of-band exactly the way Obsidian Sync does (tephra can't tell the difference), and an agent container pushing through `tephra sync`.

- [ ] `remote` service: openssh-server, single `git` user with an authorized key baked in (throwaway keypair generated at build — clean room, no real keys), bare repo `vault.git` created on init.
- [ ] `tephra` service: builds the workspace binary (multi-stage), configures `~/.config/tephra/config.toml` for vault `e2e` (url `git@remote:vault.git`), clones bridge + work, runs `tephra bridge --watch --interval 5`.
- [ ] `e2e/human-sim.sh`: loop writing timestamped edits to existing notes, creating notes (incl. unicode `Café ☕.md`), and deleting notes directly in the bridge dir at randomized 1–7 s intervals — the "external synchronizer" stand-in.
- [ ] `e2e/scenario.sh` phases, each with explicit sensed assertions (`tephra status --json` + `git ls-remote`/`git log` on the remote):
  1. **Convergence**: human-sim + agent (`tephra sync` loop in the container writing `agents/*.md`) run concurrently 60 s; stop both; settle 2 cycles; assert remote HEAD == bridge HEAD, zero dirty files, both streams' commits present (`vault: human edits` and `memory:` subjects interleaved).
  2. **Conflict**: scripted same-file simultaneous edit; assert human content in place + `* (agent conflict *)` copy on the remote with agent content.
  3. **Outage**: `docker compose pause remote` (or network disconnect) during active human-sim; assert failcount rises via bridge status sensing, commits queue; unpause; assert failcount cleared and queued commits reach the remote.
  4. **Crash**: `kill -9` the watch process mid-activity; restart; assert no conflict markers anywhere in HEAD (`git grep -l '<<<<<<<'` empty), lock recovered, convergence resumes.
- [ ] Exit non-zero on any assertion failure with a labeled dump (`status --json`, last 30 log lines).
- [ ] `.github/workflows/e2e.yml`: build + `docker compose run scenario` on push/PR, ubuntu-latest.
- [ ] Local run documented in README (Task 11 picks it up): `cd e2e && docker compose up --build --abort-on-container-exit`.
- [ ] Commit `test: docker clean-room e2e with sync simulator`.

### Task 11: docs, polish, 0.1.0

**Files:** `README.md` (full), `CHANGELOG.md`, `Cargo.toml` metadata.

- [ ] README: what/why, the diagram, install (`cargo install tephra`), 5-minute quickstart (init → clone bridge manually → service install → agent init), Obsidian Sync pairing guide (ob login/sync-setup/sync-config walk-through, doctor, service install, the E2E-password caveat), agent contract section, conflict-policy explanation, troubleshooting (doctor first, log locations per platform).
- [ ] Cargo.toml: description, license `MIT OR Apache-2.0`, repository, keywords (`obsidian`, `agents`, `notes`, `sync`, `git`), categories (`command-line-utilities`); CHANGELOG 0.1.0 entry; version 0.1.0.
- [ ] Full CI green locally: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`.
- [ ] Commit `docs: README, changelog; chore: 0.1.0 metadata`. (Publishing to crates.io is Trent's call, not part of this plan.)

---

## Self-review notes

- Every DESIGN.md section maps to a task (surface→1, config→2, git-shell-out→3, bridge semantics→4/6, agent→5/8, services→7, obsidian→9, doctor/init→10, OSS checklist→11).
- The 12 prototype assertions all land in Task 4/5 tests; the two live-drill findings (remote-name footgun, rebase wedge) are explicit test cases.
- Platform-runtime service tests can't run in CI both ways — golden files cover generation; runtime paths gated behind `TEPHRA_TEST_SERVICES=1` and exercised on the dev Mac during Task 7.
