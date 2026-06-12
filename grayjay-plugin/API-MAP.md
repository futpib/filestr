# Grayjay plugin API → filestr

How Grayjay's plugin interface (`source.*`) maps onto what filestr can back.
Grayjay calls a method only if the plugin defines it; otherwise the scaffolding's
empty stub is used.

## Implemented & backed

| Method | filestr backing |
|---|---|
| `enable(config, settings, state)` | sets the gateway URL / plugin id |
| `getHome()` | `/files` — everything servable (own shares + a one-hop browse of peers), media only |
| `getSearchCapabilities()` | `Type.Feed.Mixed` + an audio/video filter |
| `search(query, type, order, filters)` | federated `/search` (grant graph; matches tags **and** path) |
| `searchSuggestions(query)` | titles/filenames/tags from the local index |
| `isContentDetailsUrl` / `getContentDetails` | a file → details + `/file/{hash}` stream (duration, thumbnail, artist/album) |
| `isChannelUrl` / `getChannel` / `getChannelContents` / `getChannelCapabilities` | **a channel = a peer** ("local" = you); contents = that source's files from `/files` |
| `getUserSubscriptions()` | your peers — one channel URL each |
| `getChannelPlaylists(url)` | the channel's **Playlists tab**: that source's folders + album tags + artist tags as playlist stubs (login-free; the primary way to browse) |
| `isPlaylistUrl` / `getPlaylist` / `getUserPlaylists` | **a playlist = a folder / album tag / artist tag**; `getPlaylist` resolves a `/playlist/<kind>…` URL to its tracks, `getUserPlaylists` lists the whole library's groupings (used only by Grayjay's logged-in "Import playlists" flow) |

## Backable but not yet implemented

| Method | What it'd do | filestr backing |
|---|---|---|
| `getSearchCapabilities().sorts` + `search` `order` | sort by name/size/duration/date | index has size/duration/mtime (needs dates surfaced) |
| `searchChannelContents(channelUrl, …)` | search within one peer | scoped browse + filter |
| per-peer thumbnails in channel/search results | friends' cover art | needs a thumb-fetch over the p2p channel |

## No filestr concept — correctly left unimplemented (N/A)

| Method | Why |
|---|---|
| `getComments` / `getSubComments` | a file-sharing graph has no comments |
| `getShorts()` | no "shorts" notion |
| live chat / live events | no live streaming |
| `getContentRecommendations` | no recommendation engine (F2F, not a platform) |
| `getPlaybackTracker` | no view/watch-tracking backend |
| Polycentric claim mapping (`primaryClaimFieldType`, claim types) | filestr isn't a Polycentric identity platform |
| `disable()` / client-side saved state | nothing to tear down or persist |

## Channel model

Channels are derived from the `source` field on each `/files` entry:

- `"local"` → your own node's channel.
- a peer label / short node id → that friend's channel.

A channel URL is `…/channel/<source>` (interpreted only by the plugin; Grayjay
treats it as opaque). `getChannelContents` lists `/files` filtered to that
source; `getUserSubscriptions` returns one channel URL per peer (excluding
`local`). Author links on each item point at the channel, so tapping the author
opens that peer's library. This uses the daemon's existing browse aggregation —
no new gateway endpoint.

## Playlist model

A playlist is a grouping of files, tagged with its `kind`. A playlist URL is
`…/playlist/<key>` where the key is one of (`\t`-separated, opaque to Grayjay):

- `folder\t<source>\t<folder>` — a shared folder of one source (the folder is
  everything before the last `/` of a file's visible `<root>/<rel>` path);
- `album\t<name>\t<source?>` — files whose `album` tag matches;
- `artist\t<name>\t<source?>` — files whose `artist` tag matches.

The trailing `<source>` scopes album/artist to one peer (empty = the whole
reachable library), so the same album name from two peers stays distinct and an
offline peer's playlist resolves to nothing rather than another peer's tracks.
`getPlaylist` filters `/files` accordingly. Two surfaces produce these:

- **`getChannelPlaylists(channelUrl)`** — a peer's (or `local`'s) folders +
  albums + artists, all scoped to that source. This drives the **Playlists tab**
  on the channel page, which Grayjay shows for any plugin that defines the method
  (no login). It returns lightweight stubs (name + count + cover); Grayjay calls
  `getPlaylist` lazily when one is opened.
- **`getUserPlaylists()`** — the whole library's folders/albums/artists, for
  Grayjay's "Import playlists" migration (only offered for logged-in sources, so
  not reachable for filestr today, but kept correct).

Same `/files` data, no new endpoint. So a shared `music/` folder appears as a
folder playlist, a tagged collection as one album/artist playlist per tag.
