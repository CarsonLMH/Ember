#!/bin/zsh
# Design-audit helper: a thin wrapper over the tauri-mcp CLI (dev dependency)
# so an audit can drive the `npm run tauri:mcp` build before a Claude Code
# session restart has loaded the MCP tools.
#
#   em.sh wait <dev.log>          block until the bridge reports it is listening
#   em.sh start [port]            open the CLI driver session (default 9223)
#   em.sh js '<expression>'       print JSON.stringify(expression) evaluated in the webview
#   em.sh key <Key> [Shift|Meta]  press a key (names as in KeyboardEvent.key)
#   em.sh shot <name>             screenshot -> $AUDIT_DIR/<name>.png
#   em.sh click '<css>'           real click on the first match
#   em.sh dblclick '<css>'        real double-click
#   em.sh crop <shot> <x> <y> <w> <h> <name>   crop a shot -> $AUDIT_DIR/ev/<name>.jpg (≤560px wide)
#   em.sh tree [css]              accessibility snapshot (optionally scoped)
#
# Requires AUDIT_DIR (a session scratchpad dir). Read-only on user data by
# construction only if YOU keep it so — see surfaces.md for the unsafe keys.
set -u
ROOT=${0:A:h:h:h:h:h}   # repo root (skill lives at .claude/skills/design-audit/scripts)
M="$ROOT/node_modules/.bin/tauri-mcp"
[ -x "$M" ] || { echo "tauri-mcp CLI missing — run: npm install" >&2; exit 1; }
cmd=${1:-}; [ $# -gt 0 ] && shift
case "$cmd" in
  wait)
    log=${1:?dev.log path}
    for i in $(seq 1 240); do
      grep -q 'WebSocket server listening' "$log" 2>/dev/null && { grep -E 'Running|listening' "$log" | tail -2; exit 0; }
      grep -qE '^error(\[|:)' "$log" 2>/dev/null && { echo "build failed:" >&2; grep -E '^error' "$log" | head -5 >&2; exit 1; }
      sleep 1
    done
    echo "timed out waiting for the bridge" >&2; exit 1 ;;
  start)
    "$M" driver-session start --port "${1:-9223}" 2>&1 | tail -1 ;;
  js)
    "$M" webview-execute-js --json --script "JSON.stringify((() => { return (${1:?expression}); })())" 2>&1 \
      | python3 -c 'import sys,json
try:
    d=json.load(sys.stdin); t=d["content"][0]["text"]; print(t.split("\n\n[Executed")[0])
except Exception as e:
    sys.stdin.seek(0) if False else None; print("js error:", e)' ;;
  key)
    if [ -n "${2:-}" ]; then "$M" webview-keyboard --action press --key "${1:?key}" --modifiers "$2" 2>&1 | tail -1
    else "$M" webview-keyboard --action press --key "${1:?key}" 2>&1 | tail -1; fi ;;
  shot)
    : ${AUDIT_DIR:?set AUDIT_DIR to a scratchpad dir}
    mkdir -p "$AUDIT_DIR"; "$M" webview-screenshot --file "$AUDIT_DIR/${1:?name}.png" 2>&1 | tail -1 ;;
  click)
    "$M" webview-interact --action click --selector "${1:?selector}" 2>&1 | tail -1 ;;
  dblclick)
    "$M" webview-interact --action double-click --selector "${1:?selector}" 2>&1 | tail -1 ;;
  crop)
    : ${AUDIT_DIR:?set AUDIT_DIR to a scratchpad dir}
    shot=${1:?shot} x=${2:?x} y=${3:?y} w=${4:?w} h=${5:?h} name=${6:?name}
    mkdir -p "$AUDIT_DIR/ev"
    # sips takes --cropOffset <y> <x>; it writes JPEG regardless of suffix warnings.
    sips -c "$h" "$w" --cropOffset "$y" "$x" "$AUDIT_DIR/$shot.png" --out "$AUDIT_DIR/ev/$name.jpg" >/dev/null 2>&1 \
      && sips -Z 560 "$AUDIT_DIR/ev/$name.jpg" >/dev/null 2>&1 && echo "$AUDIT_DIR/ev/$name.jpg" ;;
  tree)
    if [ -n "${1:-}" ]; then "$M" webview-dom-snapshot --type accessibility --selector "$1" 2>&1 | grep -vE '^#'
    else "$M" webview-dom-snapshot --type accessibility 2>&1 | grep -vE '^#'; fi ;;
  *)
    sed -n 2,16p "$0"; exit 1 ;;
esac
