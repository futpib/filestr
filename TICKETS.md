# filestr — tickets & addresses

filestr uses four short self-contained strings. Three are **single-use,
symmetric tickets** (one redemption pairs two nodes with mutual access); the
fourth is a reusable public **address**, not a ticket. None of them is ever
published by the daemon — you hand them out yourself (paste, QR, DM); the
join-request can also be sent for you, gift-wrapped, over nostr.

| string | direction | single-use | symmetric | needs nostr | what it does |
|---|---|---|---|---|---|
| `filestr1…` | grantor → grantee | yes | yes | no | pairwise file peering |
| `filestrhub1…` | owner → member | yes¹ | yes | for the chat half | invite someone into a hub you own |
| `filestrreq1…` | member → owner | yes¹ | yes | for the chat half | ask to join a hub |
| `filestraddr1…` | public pointer | **no** | n/a | for the chat half | where/whom to send a join request |

¹ single-use via the file invite embedded in it.

---

## `filestr1…` — file invite

The base primitive. Pure iroh; **no nostr at all** (works in an iroh-only
build or with `[chat] enabled = false`).

- **Contents:** the grantor's dialable address (relay URLs + direct addrs), a
  random single-use token, the share view to expose, optional label.
- **Redeeming** (`filestrctl peer add <ticket>`): the redeemer dials the
  grantor, presents the token (burned on use), and sends its own address.
  Both sides end up allowing each other and recording each other as peers —
  **symmetric**: A can browse B and B can browse A from the one ticket.
- **Made with:** `filestrctl invite create [--view V] [--label L]
  [--no-reshare] [--relay-only]`.
- **Single-use:** the token is burned on first redemption; a different node
  reusing it is refused (the same node may re-redeem to recover a lost reply).

## `filestrhub1…` — hub ticket (owner invites you)

Wraps a `filestr1…` invite plus the hub's name and group ref. The owner mints
it for someone they want in their hub.

- **Contents:** an embedded `filestr1…` invite (owner→joiner), the hub name,
  and the MLS group ref.
- **Redeeming** (`filestrctl hub join <ticket>`): redeems the embedded invite
  (symmetric file peering — works even with chat off), then does the MLS join
  to enter the group chat.
  - **With chat off:** the file-peering half happens now and the MLS join is
    **queued** (persisted); it completes automatically when you enable
    `[chat]` and restart.
- **Made with:** `filestrctl hub invite <hub>` (owner only).
- **Single-use:** via its embedded invite token.

## `filestrreq1…` — join request (you ask to join)

The member-initiated counterpart: a self-contained request the owner can
admit unprompted. Works pasted out-of-band **or** sent over nostr.

- **Contents:** a symmetric `filestr1…` invite (the owner redeems it to reach
  you back and gain mutual access), your MLS key package, an optional target
  hub, optional label.
- **Producing** (`filestrctl hub request [<address>] [--hub R] [--label L]`):
  prints the ticket. If given a hub **address**, it is also gift-wrapped
  (NIP-17) and sent to that owner over the address's relays.
- **Admitting** (`filestrctl hub admit <ticket> [--hub R]`, owner side):
  redeems your invite (mutual access), adds your key package to the MLS group,
  and pushes the welcome back over iroh. Or auto-admitted when the owner sets
  `[chat] auto_admit = true`; otherwise it sits in `hub pending`.
- **Single-use:** via its embedded invite token.

## `filestraddr1…` — hub address (not a ticket)

A small, **reusable public pointer** to a hub. It carries no token and no
grant, is not single-use, and the daemon never publishes it — the owner shares
it however they like (paste, profile bio, QR).

- **Contents:** the hub name, group ref, owner nostr pubkey, and relays.
- **Made with:** `filestrctl hub address <hub>` (owner only).
- **Used by:** `filestrctl hub request <address>` to know whom to gift-wrap the
  join request to, and on which relays.

---

## Privacy notes

- Every nostr **message** the daemon emits is Whitenoise: MLS group messages
  (kind 445) or NIP-17 gift wraps (kind 1059). There are no public notes; the
  hub *address* is a pointer you share, not something we post.
- A `filestr1…` invite (and the embedded invite inside hub/request tickets)
  reveals the grantor's dialable address only to whoever holds the ticket —
  this is how access stays invitation-gated (see DESIGN.md §2–§3).
