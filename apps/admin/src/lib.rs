// annessaia admin — node management as a WASM app.
//
// The desktop runtime executes WASM modules, not HTML, so the operator console
// lives here rather than in a web page. Everything it touches is an endpoint the
// server already exposes.

use annessaia_sdk::prelude::*;
use core::sync::atomic::{AtomicI32, Ordering::Relaxed};
use std::sync::Mutex;

// Which node this build administers. Defaults to a local one; override at
// compile time with ANNESSAIA_SERVER.
const SERVER: &str = match option_env!("ANNESSAIA_SERVER") {
    Some(s) => s,
    None => "http://localhost:3000",
};

// ── Palette ───────────────────────────────────────────────────────────────────

const CYAN:   Color = Color::rgb( 56, 189, 248);
const BLUE:   Color = Color::rgb( 37,  99, 235);
const GREEN:  Color = Color::rgb( 74, 222, 128);
const RED:    Color = Color::rgb(239,  68,  68);
const MUTED:  Color = Color::rgb( 71,  85, 105);
const PANEL:  Color = Color::rgb( 15,  23,  42);

// ── State ─────────────────────────────────────────────────────────────────────

static REQ:       AtomicI32 = AtomicI32::new(1);
static APPS_ID:   AtomicI32 = AtomicI32::new(-1);
static MINE_ID:   AtomicI32 = AtomicI32::new(-1);
static PEERS_ID:  AtomicI32 = AtomicI32::new(-1);
static ACTIVE_ID: AtomicI32 = AtomicI32::new(-1);
static DIR_ID:    AtomicI32 = AtomicI32::new(-1);
static ACT_ID:    AtomicI32 = AtomicI32::new(-1);

static VIEW: AtomicI32 = AtomicI32::new(0);   // 0 = apps, 1 = peers, 2 = directory

// ── Auth ──────────────────────────────────────────────────────────────────────
//
// Session tokens live only in this WASM instance's memory (see init()) — a
// fresh launch of admin.wasm always re-prompts for the password, even if the
// server that granted the previous session is still running.

static SESSION_TOKEN: Mutex<String> = Mutex::new(String::new());

static STAGE: AtomicI32 = AtomicI32::new(STAGE_LOADING);
const STAGE_LOADING:   i32 = 0;   // waiting on GET /api/admin/status
const STAGE_SETUP:     i32 = 1;   // no admin password set yet
const STAGE_LOGIN:     i32 = 2;   // password set, no valid session held
const STAGE_DASHBOARD: i32 = 3;

static STATUS_ID: AtomicI32 = AtomicI32::new(-1);
static AUTH_ID:   AtomicI32 = AtomicI32::new(-1);   // setup/login request in flight
static PASS_ERROR: Mutex<String> = Mutex::new(String::new());

struct Entry { name: String, url: String, author: String }
static APPS:   Mutex<Vec<Entry>> = Mutex::new(Vec::new());
// URLs this node published. Everything else arrived by gossip and belongs to
// whoever published it, so it is shown but cannot be deleted from here.
static MINE:   Mutex<Vec<String>> = Mutex::new(Vec::new());
static PEERS:  Mutex<Vec<String>> = Mutex::new(Vec::new());
static ACTIVE: Mutex<Vec<String>> = Mutex::new(Vec::new());
static FLASH:  Mutex<String> = Mutex::new(String::new());

struct Dir { url: String, published: bool, self_url: String, loaded: bool }
static DIR: Mutex<Dir> = Mutex::new(Dir {
    url: String::new(), published: false, self_url: String::new(), loaded: false,
});

fn next_id() -> i32 { REQ.fetch_add(1, Relaxed) }

fn flash(msg: &str) { *FLASH.lock().unwrap() = msg.to_string(); }

// ── Requests ──────────────────────────────────────────────────────────────────

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

// ── Polling ───────────────────────────────────────────────────────────────────

// Resolves a GET slot, calling `apply` on the body. Clears the slot on failure
// too, so a dead server shows an error rather than spinning forever.
fn poll_into(slot: &AtomicI32, apply: impl FnOnce(String)) {
    let id = slot.load(Relaxed);
    if id < 0 { return; }
    match net::poll_result_str(id) {
        PollStr::Pending => {}
        PollStr::Done(body) => { slot.store(-1, Relaxed); apply(body); }
        PollStr::Failed => {
            slot.store(-1, Relaxed);
            flash("Cannot reach the server. Is it running?");
        }
    }
}

fn poll_all() {
    poll_into(&APPS_ID, |body| {
        let mut v = APPS.lock().unwrap();
        v.clear();
        for line in body.lines().filter(|l| !l.trim().is_empty()) {
            let mut p = line.splitn(5, '\t');
            let name = p.next().unwrap_or("").to_string();
            let _desc = p.next();
            let url = p.next().unwrap_or("").to_string();
            let author = p.next().unwrap_or("").to_string();
            if !url.is_empty() { v.push(Entry { name, url, author }); }
        }
    });

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

    poll_action();
}

// Action replies are "ok\t<msg>" or "err\t<msg>" — the server keeps the status at
// 200 so the message survives the host's HTTP client.
fn poll_action() {
    let id = ACT_ID.load(Relaxed);
    if id < 0 { return; }
    match net::poll_result_str(id) {
        PollStr::Pending => {}
        PollStr::Failed => {
            ACT_ID.store(-1, Relaxed);
            flash("Request failed. Is the server running?");
        }
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
    }
}

fn busy() -> bool { ACT_ID.load(Relaxed) >= 0 }

// ── Entry points ──────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn init() {
    // Deliberately not restored from disk: the session lives only in this
    // WASM instance's memory, so every fresh launch of admin.wasm asks for
    // the password again, even if the server is still running from a
    // previous session.
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
            STAGE.store(if body.trim() == "1" { STAGE_LOGIN } else { STAGE_SETUP }, Relaxed);
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
                    *PASS_ERROR.lock().unwrap() = String::new();
                    STAGE.store(STAGE_DASHBOARD, Relaxed);
                }
                Some(("err", msg)) => *PASS_ERROR.lock().unwrap() = msg.to_string(),
                _ => *PASS_ERROR.lock().unwrap() = "Unexpected response from server.".to_string(),
            }
        }
    }
}

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
        text("annessaia", 28.0, CYAN);
        space(6.0);
        badge("admin", BLUE);
    });
    label("Manage this node, its peers, and its directory listing.");
    separator();

    let view = VIEW.load(Relaxed);
    row(|| {
        tab("  Apps  ",      0, view);
        tab("  Peers  ",     1, view);
        tab("  Directory  ", 2, view);
        space(12.0);
        if button_ghost(" ⟳ Refresh ") { refresh(); }
    });

    let msg = FLASH.lock().unwrap().clone();
    if !msg.is_empty() {
        space(8.0);
        row(|| {
            badge("STATUS", MUTED);
            space(8.0);
            label(&msg);
            space(8.0);
            if button_ghost(" ✕ ") { flash(""); }
        });
    }

    space(12.0);
    match view {
        0 => apps_tab(),
        1 => peers_tab(),
        _ => directory_tab(),
    }
}

fn tab(title: &str, index: i32, current: i32) {
    if current == index {
        button_styled(title, Color::WHITE, BLUE, BLUE);
    } else if button_ghost(title) {
        VIEW.store(index, Relaxed);
    }
}

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

// ── Apps ──────────────────────────────────────────────────────────────────────

fn apps_tab() {
    let apps: Vec<(String, String, String)> = APPS.lock().unwrap()
        .iter().map(|a| (a.name.clone(), a.url.clone(), a.author.clone())).collect();
    let mine = MINE.lock().unwrap().clone();

    row(|| {
        text("Live apps", 18.0, Color::WHITE);
        space(8.0);
        badge(&format!("{}", apps.len()), MUTED);
        space(6.0);
        badge(&format!("{} yours", mine.len()), GREEN);
    });
    space(8.0);

    if apps.is_empty() {
        card_color(PANEL, || {
            label("No apps yet.");
            small("Anything submitted here propagates to every connected peer.");
        });
        return;
    }

    for (name, url, author) in &apps {
        let owned = mine.iter().any(|m| m == url);
        card(|| {
            row(|| {
                text(name, 15.0, Color::WHITE);
                space(8.0);
                if owned { badge("YOURS", GREEN); space(6.0); }
                if !author.is_empty() { small(&format!("by {author}")); }
            });
            space(4.0);
            small(url);
            space(8.0);
            row(|| {
                if button_ghost(" Open ") { sys::nav(url); }
                if button_ghost(" Copy ") { sys::copy(url); }
                // Only apps published here can be deleted. Anything else belongs
                // to the node that published it and would sync straight back.
                if owned {
                    if !busy() && button_danger(" Delete ") { action("/api/revoke", url); }
                } else {
                    space(8.0);
                    small("published elsewhere");
                }
            });
        });
        space(8.0);
    }
}

// ── Peers ─────────────────────────────────────────────────────────────────────

fn peers_tab() {
    let peers  = PEERS.lock().unwrap().clone();
    let active = ACTIVE.lock().unwrap().clone();

    row(|| {
        text("Network peers", 18.0, Color::WHITE);
        space(8.0);
        badge(&format!("{} of {} connected", active.len(), peers.len()), MUTED);
    });
    space(8.0);

    card(|| {
        label("Add a peer");
        small("Paste another node's base URL. Its apps sync over immediately.");
        space(8.0);
        let url = text_field(1, "http://other-node:3000");
        space(8.0);
        if button_success("  Connect  ") && !url.is_empty() {
            action("/api/peer/connect", &url);
        }
    });
    space(12.0);

    if peers.is_empty() {
        card_color(PANEL, || {
            label("No peers yet.");
            small("Add one above, or publish this node so others can find it.");
        });
        return;
    }

    for peer in &peers {
        let live = active.contains(peer);
        card(|| {
            row(|| {
                badge(if live { "CONNECTED" } else { "OFFLINE" }, if live { GREEN } else { MUTED });
                space(8.0);
                label(peer);
            });
            space(8.0);
            if !busy() && button_danger(" Remove ") { action("/api/peer/remove", peer); }
        });
        space(8.0);
    }
}

// ── Directory ─────────────────────────────────────────────────────────────────

fn directory_tab() {
    let (url, published, self_url, loaded) = {
        let d = DIR.lock().unwrap();
        (d.url.clone(), d.published, d.self_url.clone(), d.loaded)
    };

    text("Bootstrap directory", 18.0, Color::WHITE);
    space(4.0);
    label("A public list of nodes, used only so new nodes can find their first peer.");
    small("Reading it is automatic. This node is never listed until you publish it.");
    space(12.0);

    if !loaded {
        card_color(PANEL, || { label("Loading…"); });
        return;
    }

    card(|| {
        field("Directory", &url);
        field("This node", &self_url);
        row(|| {
            small("Status");
            space(12.0);
            badge(if published { "PUBLISHED" } else { "NOT PUBLISHED" },
                  if published { GREEN } else { MUTED });
        });
    });
    space(12.0);

    row(|| {
        if busy() {
            badge("WORKING…", MUTED);
        } else if published {
            if button_danger("  Unpublish this node  ") { action("/api/directory/unpublish", ""); }
        } else if button_success("  Publish this node  ") {
            action("/api/directory/publish", "");
        }
        space(8.0);
        if !busy() && button_ghost("  Refresh peers from directory  ") {
            action("/api/directory/refresh", "");
        }
    });

    if !published {
        space(12.0);
        card_color(PANEL, || {
            colored("Publishing requires a public address.", RED);
            space(4.0);
            small("The directory fetches this node's /api/apps before listing it, so a");
            small("localhost URL is rejected. Tunnel it or host it somewhere reachable.");
        });
    }
}

fn field(name: &str, value: &str) {
    row(|| {
        small(name);
        space(12.0);
        colored(if value.is_empty() { "—" } else { value }, CYAN);
    });
    space(4.0);
}
