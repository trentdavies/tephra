#!/usr/bin/env bash
# e2e/scenario.sh — orchestrator for the tephra clean-room e2e, run inside
# the `scenario` container (docs/plans/2026-07-03-v1-implementation.md
# Task 12). Proves the whole system converges with a git-over-ssh remote,
# a human/sync simulator writing directly in the bridge dir, and an agent
# pushing through `tephra sync` -- sensed exclusively through tephra's own
# `status --json` plus git plumbing, per the plan's verification-as-artifact
# rule.
#
# Not `set -e`: every step that matters is checked explicitly (via
# `|| fail_dump ...` or a `wait_until` assertion) so a failure always goes
# through the labeled diagnostic dump before exiting non-zero.
set -uo pipefail

VAULT=e2e
BRIDGE=/root/bridge
WORK=/root/work
SEED=/root/seed
LOG=/root/watch.log
HS_STOP=/root/human-sim.stop
HS_LOG=/root/human-sim.log
SHARED=/shared
KEYFILE="$SHARED/keys/id_ed25519.pub"
CHAOS="$SHARED/chaos/kill-remote"
# Matches the --interval passed to `tephra bridge --watch` below. tephra
# clamps --watch intervals below 10s up to 10s (src/bridge.rs
# MIN_INTERVAL_SECS), so 10 is both the requested and the effective cycle
# length -- no clamp-warning noise in the log.
CYCLE=10

banner() {
  echo
  echo "=== $* ==="
  echo
}

status_json() { tephra status "$VAULT" --json; }

fail_dump() {
  echo "FAIL: $*" >&2
  echo "--- tephra status --json ---" >&2
  status_json >&2 2>&1 || true
  echo "--- last 40 lines of $LOG ---" >&2
  tail -n 40 "$LOG" >&2 2>&1 || true
  echo "--- git -C $BRIDGE log --oneline -12 ---" >&2
  git -C "$BRIDGE" log --oneline -12 >&2 2>&1 || true
  echo "E2E: FAILED" >&2
  exit 1
}

# Poll `check` (a bash snippet, eval'd in this shell) until it succeeds or
# max_secs elapses, sleeping 1s between attempts. Expresses the plan's
# "wait N cycle-lengths" as an upper bound rather than a blind fixed sleep
# -- far less flake-prone under container scheduling jitter, without
# changing what's being asserted.
wait_until() {
  local desc="$1" max_secs="$2" check="$3" waited=0
  while ((waited < max_secs)); do
    if eval "$check"; then
      return 0
    fi
    sleep 1
    waited=$((waited + 1))
  done
  fail_dump "timed out after ${max_secs}s waiting for: $desc"
}

remote_head() { git -C "$BRIDGE" ls-remote origin main 2>/dev/null | cut -f1; }
bridge_head() { git -C "$BRIDGE" rev-parse HEAD 2>/dev/null; }
converged() {
  local r b
  r=$(remote_head)
  b=$(bridge_head)
  [ -n "$r" ] && [ "$r" = "$b" ]
}

# `git grep -l` exits 1 on "no match" -- that's the PASSING case here, not
# a failure; only exit 0 (a match found) or >1 (a real error) are failures.
no_conflict_markers_in_head() {
  local out rc
  out=$(git -C "$BRIDGE" grep -I -l '<<<<<<<' HEAD -- . 2>&1)
  rc=$?
  if [ "$rc" -eq 1 ]; then
    return 0
  fi
  echo "$out" >&2
  return 1
}

stop_human_sim() {
  local pid="$1"
  touch "$HS_STOP"
  for _ in $(seq 1 15); do
    kill -0 "$pid" 2>/dev/null || return 0
    sleep 1
  done
  kill -9 "$pid" 2>/dev/null || true
}

git_clone_retry() {
  local url="$1" dest="$2" attempt=1
  while [ "$attempt" -le 20 ]; do
    if git clone --quiet "$url" "$dest" 2>/tmp/clone.err; then
      return 0
    fi
    attempt=$((attempt + 1))
    sleep 1
  done
  cat /tmp/clone.err >&2
  fail_dump "git clone $url -> $dest failed after retries"
}

# ---------------------------------------------------------------------
banner "Phase 0: setup"
# ---------------------------------------------------------------------

mkdir -p "$SHARED/keys" "$SHARED/chaos"
cp /root/.ssh/id_ed25519.pub "$KEYFILE"
echo "wrote public key to $KEYFILE"

CONFIG_DIR="/root/.config/tephra"
mkdir -p "$CONFIG_DIR" "$BRIDGE" "$WORK"
cat >"$CONFIG_DIR/config.toml" <<EOF
[vaults.$VAULT]
bridge = "$BRIDGE"
work   = "$WORK"
url    = "git@remote:/srv/vault.git"
branch = "main"
EOF
echo "wrote $CONFIG_DIR/config.toml"

# The remote's background watcher (remote-entrypoint.sh) polls the shared
# volume for our public key every ~1s, so the very first ssh-based git
# operation may transiently fail if it lands in that window;
# git_clone_retry absorbs that rather than a blind up-front sleep.
git_clone_retry "git@remote:/srv/vault.git" "$SEED"
git -C "$SEED" checkout -B main >/dev/null
echo "# Home" >"$SEED/Home.md"
echo "seeded $(date -u +%FT%TZ)" >>"$SEED/Home.md"
git -C "$SEED" add -A
git -C "$SEED" commit -q -m "vault: seed"
git -C "$SEED" push -q -u origin main
echo "seeded bare repo with Home.md"

git_clone_retry "git@remote:/srv/vault.git" "$BRIDGE"
echo "cloned bridge checkout: $BRIDGE"

tephra clone "$VAULT" || fail_dump "tephra clone failed"

: >"$LOG"
tephra bridge --watch --interval "$CYCLE" "$VAULT" >>"$LOG" 2>&1 &
WATCH_PID=$!
sleep 2
kill -0 "$WATCH_PID" 2>/dev/null || fail_dump "bridge watch exited immediately; see $LOG"
echo "bridge watch running, pid=$WATCH_PID, interval=${CYCLE}s, log=$LOG"

# ---------------------------------------------------------------------
banner "Phase 1: convergence (human-sim + agent sync loop)"
# ---------------------------------------------------------------------

rm -f "$HS_STOP"
bash /e2e/human-sim.sh "$BRIDGE" "$HS_STOP" >>"$HS_LOG" 2>&1 &
HS_PID=$!
echo "human-sim running, pid=$HS_PID"

ITERS=$(((RANDOM % 3) + 6)) # 6..8
mkdir -p "$WORK/agents"
echo "agent loop: $ITERS iterations"
for i in $(seq 1 "$ITERS"); do
  echo "agent note $i at $(date -u +%FT%TZ)" >"$WORK/agents/agent-note-$i.md"
  tephra sync "$VAULT" -m "memory: agent update $i" || fail_dump "tephra sync failed on iteration $i"
  sleep $(((RANDOM % 4) + 2)) # 2..5s
done

stop_human_sim "$HS_PID"
echo "human-sim stopped"

wait_until "phase 1 convergence (dirty=0 ahead=0 behind=0 outcome=ok)" $((CYCLE * 3)) '
  j=$(status_json)
  [ "$(jq -r ".bridge.dirty"           <<<"$j")" = "0" ] &&
  [ "$(jq -r ".bridge.ahead"           <<<"$j")" = "0" ] &&
  [ "$(jq -r ".bridge.behind"          <<<"$j")" = "0" ] &&
  [ "$(jq -r ".bridge.last_cycle_outcome" <<<"$j")" = "ok" ]
'
converged || fail_dump "bridge HEAD != remote HEAD after phase 1"

subjects=$(git -C "$BRIDGE" log --format=%s)
grep -qx "vault: human edits" <<<"$subjects" || fail_dump "no 'vault: human edits' commit found"
grep -q "^memory:" <<<"$subjects" || fail_dump "no 'memory:' commit found"

echo "Phase 1: PASSED"

# ---------------------------------------------------------------------
banner "Phase 2: conflict (same-path divergent edits)"
# ---------------------------------------------------------------------

CONFLICT_NAME="conflict-note.md"
HUMAN_CONTENT="human version $(date -u +%FT%TZ)"
AGENT_CONTENT="agent version $(date -u +%FT%TZ)"

echo "$HUMAN_CONTENT" >"$BRIDGE/$CONFLICT_NAME"
echo "wrote human version of $CONFLICT_NAME directly in the bridge"

echo "$AGENT_CONTENT" >"$WORK/$CONFLICT_NAME"
tephra sync "$VAULT" -m "memory: conflicting note" || fail_dump "agent sync failed writing conflicting note"

shopt -s nullglob
wait_until "conflict resolved and pushed" $((CYCLE * 2)) '
  converged && [ -f "$BRIDGE/$CONFLICT_NAME" ] &&
  { copies=( "$BRIDGE/conflict-note (agent conflict "*").md" ); [ "${#copies[@]}" -ge 1 ]; }
'

copies=("$BRIDGE/conflict-note (agent conflict "*").md")
shopt -u nullglob

if [ "${#copies[@]}" -ne 1 ]; then
  fail_dump "expected exactly one agent-conflict copy of $CONFLICT_NAME, found: ${copies[*]:-<none>}"
fi

actual_human=$(cat "$BRIDGE/$CONFLICT_NAME")
[ "$actual_human" = "$HUMAN_CONTENT" ] || fail_dump "human content not preserved in $CONFLICT_NAME (got: $actual_human)"

actual_agent=$(cat "${copies[0]}")
[ "$actual_agent" = "$AGENT_CONTENT" ] || fail_dump "agent-conflict copy content mismatch (got: $actual_agent)"

converged || fail_dump "bridge HEAD != remote HEAD after phase 2"

echo "Phase 2: PASSED"

# ---------------------------------------------------------------------
banner "Phase 3: outage (chaos: sshd unreachable mid-activity)"
# ---------------------------------------------------------------------

touch "$CHAOS"
echo "chaos flag set: $CHAOS"
sleep 2 # let remote's ~1s poll loop notice and stop sshd

rm -f "$HS_STOP"
bash /e2e/human-sim.sh "$BRIDGE" "$HS_STOP" >>"$HS_LOG" 2>&1 &
HS_PID=$!
echo "human-sim resumed during outage, pid=$HS_PID"

wait_until "failcount rises during outage" $((CYCLE * 3)) '
  j=$(status_json)
  fc=$(jq -r ".bridge.failcount" <<<"$j")
  [ "$fc" != "null" ] && [ "$fc" -ge 1 ] &&
  [ "$(jq -r ".bridge.last_cycle_outcome" <<<"$j")" = "remote-failure" ]
'
echo "outage confirmed via status --json (failcount >= 1, outcome remote-failure)"

stop_human_sim "$HS_PID"
echo "human-sim stopped"

rm -f "$CHAOS"
echo "chaos flag cleared: sshd restarting"

wait_until "recovery: failcount clears, outcome ok, queued commits reach remote" $((CYCLE * 4)) '
  j=$(status_json)
  [ "$(jq -r ".bridge.failcount" <<<"$j")" = "null" ] &&
  [ "$(jq -r ".bridge.last_cycle_outcome" <<<"$j")" = "ok" ] &&
  converged
'

echo "Phase 3: PASSED"

# ---------------------------------------------------------------------
# Phase 4 simulates a process *supervisor* recovering from a crash: kill
# -9 the watch process, clear the lock a supervisor would know to clear
# before restarting its own just-crashed service, then restart. This is
# NOT an exercise of tephra's own 30-minute lock-staleness auto-recovery
# (src/bridge.rs LOCK_STALE_AFTER) -- that path is covered by unit tests,
# not here, since waiting out 30 real minutes in this scenario would be
# impractical.
banner "Phase 4: crash recovery"
# ---------------------------------------------------------------------

echo "burst of activity right before the crash"
echo "pre-crash human note $(date -u +%FT%TZ)" >"$BRIDGE/pre-crash-note.md"
echo "pre-crash agent note $(date -u +%FT%TZ)" >"$WORK/pre-crash-agent-note.md"
tephra sync "$VAULT" -m "memory: pre-crash agent note" || fail_dump "pre-crash agent sync failed"

sleep 1
echo "kill -9 watch pid=$WATCH_PID"
kill -9 "$WATCH_PID" 2>/dev/null || true
wait "$WATCH_PID" 2>/dev/null || true

no_conflict_markers_in_head || fail_dump "conflict markers found in bridge HEAD after crash"

# The killed watch process leaves its mkdir-based lock behind (there's no
# Drop-on-SIGKILL) -- clear it the way a supervisor's crash-recovery hook
# would, per the phase header comment above.
LOCK_DIR="$BRIDGE/.git/tephra-bridge.lock"
if [ -d "$LOCK_DIR" ]; then
  echo "clearing stale lock left by the killed watch process: $LOCK_DIR"
  rmdir "$LOCK_DIR" 2>/dev/null || rm -rf "$LOCK_DIR"
fi

tephra bridge --watch --interval "$CYCLE" "$VAULT" >>"$LOG" 2>&1 &
WATCH_PID=$!
sleep 2
kill -0 "$WATCH_PID" 2>/dev/null || fail_dump "restarted bridge watch exited immediately; see $LOG"
echo "bridge watch restarted, pid=$WATCH_PID"

banner "Phase 4: final full-convergence"

rm -f "$HS_STOP"
bash /e2e/human-sim.sh "$BRIDGE" "$HS_STOP" >>"$HS_LOG" 2>&1 &
HS_PID=$!

for i in 1 2 3; do
  echo "post-crash agent note $i at $(date -u +%FT%TZ)" >"$WORK/post-crash-note-$i.md"
  tephra sync "$VAULT" -m "memory: post-crash agent update $i" || fail_dump "post-crash sync failed on iteration $i"
  sleep 2
done

stop_human_sim "$HS_PID"
echo "human-sim stopped"

wait_until "final full convergence" $((CYCLE * 3)) '
  j=$(status_json)
  [ "$(jq -r ".bridge.dirty"           <<<"$j")" = "0" ] &&
  [ "$(jq -r ".bridge.ahead"           <<<"$j")" = "0" ] &&
  [ "$(jq -r ".bridge.behind"          <<<"$j")" = "0" ] &&
  [ "$(jq -r ".bridge.last_cycle_outcome" <<<"$j")" = "ok" ] &&
  converged
'
no_conflict_markers_in_head || fail_dump "conflict markers found in bridge HEAD after final convergence"

echo "Phase 4: PASSED"

echo
echo "E2E: ALL PHASES PASSED"
