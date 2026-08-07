# annessaia

A sandboxed WASM app runtime with its own UI toolkit — apps are compiled Rust,
not HTML/JS, run by a native desktop host (`annessaia`) through a small
widget/GPU API. Apps are hosted on **nodes**: lightweight servers
(`annessaia-server`) that index, gossip, and serve apps to each other and to
the desktop host, forming a small decentralized-ish app registry rather than
depending on one central store.

```
┌─────────────┐   fetches .wasm   ┌──────────────────┐   gossips over WS   ┌──────────────────┐
│  annessaia   │ ────────────────▶│  annessaia-server │◀───────────────────▶│  annessaia-server │
│ (desktop app)│                  │      (a node)     │                      │   (another node)  │
└─────────────┘                  └──────────────────┘                      └──────────────────┘
```

## Quickstart

Requires a recent Rust toolchain ([rustup.rs](https://rustup.rs)).

```bash
git clone <this repo>
cd annessaia
./build.sh                       # builds every example WASM app into dist/
```

Then, in two terminals:

```bash
# Terminal 1 — run a node (serves apps, indexes/search, gossip)
cargo run -p annessaia-server

# Terminal 2 — run the desktop app
cargo run -p annessaia --release
```

The desktop app opens to a hosted public search/registry by default. Paste
one of these into its address bar to try the bundled examples against your
own local node instead:

```
http://localhost:3000/nova.wasm       NOVA — mouse-only survival game
http://localhost:3000/search.wasm     the app registry: search + submit
http://localhost:3000/admin.wasm      node admin: password-protected
http://localhost:3000/gpu.wasm        low-level GPU-primitive demo
http://localhost:3000/pixel.wasm      pixel-art paint tool
http://localhost:3000/widgets.wasm    live reference for every SDK widget
```

The first time `annessaia` or `annessaia-server` runs, it downloads a small
on-device embedding model (~1.3GB) so search can rank by meaning, not just
keywords. This is entirely optional:

- the desktop app asks once, on first launch, whether to enable it — change
  your mind later from the toggle in its nav bar
- a node operator controls it server-wide from the admin panel (see below),
  or by building with `--no-default-features` to drop the dependency
  (`fastembed`) entirely

## Writing your own app

```bash
cargo run -p annessaia-cli -- new my-app
cd my-app
cargo run -p annessaia-cli -- build       # → my-app.wasmh
```

(Or install the CLI once with `cargo install --path tools/annessaia` and use
`annessaia new`/`annessaia build` directly.)

An app is just a `#[no_mangle] pub extern "C" fn render()` (or `render_gpu()`
/ `render_pixels(w, h)` for lower-level drawing) built against
`annessaia_sdk`:

```rust
use annessaia_sdk::prelude::*;

#[no_mangle]
pub extern "C" fn render() {
    heading("Hello, annessaia!");
    label("A WASM app written in Rust.");
    if button("Click me") {
        // ...
    }
}
```

`apps/widgets` is a live reference for every widget the SDK offers, each
paired with the exact call that produced it — open it in the desktop app and
read the source side by side. The other `apps/*` crates are worked examples
at increasing complexity: `apps/calculator` (basic layout + state),
`apps/keys` (the full input API), `apps/gpu`/`apps/pixel` (drawing below the
widget layer), `apps/nova` (a small real-time game), `apps/search` and
`apps/admin` (talking to a node's HTTP API).

## Running your own node

```bash
cargo run -p annessaia-server
```

Configuration is via environment variables:

| Variable             | Meaning                                   | Default                     |
|-----------------------|--------------------------------------------|------------------------------|
| `PORT`                | port to listen on                          | `3000`                       |
| `ANNESSAIA_URL`       | this node's own public base URL            | `http://localhost:$PORT`     |
| `ANNESSAIA_DATA`      | where to persist state                     | `./data`                     |
| `ANNESSAIA_PEERS`     | comma-separated peer URLs to gossip with   | *(none)*                     |
| `ANNESSAIA_DIRECTORY` | bootstrap directory, for discovering peers | the public bootstrap Worker  |

Open `http://localhost:$PORT/admin.wasm` in the desktop app to manage the
node — apps, peers, and the public directory listing. The admin panel is
password-protected: the first time you open it, it asks you to set a
password (bcrypt-hashed on disk, never stored in plaintext); every launch
after that asks you to log in, even if the server itself never restarted.
From the dashboard you can also flip AI-powered search on or off for every
user of that node.

## Layout

```
annessaia/        desktop host — the WASM runtime + native UI (egui/wgpu)
sdk/               annessaia_sdk — the guest-side API apps are written against
apps/              example apps (see "Writing your own app" above)
server/            annessaia-server — a node: registry, search, gossip, admin API
tools/annessaia    CLI: scaffold a new app, build a .wasm/.wasmh
directory/         the public bootstrap directory (Cloudflare Worker), for
                   nodes to discover their first peer
```
