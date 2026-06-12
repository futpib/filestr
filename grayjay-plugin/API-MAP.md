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
| `getSubscriptionsUser()` | your peers — one channel URL each |

## Backable but not yet implemented

| Method | What it'd do | filestr backing |
|---|---|---|
| `getSearchCapabilities().sorts` + `search` `order` | sort by name/size/duration/date | index has size/duration/mtime (needs dates surfaced) |
| `getPlaylistsUser()` / `isPlaylistUrl` / `getPlaylist` | folders/albums as playlists | shared folder structure (currently flattened by `baseName`) |
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
source; `getSubscriptionsUser` returns one channel URL per peer (excluding
`local`). Author links on each item point at the channel, so tapping the author
opens that peer's library. This uses the daemon's existing browse aggregation —
no new gateway endpoint.
