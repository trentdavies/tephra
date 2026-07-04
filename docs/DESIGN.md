# tephra — Design (v1)

**Status:** approved 2026-07-03. Extracted from a live, drill-tested bash
prototype (git bridge between an Obsidian-Sync'd vault and coding agents);
this document is the v1 contract for the Rust rewrite.

## What it is

tephra turns any cloud-synced notes folder into versioned, auto-merged,
agent-writable memory. Humans keep editing through their sync app (Obsidian
Sync, Syncthing, iCloud, Dropbox — tephra doesn't care); agents read and
write through git. A **bridge checkout** — a second copy of the folder that
is both a git working tree and a sync client — is the only place the two
streams meet. A small daemon merges them continuously:

```
 phone / laptop apps ──── sync service ────┐
                                           ▼
                                    bridge checkout   ← tephra bridge (daemon)
                                           ▲
                                           │ push / fetch
                                    git remote (authoritative)
                                           ▲
                                           │ clone / pull / push
                                    agent clones (tephra clone / sync)
```

Every human edit becomes a commit. Every agent change is versioned before it
reaches a human device. Conflicts never block and never lose data.

## Command surface

```
tephra init                        register a vault (writes config.toml)
tephra bridge --once [VAULT]       one merge cycle (what the service invokes)
tephra bridge --watch [VAULT]      foreground loop (for debugging or custom supervisors;
                                   `service install` uses --once on a timer on both platforms)
tephra clone [VAULT]               clone the vault repo to the work path
tephra sync [VAULT] [-m MSG]       commit-all → pull --rebase → push (agent entry point)
tephra status [VAULT] [--json]     work clone + bridge + service + remote state
tephra service install [VAULT]     write + load launchd plist / systemd user units
tephra service uninstall [VAULT]
tephra service status [VAULT]
tephra agent init [VAULT]          scaffold AGENTS.md + CLAUDE.md from embedded template
tephra obsidian doctor [VAULT]     ob CLI present, logged in, vault bound, binding loads
tephra obsidian service install [VAULT]   KeepAlive service for `ob sync --continuous`
tephra doctor [VAULT]              identity resolves, remote reachable, stale locks, …
```

`VAULT` defaults to the sole configured vault, or errors listing choices.

## Configuration

`~/.config/tephra/config.toml` (XDG on both platforms; `TEPHRA_CONFIG` overrides):

```toml
[vaults.personal]
bridge = "~/dev/memory/bridge-personal"   # bridge checkout (daemon operates here)
work   = "~/dev/memory/work-personal"     # default agent clone location
url    = "tailgit:obsidian-personal"      # used by `tephra clone` and bridge re-clone advice
branch = "main"
```

No remote-name assumptions: the bridge pushes/fetches the upstream of the
configured branch, whatever the remote is called. (The bash prototype's
hardcoded remote name was a real-world footgun; the port kills it.)

## Bridge cycle semantics (the battle-tested part)

Exact port of the hardened prototype, in order:

1. **Abort any in-progress merge first** (crashed prior run). Ordering is
   load-bearing: committing a dirty tree while MERGE_HEAD exists bakes
   conflict markers into notes as "human edits".
2. Commit anything the sync app delivered: `vault: human edits`.
3. Fetch upstream; on failure increment a failure counter, notify the desktop
   at threshold (~30 min), exit 0 — commits keep queuing locally.
4. Merge. On conflict, per file: keep the human version in place
   (`checkout --ours`), write the agent version to
   `<name> (agent conflict YYYY-MM-DD).<ext>`, stage both. Conflicted-path
   iteration is NUL-delimited (unicode filenames are routine in note vaults).
   If the resolution commit fails: abort the merge, log, skip the push.
5. Push, one bounded retry after re-fetch/merge. Never `--force`.
6. Single-instance lock (lock dir in `.git/`), stale after 30 min.

`tephra sync` (agent side) mirrors the prototype's `mem sync`: commit-all,
`pull --rebase --autostash`; a conflicted rebase is always aborted with a
clear message and exit 1 — a wedged agent clone that silently commits
conflict markers is the worst failure mode this tool has.

## Core decision: shell out to `git`

tephra invokes the system `git` binary (no libgit2/git2 crate). Users'
identity (includeIf), SSH config, commit signing, and credential helpers all
live in git's own config machinery, which libgit2 reimplements incompletely.
Shelling out preserves exactly the semantics the prototype proved. git ≥ 2.36
required (checked by `tephra doctor`).

## Service management

`tephra service install` detects the platform and writes units that invoke
`std::env::current_exe()` (no PATH guessing):

- **macOS**: `~/Library/LaunchAgents/com.tephra.<vault>.plist`, StartInterval
  120 s, logs to `~/Library/Logs/tephra-<vault>.log`; loaded via
  `launchctl bootout` (tolerated failure) + `bootstrap` with bounded retry
  (bootout→bootstrap of a live service races).
- **Linux**: `~/.config/systemd/user/tephra-<vault>.{service,timer}` (oneshot
  + 2 min timer), enabled via `systemctl --user enable --now`; journal logging.

`tephra obsidian service install` writes the sibling KeepAlive/`Restart=always`
unit running `ob sync --continuous` in the bridge, with an explicit node
interpreter path when the doctor detects the ABI mismatch case (see below).

Notifications: `osascript` on macOS, `notify-send` on Linux, silently skipped
if absent.

## Obsidian pairing (`tephra obsidian`)

The optional half for Obsidian Sync users, wrapping the official
`obsidian-headless` beta:

- `doctor`: ob installed and logged in; bridge bound to a remote vault;
  **native-module smoke test** (spawn ob trivially — catches the
  NODE_MODULE_VERSION drift that bit the prototype when brew upgraded node);
  warns if npm's script-blocking left a stale prebuilt binding.
- `service install`: as above. The interactive steps (`ob login`,
  `ob sync-setup` with the E2E password) remain the user's — doctor names
  them precisely instead of automating credentials.

## Agent awareness

- `--json` on `status` (and `sync` result summary); exit codes are a
  contract: 0 ok, 1 action failed cleanly (e.g. rebase conflict, aborted),
  2 configuration/usage error.
- `tephra agent init` scaffolds `AGENTS.md` + identical `CLAUDE.md` into the
  vault repo from an embedded template: clone/pull/push mechanics, commit
  prefix convention, conflict-copy semantics ("files matching
  `* (agent conflict *)` are preserved merge losers — reconcile, don't
  delete unprompted"), where agent-owned structures live.

## Testing

Integration tests (`assert_cmd` + tempdir fixtures: bare remote + bridge +
agent clones, fixture git identity) porting all 12 prototype harness
assertions: both propagation directions, conflict policy incl. content and
unicode filenames, offline queueing, recovery, plus rebase-conflict abort on
the agent path and lock behavior. Unit tests for config parsing and unit-file
generation (golden files). CI: GitHub Actions, macOS + Ubuntu, fmt +
clippy `-D warnings` + tests.

## OSS checklist (v1 ships with)

Dual license MIT/Apache-2.0 · README (diagram, 5-minute quickstart, Obsidian
pairing guide) · CHANGELOG (keep-a-changelog) · `cargo install tephra` ·
rustfmt/clippy clean · semver 0.1.0 · docs/DESIGN.md (this file).

## Out of scope for v1

MCP server · Windows services · non-git backends · binary releases/cargo-dist
(follow-up) · migrating the author's chezmoi setup off the bash prototype
(separate follow-up once tephra reaches parity).
