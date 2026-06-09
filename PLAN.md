# filestr — implementation plan

Rust workspace structured after slopd/slopctl: a foreground daemon (`filestrd`) owning all state, a thin CLI (`filestrctl`) talking to it over a unix socket at `$XDG_RUNTIME_DIR/filestrd/filestrd.sock` with newline-delimited JSON `{id, body}` request/response plus broadcast event subscriptions; TOML config at `~/.config/filestr/config.toml`; data (blob store, grants, secret key) under `~/.local/share/filestr/`; optional systemd user unit; `tracing` to stderr.

## Crate layout

```
filestr/
  libfilestr      # shared: XDG paths, config, ctl protocol types, p2p wire types,
                  # tickets, grant/handle/view model (mirrors libslop)
  libfilestrctl   # client library: socket transport, request/response, subscriptions
  filestrd        # daemon binary
  filestrctl      # CLI binary (thin wrapper over libfilestrctl)
  filestr-chat    # FUTURE, optional: nostr/Whitenoise (MDK) — hubs, DMs, nostr-over-iroh
```

## Milestones

Each milestone ends with a runnable demo and a test gate (bash e2e harness driving N local nodes on localhost, same spirit as kmux's `scripts/autotests/`).

**Progress: M0–M4 implemented and passing their gates** (`scripts/autotests/`), now with **streaming transfers**: relays splice bytes through without caching (asserted in tests), `get` supports byte ranges (`--range`), and a background transfer manager runs many downloads concurrently (`get -b`, `transfers`, `cancel`). Remaining simplifications: multi-source is sequential not parallel, no per-grant rate limits (see DESIGN.md §12). M5–M6 not started.

### M0 — spike: gated pipe (≈ a day)

- Workspace scaffold; daemon skeleton with slopd-style control socket + `filestrctl status`.
- Two iroh nodes, discovery off, custom ALPN, hardcoded allowlist; fetch one blob via iroh-blobs.
- **Gate**: stranger NodeId is rejected at accept; allowlisted fetch verifies.

### M1 — grants & invites

- `libfilestr` grant/invite/ticket types; atomic-JSON grant store.
- Token mint / `filestr1…` ticket string / redeem / atomic burn / NodeId pinning; revoke.
- CLI: `invite create [--view V] [--relay-only]`, `peer add <ticket>`, `peer ls|revoke`.
- **Gate**: token reuse fails; second connection after redemption succeeds with no token; revoked peer rejected.

### M2 — share, views, browse, fetch

- Share indexer (BLAKE3, incremental rescan); named views; per-view signed file lists.
- `ListRequest` + `FetchRequest` (ranged, bao-verified, resume).
- CLI: `share add/ls`, `view create`, `ls <peer>`, `get <peer> <hash>`.
- **Gate**: two grants with different views see different lists; interrupted fetch resumes; fetch outside view denied.

### M3 — search v1 (local, streaming)

- `SearchRequest` over one bidi stream, streaming incremental results scoped to caller's view.
- CLI: `search <peer> <query>` printing results as they arrive.
- **Gate**: results stream before scan completes on a large share.

### M4 — reshare: recursive search + streaming relayed fetch

- query-id LRU, TTL decrement + per-grant clamp, fan-out/rate caps.
- Handle table; result re-attribution (origin-free wire format); **streaming** relayed transfer through ≥2 hops (relay splices bytes, no caching); byte-range requests; background transfer manager (concurrent downloads, list/cancel).
- `reshare.serve` / `reshare.allow` flags wired through.
- **Gate** (3–4 node line A–B–C[–D]): C's search finds A's file with no A-identifying bytes on the C wire; relayed fetch streams + verifies; the relay's store stays empty (no cache); a clipped byte range returns exact bytes; concurrent background gets complete; cycle graph terminates; `reshare=false` prunes.

### M5 — optional chat plane: hubs (feature-gated `filestr-chat`)

- MDK/whitenoise-rs integration: create hub (MLS group), join, member list, chat send/recv.
- Invite tickets over E2EE DM; **join ⇒ auto-grant to owner**; leave/kick ⇒ auto-revoke (watch own MLS removal).
- **nostr-over-iroh** (DESIGN §8.2): embedded relay served to grantees over the reserved `nostr` stream type, so hubs work with zero public relays.
- Hub-level "looking for X" structured message + opt-in auto-offer.
- CLI: `hub create|join|members|chat`, `invite send <nostr-pubkey>`.
- Everything in M0–M4 must keep working with this feature compiled out or disabled.
- **Gate**: full join flow on local relay (e.g. `nak serve`) *and* over the iroh tunnel with no relay configured; kick revokes within one poll interval.

### M6 — hardening & daily-driver polish

- Slots/bandwidth limits, relay cache (LRU, off by default), handle expiry/refresh.
- Persistence across restarts; graceful NodeAddr changes (re-ticket via DM).
- TUI or minimal web UI; download queue.
- **Gate**: soak test — 5 nodes, churn (restarts, revokes, reindex), 24 h, no leaks/stalls.

## Order rationale

Data plane before chat plane: M0–M4 are testable hermetically on localhost with zero external services, and they contain all the novel protocol work (grants, attribution-free reshare, relayed verified fetch). M5 is mostly integration with audited-but-moving deps (Marmot), so it benefits from landing late against pinned versions.

## Risks

| risk | mitigation |
|---|---|
| iroh 1.0-rc API churn | pin rc; M0 isolates endpoint glue in one module |
| Marmot/MDK spec churn, DM ergonomics unknown | pin versions; invite *format* is ours, only transport is theirs; fallback NIP-17 |
| search amplification on dense graphs | caps in M4 gate from day one, not retrofitted |
| O(share) rehash on startup | mtime+size fast path, hash only changed files |
