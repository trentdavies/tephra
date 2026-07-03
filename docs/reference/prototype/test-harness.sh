#!/bin/bash
# Sandbox test for memory-bridge. Run: bash scripts/tests/test-memory-bridge.sh
set -euo pipefail
# Fixture repos live in /tmp where no includeIf identity matches; make the
# harness independent of the machine's gitconfig (useConfigOnly, gpgsign).
export GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.com
export GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.com
export GIT_CONFIG_COUNT=2
export GIT_CONFIG_KEY_0=commit.gpgsign GIT_CONFIG_VALUE_0=false
export GIT_CONFIG_KEY_1=user.useConfigOnly GIT_CONFIG_VALUE_1=false
SB="$(mktemp -d)"; trap 'rm -rf "$SB"' EXIT
BIN="$(cd "$(dirname "$0")/../.." && pwd)/dot_local/bin/executable_memory-bridge"
pass=0; fail=0
ok(){ echo "ok: $1"; pass=$((pass+1)); }
bad(){ echo "FAIL: $1"; fail=$((fail+1)); }

git init -q --bare "$SB/remote.git"
git clone -q "$SB/remote.git" "$SB/seed"
( cd "$SB/seed" && echo "# Home" > Home.md && git add -A && git commit -qm init && git branch -M main && git push -q origin main )
git clone -q "$SB/remote.git" "$SB/bridge-testvault"
git clone -q "$SB/remote.git" "$SB/agent"

run_bridge(){ MEMORY_BRIDGE_ROOT="$SB" MEMORY_BRIDGE_REMOTE=origin bash "$BIN" testvault; }

# 1. human edit in bridge gets committed and pushed
echo "human line" >> "$SB/bridge-testvault/Home.md"
run_bridge
( cd "$SB/agent" && git pull -q )
grep -q "human line" "$SB/agent/Home.md" && ok "human edit propagated" || bad "human edit propagated"

# 2. agent push gets merged into bridge
( cd "$SB/agent" && echo "agent note" > Agent.md && git add -A && git commit -qm "memory: note" && git push -q )
run_bridge
[ -f "$SB/bridge-testvault/Agent.md" ] && ok "agent push merged" || bad "agent push merged"

# 3. same-file conflict -> human wins in place, agent copy preserved
( cd "$SB/agent" && git pull -q && echo "AGENT VERSION" > Home.md && git add -A && git commit -qm "memory: edit home" && git push -q )
echo "HUMAN VERSION" > "$SB/bridge-testvault/Home.md"
run_bridge
grep -q "HUMAN VERSION" "$SB/bridge-testvault/Home.md" && ok "human wins in place" || bad "human wins in place"
ls "$SB/bridge-testvault/"Home*agent\ conflict* >/dev/null 2>&1 && ok "agent conflict copy created" || bad "agent conflict copy created"
grep -q "AGENT VERSION" "$SB/bridge-testvault/"Home*agent\ conflict*.md 2>/dev/null && ok "agent conflict copy content" || bad "agent conflict copy content"

# 3b. unicode-filename conflict -> same policy (exercises NUL-safe conflict loop)
( cd "$SB/agent" && git pull -q && echo "AGENT CAFE" > "Café ☕.md" && git add -A && git commit -qm "memory: cafe" && git push -q )
echo "HUMAN CAFE" > "$SB/bridge-testvault/Café ☕.md"
run_bridge
grep -q "HUMAN CAFE" "$SB/bridge-testvault/Café ☕.md" && ok "unicode: human wins in place" || bad "unicode: human wins in place"
grep -q "AGENT CAFE" "$SB/bridge-testvault/Café ☕ (agent conflict "*.md 2>/dev/null && ok "unicode: agent copy content" || bad "unicode: agent copy content"

# 4. remote unreachable -> local commit still made, exit 0, failure counter bumped
mv "$SB/remote.git" "$SB/remote.gone"
echo "offline edit" >> "$SB/bridge-testvault/Home.md"
run_bridge && ok "offline run exits 0" || bad "offline run exits 0"
( cd "$SB/bridge-testvault" && git log -1 --format=%s | grep -q "human edits" ) && ok "offline edit committed" || bad "offline edit committed"
[ -f "$SB/bridge-testvault/.git/memory-bridge.failcount" ] && ok "failure counter exists" || bad "failure counter exists"
mv "$SB/remote.gone" "$SB/remote.git"

# 5. remote restored -> failcount cleared, queued offline commit reaches remote
run_bridge
[ ! -f "$SB/bridge-testvault/.git/memory-bridge.failcount" ] && ok "failcount cleared after recovery" || bad "failcount cleared after recovery"
( cd "$SB/agent" && git pull -q && grep -q "offline edit" Home.md ) && ok "offline edit reached remote" || bad "offline edit reached remote"

# expected total: 12
echo; echo "passed=$pass failed=$fail"; [ "$fail" -eq 0 ]
