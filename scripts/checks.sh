#!/bin/bash
# Pre-commit checks + hidden regression gates. Kill patterns use [b]racket
# tricks so they can never match the invoking command line (self-pkill).
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

TP=${1:?usage: scripts/checks.sh <test-photos-folder>}

pkill -f 'target/debug/embe[r]' 2>/dev/null || true
pkill -f 'vite.*1421[0]' 2>/dev/null || true
sleep 1

npx tsc --noEmit && echo "tsc OK"
npx vitest run --silent 2>&1 | grep -E 'Test Files|Tests '
(cd src-tauri && cargo fmt && cargo test 2>&1 | grep -E 'test result: (ok|FAILED)' | head -1)
(cd src-tauri && cargo clippy --all-targets --quiet -- -D warnings)

# No display filter: gate diagnostics must reach the operator on failure.
./scripts/gate.sh "$TP"
