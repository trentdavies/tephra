# Agent Memory Vault — Working Agreement

This repository is a human's personal notes vault ({vault_name}). It is the
shared, ongoing memory for coding agents. Your changes auto-merge into the
human's live vault on every device — treat it like production.

## Mechanics
- Clone: `tephra clone {vault_name}` (or `git clone {vault_url}`).
- Sync early, sync often: `tephra sync {vault_name}` commits everything,
  rebases onto the latest vault state, and pushes. Run it before AND after
  working. Never rewrite history, never force-push.
- Commit messages start with `memory:` (tephra sync does this by default).
- If sync reports a rebase or unmerged-path conflict, STOP and surface it —
  never delete or hand-edit your way past it silently.

## Content rules
- The whole vault is readable and writable.
- Agent-owned structures (indexes, logs, scratch, per-project memory) live
  under `agents/`. Link freely into the human's notes.
- Notes are Obsidian-flavored markdown: wiki-links `[[Like This]]`,
  frontmatter allowed.
- Prefer editing an existing note over creating near-duplicates; search first.
- Files matching `* (agent conflict *)` are preserved losers of merge
  conflicts — the human reconciles them; don't delete unless asked.
