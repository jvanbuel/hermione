#!/usr/bin/env bash
# One-shot: start the fixtures server, screenshot every page in both themes,
# stop the server. Screenshots land in OUT_DIR.
#
# Usage:
#   OUT_DIR=/path/to/out LABEL=after scripts/capture.sh
#
# Env (all optional except OUT_DIR):
#   OUT_DIR      where PNGs are written (required)
#   LABEL        filename prefix, e.g. before/after (default: shot)
#   STATIC_DIR   path to crates/server/static (default: inferred from repo)
#   PORT         server port (default: an unlikely-busy 8799)
#   PW_CHROMIUM  chromium binary (default: /opt/pw-browsers/chromium)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
: "${OUT_DIR:?set OUT_DIR to the directory where screenshots should go}"
export LABEL="${LABEL:-shot}"
export PORT="${PORT:-8799}"
# Fixed clock shared by server + shooter so relative timestamps are stable.
export FIXED_NOW="${FIXED_NOW:-$(node -e 'process.stdout.write(String(Date.parse("2025-07-19T13:30:00Z")))')}"
# Playwright is usually only installed globally in this environment.
export NODE_PATH="${NODE_PATH:-$(npm root -g)}"

mkdir -p "$OUT_DIR"

node "$SCRIPT_DIR/server.js" &
SRV=$!
trap 'kill $SRV 2>/dev/null || true' EXIT

# Wait for the server to accept connections (up to ~5s).
for _ in $(seq 1 50); do
  if node -e "require('http').get('http://localhost:'+process.env.PORT+'/api/courses',r=>process.exit(r.statusCode===200?0:1)).on('error',()=>process.exit(1))" 2>/dev/null; then
    break
  fi
  sleep 0.1
done

OUT_DIR="$OUT_DIR" LABEL="$LABEL" node "$SCRIPT_DIR/shoot.js"
echo "Screenshots written to $OUT_DIR"
