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

The iroh-only core (PLAN.md milestones M0–M4) is implemented and covered by
e2e tests: grants/invites with single-use tickets, view-scoped browse,
recursive streaming search with attribution-free resharing, and verified
relayed fetch. The optional nostr chat plane (M5) is designed but not built;
the wire protocol reserves the `nostr` tunnel stream for it.

See [DESIGN.md](DESIGN.md) for the protocol and [PLAN.md](PLAN.md) for the
roadmap.

## Quickstart

```sh
cargo build --workspace          # binaries: filestrd, filestrctl

filestrd -v                      # foreground daemon (or use filestrd.service)

# share something: ~/.config/filestr/config.toml
#   [[share]]
#   name = "music"
#   path = "/home/me/music"
filestrctl rescan

filestrctl invite create --label alice    # prints a filestr1… ticket; send it
                                          # to Alice over any channel you trust

# on Alice's node:
filestrctl peer add filestr1… --label bob
filestrctl browse bob                     # bob's file list (her view of it)
filestrctl search led zeppelin            # searches the whole grant graph
filestrctl get <hash> -o song.flac        # streaming, BLAKE3-verified download
filestrctl get <hash> --range 0-1048575   # just the first 1 MiB
filestrctl get <hash> -b -o big.iso       # background; returns a transfer id
filestrctl transfers                      # watch active/queued/done downloads

filestrctl status                         # also: peer ls, invite ls, listen
```

Daemon and CLI follow the slopd/slopctl shape: unix socket at
`$XDG_RUNTIME_DIR/filestrd/filestrd.sock`, JSON-lines protocol, TOML config,
state under `~/.local/share/filestr/`.

## Tests

```sh
scripts/autotests/run-all.sh
```

Hermetic multi-daemon e2e on localhost (relay disabled): two-node grant
lifecycle, three-node reshare chain asserting zero origin attribution in
results, relayed verified fetch, search-cycle termination, the
`allow_reshare=false` contract, and streaming/ranges/background — including an
assertion that a relay does **not** cache the bytes it forwards, a clipped
byte-range fetch, and several concurrent background downloads.

## Lineage

DC++ (hubs, share-to-join, browsable file lists, TTH→BLAKE3) × RetroShare (friend-to-friend trust graph, recursive search/transfer) × nostr (identity, async E2EE community chat) × iroh (NAT-proof authenticated p2p QUIC).
