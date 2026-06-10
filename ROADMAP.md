# filestr — UX roadmap

A fair overview of where filestr stands against one target experience:

> *I see it on nostr → join the network → start sharing → explore files.*

For each step: what works today, and what's missing for it to feel seamless.
See [DESIGN.md](DESIGN.md) for the architecture and [PLAN.md](PLAN.md) for the
build history.

## The framing tension

filestr today is **invite-only friend-to-friend by design** — anti-doxxing
(nobody discovers your node or files without an explicit grant) was an explicit
goal. The journey above is **public-discovery-driven** ("I *see* it on
nostr"). Those pull in opposite directions, so much of the "missing UX" is
really *"we deliberately didn't build public discovery, and a smooth public
onboarding needs some of it back — carefully, without throwing away the
privacy properties."*

---

## 1. "I see it on nostr"

**Works**
- The node identity is a real `nsec`/`npub` (portable, importable into Amber /
  Damus / other nostr clients). This is the one genuinely nostr-native piece.

**Missing**
- **Nothing is ever published publicly.** A hub posts no discoverable note, has
  no profile (NIP-05), no `naddr`/`nevent` to click. There is literally no
  "it" to see on nostr yet.
- **No nostr deep-link / handoff** — seeing a hub in a nostr client can't "open
  in filestr."
- **Invites are manual copy-paste.** The design called for delivering tickets
  over an E2EE DM, but nostr DM (NIP-17) send/receive was never built; you
  paste a `filestrhub1…` string by hand.

## 2. "...and join the network"

**Works**
- One `filestrhub1…` ticket joins chat **and** peers files both ways
  (share-to-join) — slick, once you *have* the ticket.

**Missing**
- **No self-serve join.** Joining requires the owner to *proactively mint and
  hand you* a single-use ticket. There is no "see your hub's npub → request to
  join → you approve" loop. That request/approval flow is the thing that would
  make "see on nostr → join" actually work, and none of it exists.
- **"The network" is ambiguous** — filestr has no single network; it's a trust
  graph of hubs + pairwise grants. Joining = joining *a hub* (a community).
  Coherent, but there's no onboarding concept of "the network."
- **Availability:** the hub owner is the sole admitter *and* sole relay host.
  Offline owner ⇒ nobody can join and the hub goes dark. No multi-admin, no
  member↔member relay federation.

## 3. "...and start sharing"

**Works**
- Share-to-join means joining already shares with the owner. Transfers are
  verified, streaming, resumable, ranged, and backgroundable — the transfer
  primitives are solid.

**Missing**
- **No runtime share management.** You share by hand-editing `config.toml`
  `[[share]]` + `rescan`. No `filestrctl share add <dir>`, no GUI folder
  picker, no drag-and-drop.
- **No quick one-shot share** (sendme-style `filestr share ./file` → a link)
  for the "just send this" case.
- **No GUI / mobile** — daemon + CLI only. For this UX target, the heaviest
  lift.

## 4. "...and explore files"

**Works**
- Search recurses the grant graph (reshare), so you can explore transitively
  across your trust set, attribution-hidden. This is the real, working
  exploration story — better than it sounds.

**Missing**
- Exploration is **bounded by who you've joined** — no public catalog (by
  design). "Explore the network" is really "explore your hub's reach," which
  depends on landing in a well-connected hub.
- Search is **filename-substring only** — no tags, full-text, categories,
  previews, thumbnails, or rich metadata. Results are CLI lines.
- **Pull-based, no live updates** — `hub log` / `search` are on-demand; no push
  of new messages/files, no notifications.

---

## Biggest blockers, ranked

1. ~~**MLS state is in-memory** — hubs vanish on daemon restart.~~ **DONE** —
   MLS now persists via `mdk-sqlite-storage` (SQLCipher-encrypted at rest, key
   derived from the root); the hub registry persists as `hubs.json`. Verified
   by `test-persistence.sh` (groups, membership, history survive cold restart;
   db has no plaintext). `hub log` also no longer fails when the owner is
   offline — it returns stored history.
2. ~~**No discover + request-to-join over nostr**~~ **DONE** — `hub announce`
   publishes a discoverable hub note; `hub discover` lists hubs from relays;
   `hub request --to <owner>` sends the `filestrreq1…` ticket as a NIP-44
   encrypted DM over a relay; the owner's daemon listens, decrypts, and either
   auto-admits (`[chat].auto_admit`) or queues for `hub pending` / `hub admit`.
   Verified by `test-hub-discover.sh` (announce → discover → request →
   auto-admit, **no prior grant**). Hardening left: full NIP-17 gift-wrap for
   sender anonymity, and gating auto-admit on the reputation/vouch policy.
3. **Hub availability** — owner-offline kills join + sync; no multi-admin or
   member relay federation. *(Now the top open blocker.)*
4. **Onboarding friction** — no packaged install (no AUR/binary release), no
   `share add`, no GUI/mobile, manual ticket paste.
5. **Live updates + richer search/metadata** — for exploration to feel alive.

## What's genuinely good already

Portable `nsec` identity; one-ticket chat + files + share-to-join; trust-graph
reshare search; verified/streaming/ranged/background transfers; configurable
iroh **and** nostr relays (zero-infra *or* federated); anti-free-riding
reputation; SSH-grade key handling; XDG layout. The **plumbing is strong**;
what's thin is the **public-facing onboarding and the always-on / GUI layer**.

## Recommended order

For the "see → join → share → explore" flow specifically:

1. ~~**MLS persistence**~~ — **done**.
2. ~~**nostr discover + request-to-join**~~ — **done** (announce / discover /
   request-DM / auto-admit; built on the `filestrreq1` request ticket).
3. **`share add` + packaged install** — remove the config-editing friction.
   *Next up.*

Items 1 and 2 are done: you can now find a hub on a relay and join it with a
request, no hand-passed ticket. What's left for the full "tap and you're in"
is reducing onboarding friction (3) and hub availability when the owner is
offline.
