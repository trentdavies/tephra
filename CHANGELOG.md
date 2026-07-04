# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-07-03

Initial release: a git bridge daemon that merges a cloud-synced notes vault
with coding-agent clones, plus the commands and services around it.

### Added

- `tephra init` — register a vault (interactive prompts or `--yes` with
  flags), writing/merging `~/.config/tephra/config.toml`.
- `tephra bridge --once` / `--watch` — the merge-cycle daemon: abort a
  stale merge, commit human edits (`vault: human edits`), fetch, merge with
  human-wins conflict resolution plus dated agent-conflict copies, push
  with one bounded retry, single-instance lock, failure counter, and a
  desktop notification after sustained remote failure.
- `tephra clone` / `tephra sync [-m MSG]` / `tephra status [--json]` — the
  agent-facing entry points: idempotent clone, commit-all → pull --rebase
  --autostash → push (aborting cleanly on a wedged rebase), and a status
  report of the work clone, bridge, and service.
- `tephra service install|uninstall|status` — self-installing launchd
  plist / systemd user service+timer that runs `bridge --once` on a
  2-minute cycle.
- `tephra agent init` — scaffolds `AGENTS.md` + identical `CLAUDE.md` into
  the work clone from an embedded template (mechanics, commit convention,
  conflict-copy handling).
- `tephra obsidian doctor` / `tephra obsidian service install|uninstall` —
  Obsidian Sync pairing: `ob` CLI presence/login/native-binding checks, and
  a KeepAlive/`Restart=always` service running `ob sync --continuous`
  (with `--node` pinning for node-ABI drift).
- `tephra doctor` — git version, bridge repo/identity/upstream/remote
  reachability, and stale lock/failcount/heartbeat reporting.
- Exit-code contract (0 ok / 1 domain error / 2 usage error) for scripting
  and agent consumption.
- Docker clean-room e2e (convergence, conflict, outage, and crash-recovery
  phases against a git-over-ssh remote) and GitHub Actions CI (fmt,
  clippy, tests, e2e).
