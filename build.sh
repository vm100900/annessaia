#!/usr/bin/env bash
set -e

rustup target add wasm32-unknown-unknown 2>/dev/null || true

echo "→ Building WASM apps..."
cargo build -p annessaia-gpu -p annessaia-pixel -p annessaia-widgets \
            -p annessaia-search -p annessaia-admin -p annessaia-nova \
    --target wasm32-unknown-unknown --release

echo "→ Copying to dist/..."
mkdir -p dist
cp target/wasm32-unknown-unknown/release/annessaia_gpu.wasm     dist/gpu.wasm
cp target/wasm32-unknown-unknown/release/annessaia_pixel.wasm   dist/pixel.wasm
cp target/wasm32-unknown-unknown/release/annessaia_widgets.wasm dist/widgets.wasm
cp target/wasm32-unknown-unknown/release/annessaia_search.wasm  dist/search.wasm
cp target/wasm32-unknown-unknown/release/annessaia_admin.wasm   dist/admin.wasm
cp target/wasm32-unknown-unknown/release/annessaia_nova.wasm    dist/nova.wasm

echo ""
ls -lh dist/
echo ""
echo "Done. Now open two terminals:"
echo ""
echo "  Terminal 1 — start the app server:"
echo "    cargo run -p annessaia-server"
echo ""
echo "  Terminal 2 — open annessaia:"
echo "    cargo run -p annessaia --release"
echo ""
echo "Then paste one of these into the annessaia address bar:"
echo "    http://localhost:3000/nova.wasm      ← NOVA: mouse-only survival game"
echo "    http://localhost:3000/search.wasm    ← registry: search + submit"
echo "    http://localhost:3000/admin.wasm     ← node admin: apps, peers, directory"
echo "    http://localhost:3000/gpu.wasm"
echo "    http://localhost:3000/pixel.wasm"
echo "    http://localhost:3000/widgets.wasm"
