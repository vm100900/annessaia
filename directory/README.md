# annessaia directory

A Cloudflare Worker holding the bootstrap peer list for the annessaia registry.

Nodes gossip app entries to each other over WebSockets, but a brand-new node has no
way to find its first peer. It reads `GET /peers` here, connects to what it finds,
and from then on gossip handles everything. No app data passes through this Worker —
it is only a phone book.

## Deploy

Needs a free Cloudflare account and `npx wrangler`.

```sh
cd directory
npx wrangler kv namespace create PEERS   # paste the returned id into wrangler.toml
npx wrangler deploy
```

Copy the resulting `https://bootstrap.<you>.workers.dev` URL into
`DEFAULT_DIRECTORY` in `server/src/main.rs`.

## Run locally

```sh
npx wrangler dev --var ALLOW_LOCAL:1     # serves :8787, KV is simulated locally
```

`ALLOW_LOCAL=1` permits registering `localhost` URLs, which the public deployment
rejects. Point a node at it with `ANNESSAIA_DIRECTORY=http://localhost:8787`.

## API

| Route | Body | Behaviour |
|---|---|---|
| `GET /peers` | — | newline-separated node URLs |
| `POST /register` | `<url>\n<token>` | claim or refresh a listing |
| `POST /unregister` | `<url>\n<token>` | delete a listing |

## Why writes need no API key

A key committed to an open-source repo is not a key. Three guards stand in for auth:

1. **Public URLs only** — localhost and private ranges are rejected.
2. **Liveness probe** — the Worker fetches `<url>/api/apps` before storing anything,
   so a URL that isn't already running an annessaia node can't be listed.
3. **Token ownership** — the first `/register` for a URL stores a token the node
   generated locally. Later writes to that entry must present the same token, so
   nobody else can hijack or delist it.

Listings expire after 3 days; published nodes re-register every 12 hours to stay
listed, which also prunes dead nodes automatically.

## Free tier

Workers KV gives 100k reads/day, 1k writes/day, 1GB. At 2 writes per node per day
the write budget supports roughly 500 nodes. Workers never sleep.
