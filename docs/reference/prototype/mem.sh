#!/bin/bash
# mem — thin helper for working against the agent memory vault repo.
#   mem clone [vault]    clone tailgit:obsidian-<vault> to ~/dev/memory/work-<vault>
#   mem sync  [vault] [msg]  commit all, pull --rebase, push (bounded retry)
#   mem status [vault]
set -euo pipefail
CMD="${1:?usage: mem clone|sync|status [vault] [msg]}"; VAULT="${2:-personal}"
REPO="tailgit:obsidian-$VAULT"; DIR="$HOME/dev/memory/work-$VAULT"

enter(){ cd "$DIR" 2>/dev/null || { echo "mem: not cloned; run: mem clone $VAULT" >&2; exit 1; }; }
# A conflicted rebase must never be left in progress: the next sync would
# git-add the conflict markers and commit them on a detached HEAD.
pull_rebase(){
  git pull --rebase --autostash || {
    git rebase --abort 2>/dev/null || true
    echo "mem sync: rebase conflict in $DIR — resolve manually (local commit kept)" >&2
    exit 1
  }
}

case "$CMD" in
  clone)
    [ -d "$DIR/.git" ] && { echo "already cloned: $DIR"; exit 0; }
    mkdir -p "$(dirname "$DIR")" && git clone "$REPO" "$DIR" && echo "cloned: $DIR" ;;
  sync)
    MSG="${3:-memory: agent update}"
    enter
    [ -n "$(git status --porcelain)" ] && git add -A && git commit -m "$MSG"
    pull_rebase
    git push || { pull_rebase && git push; } ;;
  status)
    enter && git status -sb && git log --oneline -5 ;;
  *) echo "unknown command: $CMD" >&2; exit 1 ;;
esac
