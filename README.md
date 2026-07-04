# tephra

Layered memory for humans and their agents.

tephra turns any cloud-synced notes folder (Obsidian Sync, Syncthing, iCloud,
Dropbox, …) into versioned, auto-merged, agent-writable memory.

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

You keep editing notes the same way you always have, through whatever sync
app you already use. Coding agents read and write the same notes through
git, in their own clones. A **bridge checkout** — a second copy of the
folder that's simultaneously a git working tree and a sync client — is the
one place the two streams meet, and a small daemon (`tephra bridge`) merges
them continuously. Every human edit becomes a commit; every agent change is
versioned before it reaches a human device. Conflicts never block and never
lose data — the human's version always wins in place, and the agent's
losing version is preserved next to it for you to reconcile on your own
time.

## Install

```
cargo install tephra
```

Not yet published to crates.io. Until the crates.io release:

```
cargo install --git https://github.com/trentdavies/tephra
```

Requirements: git ≥ 2.36, macOS or Linux (no Windows support in v1).

## Five-minute quickstart

1. Register a vault:

   ```
   tephra init
   ```

   Prompts for a name, a bridge checkout path, a work-clone path, a remote
   URL, and a branch (or pass `--name --bridge --work --url --branch --yes`
   to skip the prompts). This writes `~/.config/tephra/config.toml` — it
   doesn't touch git yet.

2. Get the bridge checkout onto disk. If your sync app (Obsidian Sync,
   Syncthing, …) already manages that folder, skip this and just make sure
   it's a git clone of the same remote; otherwise:

   ```
   git clone <url> <bridge>
   ```

   (`<url>` and `<bridge>` are whatever you gave `tephra init` above.) The
   branch you cloned needs an upstream — a plain `git clone` sets one up
   automatically; if you're pointing the bridge at a folder some other way,
   confirm `git branch --set-upstream-to` is set, since the bridge follows
   the branch's upstream rather than assuming a remote name.

3. Install the background service that runs the merge daemon:

   ```
   tephra service install
   ```

4. Clone your agent work copy:

   ```
   tephra clone
   ```

5. Scaffold the agent-facing instructions into it:

   ```
   tephra agent init
   ```

6. Check that everything's healthy:

   ```
   tephra status
   ```

If anything looks off at any point, `tephra doctor` is the first move —
it checks git version, bridge/identity/upstream/remote reachability, and
stale locks before you go digging further.

(All the commands above take a `VAULT` argument; it's optional and defaults
to the sole configured vault. Once you have more than one, name it
explicitly.)

## Obsidian Sync pairing (optional)

tephra doesn't care what syncs the bridge folder — this section only
applies if that's Obsidian Sync. It wraps Obsidian's official
`obsidian-headless` beta CLI (`ob`) so the bridge checkout keeps syncing
even with no Obsidian.app window open.

1. Install `obsidian-headless` (Node 22+): `npm install -g obsidian-headless`
2. Log in: `ob login`
3. Bind the bridge folder to a vault: `cd <bridge> && ob sync-setup --vault <name>`
   — this prompts for the vault's E2E encryption password interactively;
   that's yours to type, tephra never touches it.
4. Set the conflict strategy tephra's merge policy expects:
   `ob sync-config --conflict-strategy merge`
5. Verify: `tephra obsidian doctor` — checks `ob` is on `PATH` and its
   version, that you're logged in, that the bridge is bound, and a
   native-module smoke test (catches the
   `NODE_MODULE_VERSION`/`ERR_DLOPEN_FAILED` drift that happens when
   `brew upgrade node` invalidates a prebuilt `better-sqlite3` binding —
   a real failure mode, not a hypothetical one).
6. Install the sync service: `tephra obsidian service install` — runs
   `ob sync --continuous` in the bridge under a KeepAlive (macOS) /
   `Restart=always` (Linux) unit. If the service's node differs from
   whatever `ob`'s shebang would resolve under launchd/systemd's minimal
   environment, pin it explicitly: `tephra obsidian service install --node <path>`.

## How agents use it

`tephra status --json` is the machine-readable surface for scripting: it emits the work clone's branch/dirty/ahead/behind
counts, the bridge's same plus failcount/lock state, the last bridge
commit, and the platform service's loaded state, as stable JSON keys.

`tephra agent init [VAULT]` scaffolds `AGENTS.md` (and a byte-identical
`CLAUDE.md`) into the work clone — that file is the actual agent contract:
clone/sync mechanics, the `memory:` commit-message convention, never
rewriting history, and how to treat conflict-copy files. Read it there
rather than here; it's generated to match the vault it's dropped into.

Exit codes are a contract, not an accident:

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | A domain action failed cleanly (e.g. a rebase conflict, aborted) |
| 2 | Configuration or usage error (bad flags, unknown vault, missing config) |

## Conflict policy

When the bridge merges and a file conflicts, the human's version wins in
place — the file on disk after the merge is exactly what the sync app last
wrote, never a merge of the two. The agent's losing version isn't
discarded: it's written alongside as `<note> (agent conflict YYYY-MM-DD).md`
(or `<name>.agent-conflict-YYYY-MM-DD` for non-`.md` files). A second
conflict on the same file on the same day gets a uniquified ` (2)`,
` (3)`, … suffix rather than clobbering the first copy. Reconciling is
manual and low-ceremony: read the copy, fold anything worth keeping back
into the real note, delete the copy. Until you delete it, it's just
another file in the vault — nothing else in tephra treats its existence
as blocking.

## Troubleshooting

Run `tephra doctor` first, and `tephra obsidian doctor` too if you're
pairing with Obsidian Sync — both name the specific failing check and its
remediation rather than a bare error.

- **Logs**: `~/Library/Logs/tephra-<vault>.log` on macOS; on Linux, the
  bridge service logs to the systemd user journal
  (`journalctl --user -u tephra-<vault>`). The Obsidian sync service logs
  to `~/Library/Logs/tephra-obsidian-<vault>.log` on macOS, or its own
  systemd user journal unit on Linux.
- **Failcount / heartbeat**: the bridge tracks consecutive remote-fetch
  failures in `.git/tephra-bridge.failcount` inside the bridge checkout
  (visible via `tephra status --json`'s `bridge.failcount`); it's deleted
  on the next successful fetch, so its absence just means "no failures
  right now," not "healthy forever." A completed cycle's outcome and
  timestamp are recorded in a heartbeat file and surfaced as
  `bridge.last_cycle_at` / `bridge.last_cycle_outcome`.
- **Desktop notification**: after 15 consecutive failed cycles (~30
  minutes at the service's default 2-minute interval) the bridge fires one
  desktop notification (`osascript` on macOS, `notify-send` on Linux) that
  the remote's unreachable — commits keep queuing locally in the
  meantime, nothing is lost.

## Development

```
cargo test
```

The full clean-room end-to-end test (git-over-ssh remote, a human/sync
simulator writing directly into the bridge folder, an agent pushing
through `tephra sync`, and chaos phases for outage/crash recovery) runs in
Docker and doesn't touch anything on your machine outside its own
containers:

```
cd e2e && docker compose up --build --abort-on-container-exit --exit-code-from scenario
```

Architecture and the bridge's merge-cycle semantics are documented in
[docs/DESIGN.md](docs/DESIGN.md). The Rust implementation is a port of a
live, drill-tested bash prototype; the original scripts are kept for
reference at
[docs/reference/prototype/](docs/reference/prototype/).

## Known limitations (v1)

- A conflicted filename that isn't valid UTF-8 stalls that bridge's merge
  cycle (data stays safe — the merge aborts and retries — but the bridge
  stops converging until the file is renamed). Unreachable on APFS, possible
  on Linux.
- The bridge's single-instance lock uses a 30-minute staleness window: a
  SIGKILL'd cycle can block subsequent cycles for up to 30 minutes, and a
  cycle running longer than 30 minutes could have its lock stolen.
- `tephra status` never touches the network: `ahead`/`behind` are computed
  against last-known remote-tracking refs, so `behind: 0` means "nothing
  fetched yet," not necessarily "up to date."

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
