// annessaia-server — decentralized app registry over WebSockets.
//
// Every node keeps persistent WS connections to its peers. When an app is
// approved the entry is sent over the open socket to all connected peers.
// Each peer checks if the URL is new: if so, stores it and forwards to its
// own connections. Propagation stops the moment a node already has the entry.
//
// WS message protocol (plain text, one message per entry):
//   HELLO <self_url>        — sent immediately on connect (both sides)
//   ANNOUNCE <tsv_line>     — gossip an app entry
//   REVOKE <url>            — delist an app. Only honoured once the URL stops
//                             serving, which only its host can arrange; that
//                             stands in for an ownership token.
//   DISCOVER <peer_url>     — share a peer so the receiver can connect to it
//
// Env vars:
//   PORT                 (default 3000)
//   ANNESSAIA_URL        this node's public base URL   (default http://localhost:PORT)
//   ANNESSAIA_DATA       path to data directory        (default ./data)
//   ANNESSAIA_PEERS      comma-separated bootstrap peers
//   ANNESSAIA_DIRECTORY  bootstrap directory URL       (default DEFAULT_DIRECTORY)

use axum::{
    Router,
    body::Bytes,
    extract::{Query, State, WebSocketUpgrade, Request},
    extract::ws::{Message as AxMsg, WebSocket},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use futures::{SinkExt, StreamExt};
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::Message as TMsg;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;
use base64::{engine::general_purpose::STANDARD, Engine as _};
#[cfg(feature = "ai")]
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

// The "ai" feature is what actually needs fastembed on the dependency graph
// at all — building without it (--no-default-features) gets a node that
// still ranks by keyword just fine, just never computes or compares any
// vector. `Embedder` stays a real type either way so every signature that
// threads one through (AppState, Registry::load, embed_text) doesn't need
// its own #[cfg]; only the bodies that actually touch a model do.
#[cfg(feature = "ai")]
type Embedder = TextEmbedding;
#[cfg(not(feature = "ai"))]
type Embedder = ();


// ── Data model ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
struct App {
    name: String, desc: String, url: String, author: String, tags: String,
    // A short (~200 char), server-capped human-readable preview of an app's
    // extended indexed content (see doc_vec below) — public, shown in search
    // results as a Google-style snippet. Empty for apps that don't opt in.
    doc_snippet: String,
    // Base64, int8-quantized 256-dim embedding of this app's text — empty until
    // computed. Carried on the wire (gossip + disk) but deliberately never in the
    // public HTTP form: see to_tsv/to_wire below.
    vec: String,
    // A second, optional embedding computed by the app itself from richer
    // internal text (e.g. every widget's name+desc in the Widget Gallery) that
    // its name/tags/desc alone don't capture. Additive to `vec` for ranking,
    // never a replacement — see doc_semantic_score. Client-computed only: the
    // raw text behind it never reaches this server, only the vector does.
    doc_vec: String,
}

impl App {
    /// Public form — used by every HTTP endpoint (`api_apps`/`api_search`/
    /// `api_stats`) and parsed by `apps/search/src/lib.rs`. 6 tab-separated
    /// fields; a missing 6th (`doc_snippet`) means an app that hasn't opted
    /// into extended indexing, not a parse failure. Vectors never appear here.
    fn to_tsv(&self) -> String {
        format!("{}\t{}\t{}\t{}\t{}\t{}", self.name, self.desc, self.url, self.author, self.tags, self.doc_snippet)
    }
    fn from_tsv(line: &str) -> Option<Self> {
        let mut p = line.splitn(6, '\t');
        let name        = p.next()?.trim().to_string();
        let desc        = p.next().unwrap_or("").trim().to_string();
        let url         = p.next()?.trim().to_string();
        let author      = p.next().unwrap_or("").trim().to_string();
        let tags        = p.next().unwrap_or("").trim().to_string();
        let doc_snippet = p.next().unwrap_or("").trim().to_string();
        if name.is_empty() || url.is_empty() { return None; }
        Some(App { name, desc, url, author, tags, doc_snippet, vec: String::new(), doc_vec: String::new() })
    }

    /// Wire form — gossip and disk persistence only. Fields 7 and 8 carry the
    /// two embeddings so they survive restarts and reach peers, without ever
    /// crossing into the public API WASM clients parse.
    fn to_wire(&self) -> String {
        format!("{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            self.name, self.desc, self.url, self.author, self.tags, self.doc_snippet, self.vec, self.doc_vec)
    }
    fn from_wire(line: &str) -> Option<Self> {
        let mut p = line.splitn(8, '\t');
        let name        = p.next()?.trim().to_string();
        let desc        = p.next().unwrap_or("").trim().to_string();
        let url         = p.next()?.trim().to_string();
        let author      = p.next().unwrap_or("").trim().to_string();
        let tags        = p.next().unwrap_or("").trim().to_string();
        let doc_snippet = p.next().unwrap_or("").trim().to_string();
        // A missing trailing field — an older peer, or data written before
        // this feature — just means no vector yet, not a parse failure.
        let vec         = p.next().unwrap_or("").trim().to_string();
        let doc_vec     = p.next().unwrap_or("").trim().to_string();
        if name.is_empty() || url.is_empty() { return None; }
        Some(App { name, desc, url, author, tags, doc_snippet, vec, doc_vec })
    }

    /// Equality on the fields a submitter can actually edit. Vectors are
    /// excluded — recomputed for otherwise-identical text, they must not look
    /// like a real change, or every resubmission would re-announce and print
    /// "updated" for nothing. `doc_snippet` IS visible content, though, so a
    /// snippet-only edit counts as a real change same as a desc edit would.
    fn same_content(&self, other: &App) -> bool {
        self.name == other.name && self.desc == other.desc
            && self.author == other.author && self.tags == other.tags
            && self.doc_snippet == other.doc_snippet
    }

    // Per-word, not whole-phrase: a query like "how to use sliders" should hit
    // an entry containing "sliders" even though that literal 4-word phrase
    // appears nowhere. Requiring the entire query as one substring meant any
    // natural-language question failed the keyword gate outright, however
    // strong the actual word overlap was — this fixes that without touching
    // the separate multi-word bonus already in text_score, which affects
    // ranking, not whether an app appears at all.
    //
    // Stopwords are filtered before matching: short connective words like
    // "to" or "use" are common enough as bare *substrings* of unrelated words
    // ("tool", "mouse") that leaving them in turns "how to use sliders" into
    // a false-positive magnet, confirmed empirically — Calculator and Paint
    // both matched on "to" via "tool" alone. Filtering them costs nothing
    // real: they carry no content on their own.
    fn matches(&self, q: &str) -> bool {
        let q = q.to_lowercase();
        let hay = format!("{} {} {} {} {}",
            self.name, self.desc, self.tags, self.author, self.doc_snippet).to_lowercase();
        let words: Vec<&str> = q.split_whitespace()
            .filter(|w| w.len() > 1 && !STOPWORDS.contains(w))
            .collect();
        if !words.is_empty() {
            return words.iter().any(|w| word_hit(&hay, w));
        }
        // Falls back to the old whole-string check for queries that are
        // nothing but stopwords/single letters (e.g. "a", "to").
        hay.contains(q.trim())
    }
}

// Common connective words excluded from per-word keyword matching (see
// `App::matches`) — carry no searchable content and are prone to matching as
// bare substrings of unrelated words.
const STOPWORDS: &[&str] = &[
    "how", "to", "use", "used", "using", "the", "a", "an", "of", "in", "on",
    "for", "and", "or", "is", "are", "do", "does", "with", "at", "by", "it",
    "this", "that", "what", "you", "your", "i",
];

// `doc_snippet` holds one or more short, self-contained chunks joined by this
// separator — e.g. one per widget in the Widget Gallery ("slider — drag to
// change; returns the current value every frame."). A plain single-sentence
// snippet with no separator at all still works: `chunks()` on it just yields
// one chunk. \x1e (ASCII record separator) rather than a visible character
// like '|' because it can never appear in ordinary app-authored text by
// accident, so no escaping is ever needed on the way in.
const CHUNK_SEP: char = '\u{1e}';

fn chunks(doc_snippet: &str) -> Vec<&str> {
    doc_snippet.split(CHUNK_SEP).filter(|c| !c.is_empty()).collect()
}

const MAX_CHUNKS: usize = 40;
const MAX_CHUNK_CHARS: usize = 200;

// Bounds what a submitter's doc_snippet can actually cost to store: each
// chunk capped individually (so one long chunk can't eat the whole budget
// and truncate mid-sentence into the next one), and at most MAX_CHUNKS of
// them — comfortably more than the ~20 a gallery-style app needs. \t/\r/\n
// stripped per chunk for the same reason `App::to_tsv` needs them absent
// from every other field: they'd otherwise corrupt the TSV line itself.
fn sanitize_doc_snippet(raw: &str) -> String {
    chunks(raw).into_iter()
        .take(MAX_CHUNKS)
        .map(|c| {
            let c: String = c.chars().filter(|ch| !matches!(ch, '\t' | '\r' | '\n')).collect();
            let cut = c.char_indices().nth(MAX_CHUNK_CHARS).map(|(i, _)| i).unwrap_or(c.len());
            c[..cut].to_string()
        })
        .collect::<Vec<_>>()
        .join(&CHUNK_SEP.to_string())
}

// Which chunk to actually show under an app's name in results — the one that
// most directly answers the query, Google-snippet style, not just the app's
// generic first blurb. Picked by keyword overlap (the same stopword-filtered
// words `App::matches` already computes) rather than a per-chunk embedding:
// with a few dozen short chunks per app at most, exact word overlap already
// finds "slider — drag to change…" for a query containing "sliders" without
// the cost of a vector per chunk. Empty query, or no chunk containing any
// query word, both fall back to the first chunk as a sane default overview.
fn best_chunk(doc_snippet: &str, q: &str) -> String {
    let cs = chunks(doc_snippet);
    let Some(&first) = cs.first() else { return String::new() };
    let words: Vec<&str> = q.split_whitespace()
        .filter(|w| w.len() > 1 && !STOPWORDS.contains(w))
        .collect();
    if words.is_empty() { return first.to_string(); }

    // Strict `>`, not `>=`: on a tie, keeps whichever chunk came first in the
    // app's own list rather than the last one — confirmed necessary, not
    // theoretical: "play a sound effect" tied 2-2 between the sound::play
    // chunk and the sound::play_looped one, and Iterator::max_by_key's
    // documented last-wins-ties behavior picked the wrong one.
    let mut best = first;
    let mut best_score = 0usize;
    for c in &cs {
        let lc = c.to_lowercase();
        let score = words.iter().filter(|w| word_hit(&lc, w)).count();
        if score > best_score {
            best_score = score;
            best = c;
        }
    }
    if best_score > 0 { best.to_string() } else { first.to_string() }
}

// A query word hits a haystack if it's a literal substring, or — crudely —
// if its singular form is, so "sliders" still finds a chunk that only ever
// says "slider". Confirmed necessary: without this, "how to use sliders"
// matched nothing in any per-widget chunk (all singular) and silently fell
// back to the generic overview instead of the slider-specific one, even
// though the app-level keyword gate passed off of a coincidental plural in
// its own desc field.
fn word_hit(hay_lc: &str, w: &str) -> bool {
    if hay_lc.contains(w) { return true; }
    // "-es" checked before plain "-s": "checkboxes" needs the whole suffix
    // stripped to reach "checkbox" — stripping just one 's' leaves "checkboxe",
    // which matches nothing. Confirmed necessary: "how do checkboxes work"
    // returned zero results before this, the same class of miss as "sliders".
    if let Some(stem) = w.strip_suffix("es") {
        if stem.len() > 2 && hay_lc.contains(stem) { return true; }
    }
    if let Some(stem) = w.strip_suffix('s') {
        if stem.len() > 2 && hay_lc.contains(stem) { return true; }
    }
    false
}

// ── Ratings and popularity ────────────────────────────────────────────────────
//
// Both are stored per-origin rather than as a single number, which is what lets
// them merge across the network without double counting: a rating is keyed by who
// gave it, an open count by which node saw it. Merging is last-write-wins on each
// key and summing the counts, so any two nodes that have seen the same messages
// agree regardless of the order they arrived in.
//
// One vote per node. That is not proof against someone running several nodes, but
// it costs more than clicking twice, and it matches how the rest of the system
// already treats a node as the unit of identity.

#[derive(Clone, Debug)]
struct Rating {
    stars: u8,      // 1..5
    ts:    u64,     // millis; higher wins for the same voter
    text:  String,  // may be empty — a rating without a review
}

// Stable public identifier for a node, derived from its secret token. FNV-1a:
// this only needs to be consistent and not reveal the token, not be a hash people
// are relying on for security.
fn node_id(token: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in token.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ── Embeddings ─────────────────────────────────────────────────────────────────
//
// Runs entirely locally via `fastembed` (Google's EmbeddingGemma-300M, ONNX via
// `ort`) — no external API, no Cloudflare AI product. The model downloads once on
// first use and is cached afterwards. ~9x bge-small's size, adopted because
// bge-small's true/false cosine scores sat too close together to reliably
// threshold (a genuine match and an unrelated one landed a hundredth of a point
// apart on real queries) — see SEMANTIC_MIN below.
//
// Native output is 768-dim; truncated here to 256 via Matryoshka Representation
// Learning, whose entire design point is that a prefix of the full vector is a
// valid, independently-usable embedding at that length. 256 is required, not a
// preference: 768 dims would encode to ~1024 base64 chars, which alone exceeds
// the bootstrap Worker's 1024-byte KV metadata slot before even adding the
// `dead` liveness marker that shares it.
//
// Vectors are L2-normalized then quantized to int8 before storage: direction is
// all cosine similarity needs, magnitude is thrown away, and the result is a
// quarter the size of float32.
const EXPECTED_DIMS: usize = 256;

/// What gets embedded for an app: the name carries the most signal, so it leads.
/// Capped defensively so a pathological description can't blow up inference time.
///
/// EmbeddingGemma is trained for *asymmetric* retrieval and expects a specific
/// task-prefixed format on whichever side is being embedded — get this wrong (as
/// happened with bge-small's simpler "passage: "/"query: " convention initially
/// being omitted entirely) and similarity scores miscalibrate across the board.
/// This is the document side, per Google's model card; see `embed_query` for the
/// query side. Apps have no separate title beyond their name, which is already
/// folded into the text, so title is always "none".
fn embed_input(name: &str, tags: &str, desc: &str) -> String {
    let mut s = format!("title: none | text: {name}. {tags}. {desc}");
    s.truncate(s.char_indices().nth(1000).map(|(i, _)| i).unwrap_or(s.len()));
    s
}

/// The query side of the same prefix convention.
fn embed_query(q: &str) -> String { format!("task: search result | query: {q}") }

fn quantize_i8(v: &[f32]) -> Vec<i8> {
    let v = &v[..EXPECTED_DIMS.min(v.len())];
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-8);
    v.iter().map(|x| (((x / norm) * 127.0).round().clamp(-127.0, 127.0)) as i8).collect()
}

fn quantize(v: &[f32]) -> String {
    let bytes: Vec<u8> = quantize_i8(v).into_iter().map(|b| b as u8).collect();
    STANDARD.encode(bytes)
}

fn dequantize(b64: &str) -> Option<Vec<i8>> {
    if b64.is_empty() { return None; }
    let bytes = STANDARD.decode(b64).ok()?;
    Some(bytes.into_iter().map(|b| b as i8).collect())
}

/// Does this vector match the model currently in use? A stale vector from a
/// previous model isn't "empty" — it's a real string that decodes to the wrong
/// length — so backfill logic must check this, not just emptiness, or a
/// leftover from an old model silently never gets replaced.
fn needs_embedding(vec: &str) -> bool {
    dequantize(vec).map(|v| v.len()) != Some(EXPECTED_DIMS)
}

fn cosine_i8(a: &[i8], b: &[i8]) -> f32 {
    if a.len() != b.len() || a.is_empty() { return 0.0; }
    let (mut dot, mut na, mut nb) = (0i64, 0i64, 0i64);
    for i in 0..a.len() {
        dot += a[i] as i64 * b[i] as i64;
        na  += a[i] as i64 * a[i] as i64;
        nb  += b[i] as i64 * b[i] as i64;
    }
    if na == 0 || nb == 0 { return 0.0; }
    dot as f32 / ((na as f32).sqrt() * (nb as f32).sqrt())
}

/// Similarity between a query's embedding and an app's stored one. 0.0 whenever
/// either is unavailable — an app not yet embedded, an empty query, or a model
/// that failed to load all degrade to "no semantic contribution" rather than
/// failing the request.
fn semantic_score(query_vec: Option<&[i8]>, app: &App) -> f32 {
    let Some(q) = query_vec else { return 0.0 };
    let Some(a) = dequantize(&app.vec) else { return 0.0 };
    cosine_i8(q, &a).max(0.0)
}

/// Same as `semantic_score`, but against an app's optional, additional
/// doc_vec (richer internal content, e.g. every widget's name+desc in the
/// Widget Gallery) instead of its name/tags/desc vector. 0.0 for apps that
/// haven't opted in — same degrade-gracefully rule as `semantic_score`.
fn doc_semantic_score(query_vec: Option<&[i8]>, app: &App) -> f32 {
    let Some(q) = query_vec else { return 0.0 };
    let Some(a) = dequantize(&app.doc_vec) else { return 0.0 };
    cosine_i8(q, &a).max(0.0)
}

/// Runs the model on a blocking thread: embedding is real CPU-bound work, unlike
/// everything else in these handlers, and doing it inline would stall the async
/// runtime's worker thread for the duration. Every caller (api_search,
/// api_submit, api_search_debug) treats `None` as "no semantic signal this
/// time" regardless of *why* — model unavailable and feature disabled look
/// identical to them, which is what lets this stay a single call site rather
/// than needing its own #[cfg] at every use.
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
#[cfg(not(feature = "ai"))]
async fn embed_text(_st: &St, _text: String) -> Option<Vec<f32>> {
    None
}

// ── Registry (persisted to disk) ──────────────────────────────────────────────

struct Registry {
    approved:  Vec<App>,
    // URLs published from this node. Everything else in `approved` arrived by
    // gossip and belongs to someone else, so it is not ours to delete.
    mine:      HashSet<String>,
    // url -> voter id -> rating, and url -> node id -> times opened there.
    ratings:   HashMap<String, HashMap<String, Rating>>,
    opens:     HashMap<String, HashMap<String, u64>>,
    peers:     HashSet<String>,  // known peer base-URLs (persisted)
    token:     String,           // proves ownership of our directory listing
    published: bool,             // opt-in: are we listed in the bootstrap directory?
    data_dir:  PathBuf,
    self_url:  String,
}

impl Registry {
    fn load(data_dir: PathBuf, self_url: String, embedder: &mut Option<Embedder>) -> Self {
        fs::create_dir_all(&data_dir).ok();
        // Only actually mutated in the backfill loop below, which is
        // #[cfg]'d out entirely without "ai".
        #[cfg_attr(not(feature = "ai"), allow(unused_mut))]
        let mut approved = read_tsv(&data_dir.join("approved.tsv"));
        let peers     = read_lines(&data_dir.join("peers.txt")).into_iter().collect();
        let published = data_dir.join("published").exists();
        let token     = load_or_make_token(&data_dir);

        let mine_path = data_dir.join("mine.txt");
        let mine: HashSet<String> = if mine_path.exists() {
            read_lines(&mine_path).into_iter().collect()
        } else {
            // First run after ownership was introduced: anything we are hosting
            // ourselves was necessarily published here, so claim those rather
            // than locking the operator out of their own uploads.
            let prefix = format!("{}/apps/", self_url.trim_end_matches('/'));
            approved.iter().map(|a| a.url.clone()).filter(|u| u.starts_with(&prefix)).collect()
        };

        let ratings = read_ratings(&data_dir.join("ratings.tsv"));
        let opens   = read_opens(&data_dir.join("opens.tsv"));

        // Backfill: apps stored before embeddings existed, or gossiped in from a
        // peer that doesn't compute them, get one now using our own local model.
        // Synchronous and at startup, before the server accepts requests — fine
        // at this project's scale (dozens to low hundreds of apps); revisit only
        // if that stops being true.
        #[cfg_attr(not(feature = "ai"), allow(unused_mut))]
        let mut changed = false;
        #[cfg(feature = "ai")]
        if let Some(model) = embedder.as_mut() {
            for a in approved.iter_mut() {
                if needs_embedding(&a.vec) {
                    let input = embed_input(&a.name, &a.tags, &a.desc);
                    if let Ok(mut out) = model.embed(vec![input], None) {
                        if let Some(v) = out.pop() {
                            a.vec = quantize(&v);
                            changed = true;
                        }
                    }
                }
            }
        }
        #[cfg(not(feature = "ai"))]
        let _ = embedder;

        let reg = Registry { approved, mine, ratings, opens, peers, token, published, data_dir, self_url };
        if changed { reg.save_approved(); }
        reg
    }

    // ── Ratings ───────────────────────────────────────────────────────────────

    /// Merge one rating. Returns true if it changed anything — which is what
    /// decides whether to pass it on, so gossip settles instead of echoing.
    fn merge_rating(&mut self, url: &str, voter: &str, r: Rating) -> bool {
        if r.stars < 1 || r.stars > 5 { return false; }
        let per_app = self.ratings.entry(url.to_string()).or_default();
        match per_app.get(voter) {
            Some(existing) if existing.ts >= r.ts => false,   // ours is newer or the same
            _ => { per_app.insert(voter.to_string(), r); self.save_ratings(); true }
        }
    }

    /// Merge an open count. Counts only ever grow, so a lower number is stale.
    fn merge_opens(&mut self, url: &str, node: &str, count: u64) -> bool {
        let per_app = self.opens.entry(url.to_string()).or_default();
        match per_app.get(node) {
            Some(&existing) if existing >= count => false,
            _ => { per_app.insert(node.to_string(), count); self.save_opens(); true }
        }
    }

    /// `(average stars, number of ratings)`.
    fn score(&self, url: &str) -> (f32, usize) {
        match self.ratings.get(url) {
            Some(m) if !m.is_empty() => {
                let sum: u32 = m.values().map(|r| r.stars as u32).sum();
                (sum as f32 / m.len() as f32, m.len())
            }
            _ => (0.0, 0),
        }
    }

    /// Total opens across every node that has reported any.
    fn open_count(&self, url: &str) -> u64 {
        self.opens.get(url).map(|m| m.values().sum()).unwrap_or(0)
    }

    fn save_ratings(&self) {
        let mut out = String::new();
        for (url, per) in &self.ratings {
            for (voter, r) in per {
                out.push_str(&format!("{url}\t{voter}\t{}\t{}\t{}\n", r.stars, r.ts, r.text));
            }
        }
        fs::write(self.data_dir.join("ratings.tsv"), out).ok();
    }
    fn save_opens(&self) {
        let mut out = String::new();
        for (url, per) in &self.opens {
            for (node, c) in per { out.push_str(&format!("{url}\t{node}\t{c}\n")); }
        }
        fs::write(self.data_dir.join("opens.tsv"), out).ok();
    }
    fn save_approved(&self) { write_tsv(&self.data_dir.join("approved.tsv"), &self.approved); }
    fn save_mine(&self) {
        let s = self.mine.iter().cloned().collect::<Vec<_>>().join("\n");
        fs::write(self.data_dir.join("mine.txt"), s).ok();
    }
    fn save_peers(&self) {
        let s = self.peers.iter().cloned().collect::<Vec<_>>().join("\n");
        fs::write(self.data_dir.join("peers.txt"), s).ok();
    }
    fn set_published(&mut self, on: bool) {
        self.published = on;
        let marker = self.data_dir.join("published");
        if on { fs::write(&marker, "").ok(); } else { fs::remove_file(&marker).ok(); }
    }
    fn has_approved(&self, url: &str) -> bool { self.approved.iter().any(|a| a.url == url) }
    // Returns true if the app was new.
    // Returns true if this is worth telling other peers about — either the app
    // was genuinely new, or an already-known one just gained a vector it didn't
    // have. Without the second case, an app's embedding could never spread past
    // one hop: ordinary resync gossip re-sends full state on every connect, but
    // once every visible field already matches, a peer that already has the URL
    // would otherwise treat it as nothing new and silently drop the vector.
    fn receive(&mut self, app: App) -> bool {
        match self.approved.iter_mut().find(|a| a.url == app.url) {
            None => {
                self.approved.push(app);
                self.save_approved();
                true
            }
            Some(existing) => {
                let mut changed = false;
                if needs_embedding(&existing.vec) && !app.vec.is_empty() {
                    existing.vec = app.vec;
                    changed = true;
                }
                // doc_vec/doc_snippet can legitimately change even when the
                // base vector doesn't need backfilling (e.g. a gallery gains
                // a new section) — plain inequality, not needs_embedding, is
                // the right trigger for these two.
                if !app.doc_vec.is_empty() && app.doc_vec != existing.doc_vec {
                    existing.doc_vec = app.doc_vec;
                    changed = true;
                }
                if !app.doc_snippet.is_empty() && app.doc_snippet != existing.doc_snippet {
                    existing.doc_snippet = app.doc_snippet;
                    changed = true;
                }
                if changed { self.save_approved(); }
                changed
            }
        }
    }
}

// The on-disk file uses the wire form (6 fields, vector included) so a computed
// embedding survives a restart instead of being recomputed every boot.
fn read_tsv(path: &PathBuf) -> Vec<App> {
    fs::read_to_string(path).unwrap_or_default()
        .lines().filter(|l| !l.trim().is_empty())
        .filter_map(App::from_wire).collect()
}
fn write_tsv(path: &PathBuf, apps: &[App]) {
    fs::write(path, apps.iter().map(|a| a.to_wire()).collect::<Vec<_>>().join("\n")).ok();
}
fn read_ratings(path: &PathBuf) -> HashMap<String, HashMap<String, Rating>> {
    let mut out: HashMap<String, HashMap<String, Rating>> = HashMap::new();
    for line in fs::read_to_string(path).unwrap_or_default().lines() {
        let mut p = line.splitn(5, '\t');
        let (Some(url), Some(voter), Some(stars), Some(ts)) = (p.next(), p.next(), p.next(), p.next())
            else { continue };
        let text = p.next().unwrap_or("").to_string();
        let (Ok(stars), Ok(ts)) = (stars.parse::<u8>(), ts.parse::<u64>()) else { continue };
        out.entry(url.to_string()).or_default()
           .insert(voter.to_string(), Rating { stars, ts, text });
    }
    out
}

fn read_opens(path: &PathBuf) -> HashMap<String, HashMap<String, u64>> {
    let mut out: HashMap<String, HashMap<String, u64>> = HashMap::new();
    for line in fs::read_to_string(path).unwrap_or_default().lines() {
        let mut p = line.splitn(3, '\t');
        let (Some(url), Some(node), Some(c)) = (p.next(), p.next(), p.next()) else { continue };
        let Ok(c) = c.parse::<u64>() else { continue };
        out.entry(url.to_string()).or_default().insert(node.to_string(), c);
    }
    out
}

fn read_lines(path: &PathBuf) -> Vec<String> {
    fs::read_to_string(path).unwrap_or_default()
        .lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
}

// Secret generated once per node. The directory stores it alongside our listing so
// nobody else can refresh or delete that entry. Never leaves the data dir.
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

// ── Shared state ──────────────────────────────────────────────────────────────

type Tx = mpsc::UnboundedSender<String>;

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

type St = Arc<AppState>;

// Send a message to all active WS peers except `skip`.
fn ws_broadcast(conns: &Mutex<HashMap<String, Tx>>, msg: &str, skip: Option<&str>) {
    let c = conns.lock().unwrap();
    for (url, tx) in c.iter() {
        if skip.map_or(true, |s| url.as_str() != s) {
            let _ = tx.send(msg.to_string());
        }
    }
}

// ── Shared peer session loop ──────────────────────────────────────────────────
// Used for both incoming (axum WS) and outgoing (tungstenite) connections.
// `stream` yields incoming text messages; `tx` sends outgoing text messages.

async fn run_peer_loop(
    stream: std::pin::Pin<Box<dyn futures::Stream<Item = String> + Send>>,
    tx: Tx,
    state: St,
) {
    let mut stream = stream;
    let mut peer_url: Option<String> = None;

    // Introduce ourselves immediately.
    {
        let self_url = state.registry.lock().unwrap().self_url.clone();
        let _ = tx.send(format!("HELLO {self_url}"));
    }

    while let Some(text) = stream.next().await {
        let text = text.trim().to_string();
        if text.is_empty() { continue; }

        if let Some(url) = text.strip_prefix("HELLO ") {
            let url = url.trim().to_string();
            if url.is_empty() { continue; }
            peer_url = Some(url.clone());
            state.conns.lock().unwrap().insert(url.clone(), tx.clone());

            // Sync: push our full approved list + share our peer list.
            let (approved, others) = {
                let mut r = state.registry.lock().unwrap();
                r.peers.insert(url.clone());
                r.save_peers();
                let approved = r.approved.clone();
                let others: Vec<String> = r.peers.iter()
                    .filter(|p| p.as_str() != url).cloned().collect();
                (approved, others)
            };
            println!("[ws] ↔ {url}  (syncing {} apps)", approved.len());
            // Wire form (not to_tsv): this is what carries each app's embedding
            // to a peer that doesn't have it yet.
            for app in &approved { let _ = tx.send(format!("ANNOUNCE {}", app.to_wire())); }
            for p   in &others   { let _ = tx.send(format!("DISCOVER {p}")); }

        } else if let Some(line) = text.strip_prefix("ANNOUNCE ") {
            if let Some(app) = App::from_wire(line) {
                let is_new = state.registry.lock().unwrap().receive(app.clone());
                if is_new {
                    println!("[gossip] '{}' — forwarding", app.name);
                    ws_broadcast(&state.conns, &format!("ANNOUNCE {}", app.to_wire()), peer_url.as_deref());
                }
            }

        } else if let Some(rest) = text.strip_prefix("RATE ") {
            // "<url>\t<voter>\t<stars>\t<ts>\t<text>"
            let mut p = rest.splitn(5, '\t');
            if let (Some(url), Some(voter), Some(stars), Some(ts)) = (p.next(), p.next(), p.next(), p.next()) {
                let body = p.next().unwrap_or("").to_string();
                if let (Ok(stars), Ok(ts)) = (stars.parse::<u8>(), ts.parse::<u64>()) {
                    let changed = state.registry.lock().unwrap()
                        .merge_rating(url, voter, Rating { stars, ts, text: body });
                    // Only pass on what was actually new to us, or ratings would
                    // circulate forever.
                    if changed {
                        ws_broadcast(&state.conns, &text, peer_url.as_deref());
                    }
                }
            }

        } else if let Some(rest) = text.strip_prefix("OPENS ") {
            // "<url>\t<node>\t<count>"
            let mut p = rest.splitn(3, '\t');
            if let (Some(url), Some(node), Some(c)) = (p.next(), p.next(), p.next()) {
                if let Ok(c) = c.trim().parse::<u64>() {
                    let changed = state.registry.lock().unwrap().merge_opens(url, node, c);
                    if changed {
                        ws_broadcast(&state.conns, &text, peer_url.as_deref());
                    }
                }
            }

        } else if let Some(url) = text.strip_prefix("REVOKE ") {
            let url = url.trim().to_string();
            let known = state.registry.lock().unwrap().has_approved(&url);
            if known && !url_alive(&url).await {
                // Verified gone, so this is the host delisting their own app.
                {
                    let mut r = state.registry.lock().unwrap();
                    r.approved.retain(|a| a.url != url);
                    r.save_approved();
                }
                println!("[revoke] '{url}' — gone, forwarding");
                ws_broadcast(&state.conns, &format!("REVOKE {url}"), peer_url.as_deref());
            } else if known {
                // Still serving: someone is trying to delist an app they do not host.
                println!("[revoke] ignored for {url} — still reachable");
            }

        } else if let Some(url) = text.strip_prefix("DISCOVER ") {
            let url = url.trim().to_string();
            if url.is_empty() { continue; }

            let (self_url, already_known) = {
                let r = state.registry.lock().unwrap();
                (r.self_url.clone(), r.peers.contains(&url))
            };
            let active = state.conns.lock().unwrap().contains_key(&url);
            if already_known || active || url == self_url { continue; }

            println!("[discover] new peer: {url}");
            {
                let mut r = state.registry.lock().unwrap();
                r.peers.insert(url.clone());
                r.save_peers();
            }
            // Delegate to the connection manager task — avoids a circular Send requirement.
            let _ = state.connect_tx.send(url);
        }
    }

    if let Some(url) = &peer_url {
        state.conns.lock().unwrap().remove(url);
        println!("[ws] ✗ disconnected: {url}");
    }
}

// ── Incoming WS connections (axum accepts) ────────────────────────────────────

async fn ws_endpoint(ws: WebSocketUpgrade, State(state): State<St>) -> impl IntoResponse {
    ws.on_upgrade(|socket: WebSocket| async move {
        let (mut sink, receiver) = socket.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();

        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if sink.send(AxMsg::Text(msg)).await.is_err() { break; }
            }
        });

        let stream = Box::pin(receiver.filter_map(|r| {
            let v = match r { Ok(AxMsg::Text(t)) => Some(t), _ => None };
            futures::future::ready(v)
        }));

        run_peer_loop(stream, tx, state).await;
    })
}

// ── Outgoing WS connections (we connect to peers) ─────────────────────────────

fn to_ws_url(http_url: &str) -> String {
    let base = if http_url.starts_with("https://") {
        http_url.replacen("https://", "wss://", 1)
    } else {
        http_url.replacen("http://", "ws://", 1)
    };
    format!("{}/ws", base.trim_end_matches('/'))
}

async fn connect_to_peer(url: String, state: St) {
    // Don't connect if already active.
    if state.conns.lock().unwrap().contains_key(&url) { return; }

    let ws_url = to_ws_url(&url);
    println!("[ws] → connecting to {url}");

    match tokio_tungstenite::connect_async(&ws_url).await {
        Ok((stream, _)) => {
            let (mut sink, receiver) = stream.split();
            let (tx, mut rx) = mpsc::unbounded_channel::<String>();

            tokio::spawn(async move {
                while let Some(msg) = rx.recv().await {
                    if sink.send(TMsg::Text(msg)).await.is_err() { break; }
                }
            });

            let stream = Box::pin(receiver.filter_map(|r| {
                let v = match r { Ok(TMsg::Text(t)) => Some(t.to_string()), _ => None };
                futures::future::ready(v)
            }));

            run_peer_loop(stream, tx, state).await;
        }
        Err(e) => eprintln!("[ws] ✗ {url}: {e}"),
    }
}

// ── HTTP handlers ─────────────────────────────────────────────────────────────

// The client is the desktop runtime, which executes WASM and cannot render HTML.
// Serving a page here meant that typing the bare host produced a baffling
// "expected `(` --> <!DOCTYPE html>" from the WASM parser, so send callers to the
// real app instead. The runtime's HTTP client follows redirects.
async fn serve_index() -> Redirect { Redirect::temporary("/search.wasm") }

async fn serve_admin() -> Redirect { Redirect::temporary("/admin.wasm") }

async fn api_apps(State(st): State<St>) -> String {
    let r = st.registry.lock().unwrap();
    r.approved.iter()
        .map(|a| {
            let mut a = a.clone();
            a.doc_snippet = best_chunk(&a.doc_snippet, "");
            a.to_tsv()
        })
        .collect::<Vec<_>>().join("\n")
}

// How well an app answers a query, before popularity is taken into account.
// A name hit counts for far more than a mention buried in a description, and an
// exact name match outranks a prefix, which outranks a substring.
fn text_score(app: &App, q: &str) -> f32 {
    if q.is_empty() { return 1.0; }
    let (name, desc, tags, author) =
        (app.name.to_lowercase(), app.desc.to_lowercase(), app.tags.to_lowercase(), app.author.to_lowercase());

    let mut s = 0.0;
    if name == q { s += 12.0; }
    else if name.starts_with(q) { s += 8.0; }
    else if name.contains(q) { s += 5.0; }

    if tags.split(',').any(|t| t.trim() == q) { s += 4.0; }
    else if tags.contains(q) { s += 2.0; }

    if desc.contains(q) { s += 1.0; }
    if author.contains(q) { s += 1.0; }

    // Every word matching somewhere is worth a little, so multi-word queries
    // still rank sensibly even when no single field contains the whole phrase.
    let words: Vec<&str> = q.split_whitespace().filter(|w| w.len() > 1).collect();
    if words.len() > 1 {
        let hits = words.iter()
            .filter(|w| name.contains(**w) || tags.contains(**w) || desc.contains(**w))
            .count();
        s += hits as f32 * 0.8;
    }
    s
}

// Final ordering: relevance first, then how used and how well liked it is.
// Opens are damped with a log so a single popular app cannot bury everything
// else, and ratings only count once a few people have voted. Semantic similarity
// is weighted below an exact keyword hit but above raw popularity, so a real
// name/tag match still wins outright while a conceptually related but
// differently-worded query still gets a meaningful boost.
fn rank(text: f32, semantic: f32, opens: u64, avg: f32, votes: usize) -> f32 {
    let popularity = (1.0 + opens as f32).ln();
    let quality = if votes == 0 { 0.0 } else {
        let confidence = (votes as f32 / (votes as f32 + 3.0)).min(1.0);
        (avg - 3.0) * confidence          // above or below "average"
    };
    text * 10.0 + semantic * 6.0 + popularity * 1.5 + quality * 2.0
}

// Below this cosine similarity, "meaning-only" relevance isn't strong enough to
// surface a result that shares no words with the query — otherwise every app
// would technically match every query once every app has *some* vector.
//
// Re-measured under EmbeddingGemma-300M (previously 0.725, calibrated for
// bge-small — a different model's scores don't transfer). The same four
// ground-truth pairs, measured fresh:
//   0.4852  Paint            / "sketching and canvas app"     — true match
//   0.4284  Calculator       / "math"                         — true match
//   0.3048  Widget Gallery   / "sketching and canvas app"      — false match
//   0.3027  NOVA             / "multiplayer chess"             — false match
// The gap between the lowest true match and the highest false match is 0.1236
// — twelve times wider than bge-small's ever was (0.732 vs 0.722, a tenth of a
// point). 0.37 sits at the midpoint, with equal margin either side, rather
// than threaded through a needle like the old value had to be.
const SEMANTIC_MIN: f32 = 0.37;

async fn api_search(State(st): State<St>, Query(p): Query<HashMap<String, String>>) -> String {
    let q = p.get("q").cloned().unwrap_or_default().trim().to_lowercase();

    // Embedding happens before the registry lock is taken — it can take real
    // time, and holding the mutex across it would stall every other request
    // this node is handling.
    let query_vec: Option<Vec<i8>> = if q.is_empty() {
        None
    } else {
        embed_text(&st, embed_query(&q)).await.map(|v| quantize_i8(&v))
    };

    let r = st.registry.lock().unwrap();

    let mut scored: Vec<(f32, &App)> = r.approved.iter()
        .filter_map(|a| {
            // The better of the two matches, not a weighted sum: a great match
            // on either the app's basic description or its (optional) richer
            // indexed content is enough to surface it fully, with no extra
            // weight to hand-calibrate against real queries.
            let semantic = semantic_score(query_vec.as_deref(), a)
                .max(doc_semantic_score(query_vec.as_deref(), a));
            let keyword_hit = q.is_empty() || a.matches(&q);
            if !keyword_hit && semantic < SEMANTIC_MIN { return None; }
            let (avg, votes) = r.score(&a.url);
            let text = text_score(a, &q);
            Some((rank(text, semantic, r.open_count(&a.url), avg, votes), a))
        })
        .collect();

    // Ties broken by name so the order is stable between identical requests.
    scored.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| x.1.name.cmp(&y.1.name)));

    scored.iter()
        .map(|(_, a)| {
            let mut a = (*a).clone();
            a.doc_snippet = best_chunk(&a.doc_snippet, &q);
            a.to_tsv()
        })
        .collect::<Vec<_>>().join("\n")
}

// GET /api/search/debug?q= — every app's raw score against a query, unfiltered
// by SEMANTIC_MIN and sorted by semantic descending. Exists because "no
// results" is otherwise a dead end to debug from outside the process: this
// makes visible whether a query embedded at all, what its best match actually
// scored, and whether that's below threshold (a ranking/tuning question) or
// near zero (a real fault — the vector never reached here, or the app being
// searched for has no stored vector of its own).
async fn api_search_debug(State(st): State<St>, Query(p): Query<HashMap<String, String>>) -> String {
    let q = p.get("q").cloned().unwrap_or_default().trim().to_lowercase();
    let query_vec: Option<Vec<i8>> = if q.is_empty() {
        None
    } else {
        embed_text(&st, embed_query(&q)).await.map(|v| quantize_i8(&v))
    };

    let r = st.registry.lock().unwrap();
    let mut rows: Vec<(String, bool, f32, f32)> = r.approved.iter()
        .map(|a| (
            a.name.clone(),
            q.is_empty() || a.matches(&q),
            semantic_score(query_vec.as_deref(), a),
            doc_semantic_score(query_vec.as_deref(), a),
        ))
        .collect();
    // Sorted by whichever score would actually win the max() used in ranking —
    // matches what api_search does, not just the base vector alone.
    rows.sort_by(|a, b| b.2.max(b.3).partial_cmp(&a.2.max(a.3)).unwrap_or(std::cmp::Ordering::Equal));

    let header = format!(
        "# query={q:?}  embedded={}  threshold={SEMANTIC_MIN}",
        query_vec.is_some()
    );
    let lines: Vec<String> = rows.iter()
        .map(|(name, kw, sem, doc_sem)| format!("{name}\t{}\t{sem:.4}\t{doc_sem:.4}", if *kw {"keyword"} else {"-"}))
        .collect();
    format!("{header}\n{}", lines.join("\n"))
}

// GET /api/stats — one line per app: url, average, votes, opens.
// Kept separate from /api/apps so the index format stays as it is.
async fn api_stats(State(st): State<St>) -> String {
    let r = st.registry.lock().unwrap();
    r.approved.iter().map(|a| {
        let (avg, votes) = r.score(&a.url);
        format!("{}\t{:.2}\t{}\t{}", a.url, avg, votes, r.open_count(&a.url))
    }).collect::<Vec<_>>().join("\n")
}

// GET /api/reviews?url= — the written reviews for one app, newest first.
async fn api_reviews(State(st): State<St>, Query(p): Query<HashMap<String, String>>) -> String {
    let url = p.get("url").cloned().unwrap_or_default();
    let r = st.registry.lock().unwrap();
    let Some(per) = r.ratings.get(&url) else { return String::new() };

    let mut rows: Vec<&Rating> = per.values().filter(|r| !r.text.is_empty()).collect();
    rows.sort_by(|a, b| b.ts.cmp(&a.ts));
    rows.iter().map(|r| format!("{}\t{}\t{}", r.stars, r.ts, r.text))
        .collect::<Vec<_>>().join("\n")
}

// POST /api/rate — body "<url>\t<stars>\t<review text>"
async fn api_rate(State(st): State<St>, body: Bytes) -> String {
    let raw = String::from_utf8_lossy(&body).to_string();
    let mut p = raw.splitn(3, '\t');
    let (Some(url), Some(stars)) = (p.next(), p.next()) else { return err("url and stars are required") };
    let text = p.next().unwrap_or("").replace(['\t', '\n', '\r'], " ").chars().take(500).collect::<String>();
    let Ok(stars) = stars.trim().parse::<u8>() else { return err("stars must be a number") };
    if !(1..=5).contains(&stars) { return err("stars must be between 1 and 5"); }

    let (url, voter) = (url.trim().to_string(), {
        let r = st.registry.lock().unwrap();
        node_id(&r.token)
    });

    let msg = {
        let mut r = st.registry.lock().unwrap();
        if !r.has_approved(&url) { return err("that app is not in this registry"); }
        let rating = Rating { stars, ts: now_ms(), text: text.clone() };
        if !r.merge_rating(&url, &voter, rating.clone()) { return ok("unchanged"); }
        format!("RATE {url}\t{voter}\t{stars}\t{}\t{text}", rating.ts)
    };
    ws_broadcast(&st.conns, &msg, None);
    ok("rated")
}

// POST /api/open — body "<url>". Counts a launch so popularity can be ranked.
async fn api_open(State(st): State<St>, body: Bytes) -> String {
    let url = String::from_utf8_lossy(&body).trim().to_string();
    let msg = {
        let mut r = st.registry.lock().unwrap();
        if !r.has_approved(&url) { return err("unknown app"); }
        let me = node_id(&r.token);
        let next = r.opens.get(&url).and_then(|m| m.get(&me)).copied().unwrap_or(0) + 1;
        r.merge_opens(&url, &me, next);
        format!("OPENS {url}\t{me}\t{next}")
    };
    ws_broadcast(&st.conns, &msg, None);
    ok("counted")
}

async fn api_submit(State(st): State<St>, body: Bytes) -> String {
    let line = String::from_utf8_lossy(&body).to_string();

    // The public 6-field form (name, desc, url, author, tags, doc_snippet)
    // plus an optional 7th field: a client-computed doc_vec. Peeled off here
    // rather than folded into App::from_tsv, which is also the parser used to
    // read ordinary public listings elsewhere and must stay vector-free.
    let mut fields: Vec<&str> = line.splitn(7, '\t').collect();
    let doc_vec_field = if fields.len() == 7 { fields.pop().unwrap().trim().to_string() } else { String::new() };
    let base_line = fields.join("\t");

    let Some(mut app) = App::from_tsv(&base_line) else {
        return err("name and url are required");
    };
    if !app.url.starts_with("http") {
        return err("url must start with http");
    }

    // Defensive caps on client-supplied fields — a submitter could send
    // anything here. A malformed or wrong-dimension doc_vec is dropped
    // (treated as "app didn't send one") rather than failing the submission.
    app.doc_snippet = sanitize_doc_snippet(&app.doc_snippet);
    if !needs_embedding(&doc_vec_field) {
        app.doc_vec = doc_vec_field;
    }

    // Embedding happens before the lock, same reasoning as api_search: it can
    // take real time and must not stall every other request on this node.
    let input = embed_input(&app.name, &app.tags, &app.desc);
    if let Some(v) = embed_text(&st, input).await {
        app.vec = quantize(&v);
    }

    let (announce, backfilled_only) = {
        let mut r = st.registry.lock().unwrap();
        let mut backfilled_only = false;
        // Re-submitting a URL edits the existing entry rather than being refused,
        // so fixing a description or tags does not mean deleting and re-adding.
        match r.approved.iter_mut().find(|a| a.url == app.url) {
            Some(existing) => {
                if existing.same_content(&app) {
                    let vec_needs_backfill = needs_embedding(&existing.vec) && !app.vec.is_empty();
                    // doc_vec can legitimately be refreshed (a gallery gains a
                    // new section) without name/desc/tags/doc_snippet changing,
                    // so plain inequality — not needs_embedding — is the right
                    // trigger for it, independent of whether vec also needs one.
                    let doc_vec_changed = !app.doc_vec.is_empty() && app.doc_vec != existing.doc_vec;
                    if vec_needs_backfill || doc_vec_changed {
                        // Content is unchanged, but this fills in or refreshes
                        // a vector — the *only* way that ever reaches a peer or
                        // the bootstrap Worker, since an app that already
                        // matches everywhere never changes again on its own.
                        // Returning early here, as if nothing happened, would
                        // silently strand it on this node forever.
                        if vec_needs_backfill { existing.vec = app.vec.clone(); }
                        if doc_vec_changed { existing.doc_vec = app.doc_vec.clone(); }
                        backfilled_only = true;
                    } else {
                        return ok("no changes");
                    }
                } else {
                    println!("[submit] '{}' — updated", app.name);
                    // Don't let a slow or unavailable embedder wipe out a good
                    // vector the entry already had.
                    if app.vec.is_empty() { app.vec = existing.vec.clone(); }
                    if app.doc_vec.is_empty() { app.doc_vec = existing.doc_vec.clone(); }
                    *existing = app.clone();
                }
            }
            None => {
                println!("[submit] '{}' — live immediately", app.name);
                r.approved.push(app.clone());
            }
        }
        // Publishing here is what makes it ours to edit or delete later.
        r.mine.insert(app.url.clone());
        r.save_mine();
        r.save_approved();
        if backfilled_only { println!("[submit] '{}' — vector backfilled, gossiping", app.name); }
        (format!("ANNOUNCE {}", app.to_wire()), backfilled_only)
    };
    ws_broadcast(&st.conns, &announce, None);
    ok(if backfilled_only { "vector added, peers notified" } else { "published" })
}

// GET /api/mine — URLs this node published, so the admin app knows which entries
// it may offer to delete.
async fn api_mine(State(st): State<St>) -> String {
    let r = st.registry.lock().unwrap();
    r.mine.iter().cloned().collect::<Vec<_>>().join("\n")
}

// POST /api/revoke — delist an app, delete it if we were hosting it, and ask the
// network to drop it too.
//
// Ownership is proved by the file actually being gone rather than by a token:
// peers only honour a REVOKE once the URL stops answering, and only whoever hosts
// it can make that true. So deleting a file we serve is what gives us the right to
// have the entry removed everywhere.
async fn api_revoke(State(st): State<St>, body: Bytes) -> String {
    let url = String::from_utf8_lossy(&body).trim().to_string();

    let (removed, self_url, data_dir) = {
        let mut r = st.registry.lock().unwrap();
        if !r.approved.iter().any(|a| a.url == url) {
            return err("not in this registry");
        }
        // Entries that gossiped in belong to whoever published them. Removing one
        // here would only hide it locally anyway — it would arrive again on the
        // next sync — so refusing is both correct and less confusing.
        if !r.mine.contains(&url) {
            return err("you can only delete apps published from this node");
        }
        r.approved.retain(|a| a.url != url);
        r.mine.remove(&url);
        r.save_approved();
        r.save_mine();
        (true, r.self_url.clone(), r.data_dir.clone())
    };
    if !removed { return err("not in this registry"); }

    // If this is one of our own uploads, take the file down as well — otherwise
    // it stays reachable and every peer would refuse the revoke.
    let prefix = format!("{}/apps/", self_url.trim_end_matches('/'));
    let mut deleted_file = false;
    if let Some(name) = url.strip_prefix(&prefix) {
        if !name.contains('/') && !name.contains("..") {
            deleted_file = fs::remove_file(data_dir.join("apps").join(name)).is_ok();
        }
    }

    println!("[revoke] {url}{}", if deleted_file { "  (file deleted)" } else { "" });
    ws_broadcast(&st.conns, &format!("REVOKE {url}"), None);

    ok(if deleted_file { "removed, file deleted, peers notified" } else { "removed, peers notified" })
}

// Does this URL still serve something? Used to decide whether a REVOKE is genuine.
async fn url_alive(url: &str) -> bool {
    match reqwest::Client::new()
        .get(url)
        .timeout(std::time::Duration::from_secs(5))
        .send().await
    {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

// POST /api/upload?name=<slug> — body is the raw .wasm.
//
// The point of this node already being a web server: rather than telling people
// to find hosting before they can publish, take the file and serve it ourselves.
// Returns "ok\t<public url>" for the caller to put straight into a submission.
async fn api_upload(State(st): State<St>, Query(q): Query<HashMap<String, String>>, body: Bytes) -> String {
    let raw = q.get("name").cloned().unwrap_or_default();
    let stem = raw.trim().trim_end_matches(".wasm");
    // Keep the filename safe to put in both a path and a URL.
    let slug = stem.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .split('-').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("-");
    if slug.is_empty() { return err("name is required"); }
    if slug.len() > 64 { return err("name is too long"); }

    if body.len() > 32 * 1024 * 1024 { return err("file is larger than 32 MB"); }
    // Reject anything that isn't actually WebAssembly, so the index cannot end up
    // pointing at files the runtime will refuse to load. A .wasmpackage (a zip of
    // app.wasm + assets/) is accepted too — same content-based check the desktop
    // host uses (is_zip in annessaia/src/main.rs) rather than trusting the
    // filename, since the runtime itself never trusts the extension either.
    // A .wasmh needs no special handling here: it's just one of the above with a
    // 32-byte SHA-256 trailer *appended*, so the magic bytes at the start of the
    // body are unaffected — the host strips that trailer itself on load
    // (strip_hash_trailer in annessaia/src/main.rs), the same as it would for
    // any other content-addressed fetch.
    let is_wasm = body.len() >= 8 && &body[..4] == b"\0asm";
    let is_zip  = body.len() >= 4 && body[..4] == [0x50, 0x4B, 0x03, 0x04];
    if !is_wasm && !is_zip { return err("that is not a .wasm or .wasmpackage file"); }

    let (dir, self_url) = {
        let r = st.registry.lock().unwrap();
        (r.data_dir.join("apps"), r.self_url.clone())
    };
    if fs::create_dir_all(&dir).is_err() { return err("could not create the upload directory"); }
    if fs::write(dir.join(format!("{slug}.wasm")), &body).is_err() {
        return err("could not write the file");
    }

    println!("[upload] {slug}.wasm  ({} KB)", body.len() / 1024);
    ok(format!("{}/apps/{}.wasm", self_url.trim_end_matches('/'), slug))
}

async fn api_peers(State(st): State<St>) -> String {
    let r = st.registry.lock().unwrap();
    r.peers.iter().cloned().collect::<Vec<_>>().join("\n")
}

async fn api_active_peers(State(st): State<St>) -> String {
    let c = st.conns.lock().unwrap();
    c.keys().cloned().collect::<Vec<_>>().join("\n")
}

// POST /api/peer/connect — admin triggers outgoing WS connection to a new peer.
async fn api_peer_connect(State(st): State<St>, body: Bytes) -> String {
    let url = String::from_utf8_lossy(&body).trim().to_string();
    if url.is_empty() || !url.starts_with("http") {
        return err("url must start with http");
    }
    if st.conns.lock().unwrap().contains_key(&url) {
        return err("already connected to that peer");
    }
    {
        let mut r = st.registry.lock().unwrap();
        r.peers.insert(url.clone());
        r.save_peers();
    }
    tokio::spawn(connect_to_peer(url.clone(), Arc::clone(&st)));
    ok(format!("connecting to {url}"))
}

// POST /api/peer/remove — disconnect and remove a peer.
async fn api_peer_remove(State(st): State<St>, body: Bytes) -> String {
    let url = String::from_utf8_lossy(&body).trim().to_string();
    st.conns.lock().unwrap().remove(&url);
    let mut r = st.registry.lock().unwrap();
    r.peers.remove(&url);
    r.save_peers();
    ok("peer removed")
}

// ── Directory endpoints ───────────────────────────────────────────────────────

// These return 200 even on failure, with an "ok\t…" / "err\t…" prefixed body.
// The WASM admin app reaches the network through the host's ureq-based fetch,
// which treats any non-2xx as a transport error and throws the body away — so a
// real status code would lose the very message the operator needs to read
// ("node unreachable — is it public and running?").
fn ok(msg: impl Into<String>)  -> String { format!("ok\t{}",  msg.into()) }
fn err(msg: impl Into<String>) -> String { format!("err\t{}", msg.into()) }

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

// GET /api/directory — status line: <directory url>\t<published>\t<self url>
async fn api_directory(State(st): State<St>) -> String {
    let r = st.registry.lock().unwrap();
    format!("{}\t{}\t{}", directory_url(), r.published, r.self_url)
}

// POST /api/directory/publish — opt in to the public bootstrap list.
async fn api_directory_publish(State(st): State<St>) -> String {
    let (self_url, token) = {
        let r = st.registry.lock().unwrap();
        (r.self_url.clone(), r.token.clone())
    };
    match directory_post("register", &self_url, &token).await {
        Ok(()) => {
            st.registry.lock().unwrap().set_published(true);
            println!("[directory] published {self_url}");
            ok(format!("published {self_url}"))
        }
        Err(e) => {
            eprintln!("[directory] publish failed: {e}");
            err(e)
        }
    }
}

// POST /api/directory/unpublish — remove ourselves from the bootstrap list.
async fn api_directory_unpublish(State(st): State<St>) -> String {
    let (self_url, token) = {
        let r = st.registry.lock().unwrap();
        (r.self_url.clone(), r.token.clone())
    };
    let result = directory_post("unregister", &self_url, &token).await;
    // Clear the flag either way: the operator asked to stop publishing, so the
    // heartbeat must stop even if the directory is currently unreachable.
    st.registry.lock().unwrap().set_published(false);
    match result {
        Ok(())  => ok("no longer published"),
        Err(e)  => ok(format!("stopped publishing locally, but directory said: {e}")),
    }
}

// POST /api/directory/refresh — pull the peer list again and dial anyone new.
async fn api_directory_refresh(State(st): State<St>) -> String {
    let peers = directory_fetch().await;
    let self_url = st.registry.lock().unwrap().self_url.clone();

    let mut added = 0;
    for peer in peers {
        if peer == self_url || st.conns.lock().unwrap().contains_key(&peer) { continue; }
        {
            let mut r = st.registry.lock().unwrap();
            r.peers.insert(peer.clone());
            r.save_peers();
        }
        let _ = st.connect_tx.send(peer);
        added += 1;
    }
    ok(format!("connecting to {added} new node(s)"))
}

// ── Bootstrap directory ───────────────────────────────────────────────────────
// A Cloudflare Worker (see ../directory) holding a list of public node URLs. It is
// only a phone book — a fresh node reads it to find its first peer, then all real
// traffic goes peer-to-peer over WebSockets.
//
// Reading is automatic. Publishing is not: nothing is ever sent to the directory
// until an operator clicks "Publish this node" in /admin.

const DEFAULT_DIRECTORY: &str = "https://bootstrap.annessaia.workers.dev";
const HEARTBEAT_HOURS: u64 = 12;   // directory listings expire after 3 days

fn directory_url() -> String {
    env::var("ANNESSAIA_DIRECTORY").unwrap_or_else(|_| DEFAULT_DIRECTORY.to_string())
}

// Best-effort: a directory that is down must never stop a node from starting, so
// every failure here degrades to an empty list and the node falls back to peers.txt.
async fn directory_fetch() -> Vec<String> {
    let url = format!("{}/peers", directory_url().trim_end_matches('/'));
    match reqwest::get(&url).await {
        Ok(resp) => match resp.text().await {
            Ok(body) => body.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect(),
            Err(e) => { eprintln!("[directory] read failed: {e}"); Vec::new() }
        },
        Err(e) => { eprintln!("[directory] unreachable: {e}"); Vec::new() }
    }
}

async fn directory_post(path: &str, self_url: &str, token: &str) -> Result<(), String> {
    let url = format!("{}/{}", directory_url().trim_end_matches('/'), path);
    let resp = reqwest::Client::new()
        .post(&url)
        .body(format!("{self_url}\n{token}"))
        .send().await
        .map_err(|e| format!("directory unreachable: {e}"))?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    // Surface the Worker's own message — "node unreachable" is the most likely
    // first-try failure and the operator needs to see exactly that.
    if status.is_success() { Ok(()) } else { Err(body) }
}

// ── Bootstrap ─────────────────────────────────────────────────────────────────

async fn bootstrap(state: St) {
    let (self_url, peers_env, saved_peers) = {
        let r = state.registry.lock().unwrap();
        (r.self_url.clone(), env::var("ANNESSAIA_PEERS").unwrap_or_default(), r.peers.clone())
    };

    // Env-var peers + previously saved peers + whatever the directory knows about.
    let from_directory = directory_fetch().await;
    if !from_directory.is_empty() {
        println!("[directory] {} node(s) listed", from_directory.len());
    }

    let to_connect: HashSet<String> = peers_env
        .split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
        .chain(saved_peers.into_iter())
        .chain(from_directory.into_iter())
        .filter(|p| p != &self_url)
        .collect();

    for peer in to_connect {
        let st = Arc::clone(&state);
        tokio::spawn(async move { connect_to_peer(peer, st).await; });
    }
}

// Keeps our listing alive once the operator has opted in. Does nothing at all
// while `published` is false.
async fn heartbeat(state: St) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(HEARTBEAT_HOURS * 3600));
    tick.tick().await;   // the first tick fires immediately; publish already registered us
    loop {
        tick.tick().await;
        let (published, self_url, token) = {
            let r = state.registry.lock().unwrap();
            (r.published, r.self_url.clone(), r.token.clone())
        };
        if !published { continue; }
        match directory_post("register", &self_url, &token).await {
            Ok(())   => println!("[directory] listing refreshed"),
            Err(e)   => eprintln!("[directory] refresh failed: {e}"),
        }
    }
}

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

// Every route this node serves. A plain function (not inlined in `main`) so
// tests can build the exact same router against a throwaway AppState instead
// of a partial hand-rolled copy that could drift from what actually runs.
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
        .route("/api/admin/ai", get(api_admin_ai_get).post(api_admin_ai_set))
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

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let port     = env::var("PORT").unwrap_or_else(|_| "3000".into());
    let self_url = env::var("ANNESSAIA_URL")
        .unwrap_or_else(|_| format!("http://localhost:{port}"));
    let data_dir = env::var("ANNESSAIA_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data"));

    let (connect_tx, mut connect_rx) = mpsc::unbounded_channel::<String>();

    // Local embedding model — no external API, no Cloudflare AI product. Loaded
    // best-effort: if it fails (e.g. no network on first run to fetch the model
    // file), search just falls back to keyword-only, same as before this feature
    // — and the same as building with --no-default-features, which skips even
    // trying since fastembed isn't on the dependency graph at all in that case.
    #[cfg(feature = "ai")]
    let mut embedder: Option<Embedder> =
        match tokio::task::spawn_blocking(|| {
            TextEmbedding::try_new(TextInitOptions::new(EmbeddingModel::EmbeddingGemma300M))
        }).await {
            Ok(Ok(model)) => { println!("[embed] model ready"); Some(model) }
            Ok(Err(e)) => { eprintln!("[embed] model unavailable, falling back to keyword search: {e}"); None }
            Err(e)     => { eprintln!("[embed] model init task panicked: {e}"); None }
        };
    #[cfg(not(feature = "ai"))]
    let mut embedder: Option<Embedder> = {
        println!("[embed] built without the \"ai\" feature — keyword search only");
        None
    };

    // Backfills any app missing a vector using the model above, so this runs
    // after it loads and before AppState takes ownership of it.
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

    // Connection manager: receives peer URLs from the channel and supervises an
    // outgoing WS connection to each. Going through a channel also breaks the
    // circular Send dependency that spawning inside run_peer_loop would create.
    //
    // Each peer gets a task that reconnects when the socket drops. Without this a
    // single disconnect — a peer restarting, or a proxy timing an idle socket out —
    // left the node silently isolated until it was restarted by hand.
    {
        let st = Arc::clone(&state);
        tokio::spawn(async move {
            let mut supervised: HashSet<String> = HashSet::new();
            while let Some(url) = connect_rx.recv().await {
                // One supervisor per peer, however many times it is discovered.
                if !supervised.insert(url.clone()) { continue; }

                let s = Arc::clone(&st);
                tokio::spawn(async move {
                    let mut backoff = 2u64;
                    loop {
                        // Returns when the connection ends, however it ended.
                        connect_to_peer(url.clone(), Arc::clone(&s)).await;

                        // Stop retrying a peer the operator has removed.
                        let still_wanted = {
                            let r = s.registry.lock().unwrap();
                            r.peers.contains(&url) && r.self_url != url
                        };
                        if !still_wanted { break; }

                        tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                        backoff = (backoff * 2).min(60);
                    }
                    println!("[ws] giving up on {url}");
                });
            }
        });
    }

    {
        let r = state.registry.lock().unwrap();
        println!("annessaia registry  {self_url}");
        println!("  approved  {}", r.approved.len());
        println!("  peers     {}", r.peers.len());
        println!("  directory {}", directory_url());
        println!("  published {}", if r.published { "yes" } else { "no — opt in at /admin" });
    }

    bootstrap(Arc::clone(&state)).await;
    tokio::spawn(heartbeat(Arc::clone(&state)));

    let app = build_router(state, &data_dir);

    println!();
    println!("  Open these in annessaia (cargo run -p annessaia --release):");
    println!("    http://localhost:{port}/search.wasm   search + submit");
    println!("    http://localhost:{port}/admin.wasm    apps, peers, directory");
    println!();
    println!("  ws://localhost:{port}/ws   peer endpoint");
    println!();
    println!("  Join network:");
    println!("  ANNESSAIA_PEERS=http://other:3000 PORT=3001 \\");
    println!("  ANNESSAIA_URL=http://localhost:3001 cargo run -p annessaia-server");

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

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

    #[tokio::test]
    async fn upload_accepts_both_bare_wasm_and_wasmpackage_zips() {
        let (state, dir) = test_state();
        let app = build_router(Arc::clone(&state), dir.path());

        let mut wasm_body = b"\0asm".to_vec();
        wasm_body.extend_from_slice(&[0u8; 8]);
        let resp = app.clone()
            .oneshot(Request::post("/api/upload?name=my-app").body(Body::from(wasm_body)).unwrap())
            .await.unwrap();
        assert!(body_str(resp).await.starts_with("ok\t"));

        // .wasmpackage is a zip of app.wasm + assets/ — starts with the zip
        // local-file-header signature, not the wasm magic bytes. Regression
        // test for the bug where this was rejected outright.
        let mut zip_body = vec![0x50, 0x4B, 0x03, 0x04];
        zip_body.extend_from_slice(&[0u8; 8]);
        let resp = app.clone()
            .oneshot(Request::post("/api/upload?name=my-package").body(Body::from(zip_body)).unwrap())
            .await.unwrap();
        assert!(body_str(resp).await.starts_with("ok\t"));

        let resp = app
            .oneshot(Request::post("/api/upload?name=garbage").body(Body::from(vec![1, 2, 3, 4])).unwrap())
            .await.unwrap();
        assert_eq!(body_str(resp).await, "err\tthat is not a .wasm or .wasmpackage file");
    }

    #[tokio::test]
    async fn upload_accepts_a_wasmh_hash_trailer_without_choking_on_it() {
        // .wasmh is real wasm content with a 32-byte hash appended at the end —
        // the magic bytes stay at the start, so upload doesn't need to know
        // anything about the trailer format at all (only the desktop host, on
        // load, strips it). The trailer content itself doesn't need to be a
        // real hash for this test: upload never inspects it.
        let (state, dir) = test_state();
        let app = build_router(state, dir.path());

        let mut body = b"\0asm".to_vec();
        body.extend_from_slice(&[0u8; 8]);
        body.extend_from_slice(&[0xAB; 32]);
        let resp = app
            .oneshot(Request::post("/api/upload?name=my-app-wasmh").body(Body::from(body)).unwrap())
            .await.unwrap();
        assert!(body_str(resp).await.starts_with("ok\t"));
    }
}
