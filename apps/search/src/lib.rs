use annessaia_sdk::prelude::*;
use core::sync::atomic::{AtomicI32, Ordering::Relaxed};
use std::sync::Mutex;

// Which registry this build talks to. Defaults to a local node; set
// ANNESSAIA_SERVER at compile time to bake in a public one (that is how the
// copy hosted on the bootstrap Worker is produced).
const SERVER: &str = match option_env!("ANNESSAIA_SERVER") {
    Some(s) => s,
    None => "http://localhost:3000",
};

// The copy hosted on the bootstrap Worker is built read-only. That index accepts
// entries only by gossip from a running node, so offering a submit form there
// would just be a button that always fails — publishing means running a node.
const READ_ONLY: bool = match option_env!("ANNESSAIA_READONLY") {
    Some(_) => true,
    None => false,
};

// ── State ─────────────────────────────────────────────────────────────────────

static REQ_CTR:       AtomicI32 = AtomicI32::new(1);
static SEARCH_ID:     AtomicI32 = AtomicI32::new(-1); // -1 = idle
static SUBMIT_ID:     AtomicI32 = AtomicI32::new(-1);
static VIEW:          AtomicI32 = AtomicI32::new(0);  // 0=search 1=submit
static SUBMIT_STATUS: AtomicI32 = AtomicI32::new(0);  // 0=idle 1=pending 2=ok 3=err

// Where the app being submitted lives. This node is already a web server, so it
// can host the file itself rather than making people find hosting first.
static HOST_MODE: AtomicI32 = AtomicI32::new(0);      // 0 = upload here, 1 = my own URL
static UPLOAD_ID: AtomicI32 = AtomicI32::new(-1);

// Results are paged rather than dumped in full — the index is meant to grow.
const PER_PAGE: usize = 8;
static PAGE: AtomicI32 = AtomicI32::new(0);

// Embedding the query happens on the host, on-device — no external service, and
// it works the same whether SERVER is a self-hosted node or the read-only
// bootstrap Worker, since the vector rides along on the request rather than
// needing the server to run a model itself. Fired alongside the plain keyword
// search rather than blocked on: the first, potentially slow, call this session
// (which may need to download a model) must not delay results the keyword
// search can already show.
static EMBED_ID: AtomicI32 = AtomicI32::new(-1);
// The query text the pending embedding belongs to, so its result can be
// attached to a follow-up search once it resolves.
static PENDING_QUERY: Mutex<String> = Mutex::new(String::new());
// 0=idle 1=computing 2=unavailable. Distinct from EMBED_ID so the UI can say
// which — the first search each session may take real time if a model is
// still downloading, and that must not look identical to "will never work".
static EMBED_STATE: AtomicI32 = AtomicI32::new(0);

// On by default, toggleable per-session from the search bar — computing an
// embedding is real (if usually small) work, and someone doing a quick
// exact-name lookup may just want plain keyword matching without waiting on
// it. Not backed by `checkbox`: that widget's host-managed state always
// starts unchecked with no way to seed "on", the same reason DEBUG_MODE
// above uses a plain toggle button instead.
static AI_ENABLED: AtomicI32 = AtomicI32::new(1);

// Debug mode: shows every app's raw score against the query, unfiltered by the
// threshold, instead of just "found" or "not found" — exists because a search
// server-side (does a vector never arrive? does it decode to the wrong
// dimension? does it decode fine but just score low?) is otherwise invisible
// once the client has already reduced it to an empty results list.
static DEBUG_MODE: AtomicI32 = AtomicI32::new(0);
static DEBUG_ID:   AtomicI32 = AtomicI32::new(-1);
static DEBUG_TEXT: Mutex<String> = Mutex::new(String::new());

// Nothing is listed until you ask for it. An index that is meant to grow cannot
// meaningfully be dumped on the landing screen, so home is a search box and the
// results only appear once there is a query — or once you deliberately ask to
// browse everything.
static BROWSED: AtomicI32 = AtomicI32::new(0);

// Ratings and popularity, fetched alongside the index.
static STATS_ID: AtomicI32 = AtomicI32::new(-1);
static RATE_ID:  AtomicI32 = AtomicI32::new(-1);
static OPEN_ID:  AtomicI32 = AtomicI32::new(-1);
struct Stat { avg: f32, votes: u32, opens: u64 }
static STATS: Mutex<Vec<(String, Stat)>> = Mutex::new(Vec::new());
// Which card has its rating row expanded, as an index into the current page.
static RATING_FOR: Mutex<String> = Mutex::new(String::new());

// Written reviews are fetched per app, only when someone asks to see them —
// there is no point pulling every review for a whole page of results.
static REVIEWS_FOR: Mutex<String> = Mutex::new(String::new());
static REVIEWS_ID:  AtomicI32 = AtomicI32::new(-1);
static REVIEWS: Mutex<Vec<(u8, String)>> = Mutex::new(Vec::new());

fn load_reviews(url: &str) {
    *REVIEWS_FOR.lock().unwrap() = url.to_string();
    REVIEWS.lock().unwrap().clear();
    let id = next_id();
    REVIEWS_ID.store(id, Relaxed);
    net::get(id, &format!("{SERVER}/api/reviews?url={}", encode(url)));
}

fn stat_for(url: &str) -> (f32, u32, u64) {
    STATS.lock().unwrap().iter()
        .find(|(u, _)| u == url)
        .map(|(_, s)| (s.avg, s.votes, s.opens))
        .unwrap_or((0.0, 0, 0))
}

fn fetch_stats() {
    let id = next_id();
    STATS_ID.store(id, Relaxed);
    net::get(id, &format!("{SERVER}/api/stats"));
}

// Five characters is all the width a card can spare, and the runtime has no
// icon font, so the stars are drawn with text.
fn stars_str(avg: f32) -> String {
    let filled = (avg + 0.25).floor().max(0.0).min(5.0) as usize;
    let mut s = String::new();
    for i in 0..5 { s.push(if i < filled { '★' } else { '☆' }); }
    s
}

struct Entry { name: String, desc: String, url: String, author: String, tags: String, doc_snippet: String }
static RESULTS: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

// The chosen .wasm, held until submit: (filename, bytes).
static PICKED: Mutex<Option<(String, Vec<u8>)>> = Mutex::new(None);
// URL the node gave back after hosting the upload.
static HOSTED_URL: Mutex<String> = Mutex::new(String::new());
static ERR: Mutex<String> = Mutex::new(String::new());

fn next_id() -> i32 { REQ_CTR.fetch_add(1, Relaxed) }

fn pick_wasm() {
    if let Some((name, bytes)) = sys::pick_file("wasm") {
        *PICKED.lock().unwrap() = Some((name, bytes));
        HOSTED_URL.lock().unwrap().clear();
        ERR.lock().unwrap().clear();
    }
}

// Percent-encode anything that would break a query string.
fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// Fields captured when Submit was pressed. Held separately because an upload
// resolves its URL asynchronously, after the form has been read.
static PENDING: Mutex<Option<(String, String, String, String)>> = Mutex::new(None);

fn send_submission(url: String) {
    let Some((name, desc, author, tags)) = PENDING.lock().unwrap().clone() else { return };
    let body = format!("{name}\t{desc}\t{url}\t{author}\t{tags}");
    let id = next_id();
    SUBMIT_ID.store(id, Relaxed);
    SUBMIT_STATUS.store(1, Relaxed);
    net::post(id, &format!("{SERVER}/api/submit"), &body);
}

fn fetch_all() {
    BROWSED.store(1, Relaxed);
    PAGE.store(0, Relaxed);
    let id = next_id();
    SEARCH_ID.store(id, Relaxed);
    // /api/search with an empty query rather than /api/apps: same contents, but
    // ordered by popularity and rating instead of insertion order.
    net::get(id, &format!("{SERVER}/api/search?q="));
    fetch_stats();
}

fn parse(body: &str) {
    let mut r = RESULTS.lock().unwrap();
    r.clear();
    for line in body.lines().filter(|l| !l.is_empty()) {
        // 6 public fields now — a missing 6th (doc_snippet) just means an app
        // that hasn't opted into extended indexing, same as any other missing
        // trailing field this format already tolerates.
        let mut p = line.splitn(6, '\t');
        r.push(Entry {
            name:        p.next().unwrap_or("").to_string(),
            desc:        p.next().unwrap_or("").to_string(),
            url:         p.next().unwrap_or("").to_string(),
            author:      p.next().unwrap_or("").to_string(),
            tags:        p.next().unwrap_or("").to_string(),
            doc_snippet: p.next().unwrap_or("").to_string(),
        });
    }
}

// ── Init ──────────────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn init() {
    // Deliberately does not load the index — home starts empty.
}

// ── Render ────────────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn render() {
    // ── Poll network ──────────────────────────────────────────────────────────
    let sid = SEARCH_ID.load(Relaxed);
    if sid >= 0 {
        if let Some(body) = net::poll_str(sid) {
            parse(&body);
            SEARCH_ID.store(-1, Relaxed);
        }
    }
    // Upload finished: the node replies with the URL it is now serving the app
    // from, which then becomes the URL we submit.
    let upid = UPLOAD_ID.load(Relaxed);
    if upid >= 0 {
        match net::poll_result_str(upid) {
            PollStr::Pending => {}
            PollStr::Failed => {
                UPLOAD_ID.store(-1, Relaxed);
                SUBMIT_STATUS.store(3, Relaxed);
                *ERR.lock().unwrap() = "Upload failed — is the node still running?".into();
            }
            PollStr::Done(body) => {
                UPLOAD_ID.store(-1, Relaxed);
                let line = body.lines().next().unwrap_or("");
                match line.split_once('\t') {
                    Some(("ok", url)) => {
                        *HOSTED_URL.lock().unwrap() = url.to_string();
                        send_submission(url.to_string());
                    }
                    Some(("err", msg)) => {
                        SUBMIT_STATUS.store(3, Relaxed);
                        *ERR.lock().unwrap() = msg.to_string();
                    }
                    _ => {
                        SUBMIT_STATUS.store(3, Relaxed);
                        *ERR.lock().unwrap() = "Unexpected reply from the node.".into();
                    }
                }
            }
        }
    }

    // Once the query's embedding is ready, re-run the search with it attached —
    // this is what lets even a read-only, keyword-only server rank by meaning:
    // the vector rides along on the request. Distinguishing pending from failed
    // matters here specifically: EmbeddingGemma is a real download the first
    // time it ever runs on a machine, and without this the UI would look
    // identically "done, nothing found" whether that download was still running
    // or had failed outright.
    let eid = EMBED_ID.load(Relaxed);
    if eid >= 0 {
        match embed::poll_result_str(eid) {
            PollStr::Pending => {}
            PollStr::Failed => {
                EMBED_ID.store(-1, Relaxed);
                EMBED_STATE.store(2, Relaxed);
            }
            PollStr::Done(vec) => {
                EMBED_ID.store(-1, Relaxed);
                EMBED_STATE.store(0, Relaxed);
                let q = PENDING_QUERY.lock().unwrap().clone();
                if !q.is_empty() {
                    fire_search(&q, Some(&vec));
                    if DEBUG_MODE.load(Relaxed) == 1 {
                        let did = next_id();
                        DEBUG_ID.store(did, Relaxed);
                        let qenc: String = q.chars().map(|c| if c == ' ' { '+' } else { c }).collect();
                        net::get(did, &format!("{SERVER}/api/search/debug?q={qenc}&vec={}", url_encode_vec(&vec)));
                    }
                }
            }
        }
    }

    let did = DEBUG_ID.load(Relaxed);
    if did >= 0 {
        if let Some(body) = net::poll_str(did) {
            DEBUG_ID.store(-1, Relaxed);
            *DEBUG_TEXT.lock().unwrap() = body;
        }
    }

    // Stats arrive separately from the index, so a card renders immediately and
    // gains its rating a moment later rather than blocking on both.
    let stid = STATS_ID.load(Relaxed);
    if stid >= 0 {
        if let Some(body) = net::poll_str(stid) {
            STATS_ID.store(-1, Relaxed);
            let mut v = Vec::new();
            for line in body.lines().filter(|l| !l.trim().is_empty()) {
                let mut p = line.splitn(4, '\t');
                let (Some(u), Some(a), Some(n), Some(o)) = (p.next(), p.next(), p.next(), p.next())
                    else { continue };
                v.push((u.to_string(), Stat {
                    avg: a.parse().unwrap_or(0.0),
                    votes: n.parse().unwrap_or(0),
                    opens: o.parse().unwrap_or(0),
                }));
            }
            *STATS.lock().unwrap() = v;
        }
    }
    // A rating or an open changes the numbers, so refresh them once it lands.
    for slot in [&RATE_ID, &OPEN_ID] {
        let id = slot.load(Relaxed);
        if id >= 0 && net::poll(id).is_some() {
            slot.store(-1, Relaxed);
            fetch_stats();
            // Your own review should show up in the list straight away.
            let open = REVIEWS_FOR.lock().unwrap().clone();
            if !open.is_empty() { load_reviews(&open); }
        }
    }

    let rvid = REVIEWS_ID.load(Relaxed);
    if rvid >= 0 {
        if let Some(body) = net::poll_str(rvid) {
            REVIEWS_ID.store(-1, Relaxed);
            let mut v = Vec::new();
            for line in body.lines().filter(|l| !l.trim().is_empty()) {
                let mut p = line.splitn(3, '\t');
                let (Some(stars), Some(_ts), Some(txt)) = (p.next(), p.next(), p.next()) else { continue };
                v.push((stars.parse().unwrap_or(0u8), txt.to_string()));
            }
            *REVIEWS.lock().unwrap() = v;
        }
    }

    let subid = SUBMIT_ID.load(Relaxed);
    if subid >= 0 {
        if let Some(_) = net::poll(subid) {
            SUBMIT_STATUS.store(2, Relaxed);
            SUBMIT_ID.store(-1, Relaxed);
            fetch_all(); // refresh index after submit
        }
    }

    // ── Header ────────────────────────────────────────────────────────────────
    row(|| {
        text("annessaia", 28.0, Color::CYAN);
        space(6.0);
        badge("search", Color::rgb(37, 99, 235));
    });
    label("Discover and share WASM apps.");
    separator();

    // ── Tabs ──────────────────────────────────────────────────────────────────
    let view = if READ_ONLY { 0 } else { VIEW.load(Relaxed) };
    if !READ_ONLY {
        row(|| {
            if view == 0 {
                button_styled("  Search  ", Color::WHITE, Color::rgb(37, 99, 235), Color::rgb(37, 99, 235));
            } else if button_ghost("  Search  ") {
                VIEW.store(0, Relaxed);
            }
            if view == 1 {
                button_styled("  + Submit App  ", Color::WHITE, Color::rgb(37, 99, 235), Color::rgb(37, 99, 235));
            } else if button_ghost("  + Submit App  ") {
                VIEW.store(1, Relaxed);
            }
        });
        space(12.0);
    }

    if view == 0 { search_tab(); } else { submit_tab(); }

    if READ_ONLY {
        space(20.0);
        card_color(Color::rgb(15, 23, 42), || {
            text("Want to publish an app?", 15.0, Color::WHITE);
            space(4.0);
            small("This index is read-only. Entries arrive by gossip from nodes, so");
            small("listing an app means running one and serving the app yourself:");
            space(6.0);
            code("cargo run -p annessaia-server");
            space(4.0);
            small("Then submit at http://localhost:3000/search.wasm — your node connects");
            small("here automatically and the entry propagates within a minute.");
        });
    }
}

// ── Search tab ────────────────────────────────────────────────────────────────

fn search_tab() {
    // AI toggle sits directly to the left of the search bar, in the same row,
    // rather than in the button row below — it's a property of the search
    // itself, so it reads better right next to where the query is typed.
    // Drawn before the field so the field's infinite desired_width only
    // claims what's left of the row instead of swallowing the whole thing.
    let mut query = String::new();
    let mut ai_just_enabled = false;
    row(|| {
        let ai_on = AI_ENABLED.load(Relaxed) == 1;
        if ai_on {
            if button_styled(" AI search: ON ", Color::WHITE, Color::rgb(20, 83, 45), Color::rgb(74, 222, 128)) {
                AI_ENABLED.store(0, Relaxed);
            }
        } else if button_ghost(" AI search: OFF ") {
            AI_ENABLED.store(1, Relaxed);
            ai_just_enabled = true;
        }
        space(8.0);
        query = text_field(0, "Search by name, tag, or description…");
    });
    // Re-run right away so switching AI back on shows meaning-based results
    // immediately rather than waiting for another Search press.
    if ai_just_enabled && BROWSED.load(Relaxed) == 1 { run_search(&query); }
    space(8.0);
    row(|| {
        if button("  Search  ") { run_search(&query); }
        if button_ghost(" Browse all ") { fetch_all(); }
        space(12.0);
        let debug_on = DEBUG_MODE.load(Relaxed) == 1;
        if debug_on {
            if button_styled(" Debug: ON ", Color::WHITE, Color::rgb(120, 90, 10), Color::rgb(250, 204, 21)) {
                DEBUG_MODE.store(0, Relaxed);
                DEBUG_TEXT.lock().unwrap().clear();
            }
        } else if button_ghost(" Debug ") {
            DEBUG_MODE.store(1, Relaxed);
        }
    });
    space(16.0);

    // Raw per-app scores against the last query, unfiltered by the threshold —
    // shows whether a vector reached the server at all, whether it decoded to
    // the right dimension, and what it actually scored, rather than collapsing
    // all of that into just "found" or "not found".
    if DEBUG_MODE.load(Relaxed) == 1 {
        let dbg = DEBUG_TEXT.lock().unwrap().clone();
        card_color(Color::rgb(20, 16, 8), || {
            small("DEBUG — raw scores, unfiltered by threshold");
            space(6.0);
            if dbg.is_empty() {
                small("Run a search to populate this.");
            } else {
                for line in dbg.lines() {
                    code(line);
                }
            }
        });
        space(12.0);
    }

    if SEARCH_ID.load(Relaxed) >= 0 {
        label("Searching…");
        return;
    }

    // Home stays empty until asked. Listing the whole index here would stop
    // being useful the moment it grows past a screenful.
    if BROWSED.load(Relaxed) == 0 {
        card_color(Color::rgb(15, 23, 42), || {
            text("Find an app", 16.0, Color::WHITE);
            space(4.0);
            small("Search by name, tag, description, or author.");
            space(10.0);
            row(|| {
                small("Try:");
                space(8.0);
                for t in ["game", "tool", "art", "demo"] {
                    if button_ghost(&format!("  {t}  ")) { run_search(t); }
                }
            });
            space(10.0);
            small("Or press Browse all to page through everything.");
        });
        return;
    }

    // Snapshot results so we don't hold the Mutex across widget calls
    let entries: Vec<(String, String, String, String, String, String)> = RESULTS
        .lock().unwrap()
        .iter()
        .map(|e| (e.name.clone(), e.desc.clone(), e.url.clone(), e.author.clone(), e.tags.clone(), e.doc_snippet.clone()))
        .collect();

    // What's shown so far is keyword-only until the query's on-device embedding
    // finishes — say so plainly rather than let "no results" look final while a
    // meaning-based re-ranking (or the model download behind it, the first time
    // this ever runs on a machine) is still in flight.
    match EMBED_STATE.load(Relaxed) {
        1 => {
            row(|| {
                badge("COMPUTING", Color::rgb(250, 204, 21));
                space(8.0);
                small("Refining by meaning — the first search each session may take a while if a model is still downloading.");
            });
            space(10.0);
        }
        2 => {
            row(|| {
                badge("KEYWORD ONLY", Color::rgb(148, 163, 184));
                space(8.0);
                small("Meaning-based search isn't available right now — showing keyword matches only.");
            });
            space(10.0);
        }
        _ => {}
    }

    if entries.is_empty() {
        card_color(Color::rgb(15, 23, 42), || {
            label("No apps found.");
            space(4.0);
            small("Try a different search, or add one from the Submit tab.");
        });
        return;
    }

    // Only a page at a time: the index is meant to grow, and a few hundred cards
    // would be both unreadable and slow to draw.
    let total = entries.len();
    let pages = (total + PER_PAGE - 1) / PER_PAGE;
    let page = (PAGE.load(Relaxed).max(0) as usize).min(pages.saturating_sub(1));
    let start = page * PER_PAGE;
    let end = (start + PER_PAGE).min(total);

    row(|| {
        badge(&format!("{total} app{}", if total == 1 { "" } else { "s" }), Color::rgb(71, 85, 105));
        if pages > 1 {
            space(8.0);
            small(&format!("showing {}–{} · page {} of {}", start + 1, end, page + 1, pages));
        }
    });
    space(12.0);

    {
        for (name, desc, url, author, tags, doc_snippet) in &entries[start..end] {
            card(|| {
                row(|| {
                    text(name, 16.0, Color::WHITE);
                    space(8.0);
                    for tag in tags.split(',').map(|t| t.trim()).filter(|t| !t.is_empty()) {
                        badge(tag, Color::rgb(37, 99, 235));
                        space(3.0);
                    }
                });
                space(4.0);
                if !desc.is_empty() { label(desc); }
                // A short, Google-style preview of the app's own extended
                // indexed content (if it opted in) — dimmer than the
                // description, since it's a supporting detail, not the
                // app's own summary. "Open" below doubles as "see more":
                // it navigates straight to the app, which is the full
                // content this snippet is a preview of.
                if !doc_snippet.is_empty() { small(doc_snippet); }
                space(6.0);

                let (avg, votes, opens) = stat_for(url);
                row(|| {
                    if votes > 0 {
                        colored(&stars_str(avg), Color::rgb(250, 204, 21));
                        space(6.0);
                        small(&format!("{avg:.1} · {votes} rating{}", if votes == 1 { "" } else { "s" }));
                    } else {
                        small("not rated yet");
                    }
                    if opens > 0 {
                        space(12.0);
                        small(&format!("{opens} open{}", if opens == 1 { "" } else { "s" }));
                    }
                });
                space(8.0);

                row(|| {
                    small(&format!("by {author}"));
                    space(16.0);
                    if button_ghost(" Open ") {
                        // Count the launch before navigating — this app is about
                        // to be replaced by the one being opened.
                        let id = next_id();
                        OPEN_ID.store(id, Relaxed);
                        net::post(id, &format!("{SERVER}/api/open"), url);
                        sys::nav(url);
                    }
                    if button_ghost(" Copy URL ") { sys::copy(url); }
                    if votes > 0 {
                        let showing = REVIEWS_FOR.lock().unwrap().clone() == *url;
                        if showing {
                            if button_ghost(" Hide reviews ") { REVIEWS_FOR.lock().unwrap().clear(); }
                        } else if button_ghost(" Reviews ") {
                            load_reviews(url);
                        }
                    }
                    if !READ_ONLY {
                        let open_for = RATING_FOR.lock().unwrap().clone();
                        if open_for == *url {
                            if button_ghost(" Cancel ") { RATING_FOR.lock().unwrap().clear(); }
                        } else if button_ghost(" Rate ") {
                            *RATING_FOR.lock().unwrap() = url.clone();
                        }
                    }
                });

                if REVIEWS_FOR.lock().unwrap().clone() == *url {
                    space(8.0);
                    let loading = REVIEWS_ID.load(Relaxed) >= 0;
                    let items = REVIEWS.lock().unwrap().clone();
                    card_color(Color::rgb(15, 23, 42), || {
                        if loading {
                            small("Loading reviews…");
                        } else if items.is_empty() {
                            // Ratings without text are common, so say so rather
                            // than looking like the fetch failed.
                            small("No written reviews yet — only star ratings.");
                        } else {
                            for (stars, txt) in &items {
                                row(|| {
                                    colored(&stars_str(*stars as f32), Color::rgb(250, 204, 21));
                                    space(8.0);
                                    label(txt);
                                });
                                space(6.0);
                            }
                        }
                    });
                }

                // The rating row only appears for the card you clicked Rate on,
                // so a page of results does not become a wall of star buttons.
                if !READ_ONLY && *RATING_FOR.lock().unwrap() == *url {
                    space(8.0);
                    card_color(Color::rgb(15, 23, 42), || {
                        small("Your rating");
                        space(6.0);
                        let review = text_field(900, "Optional: a sentence about it");
                        space(8.0);
                        row(|| {
                            for n in 1..=5u8 {
                                if button_styled(&format!("  {n}★  "), Color::WHITE,
                                                 Color::rgb(120, 90, 10), Color::rgb(250, 204, 21)) {
                                    let id = next_id();
                                    RATE_ID.store(id, Relaxed);
                                    net::post(id, &format!("{SERVER}/api/rate"),
                                              &format!("{url}\t{n}\t{review}"));
                                    RATING_FOR.lock().unwrap().clear();
                                }
                            }
                        });
                        space(4.0);
                        small("One rating per node — rating again replaces your last.");
                    });
                }
            });
            space(8.0);
        }
    }

    if pages > 1 {
        space(8.0);
        row(|| {
            if page > 0 {
                if button_ghost("  ‹ Previous  ") { PAGE.store(page as i32 - 1, Relaxed); }
            }
            if page + 1 < pages {
                if button_ghost("  Next ›  ") { PAGE.store(page as i32 + 1, Relaxed); }
            }
            space(12.0);
            small(&format!("page {} of {}", page + 1, pages));
        });
    }
}

fn run_search(query: &str) {
    BROWSED.store(1, Relaxed);
    PAGE.store(0, Relaxed);           // a new search starts at the top

    // Keyword results first — they must not wait on the embedding, which can
    // take real time the first time it ever runs this session.
    fire_search(query, None);

    // Skipped entirely (not just left to fail) when the AI toggle is off — no
    // COMPUTING/KEYWORD ONLY badge either, since neither applies to a
    // deliberate choice the way they do to "unavailable right now".
    if AI_ENABLED.load(Relaxed) == 1 {
        *PENDING_QUERY.lock().unwrap() = query.to_string();
        let eid = next_id();
        EMBED_ID.store(eid, Relaxed);
        EMBED_STATE.store(1, Relaxed);
        embed::start(eid, query);
    }
}

// Base64's alphabet is `A-Za-z0-9+/=` — placed raw into a query string, `+`
// gets decoded by every standard parser as a space (the same
// application/x-www-form-urlencoded convention the `q` param above relies on
// to send literal spaces as `+`), which silently corrupts the value. This is
// exactly what "failed to decode (not valid base64)" in debug mode was: the
// vector itself was fine, it just never survived the trip through the URL.
fn url_encode_vec(v: &str) -> String {
    v.chars().map(|c| match c {
        '+' => "%2B".to_string(),
        '/' => "%2F".to_string(),
        '=' => "%3D".to_string(),
        c   => c.to_string(),
    }).collect()
}

fn fire_search(query: &str, vec: Option<&str>) {
    let id = next_id();
    SEARCH_ID.store(id, Relaxed);
    let q: String = query.chars().map(|c| if c == ' ' { '+' } else { c }).collect();
    let url = match vec {
        Some(v) => format!("{SERVER}/api/search?q={q}&vec={}", url_encode_vec(v)),
        None    => format!("{SERVER}/api/search?q={q}"),
    };
    net::get(id, &url);
    fetch_stats();
}

// ── Submit tab ────────────────────────────────────────────────────────────────

fn submit_tab() {
    card(|| {
        text("Submit your app", 18.0, Color::WHITE);
        label("Share your WASM app with the annessaia community.");
        separator();

        label("App name  *");
        let name = text_field(10, "e.g. My Awesome Game");
        space(10.0);

        label("Description");
        let desc = text_field(11, "One sentence about what it does");
        space(10.0);

        // ── Where the app is hosted ───────────────────────────────────────────
        // This node is already serving HTTP, so it can host the file itself.
        // Otherwise nobody could publish without arranging hosting first.
        let mode = HOST_MODE.load(Relaxed);
        label("Where does the app live?");
        space(4.0);
        row(|| {
            if mode == 0 {
                button_styled("  Upload to this node  ", Color::WHITE, Color::rgb(37, 99, 235), Color::rgb(37, 99, 235));
            } else if button_ghost("  Upload to this node  ") {
                HOST_MODE.store(0, Relaxed);
            }
            if mode == 1 {
                button_styled("  I have a URL  ", Color::WHITE, Color::rgb(37, 99, 235), Color::rgb(37, 99, 235));
            } else if button_ghost("  I have a URL  ") {
                HOST_MODE.store(1, Relaxed);
            }
        });
        space(10.0);

        let mut url = String::new();
        if mode == 0 {
            let picked = PICKED.lock().unwrap().as_ref().map(|(n, b)| (n.clone(), b.len()));
            match picked {
                Some((fname, len)) => {
                    row(|| {
                        badge("READY", Color::GREEN);
                        space(8.0);
                        label(&fname);
                        space(8.0);
                        small(&format!("{} KB", len / 1024));
                    });
                    space(6.0);
                    if button_ghost(" Choose a different file ") { pick_wasm(); }
                }
                None => {
                    if button("  Choose a .wasm file…  ") { pick_wasm(); }
                    space(4.0);
                    small("The node will host it and fill in the URL for you.");
                }
            }
            let hosted = HOSTED_URL.lock().unwrap().clone();
            if !hosted.is_empty() {
                space(6.0);
                small(&format!("hosted at {hosted}"));
            }
        } else {
            label("WASM URL  *");
            url = text_field(12, "https://example.com/myapp.wasm");
            space(4.0);
            small("Must be reachable by anyone, not just you.");
        }
        space(10.0);

        label("Author");
        let author = text_field(13, "Your name or handle");
        space(10.0);

        label("Tags  (comma-separated)");
        let tags = text_field(14, "e.g. game, tool, demo");
        space(16.0);

        let status = SUBMIT_STATUS.load(Relaxed);

        if status == 1 {
            row(|| {
                badge("SUBMITTING…", Color::rgb(71, 85, 105));
                space(8.0);
                if button_ghost(" Cancel ") {
                    SUBMIT_STATUS.store(0, Relaxed);
                    SUBMIT_ID.store(-1, Relaxed);
                }
            });
        } else if status != 2 {
            let has_file = PICKED.lock().unwrap().is_some();
            let ready = !name.is_empty() && if mode == 0 { has_file } else { !url.is_empty() };

            row(|| {
                if button_success("  Submit App  ") && ready {
                    // Stash the descriptive fields; for an upload the URL is not
                    // known until the node replies, so the submission is sent
                    // from the upload handler rather than here.
                    *PENDING.lock().unwrap() =
                        Some((name.clone(), desc.clone(), author.clone(), tags.clone()));
                    SUBMIT_STATUS.store(1, Relaxed);
                    ERR.lock().unwrap().clear();

                    if mode == 0 {
                        let file = PICKED.lock().unwrap().clone();
                        if let Some((_, bytes)) = file {
                            let id = next_id();
                            UPLOAD_ID.store(id, Relaxed);
                            let slug = name.clone();
                            net::post_bytes(id, &format!("{SERVER}/api/upload?name={}", encode(&slug)), &bytes);
                        }
                    } else {
                        send_submission(url.clone());
                    }
                }
                if !ready {
                    space(8.0);
                    small(if mode == 0 { "* name and a chosen file are required" }
                          else { "* name and URL are required" });
                }
            });
        }

        if status == 2 {
            space(8.0);
            card_color(Color::rgb(20, 83, 45), || {
                row(|| {
                    badge("SUBMITTED", Color::GREEN);
                    space(8.0);
                    label("Your app is now in the index!");
                });
                let hosted = HOSTED_URL.lock().unwrap().clone();
                if !hosted.is_empty() {
                    space(4.0);
                    small(&format!("served from {hosted}"));
                }
            });
            space(8.0);
            row(|| {
                if button_ghost(" Submit another ") {
                    SUBMIT_STATUS.store(0, Relaxed);
                    *PICKED.lock().unwrap() = None;
                    HOSTED_URL.lock().unwrap().clear();
                }
                let hosted = HOSTED_URL.lock().unwrap().clone();
                if !hosted.is_empty() && button_ghost(" Copy URL ") { sys::copy(&hosted); }
            });
        } else if status == 3 {
            space(8.0);
            card_color(Color::rgb(127, 29, 29), || {
                row(|| {
                    badge("FAILED", Color::RED);
                    space(8.0);
                    let e = ERR.lock().unwrap().clone();
                    label(if e.is_empty() { "Submission failed. Check the details and try again." } else { &e });
                });
            });
            space(8.0);
            if button_ghost(" Try again ") { SUBMIT_STATUS.store(0, Relaxed); }
        }
    });
}
