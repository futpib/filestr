# filestr — design

## 1. Overview

A friend-to-friend (F2F) file-sharing network wearing DC++'s social layer.

Every node runs the same software and plays three roles at once:

1. **Hub owner** — owns a Whitenoise/MLS chat group ("my hub").
2. **Hub member** — joined other people's hubs; chats with everyone there.
3. **File peer** — serves its share to the exact set of nodes it has granted access to, and (optionally) reshares content from nodes that granted *it* access.

Role 3 is the core and works **iroh-only**: peering, browse, search, and fetch require no nostr identity and no chat plane — invites travel as out-of-band ticket strings (§3.1). Roles 1–2 (hubs, chat) come from the nostr plane, which can itself be carried over iroh grants (§8.2).

The chat plane is optional **at compile time** (the `chat` cargo feature, default on; `--no-default-features` builds a pure iroh-only binary) *and* **at runtime** (`[chat] enabled`, default true). A default binary with `enabled = false` runs and peers files with no nostr at all — no identity activation, no MLS store, no relays, no listeners; hub commands return "chat disabled." Flip it on and restart to join hubs later. So a node can pair and share files first and opt into chat whenever.

**Ticket / nostr independence.** The plain invite (`filestr1…`) is fully nostr-independent — it works in an iroh-only build or with chat disabled. The hub tickets (`filestrhub1…`, `filestrreq1…`) and the hub address (`filestraddr1…`) are chat constructs; their embedded *file* invite still peers without nostr, but completing the hub (MLS) join needs the chat plane on.

### Goals

- No one sees your files (or your node address) without an explicit per-peer grant — including fellow hub members.
- Community chat with everyone in a hub, independent of file access.
- Search and fetch across the whole grant graph, without the wire format attributing content to its origin node.
- Verified transfers: a relayed fetch is exactly as tamper-proof as a direct one.
- Chat plane strictly optional; everything file-related works on iroh alone.
- Protocols evolve additively and stay backwards-compatible (§8.1).

### Non-goals

- **Anonymity.** No defense against traffic analysis, timing correlation, or network observers. iroh relays see node↔node connection metadata; nostr relays see IPs. Attribution-hiding (§7) only prevents the protocol itself from doxxing who exposed which file.
- Public/open hubs, stranger discovery, DHTs.
- Cryptographic enforcement of social rules (share-to-join, reshare permission). These are cooperative, verified socially (§5, §7.5) — same as DC++ hub rules ever were.

## 2. Identities and keys

The node's **root secret is its nostr identity** — a secp256k1 key stored as an `nsec`. The iroh transport key is *derived from it* one-way, so the single nsec is the whole node identity and is portable (importable into nostr clients).

| key | curve | source | used for |
|---|---|---|---|
| nostr identity (root) | secp256k1 | stored `nsec` in `identity.key` | Marmot/MLS hubs, member id |
| iroh endpoint | ed25519 | `BLAKE3-derive_key("…iroh transport key v1", nsec_bytes)` | QUIC connections, grants, transfers |

**Storage.** `identity.key` holds one `nsec1…` (also accepts raw hex), under the data dir; generated on first run (rejection-sampled to a valid secp256k1 scalar). Nothing else is persisted — the iroh key is recomputed each start. The derivation is one-way: the iroh key never reveals the nsec.

**Strict permissions (SSH-style).** Secret files are written `0600` and every state dir is `0700`. On load, a secret file is **refused** if group/others can access it (`mode & 0o077`) *or* it isn't owned by us (uid ≠ our euid, and ≠ root) — the same `StrictModes` checks OpenSSH does, with a fix hint (`chmod 600` / `chown`).

**XDG layout.** Paths follow the XDG Base Directory spec, split by durability:

| file(s) | base | dir |
|---|---|---|
| `identity.key`, `iroh.key` | `$XDG_DATA_HOME` | `…/filestr/` |
| `grants.json`, `reputation.json`, `hubs.json`, `mls.sqlite` (encrypted) | `$XDG_STATE_HOME` | `…/filestr/` |
| `blobs/` (regenerable by rescan) | `$XDG_CACHE_HOME` | `…/filestr/` |
| control socket | `$XDG_RUNTIME_DIR` | `…/filestrd/filestrd.sock` |
| `config.toml` | `$XDG_CONFIG_HOME` | `…/filestr/` |

So a backup of the data dir is the whole identity; clearing the cache just triggers a rescan; grants survive both. An explicit `data_dir` in the config collapses all three persistent dirs into one root (for single-dir or isolated deployments); grants found in the old data-dir location are migrated to the state dir on startup.

**Override.** Drop a hex 32-byte `iroh.key` next to `identity.key` and it takes precedence for the endpoint identity (the nsec still drives the nostr identity). Use this to pin a pre-existing endpoint id — and the tickets that reference it — while the nsec drives the rest. Conversely, to adopt an existing nostr identity, just write your `nsec` into `identity.key`.

**No public discovery.** The iroh endpoint does not publish to pkarr/DNS discovery. A node's dialable `NodeAddr` (relay URL + optional direct addrs) travels only inside invites. Knowing a NodeId alone resolves to nothing.

**Configurable iroh relays.** `relay = "default"` uses n0's public relays, `"disabled"` is direct-only, and `relay_urls = [...]` points the endpoint at self-hosted relay servers instead. Relays only assist connectivity (hole-punching / fallback); they never see plaintext.

**Identity binding** needs no extra ceremony: an invite token is delivered over a channel the grantor already trusts (a pasted ticket, or — with the chat plane — an E2EE DM to a specific nostr identity), and redemption pins the redeeming NodeId. The token links "person I gave the ticket to" to "node now connecting".

## 3. Grants

A **grant** is the single access-control primitive: *grantor* allows *grantee* to connect, browse a view of the share, fetch, and search.

### 3.1 Invite flow

1. Grantor mints an invite: `{node_addr, token (random 128-bit), expiry, view, flags}`.
2. Invite is serialized as a compact self-contained **ticket string** (`filestr1…`, base32) and delivered out-of-band over any channel the grantor trusts: pasted into a chat, QR, sneakernet. With the chat plane enabled, a Whitenoise E2EE DM is the convenient transport (nostr relays never see `node_addr`).
3. Grantee dials, opens the first stream with `InviteRedeem{token}`.
4. Grantor verifies, **burns the token atomically**, pins the redeeming NodeId. Grant is now `active`.

Tokens are single-use *enrollment*: the pinned NodeId reconnects freely until revoked. (Per-session invites would make resumed downloads miserable.)

### 3.2 Grant record (grantor side)

```
Grant {
  node_id,            // pinned at redemption
  view,               // share view id (§5.2)
  allow_reshare: bool,// advisory: may grantee re-serve my content (default true)
  max_ttl: u8,        // cap on search TTL I accept from this peer
  limits,             // slots, bandwidth, rate caps
  origin,             // pairwise | hub-join(group_id)
  state,              // issued | active | revoked
}
```

### 3.3 Lifecycle

- `issued → active` on token redemption; `issued → expired` on timeout.
- `active → revoked`: manual (`peer revoke`). Automatic revoke on hub leave/kick is planned (the MLS removal primitive is wired; the command is not yet — §4.x).
- Enforcement: accept loop checks `remote_node_id()` against active grants; strangers cost one rejected TLS handshake.

## 4. Hubs (optional chat plane — implemented)

A hub is a real **Marmot/MLS group** (via `mdk-core` + OpenMLS, the White Noise stack) owned by one node. Messages are MLS application messages (forward secrecy, post-compromise security) carried as nostr kind:445 events; membership uses MLS key packages (kind:30443) and welcomes (kind:444). The hub **owner hosts an embedded NIP-01 relay** ([§8.2](#82-nostr-over-iroh)) reachable by members over the iroh `nostr` stream — so a hub needs **no external nostr relay**.

### 4.1 Join flow (implemented)

Driven entirely by the daemons; the user just pastes a `filestrhub1…` ticket.

1. Owner mints a hub ticket = a filestr invite (owner→member grant, so the joiner can reach the owner's relay) + the hub name + the MLS group ref.
2. Joiner redeems the invite (connects to the owner) and, as the **price of admission**, mints a reciprocal filestr invite back to the owner — the share-to-join grant — alongside its MLS key package.
3. Joiner sends both to the owner over the `hub` control RPC. The owner redeems the reciprocal invite (gaining file access to the joiner), runs MLS `add_members`, and returns the MLS welcome.
4. Joiner processes the welcome → joined. Messages flow through the owner's relay; each member advances its own MLS state by processing the kind:445 events it pulls.

Membership grants file access to the **owner only** (the reciprocal grant). Member↔member file sharing remains an explicit pairwise grant.

### 4.x v1 scope

Implemented: create, invite, join (with share-to-join), members, send, log, all E2EE over the iroh tunnel, with **persistent MLS state** — groups, membership, and history survive daemon restarts (`mdk-sqlite-storage`, SQLCipher-encrypted at rest with a key derived from the root, in the state dir). The hub registry (names, roles, how to reach owners) persists alongside it as `hubs.json`. Deferred: member removal/kick rekeying (an admin can revoke the file grant today, but MLS removal that locks a member out of future messages — `remove_members` is wired but not yet exposed via a kick command) and member↔member relay federation (members reach the owner's relay only).

### 4.2 Hub-level search fallback

Structured "looking for X" chat messages let members respond manually or let clients auto-offer ("I have it — want an invite?"). This is the human-level fallback under the graph search of §6 and works with reshare disabled everywhere.

## 5. Share, views, file lists

### 5.1 Index

The node indexes configured share roots: relative path, size, mtime, BLAKE3 hash (incremental rehash on change). Hashes double as iroh-blobs content addresses.

### 5.2 Views

Every grant references a **view**: a named subset of the share (set of subtree roots ± exclusions). The file list a peer sees is generated per view. "This hub's owner gets `music/`, that friend gets everything." Default views: `full`, `nothing`.

### 5.3 Browse

`ListRequest` returns the signed, compressed file list for the caller's view — generated live, never published anywhere. **Browse is local-share only**: reshared remote content (§7) never appears in file lists, only in search results. (Merged transitive lists would grow with the network, churn constantly, and their directory structure would fingerprint origins.)

## 6. Search

Recursive, streaming, breadth-first over the grant graph.

1. Requester opens one bidi stream: `SearchRequest{query_id: random 128-bit, ttl, query}`.
2. The serving node immediately streams back local matches from the caller's view, and — if it reshares — concurrently forwards the query (ttl−1) to every neighbor *it* can search whose grant allows reshare. Incoming remote results are forwarded upstream as they arrive, re-attributed as the serving node's own (§7).
3. Stream stays open until ttl exhaustion / timeout / caller closes; results are incremental by construction.

**Loop prevention**: per-node LRU of seen `query_id`s; repeats are dropped (the grant graph has cycles).

**TTL**: plain decrementing hop cap (default 5, clamped by each grant's `max_ttl`). No fuzzing — inferring "my neighbor is probably the origin" is fine; the neighbor already has a grant relationship with you. No hop counts in results.

**Abuse control**: per-neighbor query rate limits, fan-out cap, cap on concurrent forwarded searches, result-count cap per query.

## 7. Reshare and relayed fetch

### 7.1 Semantics

With resharing enabled, content reachable through your inviters is served to your grantees **as if it were your own**. Wire format carries no origin NodeId, no nostr key, no path, no hop count — a result is `{name, size, hash, handle}` and nothing else. Side effect: peers cannot distinguish your own files from reshared ones.

The BLAKE3 hash stays global (it *is* the content's identity and what makes verification and multi-source work). Attribution-hiding hides *who exposed it*, not *what it is*.

### 7.2 Handles

Each search result carries an opaque **handle** (random 128-bit) minted by the hop that delivered it:

```
HandleTable: handle -> Local | Remote{neighbor_node_id, upstream_handle}
```

A `Local` handle means "serve from my own store" (the fetcher names the hash in its get request, so the handle need not store it). A `Remote` handle points at the neighbor the result came from. Handles expire (default 1 h, refreshed on use). Each hop knows only its neighbor mapping; no node holds a full path.

### 7.3 Streaming fetch (pass-through)

A `get{handle}` request opens a bidi stream; after the one-line header, the rest of the stream **is** an iroh-blobs get exchange. The fetcher picks the hash and byte ranges there and reads back a **bao-verified stream**.

- **Local / no handle** → the server runs the iroh-blobs provider directly on the stream, serving from its store.
- **Remote handle** → the relay dials its upstream, forwards the header, and **splices raw bytes both directions** without buffering. The transfer streams through hop by hop; the relay never stages the content to disk, and verification stays end-to-end (the fetcher checks every byte against the hash regardless of how many relays it crossed).

Every hop of a relayed fetch is a legitimate grant relationship: the relay is an authorized fetcher acting on behalf of its own grantee. Nobody ever serves a stranger. Because transfer rides the *same* `filestr/0` ALPN as control (after the header), there is no separately-dialable blob endpoint to gate — one allowlist covers everything.

### 7.4 Ranges, background transfers, multi-source

- **Byte ranges**: a get request may ask for an inclusive byte range; only the covering chunks cross the wire (rounded to chunk boundaries, clipped on export). Verification still holds for partial content.
- **Background**: the daemon runs every fetch as a tracked transfer in its own task, so many downloads proceed concurrently; clients start one and either stream its progress or detach and poll `transfers`.
- **Multi-source**: the same hash may arrive via several handles. v1 tries known sources sequentially, falling over on failure; parallel range-splitting across sources is future work.
- **Caching**: streaming pass-through means a relay does *not* retain what flows through by default. Optional relay caching (a relay keeping a copy it then genuinely owns, thickening the swarm) is future work and would be size-bounded LRU.

### 7.5 Reshare permission is advisory

`allow_reshare = false` on a grant asks the grantee's client not to forward your content. Honest clients honor it; it is not enforceable (a peer can always copy bytes and re-host manually). Stated plainly rather than pretended otherwise.

## 8. Wire protocol

- Transport: iroh QUIC, single ALPN `filestr/0`. Control and transfer share it: a `get` request switches its stream from JSON to the iroh-blobs protocol after the header (§7.3), so there is no separately-dialable blob endpoint and one grant allowlist covers everything.
- Encoding: newline-delimited JSON, tagged messages (`{"type": ...}`), one bidi stream per request. Transfer payloads are bao verified streams (iroh-blobs machinery) carried inline after a `get` header. JSON over postcard deliberately: self-describing, field-extensible, debuggable — the compatibility properties of §8.1 outweigh wire compactness for control traffic.
- The request header line is read **without buffering past the newline**, so the remainder of a `get` stream is pristine for the iroh-blobs protocol.
- Requests: `hello`, `redeem`, `list`, `search` (streaming response), `get` (streaming transfer — header then bao stream, §7.3), `nostr` (reserved, §8.2).
- Connection acceptance: unknown NodeId ⇒ only `redeem` allowed; granted NodeId ⇒ all requests, scoped to the grant's view/limits.

### 8.1 Protocol evolution and compatibility

Applies to the p2p protocol, the ticket format, and the local control socket alike:

- **Tagged, self-describing messages.** Every message carries `"type"`; unknown *fields* are ignored on decode, and new fields always ship with serde defaults — old nodes read new messages, new nodes read old ones.
- **Unknown request types** get a structured `{"type":"error","code":"unsupported"}` response — never a connection kill, never a parse abort.
- **Feature negotiation.** Each connection starts with a `hello` exchange advertising `features: ["reshare", "nostr-tunnel", ...]`; peers degrade gracefully to the intersection.
- **ALPN carries the major version** (`filestr/0`). Truly incompatible revisions bump it; a node may accept several ALPNs during a migration window.
- **Additive evolution only**: fields and message types are never repurposed or re-typed; deprecation = stop sending, keep accepting.
- Tickets carry an explicit `v` field; unknown ticket versions fail with a clear "upgrade filestr" error.

### 8.2 nostr over iroh (implemented)

The `nostr` stream type tunnels the nostr relay protocol (NIP-01 client↔relay JSON messages) over a granted iroh connection: a filestr node exposes an embedded in-memory NIP-01 relay to its grantees. After the `nostr` header line, the rest of the stream is a NIP-01 session (REQ/EVENT/EOSE/CLOSE). Chat then needs **zero public nostr relays** — hub events sync over the same grant graph as files. The chat plane is thus optional *and* self-hostable: an iroh-only network turns on hubs with no new infrastructure. (External nostr relays remain a possible interchangeable transport but are not required.)

The `hub` request is a small control RPC (opaque to the wire layer; the `chat` feature defines the payload) used for the join handshake — carrying the joiner's MLS key package + reciprocal invite and returning the MLS welcome.

**Everything we emit is Whitenoise — no public notes.** Every nostr event the daemon publishes is either an MLS group message (kind 445, ciphertext) or a **NIP-17 gift-wrapped** private message (kind 1059). We never post a plaintext note.

**Join over nostr (no announcements).** Instead of publishing a discoverable note, a hub owner produces a small shareable **hub address** (`filestraddr1…` — owner pubkey + relays + group ref). It is not a ticket (no token, not single-use) and is not published by us; the owner shares it however they like (paste, profile bio, QR). A newcomer runs `hub request <address>`, which gift-wraps a join **request ticket** (`filestrreq1…`) to the owner's pubkey and sends it over the address's relay(s) — *no prior grant needed*. The owner's daemon subscribes for gift wraps addressed to it, unwraps them, and either **auto-admits** (`[chat].auto_admit`, open-hub UX) or queues them for `hub pending` / manual `hub admit`. Admit redeems the requester's symmetric invite (mutual access) and pushes the MLS welcome back over iroh.

The **request ticket** is self-contained (symmetric invite + MLS key package + optional target hub), so the same single-use artifact works pasted out-of-band *or* gift-wrapped over nostr. (Hardening: gate auto-admit on the reputation/vouch policy.)

**Configurable nostr relays.** The chat plane's relays are configurable under `[chat]`:
- `embedded_relay` (default true) — serve the embedded relay over the iroh `nostr` stream.
- `relay_listen` (e.g. `"127.0.0.1:7777"`) — additionally expose the embedded relay as a **standard WebSocket NIP-01 relay**, so ordinary nostr clients (or other filestr nodes) can use this node as a relay.
- `relays = ["wss://…", "ws://…"]` — external nostr relays the node also publishes hub events to and reads them from (over WebSocket), and advertises in hub metadata.

So a deployment can run hubs entirely over the iroh tunnel (zero external infra, the default), entirely over public/self-hosted nostr relays (`embedded_relay = false`, `relays = [...]`), or both. The same code path verifies MLS regardless of which relay carried the event.

## 9. Settings

| setting | scope | default |
|---|---|---|
| `reshare.serve` — relay others' content to my grantees | node | `true` |
| `reshare.allow` — grantee may relay my content | per-grant | `true` |
| `search.max_ttl` | node + per-grant clamp | 5 |
| `relay_only_addr` — hand out relay-only NodeAddr (no direct addrs) | per-invite | `false` |
| slots / bandwidth / rate caps | node + per-grant | sensible |

Setting `reshare.* = false` everywhere degrades gracefully to a pure pairwise F2F sharer; the reshare layer is purely additive.

### 9.1 Reputation / anti-free-riding (implemented)

Each edge of the grant graph is a repeated game. Since BLAKE3 makes content corruption impossible and the invite system makes identity costly, the only residual cheat is **free-riding** — taking bytes without giving. We counter it with a **local, first-hand reciprocity ledger**: per direct neighbour we track decaying counters of verified bytes *served to* vs *received from* them (delivery is provable, so the ledger is tamper-evident at each edge). A peer whose debt (`served − received`) exceeds a credit limit is **denied content** until they reciprocate — search/browse, being cheap, still go through.

Game-theoretic shape: a **credit limit** tolerates the naturally lumpy/asymmetric interest in content (a friend who hosts a lot and downloads little is a *creditor*, served more, not penalised); a **newcomer budget** is the optimistic-unchoke that lets cooperation bootstrap; an exponential **half-life decay** means neither grudges nor goodwill are permanent. Relay work counts as serving the *downstream* neighbour, so resharing builds standing and stays incentivised. Reputation is deliberately **never global** — it can't be (attribution-hiding hides origins) and that also denies any slander/Sybil-reputation surface; your own first-hand bytes are the only input.

| setting | scope | default |
|---|---|---|
| `reputation.enabled` | node | `true` |
| `reputation.credit_limit_mib` — debt tolerated before denial | node | 256 |
| `reputation.newcomer_budget_mib` — bootstrap allowance | node | 64 |
| `reputation.half_life_days` — counter decay | node | 7 |
| `reputation.over_limit` — `deny` or `serve` (off) | node | `deny` |
| `[[reputation.override]]` keyed by node-id prefix or grant label | per-peer | — |

Per-peer overrides let you, say, give a trusted friend an effectively unlimited credit limit or exempt them entirely (`over_limit = "serve"`), or tighten a flaky peer. `filestrctl rep` shows the ledger and each peer's current decision. (Throttle and search-only responses, and friend-of-friend vouching for bootstrapping, are designed extensions — see §13.)

## 10. Threat model

**Prevented**
- Strangers discovering your node, browsing, fetching, or even completing a handshake.
- Hub members seeing your files via mere co-membership.
- Search/result receivers learning *which node* exposed a result (attribution-hiding).
- Relays (iroh or filestr peers) tampering with content (BLAKE3 e2e verification).
- Free-riding: a peer that only takes is cut off past its credit limit (§9.1).

**Not prevented (out of scope)**
- iroh/nostr relays observing IPs and connection metadata.
- A direct grant peer observing your traffic, queries, and share view.
- Traffic-analysis / timing correlation by colluding peers.
- A grantee copying and re-hosting your content (as with all file sharing).

## 11. Dependencies

| dep | role | note |
|---|---|---|
| `iroh` | QUIC p2p endpoint | 1.0.0-rc — pin; API may still shift before 1.0 |
| `iroh-blobs` | BLAKE3/bao verified streaming + provider/get over abstract streams | used as a library, carried inline on `filestr/0`; filestr gates access itself |
| `mdk-core` + `mdk-sqlite-storage` (Marmot, on OpenMLS) | MLS hubs — groups, key packages, welcomes, messages; persistent, SQLCipher-encrypted at rest | **`chat` feature**; audited 2026; spec still evolving — pinned at 0.8 |
| `nostr` (rust-nostr) | nostr keys/events/filters, NIP-01 relay messages for §8.2 | **`chat` feature**, pinned at 0.44 |
| `tokio-tungstenite` | WebSocket server/client for external/standard nostr relays | **`chat` feature** (wss via rustls) |
| `serde_json` | wire encoding (p2p + control socket), grant persistence (atomic writes) | |

## 12. v1 implementation notes (deliberate simplifications)

- **Hash is a capability**: any *granted* peer who knows a 32-byte BLAKE3 hash
  can fetch that blob from our store, even across views. Hashes are unguessable
  and only disclosed through view-scoped list/search responses, so this leaks
  nothing in practice, but per-view enforcement on transfer is future work.
- **Feature negotiation is per-request**: `hello` exists as a request and
  `redeemed` carries `{v, features}`, but v1 nodes don't yet vary behavior on
  the advertised feature set (there is only one implementation).
- Grant-level `max_ttl` and rate limits from §3.2 are not yet enforced
  per-grant; the node-level TTL clamp, fan-out cap, and result cap are.
- **Multi-source is sequential**: `get` tries known sources one at a time and
  fails over; it does not yet split ranges across sources in parallel.
- **Range progress is chunk-granular**: a ranged transfer reports payload bytes
  for the chunks covering the range (rounded to chunk boundaries), so reported
  `transferred` can exceed the requested range length; the exported file is
  still clipped exactly.
- Relays do **not** cache pass-through content (streaming, §7.3); optional
  swarm-thickening cache is future work.
- **Reputation (§9.1)** charges a local serve by the *full* blob size (a
  ranged fetch is billed the whole blob — conservative, can't be gamed down);
  relayed serves are billed exact bytes. Only the `served > limit` → deny lever
  is wired; throttle/search-only and quality signals (promise-keeping, stall
  rate) are designed but not yet enforced.
- **Chat (§4):** the hub owner is the sole relay host (no member↔member
  federation yet), and MLS member removal/kick is wired in the library but not
  yet exposed as a command.

## 13. Open questions

1. DM transport for invites: Whitenoise 1:1 MLS group vs NIP-17 gift wrap — decide at M5 based on MDK ergonomics.
2. Handle lifetime / refresh under long multi-source fetches.
3. Parallel multi-source range-splitting and optional relay caching, and how a cache interacts with `allow_reshare = false` upstream.
4. File-list format: flat table vs DC++-style directory tree (affects browse UX only).
5. Reputation extensions (§9.1): throttle/search-only responses instead of hard deny; quality signals (advertised-hit delivery rate, stall/abort rate) folded into the score; positive-only friend-of-friend vouching (per-hop discounted, never overriding first-hand data) to bootstrap trust in newcomers.
