#!/bin/sh
# Regenerate the README screenshots from made-up data (scripts/demo/seed.py).
# Needs: cargo, python3, node 22+, google-chrome. Nothing from your own HOME is read:
# the dashboard runs with a throwaway HOME and a PATH without claude/codex/agy, so it
# never asks your real accounts for limits.
#
#   scripts/demo/screenshots.sh            # writes docs/screenshots/*.png
set -eu

cd "$(dirname "$0")/../.."
OUT=docs/screenshots
PORT=${DEMO_PORT:-8797}
CDP=${DEMO_CDP_PORT:-9337}
TMP=$(mktemp -d)
HOME_DIR=$TMP/home
mkdir -p "$HOME_DIR/.tokenscope" "$OUT"
cleanup() {
    [ -n "${SERVER:-}" ] && kill "$SERVER" 2>/dev/null
    [ -n "${CHROME:-}" ] && kill "$CHROME" 2>/dev/null
    sleep 1
    rm -rf "$TMP"
}
trap cleanup EXIT

cargo build --release --quiet
BIN=$PWD/target/release/tokenscope
cat > "$HOME_DIR/.tokenscope/config.toml" <<'EOF'
claude_account = "work"

[labels]
"claude:work" = "Work"
"codex:a1b2c3d4" = "Work"
"codex:e5f6a7b8" = "Personal"
"agy:9c8d7e6f" = "Personal"
EOF
run() { env -i HOME="$HOME_DIR" PATH=/usr/bin:/bin TZ="${TZ:-UTC}" "$@"; }
run "$BIN" collect > /dev/null            # creates the schema
python3 -I scripts/demo/seed.py "$HOME_DIR/.tokenscope/tokenscope.db"
# not through run(): a backgrounded function is a subshell, and killing it would leave the server
env -i HOME="$HOME_DIR" PATH=/usr/bin:/bin TZ="${TZ:-UTC}" "$BIN" serve --port "$PORT" > /dev/null 2>&1 &
SERVER=$!
google-chrome --headless=new --remote-debugging-port="$CDP" --user-data-dir="$TMP/chrome" \
    --hide-scrollbars --window-size=1280,900 about:blank > /dev/null 2>&1 &
CHROME=$!
sleep 3
node scripts/demo/capture.mjs "http://127.0.0.1:$PORT" "$CDP" "$OUT"
