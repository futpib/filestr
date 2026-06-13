# filestr

Friend-to-friend file sharing with DC++'s social model, where **every node is a hub**.

- **Chat plane**: nostr — Whitenoise/Marmot (MLS) groups. A "hub" is just a group a node owns. Chat is E2EE, async (store-and-forward via relays), and involves no direct connections between members.
- **Data plane**: [iroh](https://github.com/n0-computer/iroh) (QUIC, dial-by-NodeId, hole-punching + relay fallback) and BLAKE3 verified streaming (iroh-blobs) for transfers.
- **Access control**: a pairwise **grant graph**. Nobody can browse or fetch your files — or even discover your node address — without an explicit invitation. Joining someone's hub grants exactly one node (the hub owner) access to your share: the modern descendant of DC++'s minimum-share rule.
- **Reshare** (default on): nodes may serve content their inviters expose as if it were their own. Search recurses breadth-first through the grant graph with streaming results; transfers stream hop-by-hop via opaque handles (relays splice bytes through without caching), BLAKE3-verified end to end. The wire format never attributes content to its origin.
- **Transfers**: streaming, with byte-range requests and a background transfer manager — kick off many downloads at once and watch them with `filestrctl transfers`.

## What this is not

Not an anonymity network. The goal of attribution-hiding is to avoid doxxing who-has-what across the trust graph — not to hide network metadata. iroh relays and nostr relays see IPs as usual; your direct grant peers see your traffic. See [DESIGN.md → Threat model](DESIGN.md#threat-model).

## Status

Both planes are implemented and covered by e2e tests.

- **Data plane (iroh-only, M0–M4):** grants/invites with single-use tickets,
  view-scoped browse, recursive streaming search with attribution-free
  resharing, and streaming relayed fetch with byte ranges and a background
  transfer manager.
- **Chat plane (optional, M5):** real **Marmot/MLS** hubs via
  [`mdk-core`](https://crates.io/crates/mdk-core) + OpenMLS (the White Noise
  stack — forward secrecy, post-compromise security). The hub owner hosts an
  embedded NIP-01 relay served over the iroh `nostr` stream, so hubs work with
  **zero external relays**. Joining a hub auto-grants the owner file access
  (share-to-join). Built behind the `chat` cargo feature (default on);
  `--no-default-features` gives a pure iroh-only build.

There is also an **Android app** (files only) under [`app/`](app/): a
Flutter/fvm front-end that bundles the iroh-only daemon and drives it over the
same control protocol. See [app/README.md](app/README.md).

The daemon can expose a read-only **loopback HTTP gateway**
(`[http] listen = "127.0.0.1:11780"`) so other apps on the device can list and
stream what the node serves. A [Grayjay](https://grayjay.app/) source plugin
built on it lives under [`grayjay-plugin/`](grayjay-plugin/).

See [DESIGN.md](DESIGN.md) for the protocol, [TICKETS.md](TICKETS.md) for the
ticket/address reference, and [PLAN.md](PLAN.md) / [ROADMAP.md](ROADMAP.md)
for the roadmap.

## Quickstart

```sh
cargo build --workspace          # binaries: filestrd, filestrctl

filestrd -v                      # foreground daemon (or use filestrd.service)

# share a directory (persisted to ~/.config/filestr/config.toml):
filestrctl share add ~/music                  # returns at once; hashes in the
                                              # background (parallel, low prio)
filestrctl status                             # shows "indexing: 34/512" while it runs
filestrctl rescan --cancel                    # stop an in-flight scan
filestrctl share ls                           # list roots + views
filestrctl share rm music                      # stop sharing (cancels its scan)
# files are referenced in place (not copied); the index is cached, so a restart
# doesn't re-hash unchanged files. Or hand-edit [[share]] blocks + `filestrctl rescan`.

filestrctl invite create --label alice    # prints a filestr1… ticket; send it
                                          # to Alice over any channel you trust

# join a hub over nostr (no hand-passed ticket, no public notes):
filestrctl hub address general            # owner: small shareable hub address
filestrctl hub request filestraddr1…      # newcomer: gift-wrapped join request
filestrctl hub pending                    # owner: review requests (if not auto)
#   set [chat] auto_admit = true for open hubs

# on Alice's node (file peering, no chat):
filestrctl peer add filestr1… --label bob
filestrctl browse bob                     # bob's file list (her view of it)
filestrctl search led zeppelin            # searches the whole grant graph
filestrctl get <hash> -o song.flac        # streaming, BLAKE3-verified download
filestrctl get <hash> --range 0-1048575   # just the first 1 MiB
filestrctl get <hash> -b -o big.iso       # background; returns a transfer id
filestrctl transfers                      # watch active/queued/done downloads

filestrctl status                         # also: peer ls, invite ls, listen
filestrctl rep                            # per-peer reciprocity ledger + verdict

# chat: nostr/MLS hubs (needs a chat-enabled daemon, the default build)
filestrctl hub create general             # owner: create a hub you own
filestrctl hub invite general             # owner: prints a filestrhub1… ticket
filestrctl hub join filestrhub1…          # member: join (auto-shares with owner)
filestrctl hub send general "hi everyone" # E2EE group message
filestrctl hub log general                # decrypted chat log
filestrctl hub members general
```

Daemon and CLI follow the slopd/slopctl shape: unix socket at
`$XDG_RUNTIME_DIR/filestrd/filestrd.sock`, JSON-lines protocol, TOML config,
state under `~/.local/share/filestr/`.

The chat plane is optional at runtime too: `[chat] enabled = false` runs a pure
file-peering node with no nostr (flip it on and restart to join hubs later).
Plain `filestr1…` invites work either way; hub tickets need chat on for the
MLS join.

Relays are configurable on both planes. iroh connectivity: `relay = "default"`
/ `"disabled"` / `relay_urls = ["https://my.relay./"]`. nostr chat: `[chat]`
with `embedded_relay`, `relay_listen = "127.0.0.1:7777"` (expose this node as a
standard WebSocket nostr relay), and `relays = ["wss://relay…"]` (use external
relays). Default is zero external infrastructure — hubs ride the iroh tunnel.

## Tests

```sh
scripts/autotests/run-all.sh
```

Hermetic multi-daemon e2e on localhost (relay disabled): two-node grant
lifecycle, three-node reshare chain asserting zero origin attribution in
results, relayed verified fetch, search-cycle termination, the
`allow_reshare=false` contract, and streaming/ranges/background — including an
assertion that a relay does **not** cache the bytes it forwards, a clipped
byte-range fetch, and several concurrent background downloads. The chat suite
runs a real Marmot/MLS hub over the iroh nostr tunnel (no external relay):
bidirectional E2EE messages and the share-to-join file grant.

> **Tech debt:** the e2e suite is sprawling, unreadable bash — ad-hoc
> `curl | jq` assertions, `sleep`-based synchronization, copy-pasted setup, and
> `die "msg"` for failures. It works but is painful to read, extend, and debug.
> It should be rewritten in a real test framework with a readable language
> (e.g. Rust integration tests, or a typed harness) with proper fixtures,
> structured assertions, and no fixed sleeps.

## Lineage

DC++ (hubs, share-to-join, browsable file lists, TTH→BLAKE3) × RetroShare (friend-to-friend trust graph, recursive search/transfer) × nostr (identity, async E2EE community chat) × iroh (NAT-proof authenticated p2p QUIC).
