# filestr — design

## 1. Overview

A friend-to-friend (F2F) file-sharing network wearing DC++'s social layer.

Every node runs the same software and plays three roles at once:

1. **Hub owner** — owns a Whitenoise/MLS chat group ("my hub").
2. **Hub member** — joined other people's hubs; chats with everyone there.
3. **File peer** — serves its share to the exact set of nodes it has granted access to, and (optionally) reshares content from nodes that granted *it* access.

Role 3 is the core and works **iroh-only**: peering, browse, search, and fetch require no nostr identity and no chat plane — invites travel as out-of-band ticket strings (§3.1). Roles 1–2 (hubs, chat) come from the **optional** nostr plane, which can itself be carried over iroh grants (§8.2).

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

## 2. Identities and planes

Each node has two keypairs:

| | key | used for |
|---|---|---|
| chat plane (optional) | nostr secp256k1 | Whitenoise/MLS groups, DMs, invite transport |
| data plane | iroh ed25519 NodeId | QUIC connections, grants, transfers |

**No public discovery.** The iroh endpoint does not publish to pkarr/DNS discovery. A node's dialable `NodeAddr` (relay URL + optional direct addrs) travels only inside invites. Knowing a NodeId alone resolves to nothing.

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
- `active → revoked`: manual, or automatic — leaving a hub revokes the grant to its owner; observing one's own MLS removal (kick) does the same.
- Enforcement: accept loop checks `remote_node_id()` against active grants; strangers cost one rejected TLS handshake.

## 4. Hubs (optional chat plane)

A hub is an MLS group (Marmot/Whitenoise) owned by one node. Chat semantics are entirely Whitenoise's: E2EE, async via nostr relays, admins can add/remove members.

### 4.1 Join flow

1. Prospective member asks the owner (any channel; typically nostr DM).
2. Owner adds them to the MLS group → they can chat with everyone.
3. Joiner's client automatically issues a grant + invite **to the owner only** (the price of admission) and DMs the ticket.
4. Owner's client may verify the share is nonempty / spot-check a file; stonewalling ⇒ kick.

Membership grants file access to **no one else**. Member↔member sharing is always an explicit pairwise grant (typically negotiated in chat).

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

### 8.2 nostr over iroh

A reserved `nostr` stream type tunnels the nostr relay protocol (NIP-01 client↔relay JSON messages) over a granted iroh connection: a filestr node can expose an embedded nostr relay to its grantees. Chat (Whitenoise/MLS hubs, DM invite transport) then needs **zero public nostr relays** — events sync peer-to-peer over the same grant graph as files, and public relays remain merely another interchangeable transport. This keeps the chat plane optional *and* self-hostable: an iroh-only network can later turn on hubs without any new infrastructure.

## 9. Settings

| setting | scope | default |
|---|---|---|
| `reshare.serve` — relay others' content to my grantees | node | `true` |
| `reshare.allow` — grantee may relay my content | per-grant | `true` |
| `search.max_ttl` | node + per-grant clamp | 5 |
| `relay_only_addr` — hand out relay-only NodeAddr (no direct addrs) | per-invite | `false` |
| slots / bandwidth / rate caps | node + per-grant | sensible |

Setting `reshare.* = false` everywhere degrades gracefully to a pure pairwise F2F sharer; the reshare layer is purely additive.

## 10. Threat model

**Prevented**
- Strangers discovering your node, browsing, fetching, or even completing a handshake.
- Hub members seeing your files via mere co-membership.
- Search/result receivers learning *which node* exposed a result (attribution-hiding).
- Relays (iroh or filestr peers) tampering with content (BLAKE3 e2e verification).

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
| MDK / `whitenoise-rs` (Marmot) | MLS groups + DMs over nostr | **optional feature**; audited 2026; spec still evolving — pin |
| `rust-nostr` | nostr primitives, embedded relay for §8.2 | **optional feature** |
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

## 13. Open questions

1. DM transport for invites: Whitenoise 1:1 MLS group vs NIP-17 gift wrap — decide at M5 based on MDK ergonomics.
2. Handle lifetime / refresh under long multi-source fetches.
3. Parallel multi-source range-splitting and optional relay caching, and how a cache interacts with `allow_reshare = false` upstream.
4. File-list format: flat table vs DC++-style directory tree (affects browse UX only).
