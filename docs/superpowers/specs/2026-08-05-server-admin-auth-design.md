# Server admin auth + AI toggle — design

## Problem

`annessaia-server` (the self-hosted node, used instead of the public bootstrap
Worker) exposes an admin panel (`admin.wasm`, served from `/admin`) for
managing the node: revoking apps, connecting/removing peers, publishing to the
public directory. None of the API endpoints it talks to have any
authentication today — anyone who can reach the server over the network can
call them directly. Separately, the node operator wants a way to enable or
disable AI-powered semantic search/indexing for every user of their node, not
just per-request.

This design adds a password-protected session layer to the admin surface, and
a server-wide AI on/off toggle gated behind it.

## Scope

Auth protects the *entire* admin surface (not just the new toggle): revoke,
peer management, directory publish/unpublish/refresh, and the new AI toggle.
Endpoints used by the public search app (`/api/search`, `/api/submit`,
`/api/upload`, `/api/rate`, `/api/apps`, etc.) are untouched and stay open.

## Persisted state

Two new single-purpose files under `data_dir`, alongside the existing
`token.txt` / `peers.txt` pattern:

- `admin_pass.hash` — bcrypt hash of the admin password. Its absence is the
  signal for "no password set yet" (first-run state). Never overwritten once
  set — there is no reset endpoint in this design.
- `ai_enabled` — `"1"` or `"0"`, the same one-byte convention as the desktop
  app's `~/.annessaia/ai_pref`. Missing file defaults to `"1"`, so existing
  nodes upgrading into this keep today's always-on behavior.

## In-memory state (`AppState`)

- `admin_sessions: Mutex<HashSet<String>>` — valid session tokens. Generated
  the same way `token.txt`'s token already is (32 random hex chars via the
  existing `rand` dependency). Cleared on process restart — there is no
  explicit expiry or logout; a restart is what invalidates a session.
- `ai_enabled: AtomicBool` — loaded from the `ai_enabled` file at startup,
  flipped by the admin toggle endpoint, read by `embed_text`.

## New endpoints

### Unauthenticated (these are how a session is obtained)

- `GET /api/admin/status` → `{ password_set: bool }`.
- `POST /api/admin/setup` — body: password. Fails if `admin_pass.hash`
  already exists. Hashes with bcrypt, writes the file, returns a new session
  token.
- `POST /api/admin/login` — body: password. Verifies against the stored
  hash; returns a new session token on success, an error string on failure.

### Auth-gated (require `?token=<session>`)

New:
- `GET /api/admin/ai` → current AI-enabled state.
- `POST /api/admin/ai` — body `"1"`/`"0"`. Writes the `ai_enabled` file and
  updates the `AtomicBool`.

Moved behind auth (previously open):
`GET /api/mine`, `GET /api/peers`, `GET /api/peers/active`,
`GET /api/directory`, `POST /api/revoke`, `POST /api/peer/connect`,
`POST /api/peer/remove`, `POST /api/directory/publish`,
`POST /api/directory/unpublish`, `POST /api/directory/refresh`.

The session token travels as a `?token=` query parameter uniformly for both
GET and POST requests (rather than query-for-GET/body-for-POST), so one
`axum::middleware::from_fn_with_state` guard can cover the whole group
without needing to inspect POST bodies. This requires no changes to the WASM
host or SDK — `net::get`/`net::post` already support arbitrary URLs and
bodies; a query parameter is enough.

## AI gate

`embed_text` (in `server/src/main.rs`) is the single function every
semantic-search and indexing call already funnels through — its own comment
notes that `api_search`, `api_submit`, and `api_search_debug` all treat `None`
identically regardless of *why* embedding didn't happen. This design adds one
more reason: `if !st.ai_enabled.load(Relaxed) { return None; }` at the top of
the function, before it touches the embedder at all. This is the same shape
as the gate already added to the desktop app's `embed_query_blocking`
(`AI_PREF` check before touching `EMBEDDER`).

`Registry::load`'s startup backfill (embedding already-approved apps that are
missing a vector) calls the embedder directly, not through `embed_text`, and
runs before `AppState` exists. It is intentionally left ungated: it's a
one-time startup pass over already-approved data, not a live per-request path,
so it doesn't affect "AI used for a user" in the sense the toggle is about.

## `admin.wasm` UI changes

- On load: call `GET /api/admin/status`.
  - No password set → render a "Create admin password" screen (password +
    confirmation fields, one submit action) → `POST /api/admin/setup`.
  - Password set, no valid session held → render a login screen (password
    field) → `POST /api/admin/login`.
- The session token, once obtained, is kept in the app's existing disk-backed
  SDK storage (`storage::set`/`storage::get`), so relaunching `admin.wasm`
  while the server is still running doesn't force a re-login. If the server
  has restarted, the stored token is simply rejected by the middleware and
  the app falls back to the login screen.
- Every existing admin action (`refresh()`'s polling of `/api/mine`,
  `/api/peers`, `/api/peers/active`, `/api/directory`; the revoke/peer/
  directory-publish actions) gets `&token=<session>` appended to its URL.
- New AI toggle in the dashboard: same visual pattern as the switches already
  built for the desktop app and `apps/search` (`AI on server: ON`/`OFF`,
  green filled pill vs. ghost button), with a caption noting it applies to
  every user of this node, not just the admin viewing the panel.

## Dependencies

- `bcrypt` crate added to `server/Cargo.toml` for password hashing. Chosen
  over `argon2`/`password-hash` for a smaller API surface (`bcrypt::hash`,
  `bcrypt::verify`) — appropriate for a single-operator local admin password
  rather than a multi-tenant credential store.

## Out of scope

- `POST /api/search`'s `vec` query parameter (the client-computed embedding
  the desktop app's search UI already attaches to its request) is currently
  never read server-side — the server always computes its own embedding via
  `embed_text` regardless. Found while tracing this change; unrelated to it,
  not touched here.
- No password reset flow. If the operator forgets the password, recovery is
  manual (delete `admin_pass.hash` on disk and go through setup again).
- No per-admin accounts — one shared password for the node, matching the
  existing single-operator model (`token.txt` is likewise one token per
  node, not per user).
