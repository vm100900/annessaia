#!/usr/bin/env bash
# Run annessaia-server behind a public Cloudflare quick tunnel.
#
# The node's public address has to be known at startup — it goes into ANNESSAIA_URL,
# which decides the URLs that uploads are served from and what gets published to the
# bootstrap directory. Quick tunnels hand out a different hostname every run, so this
# starts the tunnel first, waits for the address, then launches the server with it.
#
#   ./tunnel.sh            # port 3000
#   PORT=3005 ./tunnel.sh
#
# Ctrl-C stops both.

set -euo pipefail

PORT="${PORT:-3000}"
LOG="$(mktemp -t annessaia-tunnel)"
TUNNEL_PID=""

cleanup() {
  echo ""
  echo "→ shutting down"
  [ -n "$TUNNEL_PID" ] && kill "$TUNNEL_PID" 2>/dev/null || true
  rm -f "$LOG"
}
trap cleanup EXIT INT TERM

# A server still bound to the port would keep the tunnel pointing at the old process.
if lsof -ti:"$PORT" >/dev/null 2>&1; then
  echo "→ stopping what is already on :$PORT"
  lsof -ti:"$PORT" | xargs kill -9 2>/dev/null || true
  sleep 1
fi

echo "→ opening a tunnel to localhost:$PORT"
npx cloudflared tunnel --url "http://localhost:$PORT" > "$LOG" 2>&1 &
TUNNEL_PID=$!

# Wait for cloudflared to print the hostname it was given.
URL=""
for _ in $(seq 1 60); do
  URL="$(grep -o 'https://[a-z0-9-]*\.trycloudflare\.com' "$LOG" | head -1 || true)"
  [ -n "$URL" ] && break
  sleep 1
done

if [ -z "$URL" ]; then
  echo "✗ the tunnel never came up. cloudflared said:"
  tail -20 "$LOG"
  exit 1
fi

echo "→ public address: $URL"
echo "→ waiting for it to start routing"
for _ in $(seq 1 30); do
  code="$(curl -s -m 5 -o /dev/null -w '%{http_code}' "$URL/api/apps" || true)"
  [ "$code" != "000" ] && break     # anything but a connection failure means it routes
  sleep 2
done

cat <<EOF

  Node is public at $URL

  In annessaia:   $URL/search.wasm
  Uploads land at $URL/apps/<name>.wasm

  Publish the node itself from the Directory tab of admin.wasm.
  This address dies when you stop the tunnel — unpublish before you do.

EOF

ANNESSAIA_URL="$URL" PORT="$PORT" exec cargo run -p annessaia-server
