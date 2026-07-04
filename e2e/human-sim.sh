#!/usr/bin/env bash
# e2e/human-sim.sh — stand-in for the external sync client (Obsidian Sync,
# Syncthing, iCloud, Dropbox, ...) that writes and deletes files directly
# in the bridge directory, entirely out-of-band from tephra. tephra can't
# tell this apart from a real sync client, which is the point.
#
# Usage: human-sim.sh <bridge-dir> <stopfile>
# Loops with randomized 1-7s sleeps, doing one of:
#   (a) append a timestamped line to an existing note
#   (b) create note-<n>.md, or occasionally the unicode "Café ☕.md"
#   (c) delete a previously-created note (never Home.md, never anything
#       this script didn't create itself)
# until <stopfile> exists.
set -uo pipefail

BRIDGE="$1"
STOPFILE="$2"

mkdir -p "$BRIDGE"
declare -a created=()
counter=0

random_sleep() {
  sleep $(((RANDOM % 7) + 1)) # 1..7s
}

append_line() {
  local target="Home.md"
  if [ "${#created[@]}" -gt 0 ] && [ $((RANDOM % 2)) -eq 0 ]; then
    target="${created[$((RANDOM % ${#created[@]}))]}"
  fi
  if [ -f "$BRIDGE/$target" ]; then
    echo "human edit $(date -u +%FT%TZ)" >>"$BRIDGE/$target"
    echo "human-sim: appended to $target"
  fi
}

create_note() {
  local name
  if [ $((RANDOM % 4)) -eq 0 ] && [ ! -f "$BRIDGE/Café ☕.md" ]; then
    name="Café ☕.md"
  else
    counter=$((counter + 1))
    name="note-$counter.md"
  fi
  echo "human note created $(date -u +%FT%TZ)" >"$BRIDGE/$name"
  created+=("$name")
  echo "human-sim: created $name"
}

delete_note() {
  if [ "${#created[@]}" -eq 0 ]; then
    return
  fi
  local idx=$((RANDOM % ${#created[@]}))
  local name="${created[$idx]}"
  if [ -f "$BRIDGE/$name" ]; then
    rm -f "$BRIDGE/$name"
    echo "human-sim: deleted $name"
  fi
  unset 'created[idx]'
  created=("${created[@]}")
}

echo "human-sim: starting against $BRIDGE (stopfile $STOPFILE)"
while [ ! -f "$STOPFILE" ]; do
  action=$((RANDOM % 3))
  case "$action" in
  0) append_line ;;
  1) create_note ;;
  2) delete_note ;;
  esac
  random_sleep
done
echo "human-sim: stopfile seen, exiting"
