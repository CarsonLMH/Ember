#!/bin/bash
# Atomic delivery: checks+gates → commit → build → swap → VERIFY.
# Any failure aborts the whole thing loudly; the final DELIVERED line prints
# only when the freshly-built bundle is confirmed to be the running process.
# NEVER run while source files are being edited — tauri dev's watcher rebuilds
# mid-gate and invalidates the run.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

TP=${1:?usage: scripts/ship.sh <test-photos-folder> <commit-message-file>}
MSG=${2:?commit message file required}

./scripts/checks.sh "$TP"

git add -A
# Clean tree = redeliver HEAD (gates + build + swap still run).
git diff --cached --quiet || git commit -q -F "$MSG"

npm run tauri build -- --debug 2>&1 | grep -E 'Finished 2 bundles|error' || {
  echo "SHIP FAIL: build did not finish"
  exit 1
}

BIN=src-tauri/target/debug/bundle/macos/Ember.app/Contents/MacOS/ember
AGE=$(($(date +%s) - $(stat -f %m "$BIN")))
[ "$AGE" -lt 600 ] || {
  echo "SHIP FAIL: bundle is ${AGE}s old — stale"
  exit 1
}

pkill -f 'bundle/macos/Ember.app/Contents/MacO[S]' 2>/dev/null || true
sleep 1
open src-tauri/target/debug/bundle/macos/Ember.app
sleep 2
pgrep -f 'bundle/macos/Ember.app/Contents/MacO[S]' >/dev/null || {
  echo "SHIP FAIL: app did not start"
  exit 1
}
echo "DELIVERED: $(git log -1 --format='%h %s')"
