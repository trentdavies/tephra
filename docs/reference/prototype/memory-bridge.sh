#!/bin/bash
# memory-bridge <vault> — commit Obsidian-Sync-delivered human edits, merge
# agent pushes from the authoritative remote, push back. Conflict policy:
# human wins in place, agent version preserved as a sibling conflict note.
# Designed to run every ~2 min from launchd; safe to run manually.
set -uo pipefail

VAULT="${1:?usage: memory-bridge <vault>}"
ROOT="${MEMORY_BRIDGE_ROOT:-$HOME/dev/memory}"
REMOTE="${MEMORY_BRIDGE_REMOTE:-tailgit}"
BRANCH="${MEMORY_BRIDGE_BRANCH:-main}"
BRIDGE="$ROOT/bridge-$VAULT"
NOTIFY_AFTER=15   # consecutive remote failures (~30 min at 120s) before notifying

log(){ echo "[memory-bridge/$VAULT] $(date '+%H:%M:%S') $*"; }
cd "$BRIDGE" || { echo "bridge dir missing: $BRIDGE" >&2; exit 1; }

# --- single-instance lock (mkdir is atomic; flock isn't stock on macOS) ----
LOCK=".git/memory-bridge.lockdir"
if ! mkdir "$LOCK" 2>/dev/null; then
  # stale if older than 30 min (crashed run)
  if [ -n "$(find "$LOCK" -maxdepth 0 -mmin +30 2>/dev/null)" ]; then
    rmdir "$LOCK" 2>/dev/null; mkdir "$LOCK" 2>/dev/null || exit 0
  else
    exit 0
  fi
fi
trap 'rmdir "$LOCK" 2>/dev/null' EXIT

FAILCOUNT_FILE=".git/memory-bridge.failcount"
remote_failed(){
  local n=0; [ -f "$FAILCOUNT_FILE" ] && n=$(cat "$FAILCOUNT_FILE")
  n=$((n+1)); echo "$n" > "$FAILCOUNT_FILE"
  log "remote unreachable (attempt $n)"
  if [ "$n" -eq "$NOTIFY_AFTER" ] && command -v osascript >/dev/null 2>&1; then
    osascript -e "display notification \"$REMOTE unreachable; vault commits queuing locally\" with title \"memory-bridge $VAULT\"" || true
  fi
  exit 0
}
remote_ok(){ rm -f "$FAILCOUNT_FILE"; }

# --- 1. abort any half-finished merge from a crashed run -------------------
# Must run BEFORE committing human edits, or leftover conflict markers get
# baked into notes as "vault: human edits" and pushed.
[ -f .git/MERGE_HEAD ] && git merge --abort 2>/dev/null

# --- 2. commit anything Obsidian Sync delivered ----------------------------
if [ -n "$(git status --porcelain)" ]; then
  git add -A
  git commit -q -m "vault: human edits" && log "committed human edits"
fi

# --- 3. fetch + merge agent work -------------------------------------------
git fetch -q "$REMOTE" || remote_failed
remote_ok
if ! git merge -q --no-edit "$REMOTE/$BRANCH" 2>/dev/null; then
  stamp="$(date +%Y-%m-%d)"
  conflicts=0
  while IFS= read -r -d '' f; do
    conflicts=$((conflicts+1))
    case "$f" in
      *.md) copy="${f%.md} (agent conflict $stamp).md" ;;
      *)    copy="$f.agent-conflict-$stamp" ;;
    esac
    # stage 3 = "theirs" = the agent's version from the remote branch
    if git show ":3:$f" > "$copy" 2>/dev/null; then :; else rm -f "$copy"; fi
    git checkout --ours -- "$f" 2>/dev/null || git rm -q -- "$f" 2>/dev/null
    git add -- "$f" 2>/dev/null; [ -f "$copy" ] && git add -- "$copy"
  done < <(git diff --name-only -z --diff-filter=U)
  if ! commit_err="$(git commit -q --no-edit -m "merge: agent changes ($conflicts conflict(s) preserved alongside)" 2>&1)"; then
    git merge --abort 2>/dev/null
    log "conflict merge FAILED: $commit_err"
    exit 0   # tree restored clean; skip push, next cycle retries
  fi
  log "merged with $conflicts preserved conflict(s)"
fi

# --- 4. push (one bounded retry for a push race) ----------------------------
if ! git push -q "$REMOTE" "$BRANCH"; then
  git fetch -q "$REMOTE" || remote_failed
  git merge -q --no-edit "$REMOTE/$BRANCH" || { git merge --abort 2>/dev/null; remote_failed; }
  git push -q "$REMOTE" "$BRANCH" || remote_failed
fi
log "cycle complete"
