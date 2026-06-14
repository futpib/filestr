// Grayjay source plugin for filestr.
//
// Talks to the filestr app's local HTTP gateway (see filestrd's [http] bridge)
// over loopback and presents every file the node can serve — its own shares
// plus everything reachable through its grant graph — as Grayjay content with
// a directly playable stream URL.
//
// Authored in TypeScript and compiled to FilestrScript.js (see tsconfig.json).
// The Source interface is type-checked against @types/grayjay-source, so a
// misnamed method (e.g. getPlaylistsUser vs getUserPlaylists) fails the build
// instead of becoming a silent runtime no-op that Grayjay never calls.

// Shape of a file entry as the gateway serves it (/files, /search). Typing it
// here documents the contract and catches typos like f.media.albun.
interface FileMedia {
	title?: string;
	artist?: string;
	album?: string;
	duration_secs?: number;
	content_type?: string;
}
interface FileEntry {
	hash: string;
	name: string;
	size: number;
	/// File mtime in seconds since the epoch; absent/0 when unknown.
	mtime?: number;
	source: string;
	thumb?: boolean;
	media?: FileMedia;
}
interface PeerStatus {
	label: string;
	node_id?: string;
	reachable?: boolean;
}
interface IndexResponse {
	files?: FileEntry[];
	peers?: PeerStatus[];
}

// A playlist grouping summarised by the gateway's /playlists endpoint: enough to
// render a stub without pulling the whole file list. `key` is the opaque value
// that goes in the playlist URL (folder path, or album/artist name); `name` is
// the display label; `cover` (when present) is a hash served at /thumb/<cover>.
interface PlaylistGroup {
	name: string;
	key: string;
	count: number;
	cover?: string;
}
interface PlaylistsResponse {
	folders?: PlaylistGroup[];
	albums?: PlaylistGroup[];
	artists?: PlaylistGroup[];
	peers?: PeerStatus[];
}


const PLATFORM = "filestr";
let PLUGIN_ID = "filestr";
let BASE_URL = "http://127.0.0.1:11780";

source.enable = function (conf, settings, savedState) {
	if (conf && conf.id) PLUGIN_ID = conf.id;
	const s = settings as { serverUrl?: string } | null;
	if (s && s.serverUrl) {
		BASE_URL = String(s.serverUrl).replace(/\/+$/, "");
	}
};

source.getHome = function (continuationToken) {
	return new FilestrVideoPager(toVideos(fetchFiles(), null));
};

source.searchSuggestions = function (query) {
	return [];
};

// Filter group id and the media classes it offers. Grayjay passes the user's
// selection back to search() as filters[MIME_FILTER] = [value, …].
const MIME_FILTER = "mime";

source.getSearchCapabilities = function () {
	return new ResultCapabilities(
		[Type.Feed.Mixed],
		[],
		[
			new FilterGroup(
				"Type",
				[
					new FilterCapability("Audio", "audio", "audio"),
					new FilterCapability("Video", "video", "video"),
				],
				true, // multi-select
				MIME_FILTER
			),
		]
	);
};

source.search = function (query, type, order, filters) {
	return new FilestrVideoPager(toVideos(fetchSearch(query), mimeClassesFromFilters(filters)));
};

// Extract the selected media classes (e.g. ["audio"]) from Grayjay's filters
// map. Empty/absent means no restriction.
function mimeClassesFromFilters(filters: Readonly<Record<string, string[]>> | null): string[] | null {
	if (!filters) return null;
	const sel = filters[MIME_FILTER];
	if (!sel || !sel.length) return null;
	return sel.map(String);
}

source.isContentDetailsUrl = function (url) {
	return typeof url === "string" && url.indexOf("/file/") !== -1;
};

// --- channels: each peer (and "local" = this node) is a channel ------------

source.getChannelCapabilities = function () {
	return new ResultCapabilities([Type.Feed.Mixed], [], []);
};

source.isChannelUrl = function (url) {
	return typeof url === "string" && url.indexOf("/channel/") !== -1;
};

source.getChannel = function (url) {
	const src = parseChannelUrl(url);
	const name = src === "local" ? "This node" : src;
	let description = src === "local" ? "Files this node shares" : `Files reachable via ${src}`;
	// flag an offline peer right in the channel header
	if (src !== "local") {
		const peer = fetchPeers().find((p) => p.label === src);
		if (peer && peer.reachable === false) {
			description = `⚠ Offline — ${src} isn't reachable right now`;
		}
	}
	return new PlatformChannel({
		id: new PlatformID(PLATFORM, `peer:${src}`, PLUGIN_ID),
		name: name,
		thumbnail: "",
		description: description,
		url: channelUrl(src),
	});
};

source.getChannelContents = function (url, type, order, filters) {
	const src = parseChannelUrl(url);
	const index = fetchIndex("/files");
	// If this is a peer we couldn't reach, say so explicitly instead of showing
	// an empty list (which reads as "this peer has no files").
	if (src !== "local") {
		const peer = (index.peers || []).find((p) => p.label === src);
		if (peer && peer.reachable === false) {
			throw new ScriptException(
				`${src} is offline — this peer isn't reachable right now, so its files can't be loaded. It'll be available again once it's back online.`
			);
		}
	}
	const files = (index.files || []).filter((f) => f.source === src);
	return new FilestrVideoPager(toVideos(files, mimeClassesFromFilters(filters)));
};

// Your peers are your subscriptions: one channel per granted peer. Include peers
// that are offline this browse too (they have no files right now) so they stay
// visible and opening one explains that it's unreachable, rather than vanishing.
source.getUserSubscriptions = function () {
	const index = fetchIndex("/files");
	const sources: Record<string, boolean> = {};
	for (const f of index.files || []) {
		if (f.source && f.source !== "local") sources[f.source] = true;
	}
	for (const p of index.peers || []) {
		if (p.label && p.label !== "local") sources[p.label] = true;
	}
	return Object.keys(sources).map(channelUrl);
};

// Creator (channel) search: filestr's "creators" are your peers (plus this node),
// so this populates the search screen's Creators tab. Matches on the channel
// label, and uses the lightweight /peers grant list (no file browse) so it's
// instant.
source.searchChannels = function (query) {
	const q = (query || "").toLowerCase();
	const out: PlatformChannel[] = [];
	const seen: Record<string, boolean> = {};
	const add = (src: string, label: string) => {
		if (seen[src]) return;
		if (q && label.toLowerCase().indexOf(q) === -1) return;
		seen[src] = true;
		out.push(channelStub(src));
	};
	add("local", "This node");
	for (const p of fetchGrantedPeers()) {
		if (p.label && p.label !== "local") add(p.label, p.label);
	}
	return new FilestrChannelPager(out);
};

// --- playlists: shared folders, plus tag-based albums and artists ----------
//
// A library browses by album and artist, not just by directory. The daemon
// already extracts album/artist tags (they ride on /files), so we expose three
// flavours of playlist: a source's folders, and one playlist per album tag and
// per artist tag.
//
// These surface in two places:
//   - getChannelPlaylists(channelUrl): the Playlists tab on a peer's (or this
//     node's) channel page — login-free, the primary way to browse. Scoped to
//     that one source.
//   - getUserPlaylists(): the "Import playlists" migration flow (only offered by
//     Grayjay for logged-in sources). Spans the whole reachable library.

source.isPlaylistUrl = function (url) {
	return typeof url === "string" && url.indexOf("/playlist/") !== -1;
};

source.getUserPlaylists = function () {
	const out: string[] = [];
	// this node's own folders (peer folders are reached via the peer channel)
	for (const g of fetchPlaylists("local").folders || []) {
		out.push(playlistUrl("folder", g.key, "local"));
	}
	// albums and artists span the whole reachable library (no source scope)
	const all = fetchPlaylists("");
	for (const g of all.albums || []) out.push(playlistUrl("album", g.key, ""));
	for (const g of all.artists || []) out.push(playlistUrl("artist", g.key, ""));
	return out;
};

// Playlist search: match album/artist names across the whole reachable library,
// so filestr collections appear in the search screen's Playlists tab. (Folder
// playlists are per-source and aren't globally resolvable, so search covers the
// tag-based groupings.)
source.searchPlaylists = function (query) {
	const q = (query || "").toLowerCase();
	if (!q) return new FilestrPlaylistPager([]);
	const res = fetchPlaylists("");
	const out: PlatformPlaylist[] = [];
	for (const g of res.albums || []) {
		if (g.name.toLowerCase().indexOf(q) !== -1) out.push(groupStub("album", g, ""));
	}
	for (const g of res.artists || []) {
		if (g.name.toLowerCase().indexOf(q) !== -1) out.push(groupStub("artist", g, ""));
	}
	return new FilestrPlaylistPager(out);
};

// Playlists for one channel (a peer, or "local"): that source's folders, albums
// and artists, scoped so opening one shows only that source's files and so the
// same album name from two peers stays distinct. This is what populates the
// channel page's Playlists tab. The daemon does the grouping (GET /playlists),
// so this stays O(groups) instead of pulling and grouping the whole library.
source.getChannelPlaylists = function (url) {
	const src = parseChannelUrl(url);
	const res = fetchPlaylists(src);
	if (src !== "local") {
		const peer = (res.peers || []).find((p) => p.label === src);
		if (peer && peer.reachable === false) {
			throw new ScriptException(
				`${src} is offline — this peer isn't reachable right now, so its playlists can't be loaded. It'll be available again once it's back online.`
			);
		}
	}
	const out: PlatformPlaylist[] = [];
	for (const g of res.folders || []) out.push(groupStub("folder", g, src));
	for (const g of res.albums || []) out.push(groupStub("album", g, src));
	for (const g of res.artists || []) out.push(groupStub("artist", g, src));
	return new FilestrPlaylistPager(out);
};

source.getPlaylist = function (url) {
	const p = parsePlaylistUrl(url);
	// The daemon resolves the grouping to its tracks (GET /playlist), so this
	// stays a single small request instead of pulling and filtering all of /files.
	const files = fetchPlaylistFiles(p);
	let name: string;
	if (p.kind === "album") name = p.name || "Album";
	else if (p.kind === "artist") name = p.name || "Artist";
	else name = folderName(p.folder);
	const cover = files.find((f) => f.thumb);
	const author = authorOf(p.source || (files.length ? files[0].source : "local"));
	return new PlatformPlaylistDetails({
		id: new PlatformID(PLATFORM, `playlist:${p.kind}:${p.source}:${name}`, PLUGIN_ID),
		name: name,
		thumbnails: cover ? thumbsFor(cover) : new Thumbnails([]),
		thumbnail: cover ? `${BASE_URL}/thumb/${cover.hash}` : "",
		author: author,
		datetime: nowSeconds(),
		url: url,
		videoCount: files.length,
		contents: new FilestrVideoPager(files.map(fileToVideo)),
	});
};

// "Recommended" tab on a content page: filestr has no recommendation engine
// (it's F2F, not a platform), but the natural related content for a track is the
// rest of the collections it belongs to — more from the same album, folder and
// artist. The daemon resolves those siblings (GET /related?hash=) within the
// file's own source. Grayjay renders the Recommended tab as a content feed (it
// shows content, not playlist cards), so we return the sibling files as videos;
// to browse the album/artist/folder as playlists, the channel's Playlists tab
// (getChannelPlaylists) is the surface.
//
// Grayjay drives the Recommended tab from getContentRecommendations on the
// content-details object (IPlatformVideoDetailsDef), so we attach it there (see
// getContentDetails below). The Source-level method is kept too, for any surface
// that uses it.
source.getContentRecommendations = function (url) {
	const { hash } = parseFileUrl(url);
	return recommendationsFor(hash);
};

// Related content for a file (by hash): the sibling tracks from the same
// album/folder/artist, as a ContentPager of videos. Empty when the hash is
// unknown or the gateway is unreachable.
function recommendationsFor(hash: string): FilestrContentPager {
	if (!hash) return new FilestrContentPager([]);
	let files: FileEntry[];
	try {
		files = fetchRelated(hash);
	} catch (e) {
		return new FilestrContentPager([]);
	}
	return new FilestrContentPager(files.filter(isPlayable).map(fileToVideo));
}

source.getContentDetails = function (url) {
	const { hash, name, embedded } = parseFileUrl(url);
	// Prefer the metadata embedded in the URL (the exact FileEntry the card was
	// built from — no network, always present when opened from our own feed). Only
	// fall back to a library lookup for a bare URL (e.g. one persisted before this
	// existed). This is what makes the duration correct every time, cache-free.
	const item = embedded ?? findFile(hash, name);
	const display = item ? displayName(item) : name || hash;
	const sourceLabel = item ? item.source : "filestr";
	const dur = item ? durationOf(item) : 0;
	const fileName = item ? item.name : display;
	const container = item ? contentType(item) : containerOf(fileName);
	const pageUrl = item ? contentPageUrl(item) : contentUrl(hash, fileName);

	return new PlatformVideoDetails({
		id: new PlatformID(PLATFORM, hash, PLUGIN_ID),
		name: display,
		thumbnails: item ? thumbsFor(item) : new Thumbnails([]),
		author: authorOf(sourceLabel),
		datetime: item ? dateOf(item) : nowSeconds(),
		duration: dur,
		viewCount: -1,
		url: pageUrl,
		shareUrl: pageUrl,
		isLive: false,
		description: item ? describe(item) : "",
		rating: new RatingLikes(0),
		// the player streams from the plain URL (no metadata blob needed there)
		video: sourceDescriptor(contentUrl(hash, fileName), container, dur),
		// the Recommended tab: the playlists this file belongs to
		getContentRecommendations: () => recommendationsFor(hash),
	});
};

// Build the playback source descriptor. An audio file must be served as an
// AudioUrlSource (in an unmuxed descriptor with no video track) — handing
// Grayjay an audio file dressed as a VideoUrlSource makes the player ignore our
// explicit duration and guess it from the MP3 frames, which mis-reads VBR files
// (the "-12:-55" duration bug). We always pass the index's exact duration so the
// seekbar is right regardless of the container's internal headers.
function sourceDescriptor(url: string, container: string, duration: number): IVideoSourceDescriptor {
	if ((container || "").split("/")[0] === "audio") {
		return new UnMuxVideoSourceDescriptor(
			[],
			[
				new AudioUrlSource({
					name: "filestr",
					url,
					container,
					duration,
					bitrate: 0,
					codec: "",
					language: Language.UNKNOWN,
				}),
			]
		);
	}
	return new VideoSourceDescriptor([
		new VideoUrlSource({
			name: "filestr",
			url,
			container,
			duration,
			width: 0,
			height: 0,
			bitrate: 0,
			codec: "",
		}),
	]);
}

// --- helpers ---------------------------------------------------------------

// Fetch and parse a gateway endpoint. A failed request almost always means the
// filestr app isn't running on this device, so say that plainly instead of
// surfacing a raw HTTP status.
function gatewayJson(pathAndQuery: string): any {
	const res = http.GET(`${BASE_URL}${pathAndQuery}`, {}, false);
	if (!res.isOk) {
		throw new ScriptException(
			"filestr isn't reachable — open the filestr app on this device and make sure it's running, then try again."
		);
	}
	return JSON.parse(res.body) || {};
}

// A listing endpoint (shape: {files, peers}).
function fetchIndex(pathAndQuery: string): IndexResponse {
	return gatewayJson(pathAndQuery) as IndexResponse;
}

// Server-side playlist groupings for one channel ("local" or a peer label),
// computed by the daemon so we don't pull and group the whole file list.
function fetchPlaylists(source: string): PlaylistsResponse {
	return gatewayJson(`/playlists?source=${encodeURIComponent(source)}`) as PlaylistsResponse;
}

// The tracks of one grouping, resolved by the daemon (GET /playlist) so opening
// a playlist is a single small request rather than a full /files pull + filter.
function fetchPlaylistFiles(p: ParsedPlaylist): FileEntry[] {
	const key = p.kind === "folder" ? p.folder : p.name;
	const q =
		`/playlist?kind=${encodeURIComponent(p.kind)}` +
		`&key=${encodeURIComponent(key)}&source=${encodeURIComponent(p.source || "")}`;
	return (gatewayJson(q) as IndexResponse).files || [];
}

// Sibling files from the playlists one file belongs to (same album/folder/
// artist), resolved by the daemon (GET /related) so the Recommended tab is one
// small request.
function fetchRelated(hash: string): FileEntry[] {
	return (gatewayJson(`/related?hash=${encodeURIComponent(hash)}`) as IndexResponse).files || [];
}

// Everything this node can serve (its shares + a one-hop browse of peers).
function fetchFiles(): FileEntry[] {
	return fetchIndex("/files").files || [];
}

// Resolve a single file (with its media tags) by hash for getContentDetails.
// /files only lists local + directly-browsed peers, so a multi-hop file reached
// via federated search isn't there — fall back to the search index, which now
// carries full media for every hit (local or multi-hop). Without this fallback
// such a file gets duration 0 (the "-12:-55" seekbar bug) even though the search
// result that opened it had the right duration.
function findFile(hash: string, name: string): FileEntry | null {
	try {
		const local = fetchFiles().find((f) => f.hash === hash);
		if (local) return local;
	} catch (e) {
		// gateway may be momentarily unavailable; try search, then the URL
	}
	if (name) {
		try {
			const hit = fetchSearch(name).find((f) => f.hash === hash);
			if (hit) return hit;
		} catch (e) {
			// fall through to building from the URL
		}
	}
	return null;
}

// The granted peers and whether each answered the latest browse, so we can tell
// an offline peer apart from one that simply has nothing to share.
function fetchPeers(): PeerStatus[] {
	return fetchIndex("/files").peers || [];
}

// The granted peers straight from the grant graph (no file browse) — for creator
// search, which only needs the channel list, not files or live reachability.
function fetchGrantedPeers(): PeerStatus[] {
	return (gatewayJson("/peers") as IndexResponse).peers || [];
}

// The daemon's federated grant-graph search: reaches the whole reachable graph,
// not just direct peers, and matches the tag metadata (title/artist/album), not
// just the filename. The gateway records sources so the results are playable.
function fetchSearch(query: string): FileEntry[] {
	if (!query) return [];
	return fetchIndex(`/search?q=${encodeURIComponent(query)}`).files || [];
}

// Shared pipeline: keep only files Grayjay can play (audio/video — anything that
// maps to the generic octet-stream container is hidden), optionally restrict to
// the selected media classes, then map to Grayjay videos.
function toVideos(files: FileEntry[], mimeClasses: string[] | null): PlatformVideo[] {
	let out = files.filter(isPlayable);
	if (mimeClasses && mimeClasses.length) {
		out = out.filter((f) => mimeClasses.indexOf(mimeClassOf(f)) !== -1);
	}
	return out.map(fileToVideo);
}

// The content type for a file: the type sniffed at index time (correct even for
// a misnamed file), else inferred from the filename extension.
function contentType(f: FileEntry): string {
	const ct = f.media && f.media.content_type;
	return ct || containerOf(f.name);
}

function fileToVideo(f: FileEntry): PlatformVideo {
	return new PlatformVideo({
		id: new PlatformID(PLATFORM, f.hash, PLUGIN_ID),
		name: displayName(f),
		thumbnails: thumbsFor(f),
		author: authorOf(f.source),
		datetime: dateOf(f),
		duration: durationOf(f),
		viewCount: -1,
		// the page URL carries the full FileEntry so opening it needs no lookup
		url: contentPageUrl(f),
		shareUrl: contentPageUrl(f),
		isLive: false,
	});
}

// Prefer the embedded tag title (optionally "Artist — Title") over the raw
// filename, so the feed reads like a library, not a directory listing.
function displayName(f: FileEntry): string {
	const m = f.media || {};
	if (m.title) {
		return m.artist ? `${m.artist} — ${m.title}` : m.title;
	}
	return baseName(f.name);
}

// Whole-second duration for Grayjay's UI (0 when unknown).
function durationOf(f: FileEntry): number {
	const d = f.media && f.media.duration_secs;
	return typeof d === "number" && d > 0 ? Math.round(d) : 0;
}

// Cover-art thumbnail (served by the gateway at /thumb/{hash}) when one was
// extracted; otherwise an empty set.
function thumbsFor(f: FileEntry): Thumbnails {
	if (!f.thumb) return new Thumbnails([]);
	return new Thumbnails([new Thumbnail(`${BASE_URL}/thumb/${f.hash}`, 0)]);
}

function authorOf(sourceLabel: string): PlatformAuthorLink {
	const label = sourceLabel || "local";
	// the author link points at the source's channel, so tapping it opens that
	// peer's (or this node's) library
	return new PlatformAuthorLink(
		new PlatformID(PLATFORM, `peer:${label}`, PLUGIN_ID),
		label === "local" ? "This node" : label,
		channelUrl(label),
		""
	);
}

// A channel URL identifies a source ("local" or a peer label). It's interpreted
// only by this plugin; Grayjay treats it as opaque and passes it back.
function channelUrl(source: string): string {
	return `${BASE_URL}/channel/${encodeURIComponent(source)}`;
}

function parseChannelUrl(url: string): string {
	const m = /\/channel\/([^/?#]+)/.exec(url || "");
	return m ? decodeURIComponent(m[1]) : "local";
}

// A minimal channel for search results (no offline probe — getChannel adds the
// "⚠ Offline" marker when the channel is actually opened).
function channelStub(src: string): PlatformChannel {
	return new PlatformChannel({
		id: new PlatformID(PLATFORM, `peer:${src}`, PLUGIN_ID),
		name: src === "local" ? "This node" : src,
		thumbnail: "",
		description: src === "local" ? "Files this node shares" : `Files reachable via ${src}`,
		url: channelUrl(src),
	});
}

// Display name of a folder/playlist: its last path segment.
function folderName(folder: string): string {
	return baseName(folder) || folder || "files";
}

// Parsed form of a playlist URL.
interface ParsedPlaylist {
	kind: "folder" | "album" | "artist";
	source: string;
	name: string;
	folder: string;
}

// A playlist URL identifies a grouping — a source's folder, an album, or an
// artist — tagged with its kind, and (optionally) scoped to a source so
// getPlaylist resolves the right files and identical names across peers stay
// distinct. Opaque to Grayjay; interpreted only here.
//   folder:  key = folder path,  source = the owning source
//   album:   key = album name,   source = "" (whole library) or a source label
//   artist:  key = artist name,  source = "" or a source label
function playlistUrl(kind: ParsedPlaylist["kind"], key: string, source: string): string {
	const s = source || "";
	const enc = kind === "folder" ? `folder\t${s}\t${key}` : `${kind}\t${key}\t${s}`;
	return `${BASE_URL}/playlist/${encodeURIComponent(enc)}`;
}

function parsePlaylistUrl(url: string): ParsedPlaylist {
	const m = /\/playlist\/([^/?#]+)/.exec(url || "");
	const parts = (m ? decodeURIComponent(m[1]) : "").split("\t");
	switch (parts[0]) {
		case "folder":
			return { kind: "folder", source: parts[1] || "local", folder: parts[2] || "", name: "" };
		case "album":
			return { kind: "album", name: parts[1] || "", source: parts[2] || "", folder: "" };
		case "artist":
			return { kind: "artist", name: parts[1] || "", source: parts[2] || "", folder: "" };
		default:
			// legacy "source\tfolder" form (a playlist url cached by an older plugin)
			return { kind: "folder", source: parts[0] || "local", folder: parts[1] || "", name: "" };
	}
}

// Build a playlist stub (name + cover + count) for the channel Playlists tab from
// a gateway grouping. Grayjay resolves the contents lazily via getPlaylist when
// one is opened.
function groupStub(kind: ParsedPlaylist["kind"], g: PlaylistGroup, source: string): PlatformPlaylist {
	const coverUrl = g.cover ? `${BASE_URL}/thumb/${g.cover}` : "";
	return new PlatformPlaylist({
		id: new PlatformID(PLATFORM, `playlist:${kind}:${source}:${g.key}`, PLUGIN_ID),
		name: g.name,
		thumbnails: g.cover ? new Thumbnails([new Thumbnail(coverUrl, 0)]) : new Thumbnails([]),
		thumbnail: coverUrl,
		author: authorOf(source),
		datetime: nowSeconds(),
		url: playlistUrl(kind, g.key, source),
		videoCount: g.count,
	});
}

// The plain streaming URL the player GETs (the gateway routes on the hash in the
// path and ignores query params). `name` is a download-filename hint.
function contentUrl(hash: string, name: string): string {
	return `${BASE_URL}/file/${hash}?name=${encodeURIComponent(name || "")}`;
}

// The content-page / share URL. It embeds the whole FileEntry (`&m=`) we already
// hold when building the card, so getContentDetails can reconstruct duration,
// tags, date, thumbnail — everything — with ZERO network calls. Without this,
// getContentDetails re-downloads the entire library (a full browse of every
// peer) just to find one row, which for a large/slow peer arrives late or
// incomplete → duration 0 → Grayjay frame-guesses → the "-12:-55" seekbar. The
// player still streams from the plain `contentUrl`; `m` is read on open only.
function contentPageUrl(f: FileEntry): string {
	let blob = "";
	try {
		blob = encodeURIComponent(JSON.stringify(f));
	} catch (e) {
		// fall back to a metadata-less URL; getContentDetails will look it up
	}
	return blob ? `${contentUrl(f.hash, f.name)}&m=${blob}` : contentUrl(f.hash, f.name);
}

function parseFileUrl(url: string): { hash: string; name: string; embedded: FileEntry | null } {
	const m = /\/file\/([0-9a-fA-F]+)/.exec(url);
	const hash = m ? m[1] : "";
	let name = "";
	const q = url.indexOf("name=");
	if (q !== -1) {
		name = decodeURIComponent(url.substring(q + 5).split("&")[0]);
	}
	// The metadata blob embedded by contentPageUrl, if present.
	let embedded: FileEntry | null = null;
	const mi = url.indexOf("m=");
	if (mi !== -1) {
		try {
			const raw = decodeURIComponent(url.substring(mi + 2).split("&")[0]);
			const parsed = JSON.parse(raw) as FileEntry;
			if (parsed && parsed.hash) embedded = parsed;
		} catch (e) {
			// malformed/absent blob → fall back to a lookup
		}
	}
	return { hash, name, embedded };
}

function baseName(path: string): string {
	if (!path) return "";
	const parts = path.split("/");
	return parts[parts.length - 1] || path;
}

function isPlayable(f: FileEntry): boolean {
	return contentType(f) !== "application/octet-stream";
}

// "audio" or "video" (or "" for non-media) from the file's content type.
function mimeClassOf(f: FileEntry): string {
	return contentType(f).split("/")[0];
}

function containerOf(name: string): string {
	const ext = (name.split(".").pop() || "").toLowerCase();
	switch (ext) {
		case "mp4":
		case "m4v":
			return "video/mp4";
		case "webm":
			return "video/webm";
		case "mkv":
			return "video/x-matroska";
		case "mov":
			return "video/quicktime";
		case "mp3":
			return "audio/mpeg";
		case "m4a":
		case "aac":
			return "audio/mp4";
		case "flac":
			return "audio/flac";
		case "ogg":
		case "opus":
			return "audio/ogg";
		default:
			return "application/octet-stream";
	}
}

// One-line description: tag info (artist/album), then size and provenance.
function describe(f: FileEntry): string {
	const m = f.media || {};
	const parts: string[] = [];
	if (m.artist) parts.push(m.artist);
	if (m.album) parts.push(m.album);
	parts.push(humanSize(f.size));
	parts.push(`source: ${f.source}`);
	return parts.join(" · ");
}

function humanSize(n: number): string {
	const u = ["B", "KiB", "MiB", "GiB", "TiB"];
	let v = n,
		i = 0;
	while (v >= 1024 && i < u.length - 1) {
		v /= 1024;
		i++;
	}
	return `${i === 0 ? v : v.toFixed(1)} ${u[i]}`;
}

// A file's publish date for Grayjay: its real mtime when known, so the feed has
// a stable order and sort-by-date works. Falls back to "now" only when the
// gateway couldn't determine an mtime (mtime 0/absent) — without this the
// datetime was always now, so every item read as "just now" and the feed
// reshuffled on each fetch.
function dateOf(f: FileEntry): number {
	return typeof f.mtime === "number" && f.mtime > 0 ? f.mtime : nowSeconds();
}

function nowSeconds(): number {
	try {
		return Math.floor(Date.now() / 1000);
	} catch (e) {
		return 0;
	}
}

class FilestrVideoPager extends VideoPager {
	constructor(results: PlatformVideo[]) {
		super(results, false);
	}
	nextPage(): FilestrVideoPager {
		return new FilestrVideoPager([]);
	}
}

class FilestrPlaylistPager extends PlaylistPager {
	constructor(results: PlatformPlaylist[]) {
		super(results, false);
	}
	nextPage(): FilestrPlaylistPager {
		return new FilestrPlaylistPager([]);
	}
}

class FilestrChannelPager extends ChannelPager {
	constructor(results: PlatformChannel[]) {
		super(results, false);
	}
	nextPage(): FilestrChannelPager {
		return new FilestrChannelPager([]);
	}
}

// The Recommended tab takes a ContentPager (mixed content); we fill it with the
// playlists the current file belongs to.
class FilestrContentPager extends ContentPager {
	constructor(results: IPlatformContent[]) {
		super(results, false);
	}
	nextPage(): FilestrContentPager {
		return new FilestrContentPager([]);
	}
}

console.log("filestr plugin loaded");
