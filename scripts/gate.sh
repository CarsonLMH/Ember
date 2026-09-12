#!/bin/bash
# Regression gates (zoom blank-test + flip storm) in a HIDDEN window on an
# isolated port — safe to run while a user instance is open on 1420.
# Phases wait for the port to actually free (strictPort + a straggling
# listener from the previous phase was an intermittent storm-killer), and
# failures print the phase log tail instead of dying silently.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

TP=${1:?usage: scripts/gate.sh <test-photos-folder>}
PORT=14210
CFG=src-tauri/tauri.gate.conf.json

cleanup() { pkill -f "vite.*1421[0]" 2>/dev/null || true; }
trap cleanup EXIT

wait_port_free() {
  for _ in $(seq 1 30); do
    lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1 || return 0
    sleep 0.5
  done
  echo "GATE FAIL: port $PORT still busy after 15s"
  return 1
}

run_phase() { # name, expected-pattern, env assignments...
  local name=$1 expect=$2
  shift 2
  cleanup
  wait_port_free
  local log
  log=$(mktemp)
  echo "=== $name (hidden):"
  env "$@" EMBER_HIDDEN=1 EMBER_OPEN="$TP" \
    timeout 240 npm run tauri dev -- --config "$CFG" >"$log" 2>&1 || true
  grep -E "$expect" "$log" || true
  grep -qE "$expect" "$log" || {
    echo "GATE FAIL: $name produced no result; log tail:"
    tail -8 "$log"
    exit 1
  }
}

run_phase zoomtest 'zoomtest done: PASS' EMBER_ZOOMTEST=1
# stormOk covers the budget AND that flips were actually recorded — a storm
# that measures nothing must never read as a pass.
run_phase storm 'storm done:.*stormOk=true' EMBER_STORM=1
# Faces gate (plan rev 4, Slice 0): the storm must hold its budget with the
# detect→embed spike pinned active. facesSpike=ACTIVE in the result line
# proves inference actually ran during the measured window, not before it.
run_phase storm-faces 'storm done:.*facesSpike=ACTIVE.*stormOk=true' EMBER_STORM=1 EMBER_FACES_FORCE=1
# Person filter (Slice C): names a cluster mid-scan, filters to that person,
# and storms inside the filtered view — membership exactness + flip budget.
if [[ ${EMBER_SKIP_PEOPLETEST:-0} == 1 ]]; then
  echo "=== peopletest: SKIPPED by explicit EMBER_SKIP_PEOPLETEST=1"
else
  run_phase peopletest 'peopletest done: PASS' EMBER_PEOPLETEST=1
fi

echo "=== gates complete"
