// annessaia bootstrap — peer directory, public app host, and a gossip peer.
//
// Four jobs, all on Cloudflare's free tier:
//
//  1. Peer directory. A fresh node reads GET /peers to find its first peer, then
//     opens WebSockets and gossips directly.
//
//  2. App hosting. The core apps are uploaded as static assets, so anyone with the
//     annessaia runtime can open https://<worker>/calculator.wasm without running
//     anything themselves.
//
//  3. A read-only registry over KV, so the hosted search app works for people who
//     never run a node: /api/apps and /api/search. There is deliberately no submit
//     endpoint — see below.
//
//  4. A real peer. The Hub Durable Object speaks the same HELLO/ANNOUNCE/DISCOVER
//     protocol as annessaia-server over /ws, so apps published on any node gossip
//     into this index. Without it the two registries were separate islands.
//
// Publishing requires running a node. The only way into this index is gossip from
// a peer, which means whoever lists an app is also serving it — you cannot add an
// entry from the hosted browser alone.
//
// The Hub uses the WebSocket Hibernation API deliberately: a Durable Object holding
// sockets open around the clock would burn ~11,000 GB-s/day against a 13,000 GB-s
// free allowance, whereas a hibernating one is only billed while actually handling
// a message.
//
// Routes:
//   GET  /peers        newline-separated peer URLs (this Worker included)
//   POST /register     body "<url>\n<token>"   claim or refresh a listing
//   POST /unregister   body "<url>\n<token>"   delete a listing
//   GET  /ws           gossip endpoint — nodes dial this
//   GET  /api/apps     the app index, TSV
//   GET  /api/search   ?q= filtered index, TSV. ?vec= (base64 int8, 256-dim,
//                      computed on-device by the caller) adds ranking by
//                      meaning on top of keyword matching.
//   POST /api/submit   403 — publish from a node instead
//   GET  /*.wasm       served from static assets

import { DurableObject } from 'cloudflare:workers';

const TTL       = 60 * 60 * 24 * 3;  // peer listings expire after 3 days; nodes heartbeat every 12h
const MAX_PEERS = 500;
const MAX_APPS  = 2000;
const PROBE_MS  = 5000;

// Liveness sweep. An app that fails every check for an hour is dropped, so the
// index cannot fill up with links to tunnels and laptops that went away.
const DEAD_GRACE_MS = 60 * 60 * 1000;
// The free plan allows 50 subrequests per invocation and each app costs one
// probe, so a single sweep can only cover this many. Runs resume from a stored
// cursor, so successive sweeps work through a longer list.
const SWEEP_MAX = 40;
const CURSOR_KEY = 'meta:sweep-cursor';

const PEER_PREFIX = 'peer:';
const APP_PREFIX  = 'app:';

// ── Semantic search vectors ──────────────────────────────────────────────────
//
// This Worker never runs an embedding model itself — two hard reasons, not just
// a preference. First, the free plan gives 10ms of CPU time per request, and real
// transformer inference (even a small quantized model over WASM) takes tens of
// milliseconds; no bundling trick gets around that. Second, no Cloudflare AI
// product is used here at all, by design.
//
// So every vector this Worker ever sees was computed somewhere else — either
// by a node embedding an app's own text (gossiped as ANNOUNCE fields 6 and 7:
// `vec` from name/tags/desc, and an optional additional `doc_vec` an app
// computes from richer internal content it wants indexed beyond that), or
// by the annessaia desktop client embedding a search query locally before the
// request ever leaves the machine (a ?vec= param on /api/search). This
// Worker's only job is to store and compare what it's given, using nothing
// more than arithmetic — never to produce a vector itself.
//
// EmbeddingGemma-300M truncated to 256 dims (see VEC_DIMS below), stored as
// int8 in KV *metadata* rather than as a value, so a single list() returns
// every vector — searching costs one KV operation regardless of how many apps
// there are, instead of one read each. 256 int8s in base64 is ~344 bytes;
// with both `vec` and `doc_vec` present plus the liveness sweep's own `dead`
// marker sharing the same slot, that's ~700-750 bytes of an object-encoded
// 1024-byte metadata cap — measured, not assumed, and worth re-checking if a
// third field is ever added here.
//
// A search with no ?vec= (an older client, or one whose host has no local
// model available) falls back to plain keyword matching, exactly as before
// this existed — see /api/search below.

function unpackVec(b64) {
  try {
    const bin = atob(b64);
    const out = new Int8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = (bin.charCodeAt(i) << 24) >> 24;
    return out;
  } catch { return null; }
}

function cosine(a, b) {
  if (!a || !b || a.length !== b.length) return 0;
  let dot = 0, na = 0, nb = 0;
  for (let i = 0; i < a.length; i++) { dot += a[i] * b[i]; na += a[i] * a[i]; nb += b[i] * b[i]; }
  const d = Math.sqrt(na) * Math.sqrt(nb);
  return d ? dot / d : 0;
}

// Does a stored vector match the model currently in use? A vector left over
// from a previous model isn't absent — it's a real base64 string that decodes
// to the wrong length — so backfill checks must test this, not just presence,
// or a stale vector from an old model silently never gets replaced.
// Common connective words excluded from per-word keyword matching below —
// carry no searchable content and are prone to matching as bare substrings
// of unrelated words (e.g. "to" inside "tool", "use" inside "mouse").
const STOPWORDS = new Set([
  'how', 'to', 'use', 'used', 'using', 'the', 'a', 'an', 'of', 'in', 'on',
  'for', 'and', 'or', 'is', 'are', 'do', 'does', 'with', 'at', 'by', 'it',
  'this', 'that', 'what', 'you', 'your', 'i',
]);

// Per-word, not whole-phrase: mirrors annessaia-server's App::matches — a
// query like "how to use sliders" should hit a line containing "sliders"
// even though that literal 4-word phrase appears nowhere in it. Requiring
// the entire query as one substring failed every natural-language question
// outright, however strong the actual word overlap was.
//
// A query word hits a haystack if it's a literal substring, or — crudely —
// if its singular form is, so "sliders"/"checkboxes" still find chunks that
// only ever say "slider"/"checkbox". Mirrors annessaia-server's word_hit
// exactly; confirmed necessary the same way — without the "-es" case,
// "how do checkboxes work" matched nothing at all.
function wordHit(hayLc, w) {
  if (hayLc.includes(w)) return true;
  if (w.length > 4 && w.endsWith('es') && hayLc.includes(w.slice(0, -2))) return true;
  if (w.length > 3 && w.endsWith('s') && hayLc.includes(w.slice(0, -1))) return true;
  return false;
}

function matchesQuery(line, q) {
  const hay = line.toLowerCase();
  const words = q.split(/\s+/).filter(w => w.length > 1 && !STOPWORDS.has(w));
  if (words.length > 0) return words.some(w => wordHit(hay, w));
  return hay.includes(q.trim());
}

// `doc_snippet` (the TSV line's 6th field) holds one or more short chunks
// joined by \x1e (ASCII record separator — never appears in ordinary
// app-authored text by accident, so nothing needs escaping on the way in). A
// plain single-sentence snippet with no separator still works: chunksOf
// yields one chunk. Mirrors annessaia-server's CHUNK_SEP/chunks exactly.
const CHUNK_SEP = '';
// Mirrors annessaia-server's MAX_CHUNKS/MAX_CHUNK_CHARS exactly.
const MAX_CHUNKS = 40;
const MAX_CHUNK_CHARS = 200;

function chunksOf(docSnippet) {
  return docSnippet.split(CHUNK_SEP).filter(c => c.length > 0);
}

// Which chunk to show under an app's name — the one that most directly
// answers the query (Google-snippet style), not just the app's generic first
// blurb. Picked by the same stopword-filtered keyword overlap matchesQuery
// uses; mirrors annessaia-server's best_chunk exactly. Empty query, or no
// chunk containing any query word, both fall back to the first chunk.
function bestChunk(docSnippet, q) {
  const cs = chunksOf(docSnippet);
  if (cs.length === 0) return '';
  const words = q.split(/\s+/).filter(w => w.length > 1 && !STOPWORDS.has(w));
  if (words.length === 0) return cs[0];

  let best = cs[0], bestScore = 0;
  for (const c of cs) {
    const lc = c.toLowerCase();
    const score = words.filter(w => wordHit(lc, w)).length;
    if (score > bestScore) { bestScore = score; best = c; }
  }
  return bestScore > 0 ? best : cs[0];
}

// Substitutes the picked chunk into a TSV line's 6th field for display.
function withChunk(line, q) {
  const parts = line.split('\t');
  parts[5] = bestChunk(parts[5] || '', q);
  return parts.join('\t');
}

function needsEmbedding(vecB64) {
  if (!vecB64) return true;
  const decoded = unpackVec(vecB64);
  return !decoded || decoded.length !== VEC_DIMS;
}

// A stored vector must decode to exactly this many bytes before it's trusted —
// anything else is either corrupt or from a different model than the one
// currently in use. Both node-side embedders truncate EmbeddingGemma-300M's
// native 768 dims to 256 via Matryoshka Representation Learning — 768 dims
// alone would encode to ~1024 base64 chars, exceeding the 1024-byte KV
// metadata slot this shares with the liveness sweep's `dead` marker.
const VEC_DIMS = 256;

// Re-measured under EmbeddingGemma-300M (previously 0.725, calibrated for
// bge-small — a different model's scores don't transfer). Same calibration
// as annessaia-server's SEMANTIC_MIN; see that comment for the four
// ground-truth pairs. The gap between the lowest true match and highest
// false match is 0.1236 — twelve times wider than bge-small's ever was.
// 0.37 sits at the midpoint, with equal margin either side.
const SEMANTIC_MIN = 0.37;

const CORS = {
  'access-control-allow-origin':  '*',
  'access-control-allow-methods': 'GET,POST,OPTIONS',
  'access-control-allow-headers': '*',
};

const text = (body, status = 200) =>
  new Response(body, { status, headers: { ...CORS, 'content-type': 'text/plain; charset=utf-8' } });

// ── Worker ───────────────────────────────────────────────────────────────────

export default {
  async fetch(req, env) {
    const url = new URL(req.url);
    const { pathname, searchParams } = url;

    if (req.method === 'OPTIONS') return new Response(null, { headers: CORS });

    // Gossip endpoint. One Hub instance holds every connection, so all peers
    // share the same view.
    if (pathname === '/ws') {
      if (req.headers.get('Upgrade') !== 'websocket') return text('expected a websocket upgrade', 426);
      return env.HUB.get(env.HUB.idFromName('hub')).fetch(req);
    }

    if (pathname === '/peers' && req.method === 'GET') {
      // Listing ourselves means every bootstrapping node dials the Hub without
      // anyone having to configure it.
      const peers = [url.origin, ...(await listKeys(env, PEER_PREFIX))];
      return text([...new Set(peers)].join('\n'));
    }
    if (pathname === '/register'   && req.method === 'POST') return register(req, env);
    if (pathname === '/unregister' && req.method === 'POST') return unregister(req, env);

    if (pathname === '/api/apps' && req.method === 'GET') {
      return text((await allApps(env)).map(line => withChunk(line, '')).join('\n'));
    }
    if (pathname === '/api/search' && req.method === 'GET') {
      const q = (searchParams.get('q') || '').toLowerCase().trim();
      const full = await allAppsFull(env);
      if (!q) return text(full.map(e => withChunk(e.line, '')).join('\n'));

      // This Worker never runs the embedding model itself (see the comment
      // above unpackVec) — a client that has one embeds the query on-device
      // and sends the vector along here; a client that doesn't just gets
      // plain keyword results, exactly as before this existed.
      const rawVec = searchParams.get('vec') || '';
      const decoded = rawVec ? unpackVec(rawVec) : null;
      const queryVec = (decoded && decoded.length === VEC_DIMS) ? decoded : null;

      const scored = full
        .map(e => {
          const keyword = matchesQuery(e.line, q);
          const appVec = e.vec ? unpackVec(e.vec) : null;
          const baseSemantic = (queryVec && appVec && appVec.length === VEC_DIMS)
            ? Math.max(0, cosine(queryVec, appVec)) : 0;
          // The better of the two matches, not a weighted sum — same rule as
          // annessaia-server's api_search: a great match on either the app's
          // basic description or its optional richer indexed content is
          // enough to surface it fully.
          const docVec = e.docVec ? unpackVec(e.docVec) : null;
          const docSemantic = (queryVec && docVec && docVec.length === VEC_DIMS)
            ? Math.max(0, cosine(queryVec, docVec)) : 0;
          const semantic = Math.max(baseSemantic, docSemantic);
          return { line: e.line, keyword, semantic };
        })
        // Same bar as annessaia-server's SEMANTIC_MIN: below this, "meaning
        // only" relevance isn't strong enough to surface something that
        // shares no words with the query.
        .filter(e => e.keyword || e.semantic >= SEMANTIC_MIN)
        .sort((a, b) => (b.keyword - a.keyword) || (b.semantic - a.semantic));

      return text(scored.map(e => withChunk(e.line, q)).join('\n'));
    }

    // GET /api/search/debug?q=&vec= — every app's raw score, unfiltered by
    // SEMANTIC_MIN, sorted by semantic descending. "No results" from the real
    // endpoint is otherwise a dead end to debug from outside the process — this
    // makes visible whether a vec even decoded (a wrong-dimension or corrupt
    // vec is a real fault, not a ranking question) and what the best match
    // actually scored, so a below-threshold miss and "nothing computed at all"
    // don't look identical.
    if (pathname === '/api/search/debug' && req.method === 'GET') {
      const q = (searchParams.get('q') || '').toLowerCase().trim();
      const rawVec = searchParams.get('vec') || '';
      const decoded = rawVec ? unpackVec(rawVec) : null;
      const vecStatus = !rawVec ? 'none supplied'
        : !decoded ? 'failed to decode (not valid base64)'
        : decoded.length !== VEC_DIMS ? `wrong dimension: got ${decoded.length}, expected ${VEC_DIMS}`
        : 'ok';
      const queryVec = (decoded && decoded.length === VEC_DIMS) ? decoded : null;

      const full = await allAppsFull(env);
      const rows = full.map(e => {
        const appVec = e.vec ? unpackVec(e.vec) : null;
        const appVecStatus = !e.vec ? 'none' : !appVec ? 'corrupt' : appVec.length !== VEC_DIMS ? `wrong dim (${appVec.length})` : 'ok';
        const semantic = (queryVec && appVec && appVec.length === VEC_DIMS)
          ? cosine(queryVec, appVec) : 0;

        const docVec = e.docVec ? unpackVec(e.docVec) : null;
        const docVecStatus = !e.docVec ? 'none' : !docVec ? 'corrupt' : docVec.length !== VEC_DIMS ? `wrong dim (${docVec.length})` : 'ok';
        const docSemantic = (queryVec && docVec && docVec.length === VEC_DIMS)
          ? cosine(queryVec, docVec) : 0;

        return { name: e.line.split('\t')[0], keyword: matchesQuery(e.line, q), semantic, appVecStatus, docSemantic, docVecStatus };
      }).sort((a, b) => Math.max(b.semantic, b.docSemantic) - Math.max(a.semantic, a.docSemantic));

      const header = `# query=${JSON.stringify(q)}  query_vec=${vecStatus}  threshold=${SEMANTIC_MIN}`;
      const lines = rows.map(r =>
        `${r.name}\t${r.keyword ? 'keyword' : '-'}\t${r.semantic.toFixed(4)}\t${r.appVecStatus}\t${r.docSemantic.toFixed(4)}\t${r.docVecStatus}`);
      return text([header, ...lines].join('\n'));
    }

    // Deliberately not a submission endpoint. Entries reach this index only by
    // gossiping in from a real node, so publishing means running annessaia-server
    // and actually hosting what you list.
    if (pathname === '/api/submit') return text(SUBMIT_REFUSED, 403);

    if (pathname === '/' || pathname === '/index.html') return text(HELP);

    if (env.ASSETS) {
      const res = await env.ASSETS.fetch(req);
      if (res.status !== 404) return withCors(res);
    }
    return text('not found\n\n' + HELP, 404);
  },

  // Cron: prune apps whose URL has stopped answering.
  async scheduled(_event, env, ctx) {
    ctx.waitUntil(sweep(env));
  },
};

// Is this URL actually serving something right now?
//
// A Worker cannot fetch its own hostname — the request does not loop back to the
// asset binding and comes back 404. Probing our own apps over the network therefore
// declared every one of them dead, so same-origin URLs are checked against ASSETS
// directly. The origin comparison matters: matching on path alone would let
// https://elsewhere.example/paint.wasm pass because *we* happen to serve that path.
//
// GET rather than HEAD for external URLs, because plenty of static hosts reject
// HEAD; the body is never read, so this costs headers rather than a download.
async function probe(url, env) {
  try {
    const self = (env && env.SELF_URL || '').replace(/\/+$/, '');
    if (self && env.ASSETS && (url === self || url.startsWith(self + '/'))) {
      const r = await env.ASSETS.fetch(new Request(url));
      return r.ok;
    }
    const r = await fetch(url, { method: 'GET', signal: AbortSignal.timeout(PROBE_MS) });
    return r.ok;
  } catch {
    return false;
  }
}

// Walk a slice of the index, probing each app. Writes happen only on a change of
// state — a healthy app costs no writes at all, which matters against a 1,000
// writes/day KV allowance and a sweep running every 15 minutes.
async function sweep(env) {
  const now = Date.now();
  // Overridable so the removal path can be exercised without waiting an hour:
  //   npx wrangler dev --var DEAD_GRACE_MS:5000
  const grace = Number(env.DEAD_GRACE_MS) || DEAD_GRACE_MS;
  const cursor = (await env.PEERS.get(CURSOR_KEY)) || undefined;
  const page = await env.PEERS.list({ prefix: APP_PREFIX, limit: SWEEP_MAX, cursor });

  for (const k of page.keys) {
    const url = k.name.slice(APP_PREFIX.length);
    const meta = k.metadata || {};
    const deadSince = meta.dead;
    // Every metadata write below must carry this forward — the sweep and
    // storeApp share the same metadata slot, and overwriting it wholesale on a
    // liveness change used to silently erase whatever vector had been stored.
    // Both vectors are independent and must each survive on their own.
    const keepVec = {
      ...(meta.vec    ? { vec: meta.vec }       : {}),
      ...(meta.docVec ? { docVec: meta.docVec } : {}),
    };

    if (await probe(url, env)) {
      if (deadSince) {                       // recovered — clear the mark
        const line = await env.PEERS.get(k.name);
        if (line) await env.PEERS.put(k.name, line, { metadata: keepVec });
      }
      continue;
    }

    if (!deadSince) {                        // first failure — start the clock
      const line = await env.PEERS.get(k.name);
      if (line) await env.PEERS.put(k.name, line, { metadata: { dead: now, ...keepVec } });
    } else if (now - deadSince > grace) {
      await env.PEERS.delete(k.name);
    }
  }

  // Only persist a cursor when there is more to get through; for a small index
  // the list completes in one page and this costs nothing.
  if (page.list_complete) {
    if (cursor) await env.PEERS.delete(CURSOR_KEY);
  } else {
    await env.PEERS.put(CURSOR_KEY, page.cursor);
  }
}

const HELP =
  'annessaia bootstrap\n\n' +
  'Apps — paste any of these into the annessaia address bar:\n' +
  '  /search.wasm      browse the registry (read-only)\n' +
  '  /calculator.wasm  calculator\n' +
  '  /paint.wasm       pixel-art paint\n' +
  '  /nova.wasm        NOVA, a survival game\n' +
  '  /widgets.wasm     widget gallery\n' +
  '  /gpu.wasm         GPU drawing demo\n\n' +
  'Registry (read-only — publish from a node, it gossips here):\n' +
  '  GET  /api/apps            the index, TSV\n' +
  '  GET  /api/search?q=       filtered index\n\n' +
  'Network:\n' +
  '  GET  /ws                  gossip endpoint (nodes connect here)\n' +
  '  GET  /peers               known nodes\n' +
  '  POST /register            <url>\\n<token>\n' +
  '  POST /unregister          <url>\\n<token>\n';

function withCors(res) {
  const h = new Headers(res.headers);
  for (const [k, v] of Object.entries(CORS)) h.set(k, v);
  return new Response(res.body, { status: res.status, headers: h });
}

// KV list() pages at 1000 keys, so follow the cursor rather than truncating.
async function listKeys(env, prefix) {
  const out = [];
  let cursor;
  do {
    const r = await env.PEERS.list({ prefix, limit: 1000, cursor });
    for (const k of r.keys) out.push(k.name.slice(prefix.length));
    cursor = r.list_complete ? null : r.cursor;
  } while (cursor);
  return out;
}

// ── Registry ─────────────────────────────────────────────────────────────────

async function allApps(env) {
  return (await allAppsFull(env)).map(e => e.line);
}

// Like allApps, but keeps each app's stored vector alongside its line — needed
// for /api/search to rank by meaning, not just list content. list() already
// returns metadata (that's the whole reason vectors live in metadata rather
// than in the value: one list() gets every vector "for free"), but KV list()
// cannot return values, so the actual TSV content still needs one get() per key.
async function allAppsFull(env) {
  const entries = [];
  let cursor;
  do {
    const r = await env.PEERS.list({ prefix: APP_PREFIX, limit: 1000, cursor });
    for (const k of r.keys) {
      entries.push({
        url: k.name.slice(APP_PREFIX.length),
        vec: (k.metadata && k.metadata.vec) || null,
        docVec: (k.metadata && k.metadata.docVec) || null,
      });
    }
    cursor = r.list_complete ? null : r.cursor;
  } while (cursor);

  const lines = await Promise.all(entries.map(e => env.PEERS.get(APP_PREFIX + e.url)));
  return entries.map((e, i) => ({ ...e, line: lines[i] })).filter(e => e.line);
}

const SUBMIT_REFUSED =
  'This index does not accept direct submissions.\n\n' +
  'Apps get here by gossiping in from a node, so publishing means running one:\n' +
  '  cargo run -p annessaia-server\n' +
  'then submit through that node\'s own search app at http://localhost:3000/search.wasm\n\n' +
  'Your node connects to this Worker automatically and the entry propagates here.\n';

// Store an app if its content is new or changed. Returns the stored TSV line
// when something should propagate further, or null when there's nothing new to
// forward — which is what stops gossip from echoing round the network forever.
//
// Wire shape (mirrors annessaia-server's App::to_wire exactly):
//   0 name  1 desc  2 url  3 author  4 tags  5 doc_snippet  6 vec  7 doc_vec
// Fields 0-5 are public (stored in the KV value, returned by /api/apps and
// /api/search); 6-7 are vectors and live only in KV metadata, never the value.
async function storeApp(env, line) {
  const parts = line.split('\t');
  const name = (parts[0] || '').trim();
  const url  = (parts[2] || '').trim();
  if (!name || !url) return null;

  // Same rule as peer URLs: http(s) and publicly routable. A local address is
  // unreachable for everyone else, so it could only ever be a dead entry in a
  // public index. Honours ALLOW_LOCAL so the flow is testable under wrangler dev.
  if (validate(url, env)) return null;

  const key = APP_PREFIX + url;
  const { value: current, metadata: currentMeta } = await env.PEERS.getWithMetadata(key);

  const clean0 = s => (s || '').replace(/[\t\r\n]/g, ' ').slice(0, 400);
  // doc_snippet may hold several \x1e-joined chunks (see chunksOf/bestChunk) —
  // capped per chunk, and to at most MAX_CHUNKS of them, mirroring
  // annessaia-server's sanitize_doc_snippet exactly. A whole-string slice(0,200)
  // here would truncate a 20-chunk gallery down to its first sentence or two.
  const cleanDoc = s => chunksOf(s || '')
    .slice(0, MAX_CHUNKS)
    .map(c => c.replace(/[\t\r\n]/g, ' ').slice(0, MAX_CHUNK_CHARS))
    .join(CHUNK_SEP);
  const candidate = [clean0(parts[0]), clean0(parts[1]), url, clean0(parts[3]), clean0(parts[4]), cleanDoc(parts[5])].join('\t');

  // Fields 6/7 carry the app's two embeddings, computed and quantized by
  // whichever node originated this announce — this Worker never computes one
  // itself. Anything that doesn't decode to exactly VEC_DIMS bytes is treated
  // as absent, same rule for both vectors.
  const rawVec = (parts[6] || '').trim();
  const decodedVec = rawVec ? unpackVec(rawVec) : null;
  const vec = (decodedVec && decodedVec.length === VEC_DIMS) ? rawVec : null;

  const rawDocVec = (parts[7] || '').trim();
  const decodedDocVec = rawDocVec ? unpackVec(rawDocVec) : null;
  const docVec = (decodedDocVec && decodedDocVec.length === VEC_DIMS) ? rawDocVec : null;

  if (current === candidate) {
    // Content already matches what every peer has, so there is nothing to
    // forward — but if this announce carries a vector we're missing, or ours
    // is a stale leftover from a previous embedding model (wrong dimension,
    // not just absent), store the new one anyway. That backfill then happens
    // silently through ordinary resync gossip instead of needing a one-off
    // migration whenever the model changes. The two vectors are independent:
    // doc_vec can need backfilling on its own even when vec doesn't.
    const needsVecUpdate    = vec && needsEmbedding(currentMeta && currentMeta.vec);
    const needsDocVecUpdate = docVec && needsEmbedding(currentMeta && currentMeta.docVec);
    if (needsVecUpdate || needsDocVecUpdate) {
      await env.PEERS.put(key, candidate, { metadata: {
        vec:    needsVecUpdate    ? vec    : (currentMeta && currentMeta.vec),
        docVec: needsDocVecUpdate ? docVec : (currentMeta && currentMeta.docVec),
      } });
    }
    return null;
  }

  if (!current) {
    const existing = await listKeys(env, APP_PREFIX);
    if (existing.length >= MAX_APPS) return null;
  }

  // Nothing enters the index unless it is serving right now. A node can still
  // announce a URL it does not own, but it cannot announce one that is not there.
  if (!(await probe(url, env))) return null;

  // A real content change carries forward whatever vectors arrived with it, or
  // keeps whichever ones were already stored if this announce lacked them.
  const metadata = {
    vec:    vec    || (currentMeta && currentMeta.vec),
    docVec: docVec || (currentMeta && currentMeta.docVec),
  };
  await env.PEERS.put(key, candidate, { metadata });
  return candidate;
}

// Honour a delete only once the URL has actually stopped serving. Whoever hosts
// the app is the only party who can make that true, which is what stands in for
// an ownership token — and it means a stranger cannot delist someone else's app.
// Returns true if the entry was removed.
async function revokeApp(env, url) {
  const key = APP_PREFIX + url;
  if (!(await env.PEERS.get(key))) return false;
  if (await probe(url, env)) return false;      // still up: not the owner's doing
  await env.PEERS.delete(key);
  return true;
}

// ── Gossip hub ───────────────────────────────────────────────────────────────
// Speaks the annessaia peer protocol so this Worker is a peer rather than a
// separate registry:
//   HELLO <url>       sent by both sides on connect
//   ANNOUNCE <tsv>    an app; forwarded on if it was new or changed
//   REVOKE <url>      delist an app; honoured only once the URL stops serving
//   DISCOVER <url>    a peer worth connecting to

export class Hub extends DurableObject {
  async fetch(req) {
    const url = new URL(req.url);

    // Remember our public address across hibernation — webSocketMessage has no
    // request to derive it from.
    await this.ctx.storage.put('self', url.origin);

    const [client, server] = Object.values(new WebSocketPair());
    this.ctx.acceptWebSocket(server);
    server.send(`HELLO ${url.origin}`);
    return new Response(null, { status: 101, webSocket: client });
  }

  async webSocketMessage(ws, raw) {
    const msg = String(raw).trim();
    if (!msg) return;

    if (msg.startsWith('HELLO ')) {
      const peer = msg.slice(6).trim().replace(/\/+$/, '');
      if (!peer) return;
      // Attachments survive hibernation, unlike instance fields.
      ws.serializeAttachment({ url: peer });

      const self = (await this.ctx.storage.get('self')) || '';
      const apps = await allApps(this.env);
      for (const line of apps) ws.send(`ANNOUNCE ${line}`);

      // We know every published node, so help this one mesh with the others.
      const peers = await listKeys(this.env, PEER_PREFIX);
      for (const p of peers) {
        if (p !== peer && p !== self) ws.send(`DISCOVER ${p}`);
      }
      return;
    }

    if (msg.startsWith('ANNOUNCE ')) {
      const line = msg.slice(9);
      const row = await storeApp(this.env, line);
      // Only forward what was genuinely new or changed; otherwise announcements
      // would loop around the network forever.
      if (row) this.broadcast(`ANNOUNCE ${row}`, ws);
      return;
    }

    if (msg.startsWith('REVOKE ')) {
      const url = msg.slice(7).trim();
      if (await revokeApp(this.env, url)) {
        this.broadcast(`REVOKE ${url}`, ws);
      }
      return;
    }

    // DISCOVER is informational for us — we cannot dial out, so we simply note
    // nothing. Nodes use it to find each other.
  }

  async webSocketClose(ws, code, reason) {
    try { ws.close(code, reason); } catch { /* already closed */ }
  }

  async webSocketError(ws) {
    try { ws.close(1011, 'error'); } catch { /* already closed */ }
  }

  broadcast(message, except) {
    for (const s of this.ctx.getWebSockets()) {
      if (s === except) continue;
      try { s.send(message); } catch { /* dropped connection */ }
    }
  }
}

// ── Peer directory ───────────────────────────────────────────────────────────

async function register(req, env) {
  const [url, token, err] = await parse(req, env);
  if (err) return text(err, 400);

  const key = PEER_PREFIX + url;
  const existing = await env.PEERS.get(key, { type: 'json' });

  // First registration claims the entry; later ones must prove they own it.
  // Without this a stranger could hijack or silently delist someone else's node.
  if (existing && existing.token !== token) return text('token mismatch', 403);

  if (!existing) {
    const keys = await listKeys(env, PEER_PREFIX);
    if (keys.length >= MAX_PEERS) return text('directory full', 507);
  }

  // The main anti-spam measure: you cannot list a URL that isn't already serving
  // an annessaia node.
  if (!(await alive(url))) return text('node unreachable — is it public and running?', 422);

  await env.PEERS.put(key, JSON.stringify({ token, ts: Date.now() }), { expirationTtl: TTL });
  return text('ok');
}

async function unregister(req, env) {
  const [url, token, err] = await parse(req, env);
  if (err) return text(err, 400);

  const key = PEER_PREFIX + url;
  const existing = await env.PEERS.get(key, { type: 'json' });
  if (!existing) return text('ok');                                  // already gone
  if (existing.token !== token) return text('token mismatch', 403);

  await env.PEERS.delete(key);
  return text('ok');
}

// Returns [url, token, error]. Trailing slashes are stripped so the same node
// can't occupy two entries as ".../" and "...".
async function parse(req, env) {
  const [rawUrl = '', rawToken = ''] = (await req.text()).split('\n');
  const url   = rawUrl.trim().replace(/\/+$/, '');
  const token = rawToken.trim();

  if (!token) return ['', '', 'missing token'];

  const bad = validate(url, env);
  if (bad) return ['', '', bad];

  return [url, token, null];
}

function validate(raw, env) {
  let u;
  try { u = new URL(raw); } catch { return 'invalid url'; }

  if (u.protocol !== 'http:' && u.protocol !== 'https:') return 'url must be http or https';

  // wrangler dev sets ALLOW_LOCAL so the whole flow can be tested on localhost.
  if (env.ALLOW_LOCAL === '1') return null;

  const h = u.hostname;
  const private_ =
    h === 'localhost' || h === '0.0.0.0' || h === '::1' || h.endsWith('.local') ||
    /^127\./.test(h) || /^10\./.test(h) || /^192\.168\./.test(h) ||
    /^172\.(1[6-9]|2\d|3[01])\./.test(h);

  return private_ ? 'url must be a public address, not a local one' : null;
}

async function alive(url) {
  try {
    const r = await fetch(`${url}/api/apps`, { signal: AbortSignal.timeout(PROBE_MS) });
    return r.ok;
  } catch {
    return false;
  }
}
