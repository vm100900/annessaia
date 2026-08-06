# Server Admin Auth + AI Toggle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Password-protect the previously-unauthenticated admin surface of `annessaia-server` and add a server-wide AI on/off toggle to it, per `docs/superpowers/specs/2026-08-05-server-admin-auth-design.md`.

**Architecture:** A bcrypt-hashed password (`data_dir/admin_pass.hash`) gates a set of in-memory session tokens (`AppState.admin_sessions`); an axum middleware checks a `?token=` query parameter against that set on every admin-only route. A separate `AtomicBool` (`AppState.ai_enabled`, persisted to `data_dir/ai_enabled`) is flipped only through one of those gated routes and checked at the top of `embed_text`, the single function every semantic-search/indexing call already funnels through. `apps/admin` (the WASM admin UI) gets a first-run setup screen, a login screen, and a dashboard toggle, using a new masked text-field primitive added to the host/SDK.

**Tech Stack:** Rust, axum 0.7, tokio, bcrypt (new dependency), the existing `annessaia_sdk` WASM guest API, egui (via the `annessaia` desktop host).

## Global Constraints

- Every admin/API response — success or failure — is HTTP 200 with a plain-text body (`"ok\t<msg>"`, `"err\t<msg>"`, or a bare sentinel). Never return a non-2xx status from an admin endpoint: the WASM host's `ureq`-based fetch (`annessaia/src/main.rs`, `fetch_start`/`fetch_post`) treats any non-2xx as a transport error and discards the body, so a real 401 would silently become "cannot reach the server" instead of a real message.
- The auth middleware's rejection body is the exact literal string `"unauthorized"` (no tab, no prefix) — distinct from the `"ok\t"/"err\t"` convention so callers can tell "you're not logged in" apart from an ordinary application error.
- Session tokens are 32 random lowercase-hex characters, held only in `AppState.admin_sessions` (in memory). No expiry, no persistence — a server restart is the only thing that invalidates them.
- The admin password is bcrypt-hashed before it touches disk (`data_dir/admin_pass.hash`); the plaintext is never written anywhere.
- The session token travels as a `?token=` query parameter on every gated request, GET and POST alike — not a header or cookie. The WASM host's `net::get`/`net::post` only support a URL and a body; there is no header or cookie support to extend for this feature.
- `data_dir/ai_enabled` missing on disk means AI is on (`true`) — this preserves today's always-on behavior for a node upgrading into this feature.

---

## File Structure

- `annessaia/src/main.rs` — add a masked-input variant of the existing text-field widget (host side).
- `sdk/src/lib.rs` — expose that variant to WASM guests as `text_field_secret`.
- `server/src/main.rs` — new `AppState` fields, new `/api/admin/*` endpoints, auth middleware, route wiring, `embed_text` gate. All in one file, matching how this file already holds the whole server.
- `server/Cargo.toml` — add `bcrypt` (runtime) and `tempfile` (dev/test) dependencies.
- `apps/admin/src/lib.rs` — setup/login screens, session handling, AI toggle in the dashboard.

---

### Task 1: Masked password text field (host + SDK)

**Files:**
- Modify: `annessaia/src/main.rs` (the `WidgetCmd::TextEdit` variant, its `render_widgets` arm, and the `ui_text_edit` linker import)
- Modify: `sdk/src/lib.rs` (the `raw::ui_text_edit` extern declaration and the `text_field` wrapper)

**Interfaces:**
- Produces: `annessaia_sdk::prelude::text_field_secret(id: i32, hint: &str) -> String` — same contract as the existing `text_field`, but the host renders entered characters masked. Used by Task 6's setup/login screens.

**Note on testing:** This is a rendering primitive — there's no existing harness in this codebase for asserting egui widget output (no task before this one has needed one), so this task is verified by compiling both the host and every existing WASM app, not by an automated test. That matches how the rest of the widget system (`ui_button`, `ui_checkbox`, etc.) is verified today.

- [ ] **Step 1: Add the `secret` field to `WidgetCmd::TextEdit`**

In `annessaia/src/main.rs`, find:

```rust
    TextEdit { id: i32, hint: String },
```

Replace with:

```rust
    TextEdit { id: i32, hint: String, secret: bool },
```

- [ ] **Step 2: Render it masked when `secret` is true**

Find the `render_widgets` match arm:

```rust
            // ── Text input ────────────────────────────────────────────────────
            WidgetCmd::TextEdit { id, hint } => {
                let entry = text_states.entry(*id).or_default();
                let resp = ui.add(
                    egui::TextEdit::singleline(entry)
                        .hint_text(hint.as_str())
                        .desired_width(f32::INFINITY)
                );
                if resp.changed() { upd.texts.push((*id, entry.clone())); }
            }
```

Replace with:

```rust
            // ── Text input ────────────────────────────────────────────────────
            WidgetCmd::TextEdit { id, hint, secret } => {
                let entry = text_states.entry(*id).or_default();
                let resp = ui.add(
                    egui::TextEdit::singleline(entry)
                        .hint_text(hint.as_str())
                        .desired_width(f32::INFINITY)
                        .password(*secret)
                );
                if resp.changed() { upd.texts.push((*id, entry.clone())); }
            }
```

- [ ] **Step 3: Update the existing `ui_text_edit` import and add `ui_text_edit_secret`**

Find:

```rust
    // Single-line text input — returns bytes of current text written to out_ptr, -1 if empty
    l.func_wrap("env", "ui_text_edit", |mut c: Caller<'_, HostState>, id: i32, hp: i32, hl: i32, op: i32, om: i32| -> i32 {
        let hint = read_str(&mut c, hp, hl);
        let text = c.data().text_states.get(&id).cloned().unwrap_or_default();
        c.data_mut().widget_cmds.push(WidgetCmd::TextEdit { id, hint });
        write_bytes(&mut c, op, om, text.as_bytes())
    })?;
```

Replace with:

```rust
    // Single-line text input — returns bytes of current text written to out_ptr, -1 if empty
    l.func_wrap("env", "ui_text_edit", |mut c: Caller<'_, HostState>, id: i32, hp: i32, hl: i32, op: i32, om: i32| -> i32 {
        let hint = read_str(&mut c, hp, hl);
        let text = c.data().text_states.get(&id).cloned().unwrap_or_default();
        c.data_mut().widget_cmds.push(WidgetCmd::TextEdit { id, hint, secret: false });
        write_bytes(&mut c, op, om, text.as_bytes())
    })?;
    // Masked variant of ui_text_edit — identical contract, but the host draws
    // entered characters as dots. Used for password fields.
    l.func_wrap("env", "ui_text_edit_secret", |mut c: Caller<'_, HostState>, id: i32, hp: i32, hl: i32, op: i32, om: i32| -> i32 {
        let hint = read_str(&mut c, hp, hl);
        let text = c.data().text_states.get(&id).cloned().unwrap_or_default();
        c.data_mut().widget_cmds.push(WidgetCmd::TextEdit { id, hint, secret: true });
        write_bytes(&mut c, op, om, text.as_bytes())
    })?;
```

- [ ] **Step 4: Build the host to confirm it compiles (both feature configs)**

Run: `cargo build -p annessaia && cargo build -p annessaia --no-default-features`
Expected: both succeed, no warnings.

- [ ] **Step 5: Declare `ui_text_edit_secret` in the SDK's raw import block**

In `sdk/src/lib.rs`, find:

```rust
        pub fn ui_text_edit(id: i32, hp: *const u8, hl: usize, op: *mut u8, om: usize) -> i32;
```

Add immediately after it:

```rust
        pub fn ui_text_edit_secret(id: i32, hp: *const u8, hl: usize, op: *mut u8, om: usize) -> i32;
```

- [ ] **Step 6: Add the `text_field_secret` wrapper**

Find:

```rust
    pub fn text_field(id: i32, hint: &str) -> String {
        let mut buf = vec![0u8; 4096];
        let n = unsafe { raw::ui_text_edit(id, hint.as_ptr(), hint.len(), buf.as_mut_ptr(), buf.len()) };
        if n <= 0 { return String::new(); }
        String::from_utf8_lossy(&buf[..n as usize]).into_owned()
    }
```

Add immediately after it:

```rust

    /// Same as [`text_field`], but the host masks entered characters — for
    /// password/secret entry.
    ///
    /// ```rust,no_run
    /// # use annessaia_sdk::prelude::*;
    /// let password = text_field_secret(0, "Password…");
    /// ```
    pub fn text_field_secret(id: i32, hint: &str) -> String {
        let mut buf = vec![0u8; 4096];
        let n = unsafe { raw::ui_text_edit_secret(id, hint.as_ptr(), hint.len(), buf.as_mut_ptr(), buf.len()) };
        if n <= 0 { return String::new(); }
        String::from_utf8_lossy(&buf[..n as usize]).into_owned()
    }
```

- [ ] **Step 7: Build the SDK and every existing WASM app to confirm nothing else broke**

Run: `cargo build -p annessaia-sdk && cargo build -p annessaia-search --target wasm32-unknown-unknown --release && cargo build -p annessaia-admin --target wasm32-unknown-unknown --release`
Expected: all succeed.

- [ ] **Step 8: Commit**

```bash
git add annessaia/src/main.rs sdk/src/lib.rs
git commit -m "Add masked text_field_secret widget for password entry"
```

---

### Task 2: Session and AI-toggle state on `AppState`

**Files:**
- Modify: `server/src/main.rs` (imports, `load_or_make_token`, new `random_hex`/path helpers, `AppState`, `main`'s state construction)

**Interfaces:**
- Consumes: nothing new.
- Produces:
  - `fn random_hex(len: usize) -> String`
  - `fn admin_pass_path(data_dir: &Path) -> PathBuf`
  - `fn ai_enabled_path(data_dir: &Path) -> PathBuf`
  - `AppState.admin_sessions: Mutex<HashSet<String>>`
  - `AppState.ai_enabled: AtomicBool`

- [ ] **Step 1: Add the new imports**

In `server/src/main.rs`, find:

```rust
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};
```

Replace with:

```rust
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
```

- [ ] **Step 2: Extract `random_hex` out of `load_or_make_token`**

Find:

```rust
fn load_or_make_token(data_dir: &PathBuf) -> String {
    let path = data_dir.join("token.txt");
    let existing = fs::read_to_string(&path).unwrap_or_default().trim().to_string();
    if !existing.is_empty() { return existing; }

    use rand::Rng;
    let mut rng = rand::thread_rng();
    let token: String = (0..32).map(|_| char::from_digit(rng.gen_range(0..16), 16).unwrap()).collect();
    fs::write(&path, &token).ok();
    token
}
```

Replace with:

```rust
// 32 random lowercase-hex characters — used for a node's own identity token
// (persisted, see load_or_make_token) and for admin session tokens
// (in-memory only, see AppState::admin_sessions).
fn random_hex(len: usize) -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..len).map(|_| char::from_digit(rng.gen_range(0..16), 16).unwrap()).collect()
}

fn load_or_make_token(data_dir: &PathBuf) -> String {
    let path = data_dir.join("token.txt");
    let existing = fs::read_to_string(&path).unwrap_or_default().trim().to_string();
    if !existing.is_empty() { return existing; }
    let token = random_hex(32);
    fs::write(&path, &token).ok();
    token
}

// Where the admin password hash and the AI on/off flag live on disk —
// alongside token.txt/peers.txt, same data_dir.
fn admin_pass_path(data_dir: &Path) -> PathBuf { data_dir.join("admin_pass.hash") }
fn ai_enabled_path(data_dir: &Path)  -> PathBuf { data_dir.join("ai_enabled") }
```

- [ ] **Step 3: Write the failing test for `random_hex`**

At the very end of `server/src/main.rs`, add:

```rust

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_hex_has_requested_length_and_charset() {
        let s = random_hex(32);
        assert_eq!(s.len(), 32);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn random_hex_is_not_constant() {
        // Not a proof of randomness — just a guard against a copy-paste bug
        // that returns the same string every time.
        let a = random_hex(32);
        let b = random_hex(32);
        assert_ne!(a, b);
    }
}
```

- [ ] **Step 4: Run the tests to confirm they pass**

Run: `cargo test -p annessaia-server random_hex`
Expected: both tests PASS (this is a thin wrapper around an already-working code path, so this step is confirmatory rather than red/green — there's no meaningful "fails first" state for a pure extraction).

- [ ] **Step 5: Add the new `AppState` fields**

Find:

```rust
struct AppState {
    registry: Mutex<Registry>,
    conns:      Mutex<HashMap<String, Tx>>,  // active WS connections
    connect_tx: Tx,  // send a peer URL here to initiate a new WS connection
    // None if the model failed to load (e.g. no network on first run to fetch
    // it) — degrades to keyword-only search rather than panicking. Without
    // the "ai" feature this is never read at all (Embedder is `()`, and
    // embed_text's stub doesn't touch it) — dead, not a bug, so allowed
    // rather than restructured just to silence it.
    #[cfg_attr(not(feature = "ai"), allow(dead_code))]
    embedder: Mutex<Option<Embedder>>,
}
```

Replace with:

```rust
struct AppState {
    registry: Mutex<Registry>,
    conns:      Mutex<HashMap<String, Tx>>,  // active WS connections
    connect_tx: Tx,  // send a peer URL here to initiate a new WS connection
    // None if the model failed to load (e.g. no network on first run to fetch
    // it) — degrades to keyword-only search rather than panicking. Without
    // the "ai" feature this is never read at all (Embedder is `()`, and
    // embed_text's stub doesn't touch it) — dead, not a bug, so allowed
    // rather than restructured just to silence it.
    #[cfg_attr(not(feature = "ai"), allow(dead_code))]
    embedder: Mutex<Option<Embedder>>,
    // Admin session tokens issued by /api/admin/setup and /api/admin/login.
    // In-memory only — a server restart is the only logout there is.
    admin_sessions: Mutex<HashSet<String>>,
    // Server-wide AI on/off switch. Settable only through the authenticated
    // /api/admin/ai endpoint (Task 5); read by embed_text before it touches
    // the embedder at all. Persisted to ai_enabled_path(data_dir).
    ai_enabled: AtomicBool,
}
```

- [ ] **Step 6: Load the flag and construct the new fields in `main`**

Find:

```rust
    let registry = Registry::load(data_dir.clone(), self_url.clone(), &mut embedder);

    let state: St = Arc::new(AppState {
        registry:   Mutex::new(registry),
        conns:      Mutex::new(HashMap::new()),
        connect_tx,
        embedder:   Mutex::new(embedder),
    });
```

Replace with:

```rust
    let registry = Registry::load(data_dir.clone(), self_url.clone(), &mut embedder);

    let ai_enabled = fs::read_to_string(ai_enabled_path(&data_dir))
        .map(|s| s.trim() != "0")
        .unwrap_or(true);

    let state: St = Arc::new(AppState {
        registry:   Mutex::new(registry),
        conns:      Mutex::new(HashMap::new()),
        connect_tx,
        embedder:   Mutex::new(embedder),
        admin_sessions: Mutex::new(HashSet::new()),
        ai_enabled: AtomicBool::new(ai_enabled),
    });
```

- [ ] **Step 7: Build and run the full test suite**

Run: `cargo build -p annessaia-server && cargo test -p annessaia-server`
Expected: builds clean, all tests pass.

- [ ] **Step 8: Commit**

```bash
git add server/src/main.rs
git commit -m "Add admin session and AI-toggle state to AppState"
```

---

### Task 3: Admin password endpoints (status / setup / login)

**Files:**
- Modify: `server/Cargo.toml` (add `bcrypt`, and `tempfile` as a dev-dependency)
- Modify: `server/src/main.rs` (new handlers, new `test_state` test helper)

**Interfaces:**
- Consumes: `random_hex`, `admin_pass_path`, `AppState.admin_sessions`, `ok`/`err` (from Task 2 and the pre-existing helpers at `server/src/main.rs:1323-1324`).
- Produces:
  - `async fn api_admin_status(State(st): State<St>) -> String` — `"1"` if a password is set, else `"0"`.
  - `async fn api_admin_setup(State(st): State<St>, body: Bytes) -> String`
  - `async fn api_admin_login(State(st): State<St>, body: Bytes) -> String`
  - `#[cfg(test)] fn test_state() -> (St, tempfile::TempDir)` — reused by Tasks 4 and 5.

- [ ] **Step 1: Add the new dependencies**

In `server/Cargo.toml`, find:

```toml
base64 = "0.22"
```

Replace with:

```toml
base64 = "0.22"
# Password hashing for the admin panel — a single-operator local credential,
# not a multi-tenant store, so bcrypt's small API (hash/verify) is enough;
# no need for argon2's extra configurability.
bcrypt = "0.15"

[dev-dependencies]
tower = { version = "0.4", features = ["util"] }
tempfile = "3"
```

- [ ] **Step 2: Write the failing test**

At the end of `server/src/main.rs`, inside the `mod tests` block added in Task 2, add (keep the existing `random_hex` tests above these):

```rust

    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt;

    // Builds a fully-wired AppState against a fresh temp data_dir, with no
    // embedder (matches a node's own graceful "model unavailable" startup
    // path — no network access or 1.3GB download needed to run these tests).
    // The TempDir must be kept alive for as long as the state is used, or the
    // directory is deleted out from under it.
    fn test_state() -> (St, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut embedder: Option<Embedder> = None;
        let registry = Registry::load(dir.path().to_path_buf(), "http://localhost:9999".into(), &mut embedder);
        let (connect_tx, _connect_rx) = mpsc::unbounded_channel::<String>();
        let state: St = Arc::new(AppState {
            registry: Mutex::new(registry),
            conns: Mutex::new(HashMap::new()),
            connect_tx,
            embedder: Mutex::new(embedder),
            admin_sessions: Mutex::new(HashSet::new()),
            ai_enabled: AtomicBool::new(true),
        });
        (state, dir)
    }

    async fn body_str(resp: axum::response::Response) -> String {
        String::from_utf8(to_bytes(resp.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap()
    }

    #[tokio::test]
    async fn status_is_unset_then_set_after_setup() {
        let (state, dir) = test_state();
        let app = build_router(Arc::clone(&state), dir.path());

        let resp = app.clone().oneshot(Request::get("/api/admin/status").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(body_str(resp).await, "0");

        let resp = app.clone().oneshot(Request::post("/api/admin/setup").body(Body::from("hunter2pass")).unwrap()).await.unwrap();
        assert!(body_str(resp).await.starts_with("ok\t"));

        let resp = app.oneshot(Request::get("/api/admin/status").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(body_str(resp).await, "1");
    }

    #[tokio::test]
    async fn setup_rejects_short_passwords() {
        let (state, dir) = test_state();
        let app = build_router(state, dir.path());
        let resp = app.oneshot(Request::post("/api/admin/setup").body(Body::from("short")).unwrap()).await.unwrap();
        assert_eq!(body_str(resp).await, "err\tpassword must be at least 8 characters");
    }

    #[tokio::test]
    async fn setup_is_rejected_once_a_password_already_exists() {
        let (state, dir) = test_state();
        let app = build_router(Arc::clone(&state), dir.path());
        app.clone().oneshot(Request::post("/api/admin/setup").body(Body::from("firstpassword")).unwrap()).await.unwrap();
        let resp = app.oneshot(Request::post("/api/admin/setup").body(Body::from("secondpassword")).unwrap()).await.unwrap();
        assert_eq!(body_str(resp).await, "err\tan admin password is already set");
    }

    #[tokio::test]
    async fn login_succeeds_with_the_right_password_and_fails_with_the_wrong_one() {
        let (state, dir) = test_state();
        let app = build_router(Arc::clone(&state), dir.path());
        app.clone().oneshot(Request::post("/api/admin/setup").body(Body::from("hunter2pass")).unwrap()).await.unwrap();

        let resp = app.clone().oneshot(Request::post("/api/admin/login").body(Body::from("hunter2pass")).unwrap()).await.unwrap();
        assert!(body_str(resp).await.starts_with("ok\t"));

        let resp = app.oneshot(Request::post("/api/admin/login").body(Body::from("wrong")).unwrap()).await.unwrap();
        assert_eq!(body_str(resp).await, "err\twrong password");
    }
```

- [ ] **Step 3: Run the tests to confirm they fail to compile**

Run: `cargo test -p annessaia-server admin_status_ -- --list 2>&1 | head -30`
Expected: a compile error — `api_admin_status`, `api_admin_setup`, `api_admin_login`, and `build_router` don't exist yet.

- [ ] **Step 4: Implement the endpoints**

In `server/src/main.rs`, find the `ok`/`err` helpers:

```rust
fn ok(msg: impl Into<String>)  -> String { format!("ok\t{}",  msg.into()) }
fn err(msg: impl Into<String>) -> String { format!("err\t{}", msg.into()) }
```

Add immediately after them:

```rust

// ── Admin auth ────────────────────────────────────────────────────────────────
//
// See docs/superpowers/specs/2026-08-05-server-admin-auth-design.md. All
// three endpoints below stay at HTTP 200 on every path, same reasoning as
// ok()/err() just above: the WASM host's fetch treats non-2xx as a transport
// error and throws the body away.

// GET /api/admin/status — "1" if an admin password has been set, else "0".
// Unauthenticated on purpose: it's how admin.wasm decides whether to show
// the setup screen or the login screen.
async fn api_admin_status(State(st): State<St>) -> String {
    let data_dir = st.registry.lock().unwrap().data_dir.clone();
    if admin_pass_path(&data_dir).exists() { "1".into() } else { "0".into() }
}

// POST /api/admin/setup — body: the chosen admin password. One-time only:
// fails once admin_pass.hash exists on disk. There is no reset endpoint —
// a forgotten password is recovered by deleting that file by hand.
async fn api_admin_setup(State(st): State<St>, body: Bytes) -> String {
    let password = String::from_utf8_lossy(&body).trim().to_string();
    if password.len() < 8 { return err("password must be at least 8 characters"); }

    let data_dir = st.registry.lock().unwrap().data_dir.clone();
    let path = admin_pass_path(&data_dir);
    if path.exists() { return err("an admin password is already set"); }

    let hash = match bcrypt::hash(&password, bcrypt::DEFAULT_COST) {
        Ok(h) => h,
        Err(_) => return err("could not hash password"),
    };
    if fs::write(&path, &hash).is_err() { return err("could not save password"); }

    let token = random_hex(32);
    st.admin_sessions.lock().unwrap().insert(token.clone());
    ok(token)
}

// POST /api/admin/login — body: password. Returns a fresh session token on
// success — logging in twice from two tabs yields two independent tokens,
// both valid until the server restarts.
async fn api_admin_login(State(st): State<St>, body: Bytes) -> String {
    let password = String::from_utf8_lossy(&body).trim().to_string();
    let data_dir = st.registry.lock().unwrap().data_dir.clone();
    let Ok(hash) = fs::read_to_string(admin_pass_path(&data_dir)) else {
        return err("no admin password set yet");
    };
    match bcrypt::verify(&password, hash.trim()) {
        Ok(true) => {
            let token = random_hex(32);
            st.admin_sessions.lock().unwrap().insert(token.clone());
            ok(token)
        }
        _ => err("wrong password"),
    }
}
```

- [ ] **Step 5: Extract router construction into `build_router` and wire the three new routes**

Find (in `main`):

```rust
    let app = Router::new()
        .route("/",                  get(serve_index))
        .route("/admin",             get(serve_admin))
        .route("/ws",                get(ws_endpoint))
        .route("/api/apps",          get(api_apps))
        .route("/api/search",        get(api_search))
        .route("/api/search/debug",  get(api_search_debug))
        .route("/api/submit",        post(api_submit))
        .route("/api/upload",        post(api_upload))
        .route("/api/mine",          get(api_mine))
        .route("/api/stats",         get(api_stats))
        .route("/api/reviews",       get(api_reviews))
        .route("/api/rate",          post(api_rate))
        .route("/api/open",          post(api_open))
        .route("/api/peers",         get(api_peers))
        .route("/api/peers/active",  get(api_active_peers))
        .route("/api/revoke",        post(api_revoke))
        .route("/api/peer/connect",  post(api_peer_connect))
        .route("/api/peer/remove",   post(api_peer_remove))
        .route("/api/directory",              get(api_directory))
        .route("/api/directory/publish",     post(api_directory_publish))
        .route("/api/directory/unpublish",   post(api_directory_unpublish))
        .route("/api/directory/refresh",     post(api_directory_refresh))
        // Apps uploaded through /api/upload, served straight back out.
        .nest_service("/apps", ServeDir::new(data_dir.join("apps")))
        .fallback_service(ServeDir::new("dist"))
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
        .with_state(state);
```

Replace with:

```rust
    let app = build_router(state, &data_dir);
```

Then add `build_router` itself as a new top-level function, right before `async fn main()`:

```rust
// Every route this node serves. A plain function (not inlined in `main`) so
// tests can build the exact same router against a throwaway AppState instead
// of a partial hand-rolled copy that could drift from what actually runs.
//
// Tasks 4 and 5 will move some of these routes into an auth-gated group and
// add the AI-toggle route; for now this is a straight lift of the router
// `main` already built, unchanged in behavior.
fn build_router(state: St, data_dir: &Path) -> Router {
    Router::new()
        .route("/",                  get(serve_index))
        .route("/admin",             get(serve_admin))
        .route("/ws",                get(ws_endpoint))
        .route("/api/apps",          get(api_apps))
        .route("/api/search",        get(api_search))
        .route("/api/search/debug",  get(api_search_debug))
        .route("/api/submit",        post(api_submit))
        .route("/api/upload",        post(api_upload))
        .route("/api/mine",          get(api_mine))
        .route("/api/stats",         get(api_stats))
        .route("/api/reviews",       get(api_reviews))
        .route("/api/rate",          post(api_rate))
        .route("/api/open",          post(api_open))
        .route("/api/peers",         get(api_peers))
        .route("/api/peers/active",  get(api_active_peers))
        .route("/api/revoke",        post(api_revoke))
        .route("/api/peer/connect",  post(api_peer_connect))
        .route("/api/peer/remove",   post(api_peer_remove))
        .route("/api/directory",              get(api_directory))
        .route("/api/directory/publish",     post(api_directory_publish))
        .route("/api/directory/unpublish",   post(api_directory_unpublish))
        .route("/api/directory/refresh",     post(api_directory_refresh))
        .route("/api/admin/status",  get(api_admin_status))
        .route("/api/admin/setup",   post(api_admin_setup))
        .route("/api/admin/login",   post(api_admin_login))
        // Apps uploaded through /api/upload, served straight back out.
        .nest_service("/apps", ServeDir::new(data_dir.join("apps")))
        .fallback_service(ServeDir::new("dist"))
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
        .with_state(state)
}
```

- [ ] **Step 6: Run the tests to confirm they pass**

Run: `cargo test -p annessaia-server`
Expected: all tests, including the four new ones, PASS.

- [ ] **Step 7: Commit**

```bash
git add server/Cargo.toml server/Cargo.lock server/src/main.rs
git commit -m "Add admin password setup/login endpoints"
```

---

### Task 4: Auth middleware and route migration

**Files:**
- Modify: `server/src/main.rs` (imports, new `require_admin` middleware, `build_router`)

**Interfaces:**
- Consumes: `AppState.admin_sessions` (Task 2), `build_router` (Task 3), `test_state` (Task 3).
- Produces: `async fn require_admin(...) -> Response` — not called directly by later tasks, only referenced from `build_router`.

- [ ] **Step 1: Add the imports `require_admin` needs**

Find:

```rust
use axum::{
    Router,
    body::Bytes,
    extract::{Query, State, WebSocketUpgrade},
    extract::ws::{Message as AxMsg, WebSocket},
    response::{IntoResponse, Redirect},
    routing::{get, post},
};
```

Replace with:

```rust
use axum::{
    Router,
    body::Bytes,
    extract::{Query, State, WebSocketUpgrade, Request},
    extract::ws::{Message as AxMsg, WebSocket},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
```

- [ ] **Step 2: Write the failing tests**

Add to the `mod tests` block, after the login tests from Task 3:

```rust

    #[tokio::test]
    async fn admin_routes_reject_a_missing_token() {
        let (state, dir) = test_state();
        let app = build_router(state, dir.path());
        let resp = app.oneshot(Request::get("/api/peers").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        assert_eq!(body_str(resp).await, "unauthorized");
    }

    #[tokio::test]
    async fn admin_routes_reject_an_unknown_token() {
        let (state, dir) = test_state();
        let app = build_router(state, dir.path());
        let resp = app.oneshot(Request::get("/api/peers?token=not-a-real-session").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(body_str(resp).await, "unauthorized");
    }

    #[tokio::test]
    async fn admin_routes_accept_a_valid_session_token() {
        let (state, dir) = test_state();
        let app = build_router(Arc::clone(&state), dir.path());
        let resp = app.clone().oneshot(Request::post("/api/admin/setup").body(Body::from("hunter2pass")).unwrap()).await.unwrap();
        let token = body_str(resp).await.strip_prefix("ok\t").unwrap().to_string();

        let resp = app.oneshot(Request::get(&format!("/api/peers?token={token}")).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        // No peers yet, but the request got through: empty, not "unauthorized".
        assert_eq!(body_str(resp).await, "");
    }

    #[tokio::test]
    async fn public_routes_do_not_require_a_token() {
        let (state, dir) = test_state();
        let app = build_router(state, dir.path());
        let resp = app.oneshot(Request::get("/api/apps").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        assert_ne!(body_str(resp).await, "unauthorized");
    }
```

- [ ] **Step 3: Run the tests to confirm they fail**

Run: `cargo test -p annessaia-server admin_routes_ -- --list`
Expected: `admin_routes_reject_a_missing_token` and `admin_routes_reject_an_unknown_token` currently FAIL (today, `/api/peers` answers with no token needed at all — `assert_eq!(body_str(resp).await, "unauthorized")` fails because the real body is empty, not `"unauthorized"`).

- [ ] **Step 4: Implement `require_admin` and move the admin-only routes behind it**

Add `require_admin` right after `build_router`'s doc comment area — insert it as a new function directly above `fn build_router`:

```rust
// Auth gate for every admin-only route (see build_router). Checks a
// `?token=` query parameter against the in-memory session set — not a
// header or cookie, because the WASM host's net::get/net::post only support
// a URL and a body (see Global Constraints in the design doc). Always
// answers with HTTP 200: on rejection the body is the bare sentinel
// "unauthorized", distinct from the ok\t/err\t convention used elsewhere, so
// admin.wasm can tell "not logged in" apart from an ordinary error.
async fn require_admin(
    State(st): State<St>,
    Query(q): Query<HashMap<String, String>>,
    req: Request,
    next: Next,
) -> Response {
    let token = q.get("token").cloned().unwrap_or_default();
    if !token.is_empty() && st.admin_sessions.lock().unwrap().contains(&token) {
        next.run(req).await
    } else {
        "unauthorized".into_response()
    }
}
```

Then replace `build_router` (from Task 3) with:

```rust
fn build_router(state: St, data_dir: &Path) -> Router {
    // Everything reachable only from admin.wasm today (revoke, peer
    // management, directory publish/unpublish/refresh) plus the AI toggle
    // added in Task 5 — closing the open-access gap this feature exists for.
    let admin_routes = Router::new()
        .route("/api/mine",          get(api_mine))
        .route("/api/peers",         get(api_peers))
        .route("/api/peers/active",  get(api_active_peers))
        .route("/api/revoke",        post(api_revoke))
        .route("/api/peer/connect",  post(api_peer_connect))
        .route("/api/peer/remove",   post(api_peer_remove))
        .route("/api/directory",              get(api_directory))
        .route("/api/directory/publish",     post(api_directory_publish))
        .route("/api/directory/unpublish",   post(api_directory_unpublish))
        .route("/api/directory/refresh",     post(api_directory_refresh))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_admin));

    Router::new()
        .route("/",                  get(serve_index))
        .route("/admin",             get(serve_admin))
        .route("/ws",                get(ws_endpoint))
        .route("/api/apps",          get(api_apps))
        .route("/api/search",        get(api_search))
        .route("/api/search/debug",  get(api_search_debug))
        .route("/api/submit",        post(api_submit))
        .route("/api/upload",        post(api_upload))
        .route("/api/stats",         get(api_stats))
        .route("/api/reviews",       get(api_reviews))
        .route("/api/rate",          post(api_rate))
        .route("/api/open",          post(api_open))
        .route("/api/admin/status",  get(api_admin_status))
        .route("/api/admin/setup",   post(api_admin_setup))
        .route("/api/admin/login",   post(api_admin_login))
        .merge(admin_routes)
        // Apps uploaded through /api/upload, served straight back out.
        .nest_service("/apps", ServeDir::new(data_dir.join("apps")))
        .fallback_service(ServeDir::new("dist"))
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any))
        .with_state(state)
}
```

- [ ] **Step 5: Run the tests to confirm they pass**

Run: `cargo test -p annessaia-server`
Expected: all tests, including the four new ones, PASS.

- [ ] **Step 6: Commit**

```bash
git add server/src/main.rs
git commit -m "Gate the admin API surface behind session-token auth"
```

---

### Task 5: AI toggle endpoint and `embed_text` gate

**Files:**
- Modify: `server/src/main.rs` (new `/api/admin/ai` handlers, `build_router`, `embed_text`)

**Interfaces:**
- Consumes: `AppState.ai_enabled` (Task 2), `admin_routes` group (Task 4), `ai_enabled_path` (Task 2).
- Produces: `async fn api_admin_ai_get`, `async fn api_admin_ai_set` — not consumed elsewhere; this is the toggle's only entry point.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests`, after Task 4's tests:

```rust

    #[tokio::test]
    async fn ai_toggle_defaults_on_and_persists_the_chosen_state() {
        let (state, dir) = test_state();
        let app = build_router(Arc::clone(&state), dir.path());

        let resp = app.clone().oneshot(Request::post("/api/admin/setup").body(Body::from("hunter2pass")).unwrap()).await.unwrap();
        let token = body_str(resp).await.strip_prefix("ok\t").unwrap().to_string();

        let resp = app.clone().oneshot(Request::get(&format!("/api/admin/ai?token={token}")).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(body_str(resp).await, "1");

        let resp = app.clone().oneshot(Request::post(&format!("/api/admin/ai?token={token}")).body(Body::from("0")).unwrap()).await.unwrap();
        assert_eq!(body_str(resp).await, "ok\tAI disabled");

        assert!(!state.ai_enabled.load(Ordering::Relaxed));
        assert_eq!(fs::read_to_string(dir.path().join("ai_enabled")).unwrap().trim(), "0");

        let resp = app.oneshot(Request::get(&format!("/api/admin/ai?token={token}")).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(body_str(resp).await, "0");
    }

    #[tokio::test]
    async fn ai_toggle_requires_a_session() {
        let (state, dir) = test_state();
        let app = build_router(state, dir.path());
        let resp = app.oneshot(Request::get("/api/admin/ai").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(body_str(resp).await, "unauthorized");
    }

    #[tokio::test]
    async fn embed_text_short_circuits_when_ai_is_disabled() {
        let (state, _dir) = test_state();
        state.ai_enabled.store(false, Ordering::Relaxed);
        let result = embed_text(&state, "anything".into()).await;
        assert!(result.is_none());
    }
```

- [ ] **Step 2: Run the tests to confirm they fail**

Run: `cargo test -p annessaia-server ai_toggle -- --list`
Expected: compile error — `api_admin_ai_get`/`api_admin_ai_set` don't exist, and `/api/admin/ai` isn't routed.

- [ ] **Step 3: Implement the toggle endpoints**

Add right after `api_admin_login` (from Task 3):

```rust

// GET /api/admin/ai (auth-gated) — current AI-enabled state, "1" or "0".
async fn api_admin_ai_get(State(st): State<St>) -> String {
    if st.ai_enabled.load(Ordering::Relaxed) { "1".into() } else { "0".into() }
}

// POST /api/admin/ai (auth-gated) — body "1" or "0". Applies to every user
// of this node immediately: embed_text checks this flag before it does
// anything else.
async fn api_admin_ai_set(State(st): State<St>, body: Bytes) -> String {
    let enabled = match String::from_utf8_lossy(&body).trim() {
        "1" => true,
        "0" => false,
        _ => return err("expected \"1\" or \"0\""),
    };
    st.ai_enabled.store(enabled, Ordering::Relaxed);
    let data_dir = st.registry.lock().unwrap().data_dir.clone();
    let _ = fs::write(ai_enabled_path(&data_dir), if enabled { "1" } else { "0" });
    ok(if enabled { "AI enabled" } else { "AI disabled" })
}
```

- [ ] **Step 4: Route it into the admin group**

In `build_router`, find:

```rust
        .route("/api/directory/refresh",     post(api_directory_refresh))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_admin));
```

Replace with:

```rust
        .route("/api/directory/refresh",     post(api_directory_refresh))
        .route("/api/admin/ai", get(api_admin_ai_get).post(api_admin_ai_set))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_admin));
```

- [ ] **Step 5: Gate `embed_text` itself**

Find:

```rust
#[cfg(feature = "ai")]
async fn embed_text(st: &St, text: String) -> Option<Vec<f32>> {
    let st = Arc::clone(st);
    tokio::task::spawn_blocking(move || {
        let mut guard = st.embedder.lock().unwrap();
        let model = guard.as_mut()?;
        model.embed(vec![text], None).ok()?.into_iter().next()
    }).await.ok().flatten()
}
```

Replace with:

```rust
#[cfg(feature = "ai")]
async fn embed_text(st: &St, text: String) -> Option<Vec<f32>> {
    if !st.ai_enabled.load(Ordering::Relaxed) { return None; }
    let st = Arc::clone(st);
    tokio::task::spawn_blocking(move || {
        let mut guard = st.embedder.lock().unwrap();
        let model = guard.as_mut()?;
        model.embed(vec![text], None).ok()?.into_iter().next()
    }).await.ok().flatten()
}
```

- [ ] **Step 6: Run the tests to confirm they pass**

Run: `cargo test -p annessaia-server`
Expected: all tests PASS.

Note: `embed_text_short_circuits_when_ai_is_disabled` proves the toggle is respected, not that the early return specifically happens *before* the embedder lock (that would need a real loaded model — not something to pull into a unit test run). With `embedder: None` in `test_state`, the untoggled function already returns `None` too, so this test's real job is guarding against the flag being ignored entirely, e.g. a copy-paste that checks the wrong field.

- [ ] **Step 7: Commit**

```bash
git add server/src/main.rs
git commit -m "Add server-wide AI toggle, gated behind admin auth"
```

---

### Task 6: `admin.wasm` — setup and login flow

**Files:**
- Modify: `apps/admin/src/lib.rs`

**Interfaces:**
- Consumes: `text_field_secret` (Task 1), `GET/POST /api/admin/status`, `/api/admin/setup`, `/api/admin/login` (Tasks 3–4).
- Produces: `SESSION_TOKEN`, `STAGE` (and the `STAGE_*` constants), `fn handle_unauthorized(body: &str) -> bool` — the last one is reused by Task 7's AI-toggle poller.

**Note on testing:** There's no harness in this codebase for testing WASM guest UI code in isolation (it only runs meaningfully inside the wasmtime host). This task is verified by building the wasm binary and manually walking through the flow against a real server, the same way the existing admin/search apps have always been verified.

- [ ] **Step 1: Add session/stage state**

In `apps/admin/src/lib.rs`, find:

```rust
static VIEW: AtomicI32 = AtomicI32::new(0);   // 0 = apps, 1 = peers, 2 = directory
```

Add immediately after it:

```rust

// ── Auth ──────────────────────────────────────────────────────────────────────

const SESSION_KEY: &str = "admin_session";

static SESSION_TOKEN: Mutex<String> = Mutex::new(String::new());

static STAGE: AtomicI32 = AtomicI32::new(STAGE_LOADING);
const STAGE_LOADING:   i32 = 0;   // waiting on GET /api/admin/status
const STAGE_SETUP:     i32 = 1;   // no admin password set yet
const STAGE_LOGIN:     i32 = 2;   // password set, no valid session held
const STAGE_DASHBOARD: i32 = 3;

static STATUS_ID: AtomicI32 = AtomicI32::new(-1);
static AUTH_ID:   AtomicI32 = AtomicI32::new(-1);   // setup/login request in flight
static PASS_ERROR: Mutex<String> = Mutex::new(String::new());
```

- [ ] **Step 2: Replace `init()` to check status first, and add the pollers**

Find:

```rust
#[no_mangle]
pub extern "C" fn init() { refresh(); }
```

Replace with:

```rust
#[no_mangle]
pub extern "C" fn init() {
    if let Some(saved) = storage::get_str(SESSION_KEY) {
        *SESSION_TOKEN.lock().unwrap() = saved;
    }
    let id = next_id();
    STATUS_ID.store(id, Relaxed);
    net::get(id, &format!("{SERVER}/api/admin/status"));
}

// True if `body` is the auth middleware's rejection sentinel (see
// server/src/main.rs require_admin) — resets the held session and sends the
// user back to the login screen. Checked first by every poller that hits a
// token-gated endpoint: a stale token (e.g. after the server restarted) must
// not be handed to that poller's own parser as if it were real data.
fn handle_unauthorized(body: &str) -> bool {
    if body.trim() != "unauthorized" { return false; }
    *SESSION_TOKEN.lock().unwrap() = String::new();
    storage::del(SESSION_KEY);
    STAGE.store(STAGE_LOGIN, Relaxed);
    flash("Session expired — please log in again.");
    true
}

fn poll_status() {
    let id = STATUS_ID.load(Relaxed);
    if id < 0 { return; }
    match net::poll_result_str(id) {
        PollStr::Pending => {}
        PollStr::Failed => {
            STATUS_ID.store(-1, Relaxed);
            flash("Cannot reach the server. Is it running?");
        }
        PollStr::Done(body) => {
            STATUS_ID.store(-1, Relaxed);
            let has_session = !SESSION_TOKEN.lock().unwrap().is_empty();
            STAGE.store(
                if body.trim() != "1" {
                    STAGE_SETUP
                } else if has_session {
                    STAGE_DASHBOARD
                } else {
                    STAGE_LOGIN
                },
                Relaxed,
            );
        }
    }
}

fn poll_auth() {
    let id = AUTH_ID.load(Relaxed);
    if id < 0 { return; }
    match net::poll_result_str(id) {
        PollStr::Pending => {}
        PollStr::Failed => {
            AUTH_ID.store(-1, Relaxed);
            *PASS_ERROR.lock().unwrap() = "Cannot reach the server. Is it running?".to_string();
        }
        PollStr::Done(body) => {
            AUTH_ID.store(-1, Relaxed);
            match body.split_once('\t') {
                Some(("ok", token)) => {
                    *SESSION_TOKEN.lock().unwrap() = token.to_string();
                    storage::set_str(SESSION_KEY, token);
                    *PASS_ERROR.lock().unwrap() = String::new();
                    STAGE.store(STAGE_DASHBOARD, Relaxed);
                }
                Some(("err", msg)) => *PASS_ERROR.lock().unwrap() = msg.to_string(),
                _ => *PASS_ERROR.lock().unwrap() = "Unexpected response from server.".to_string(),
            }
        }
    }
}
```

- [ ] **Step 3: Gate `refresh()`/`action()` on the session token, and check `handle_unauthorized` in every gated poller**

Find:

```rust
fn refresh() {
    for (slot, path) in [
        (&APPS_ID,   "/api/apps"),
        (&MINE_ID,   "/api/mine"),
        (&PEERS_ID,  "/api/peers"),
        (&ACTIVE_ID, "/api/peers/active"),
        (&DIR_ID,    "/api/directory"),
    ] {
        let id = next_id();
        slot.store(id, Relaxed);
        net::get(id, &format!("{SERVER}{path}"));
    }
}

// Fire-and-track a POST. The reply is picked up by poll_action().
fn action(path: &str, body: &str) {
    let id = next_id();
    ACT_ID.store(id, Relaxed);
    flash("Working…");
    net::post(id, &format!("{SERVER}{path}"), body);
}
```

Replace with:

```rust
fn refresh() {
    let token = SESSION_TOKEN.lock().unwrap().clone();
    for (slot, path) in [
        (&APPS_ID,   "/api/apps"),
        (&MINE_ID,   "/api/mine"),
        (&PEERS_ID,  "/api/peers"),
        (&ACTIVE_ID, "/api/peers/active"),
        (&DIR_ID,    "/api/directory"),
    ] {
        let id = next_id();
        slot.store(id, Relaxed);
        net::get(id, &format!("{SERVER}{path}?token={token}"));
    }
}

// Fire-and-track a POST. The reply is picked up by poll_action().
fn action(path: &str, body: &str) {
    let id = next_id();
    ACT_ID.store(id, Relaxed);
    flash("Working…");
    let token = SESSION_TOKEN.lock().unwrap().clone();
    net::post(id, &format!("{SERVER}{path}?token={token}"), body);
}
```

Find (inside `poll_all`):

```rust
    poll_into(&MINE_ID, |body| {
        *MINE.lock().unwrap() =
            body.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    });

    poll_into(&PEERS_ID, |body| {
        *PEERS.lock().unwrap() =
            body.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    });

    poll_into(&ACTIVE_ID, |body| {
        *ACTIVE.lock().unwrap() =
            body.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    });

    poll_into(&DIR_ID, |body| {
        let line = body.lines().next().unwrap_or("");
        let mut p = line.splitn(3, '\t');
        let mut d = DIR.lock().unwrap();
        d.url       = p.next().unwrap_or("").to_string();
        d.published = p.next().unwrap_or("false") == "true";
        d.self_url  = p.next().unwrap_or("").to_string();
        d.loaded    = true;
    });
```

Replace with:

```rust
    poll_into(&MINE_ID, |body| {
        if handle_unauthorized(&body) { return; }
        *MINE.lock().unwrap() =
            body.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    });

    poll_into(&PEERS_ID, |body| {
        if handle_unauthorized(&body) { return; }
        *PEERS.lock().unwrap() =
            body.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    });

    poll_into(&ACTIVE_ID, |body| {
        if handle_unauthorized(&body) { return; }
        *ACTIVE.lock().unwrap() =
            body.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    });

    poll_into(&DIR_ID, |body| {
        if handle_unauthorized(&body) { return; }
        let line = body.lines().next().unwrap_or("");
        let mut p = line.splitn(3, '\t');
        let mut d = DIR.lock().unwrap();
        d.url       = p.next().unwrap_or("").to_string();
        d.published = p.next().unwrap_or("false") == "true";
        d.self_url  = p.next().unwrap_or("").to_string();
        d.loaded    = true;
    });
```

Find `poll_action`'s `PollStr::Done` arm:

```rust
        PollStr::Done(body) => {
            ACT_ID.store(-1, Relaxed);
            let line = body.lines().next().unwrap_or("").to_string();
            match line.split_once('\t') {
                Some(("err", msg)) => flash(msg),
                Some(("ok",  msg)) => flash(msg),
                _ => flash(if line.is_empty() { "Done." } else { &line }),
            }
            refresh();
        }
```

Replace with:

```rust
        PollStr::Done(body) => {
            ACT_ID.store(-1, Relaxed);
            if handle_unauthorized(&body) { return; }
            let line = body.lines().next().unwrap_or("").to_string();
            match line.split_once('\t') {
                Some(("err", msg)) => flash(msg),
                Some(("ok",  msg)) => flash(msg),
                _ => flash(if line.is_empty() { "Done." } else { &line }),
            }
            refresh();
        }
```

- [ ] **Step 4: Branch `render()` on `STAGE`, and add the setup/login screens**

Find:

```rust
#[no_mangle]
pub extern "C" fn render() {
    poll_all();

    row(|| {
```

Replace with:

```rust
#[no_mangle]
pub extern "C" fn render() {
    poll_status();
    poll_auth();

    match STAGE.load(Relaxed) {
        STAGE_LOADING => { label("Checking server…"); return; }
        STAGE_SETUP   => { setup_screen(); return; }
        STAGE_LOGIN   => { login_screen(); return; }
        _ => {}
    }

    poll_all();

    row(|| {
```

Then add the two screen functions and a shared password-form helper. Insert them right after `fn tab(...)` (which follows `render()`):

```rust
fn password_form(
    title: &str,
    hint1: &str,
    hint2: Option<&str>,
    submit_label: &str,
    on_submit: fn(String),
) {
    space(120.0);
    card(|| {
        text(title, 20.0, Color::WHITE);
        space(6.0);
        small("This protects everything on this page — revoking apps, managing peers,");
        small("publishing to the directory, and the AI toggle. There is no reset button,");
        small("so don't lose it.");
        space(12.0);
        let pass = text_field_secret(2, hint1);
        let confirm = hint2.map(|h| { space(6.0); text_field_secret(3, h) });
        space(10.0);
        let error = PASS_ERROR.lock().unwrap().clone();
        if !error.is_empty() { colored(&error, RED); space(8.0); }
        let busy = AUTH_ID.load(Relaxed) >= 0;
        if busy {
            badge("WORKING…", MUTED);
        } else if button_success(submit_label) {
            if pass.len() < 8 {
                *PASS_ERROR.lock().unwrap() = "Password must be at least 8 characters.".to_string();
            } else if confirm.as_ref().is_some_and(|c| c != &pass) {
                *PASS_ERROR.lock().unwrap() = "Passwords don't match.".to_string();
            } else {
                *PASS_ERROR.lock().unwrap() = String::new();
                on_submit(pass);
            }
        }
    });
}

fn setup_screen() {
    password_form(
        "Create an admin password",
        "Password (at least 8 characters)",
        Some("Confirm password"),
        "  Create password  ",
        |pass| {
            let id = next_id();
            AUTH_ID.store(id, Relaxed);
            net::post(id, &format!("{SERVER}/api/admin/setup"), &pass);
        },
    );
}

fn login_screen() {
    password_form(
        "Admin login",
        "Password",
        None,
        "  Log in  ",
        |pass| {
            let id = next_id();
            AUTH_ID.store(id, Relaxed);
            net::post(id, &format!("{SERVER}/api/admin/login"), &pass);
        },
    );
}
```

- [ ] **Step 5: Build the wasm binary**

Run: `cargo build -p annessaia-admin --target wasm32-unknown-unknown --release`
Expected: succeeds, no warnings.

- [ ] **Step 6: Manually verify against a real server**

```bash
# Terminal 1 — fresh data dir so the setup screen actually shows.
rm -rf server/data-admin-test
ANNESSAIA_DATA=server/data-admin-test PORT=3000 cargo run -p annessaia-server

# Terminal 2 — copy the freshly built admin.wasm where the server serves it from,
# then open it in the desktop app.
cp target/wasm32-unknown-unknown/release/annessaia_admin.wasm server/data-admin-test/../dist/admin.wasm 2>/dev/null || \
cp target/wasm32-unknown-unknown/release/annessaia_admin.wasm dist/admin.wasm
cargo run -p annessaia --release dist/admin.wasm
```

Expected, in the desktop window: "Create an admin password" screen appears first. Entering two different passwords in the two fields shows "Passwords don't match." Entering matching 8+ character passwords and clicking "Create password" lands on the normal Apps/Peers/Directory dashboard. Quitting and relaunching `cargo run -p annessaia --release dist/admin.wasm` (server still running) goes straight to the dashboard, no re-login. Restarting the server (Ctrl-C, `cargo run` again) and then clicking Refresh in the still-open admin app shows "Session expired — please log in again." and drops back to the login screen; logging in again with the same password returns to the dashboard.

- [ ] **Step 7: Commit**

```bash
git add apps/admin/src/lib.rs
git commit -m "Add password setup/login flow to the admin panel"
```

---

### Task 7: `admin.wasm` — AI toggle in the dashboard

**Files:**
- Modify: `apps/admin/src/lib.rs`

**Interfaces:**
- Consumes: `GET/POST /api/admin/ai` (Task 5), `action`/`refresh`/`handle_unauthorized` (Task 6).
- Produces: nothing consumed elsewhere — this is the last task.

- [ ] **Step 1: Add AI state and wire it into `refresh()`/`poll_all()`**

Find:

```rust
struct Dir { url: String, published: bool, self_url: String, loaded: bool }
static DIR: Mutex<Dir> = Mutex::new(Dir {
    url: String::new(), published: false, self_url: String::new(), loaded: false,
});
```

Add immediately after it:

```rust

static AI_ID: AtomicI32 = AtomicI32::new(-1);
static AI_ON: Mutex<Option<bool>> = Mutex::new(None);   // None until first loaded
```

Find, in `refresh()`:

```rust
    for (slot, path) in [
        (&APPS_ID,   "/api/apps"),
        (&MINE_ID,   "/api/mine"),
        (&PEERS_ID,  "/api/peers"),
        (&ACTIVE_ID, "/api/peers/active"),
        (&DIR_ID,    "/api/directory"),
    ] {
```

Replace with:

```rust
    for (slot, path) in [
        (&APPS_ID,   "/api/apps"),
        (&MINE_ID,   "/api/mine"),
        (&PEERS_ID,  "/api/peers"),
        (&ACTIVE_ID, "/api/peers/active"),
        (&DIR_ID,    "/api/directory"),
        (&AI_ID,     "/api/admin/ai"),
    ] {
```

Find, in `poll_all()` (right after the `DIR_ID` block added in Task 6):

```rust
    poll_into(&DIR_ID, |body| {
        if handle_unauthorized(&body) { return; }
        let line = body.lines().next().unwrap_or("");
        let mut p = line.splitn(3, '\t');
        let mut d = DIR.lock().unwrap();
        d.url       = p.next().unwrap_or("").to_string();
        d.published = p.next().unwrap_or("false") == "true";
        d.self_url  = p.next().unwrap_or("").to_string();
        d.loaded    = true;
    });

    poll_action();
```

Replace with:

```rust
    poll_into(&DIR_ID, |body| {
        if handle_unauthorized(&body) { return; }
        let line = body.lines().next().unwrap_or("");
        let mut p = line.splitn(3, '\t');
        let mut d = DIR.lock().unwrap();
        d.url       = p.next().unwrap_or("").to_string();
        d.published = p.next().unwrap_or("false") == "true";
        d.self_url  = p.next().unwrap_or("").to_string();
        d.loaded    = true;
    });

    poll_into(&AI_ID, |body| {
        if handle_unauthorized(&body) { return; }
        *AI_ON.lock().unwrap() = Some(body.trim() == "1");
    });

    poll_action();
```

- [ ] **Step 2: Add the toggle widget and place it next to Refresh**

Find:

```rust
    row(|| {
        tab("  Apps  ",      0, view);
        tab("  Peers  ",     1, view);
        tab("  Directory  ", 2, view);
        space(12.0);
        if button_ghost(" ⟳ Refresh ") { refresh(); }
    });
```

Replace with:

```rust
    row(|| {
        tab("  Apps  ",      0, view);
        tab("  Peers  ",     1, view);
        tab("  Directory  ", 2, view);
        space(12.0);
        if button_ghost(" ⟳ Refresh ") { refresh(); }
        space(12.0);
        ai_toggle();
    });
```

Then add `ai_toggle`, right after `fn tab(...)`:

```rust
// Server-wide switch: applies to every user of this node, not just the
// admin viewing this panel. Nothing renders until the first /api/admin/ai
// poll resolves, same as the directory tab's "Loading…" gate.
fn ai_toggle() {
    let on = *AI_ON.lock().unwrap();
    let Some(on) = on else { return; };
    if on {
        if button_styled(" AI on server: ON ", Color::WHITE, Color::rgb(20, 83, 45), GREEN) {
            action("/api/admin/ai", "0");
        }
    } else if button_ghost(" AI on server: OFF ") {
        action("/api/admin/ai", "1");
    }
}
```

- [ ] **Step 3: Build the wasm binary**

Run: `cargo build -p annessaia-admin --target wasm32-unknown-unknown --release`
Expected: succeeds, no warnings.

- [ ] **Step 4: Manually verify against a real server**

Reuse the same two-terminal setup as Task 6 Step 6 (server + desktop app pointed at the freshly built `admin.wasm`), logged in from that task.

Expected: after logging in, the tabs row shows an " AI on server: ON " green pill next to Refresh. Clicking it flips to " AI on server: OFF " (ghost style) and a "AI disabled" status message appears. Restart the server and confirm (via `cat server/data-admin-test/ai_enabled`) it now contains `0`. Log back in and click the toggle again — it returns to ON and `ai_enabled` reads `1`.

- [ ] **Step 5: Clean up the manual test data directory**

```bash
rm -rf server/data-admin-test
```

- [ ] **Step 6: Commit**

```bash
git add apps/admin/src/lib.rs
git commit -m "Add server-wide AI toggle to the admin dashboard"
```

---

## Self-Review Notes

- **Spec coverage:** every section of the design doc maps to a task — persisted state (Task 2), the three auth endpoints (Task 3), the auth-gated route group (Task 4), the AI toggle + `embed_text` gate (Task 5), and the `admin.wasm` UI changes (Tasks 6–7). The masked-field primitive wasn't named in the spec explicitly but is required to implement "password protected" correctly, so it's Task 1.
- **Placeholder scan:** no TBDs; every step has real code.
- **Type consistency:** `text_field_secret`, `handle_unauthorized`, `build_router`, `require_admin`, `AI_ON`/`AI_ID` names and signatures are consistent everywhere they're referenced across tasks.
