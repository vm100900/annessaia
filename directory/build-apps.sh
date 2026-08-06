#!/usr/bin/env bash
# Build the core apps for hosting on the bootstrap Worker.
#
# search and admin talk to a registry over HTTP, so the address is baked in at
# compile time — these copies point at the Worker itself rather than localhost,
# which is what lets someone use them without running a node at all.
set -e

WORKER="${ANNESSAIA_WORKER:-https://bootstrap.annessaia.workers.dev}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$(dirname "$0")/public"

echo "→ Target registry: $WORKER"
cd "$ROOT"
rustup target add wasm32-unknown-unknown 2>/dev/null || true

# Standalone apps — no server dependency. annessaia-pixel is deliberately not
# built here: its listing was pulled from the registry as redundant with Paint
# (same pixel-art territory, confusingly so), and the underlying .wasm was
# removed from public/ to match — rebuilding it here would silently put it
# back.
cargo build --target wasm32-unknown-unknown --release \
    -p annessaia-calculator -p annessaia-paint -p annessaia-nova \
    -p annessaia-widgets -p annessaia-gpu

# The hosted search browser. READONLY strips the submit form: this index only
# accepts entries gossiped in from a node, so publishing requires running one.
# apps/search has a build.rs declaring rerun-if-env-changed for both variables,
# so switching targets really does recompile rather than reusing a stale binary.
ANNESSAIA_SERVER="$WORKER" ANNESSAIA_READONLY=1 \
    cargo build --target wasm32-unknown-unknown --release -p annessaia-search

mkdir -p "$OUT"
R=target/wasm32-unknown-unknown/release
cp "$R/annessaia_calculator.wasm" "$OUT/calculator.wasm"
cp "$R/annessaia_paint.wasm"      "$OUT/paint.wasm"
cp "$R/annessaia_nova.wasm"       "$OUT/nova.wasm"
cp "$R/annessaia_widgets.wasm"    "$OUT/widgets.wasm"
cp "$R/annessaia_gpu.wasm"        "$OUT/gpu.wasm"
cp "$R/annessaia_search.wasm"     "$OUT/search.wasm"

echo ""
ls -lh "$OUT"
echo ""
echo "Deploy with:  cd directory && npx wrangler deploy"
