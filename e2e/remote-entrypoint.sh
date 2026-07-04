#!/usr/bin/env bash
# e2e/remote-entrypoint.sh — the `remote` container's PID 1.
#
# Two jobs, both driven by files on the `shared` volume the `scenario`
# container also mounts (see e2e/compose.yml):
#
#   1. Install the tephra client's public key into authorized_keys as soon
#      as it shows up at $SHARED/keys/id_ed25519.pub. The private half is
#      generated at build time inside the tephra image (Dockerfile.tephra)
#      and never leaves it; only the public key crosses this volume. This
#      runs at container *start*, not image build, because Docker's build
#      isolation gives each image's build no visibility into the other's —
#      see the e2e report for the fuller rationale.
#
#   2. Act as a tiny chaos supervisor for sshd: start it, and toggle it off
#      while $SHARED/chaos/kill-remote exists. The scenario container has
#      no docker socket, so it can't `docker compose pause remote` itself
#      from inside its own container; this flag-file protocol is the
#      in-container substitute (Task 12's designated mechanism).
set -uo pipefail

SHARED=/shared
KEY_DIR="$SHARED/keys"
CHAOS_FLAG="$SHARED/chaos/kill-remote"
PUBKEY_SRC="$KEY_DIR/id_ed25519.pub"
AUTH_KEYS=/home/git/.ssh/authorized_keys

mkdir -p "$KEY_DIR" "$(dirname "$CHAOS_FLAG")"

echo "[remote] waiting for tephra's public key at $PUBKEY_SRC ..."
(
  while true; do
    if [ -s "$PUBKEY_SRC" ] && ! grep -qF "$(cat "$PUBKEY_SRC")" "$AUTH_KEYS" 2>/dev/null; then
      cat "$PUBKEY_SRC" >>"$AUTH_KEYS"
      chown git:git "$AUTH_KEYS"
      chmod 600 "$AUTH_KEYS"
      echo "[remote] installed tephra's public key into authorized_keys"
    fi
    sleep 1
  done
) &

sshd_running() { pgrep -x sshd >/dev/null 2>&1; }

echo "[remote] chaos supervisor starting"
while true; do
  if [ -f "$CHAOS_FLAG" ]; then
    if sshd_running; then
      echo "[remote] chaos flag present: stopping sshd"
      pkill -x sshd || true
    fi
  else
    if ! sshd_running; then
      echo "[remote] starting sshd"
      /usr/sbin/sshd
    fi
  fi
  sleep 1
done
