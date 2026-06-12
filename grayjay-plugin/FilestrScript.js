"use strict";
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
const PLATFORM = "filestr";
let PLUGIN_ID = "filestr";
let BASE_URL = "http://127.0.0.1:11780";
source.enable = function (conf, settings, savedState) {
    if (conf && conf.id)
        PLUGIN_ID = conf.id;
    const s = settings;
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
    return new ResultCapabilities([Type.Feed.Mixed], [], [
        new FilterGroup("Type", [
            new FilterCapability("Audio", "audio", "audio"),
            new FilterCapability("Video", "video", "video"),
        ], true, // multi-select
        MIME_FILTER),
    ]);
};
source.search = function (query, type, order, filters) {
    return new FilestrVideoPager(toVideos(fetchSearch(query), mimeClassesFromFilters(filters)));
};
// Extract the selected media classes (e.g. ["audio"]) from Grayjay's filters
// map. Empty/absent means no restriction.
function mimeClassesFromFilters(filters) {
    if (!filters)
        return null;
    const sel = filters[MIME_FILTER];
    if (!sel || !sel.length)
        return null;
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
            throw new ScriptException(`${src} is offline — this peer isn't reachable right now, so its files can't be loaded. It'll be available again once it's back online.`);
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
    const sources = {};
    for (const f of index.files || []) {
        if (f.source && f.source !== "local")
            sources[f.source] = true;
    }
    for (const p of index.peers || []) {
        if (p.label && p.label !== "local")
            sources[p.label] = true;
    }
    return Object.keys(sources).map(channelUrl);
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
function albumOf(f) {
    return (f.media && f.media.album) || "";
}
function artistOf(f) {
    return (f.media && f.media.artist) || "";
}
source.getUserPlaylists = function () {
    const files = fetchFiles().filter(isPlayable);
    const out = [];
    const seenFolder = {};
    const seenAlbum = {};
    const seenArtist = {};
    for (const f of files) {
        // this node's own folders (peer folders are reached via the peer channel)
        if (f.source === "local") {
            const folder = dirOf(f.name);
            if (!seenFolder[folder]) {
                seenFolder[folder] = true;
                out.push(playlistUrl("folder", folder, "local"));
            }
        }
        // albums and artists span the whole reachable library here (no source scope)
        const album = albumOf(f);
        if (album && !seenAlbum[album]) {
            seenAlbum[album] = true;
            out.push(playlistUrl("album", album, ""));
        }
        const artist = artistOf(f);
        if (artist && !seenArtist[artist]) {
            seenArtist[artist] = true;
            out.push(playlistUrl("artist", artist, ""));
        }
    }
    return out;
};
// Playlists for one channel (a peer, or "local"): that source's folders, albums
// and artists, scoped so opening one shows only that source's files and so the
// same album name from two peers stays distinct. This is what populates the
// channel page's Playlists tab.
source.getChannelPlaylists = function (url) {
    const src = parseChannelUrl(url);
    const index = fetchIndex("/files");
    if (src !== "local") {
        const peer = (index.peers || []).find((p) => p.label === src);
        if (peer && peer.reachable === false) {
            throw new ScriptException(`${src} is offline — this peer isn't reachable right now, so its playlists can't be loaded. It'll be available again once it's back online.`);
        }
    }
    const files = (index.files || []).filter((f) => f.source === src && isPlayable(f));
    const folders = {};
    const albums = {};
    const artists = {};
    for (const f of files) {
        const folder = dirOf(f.name);
        (folders[folder] = folders[folder] || []).push(f);
        const al = albumOf(f);
        if (al)
            (albums[al] = albums[al] || []).push(f);
        const ar = artistOf(f);
        if (ar)
            (artists[ar] = artists[ar] || []).push(f);
    }
    const out = [];
    for (const folder of Object.keys(folders)) {
        out.push(playlistStub("folder", folderName(folder), folder, src, folders[folder]));
    }
    for (const al of Object.keys(albums)) {
        out.push(playlistStub("album", al, al, src, albums[al]));
    }
    for (const ar of Object.keys(artists)) {
        out.push(playlistStub("artist", ar, ar, src, artists[ar]));
    }
    return new FilestrPlaylistPager(out);
};
source.getPlaylist = function (url) {
    const p = parsePlaylistUrl(url);
    const all = fetchFiles().filter(isPlayable);
    let files;
    let name;
    if (p.kind === "album") {
        files = all.filter((f) => albumOf(f) === p.name && (!p.source || f.source === p.source));
        name = p.name || "Album";
    }
    else if (p.kind === "artist") {
        files = all.filter((f) => artistOf(f) === p.name && (!p.source || f.source === p.source));
        name = p.name || "Artist";
    }
    else {
        files = all.filter((f) => f.source === p.source && dirOf(f.name) === p.folder);
        name = folderName(p.folder);
    }
    const cover = files.find((f) => f.thumb);
    const author = authorOf(files.length ? files[0].source : "local");
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
source.getContentDetails = function (url) {
    const { hash, name } = parseFileUrl(url);
    // Find the matching file for accurate name/size; fall back to the URL.
    let item = null;
    try {
        item = fetchFiles().find((f) => f.hash === hash) || null;
    }
    catch (e) {
        // gateway may be momentarily unavailable; build from the URL instead
    }
    const display = item ? displayName(item) : name || hash;
    const sourceLabel = item ? item.source : "filestr";
    const dur = item ? durationOf(item) : 0;
    const fileName = item ? item.name : display;
    const container = item ? contentType(item) : containerOf(fileName);
    return new PlatformVideoDetails({
        id: new PlatformID(PLATFORM, hash, PLUGIN_ID),
        name: display,
        thumbnails: item ? thumbsFor(item) : new Thumbnails([]),
        author: authorOf(sourceLabel),
        datetime: nowSeconds(),
        duration: dur,
        viewCount: -1,
        url: contentUrl(hash, fileName),
        shareUrl: contentUrl(hash, fileName),
        isLive: false,
        description: item ? describe(item) : "",
        rating: new RatingLikes(0),
        video: sourceDescriptor(contentUrl(hash, fileName), container, dur),
    });
};
// Build the playback source descriptor. An audio file must be served as an
// AudioUrlSource (in an unmuxed descriptor with no video track) — handing
// Grayjay an audio file dressed as a VideoUrlSource makes the player ignore our
// explicit duration and guess it from the MP3 frames, which mis-reads VBR files
// (the "-12:-55" duration bug). We always pass the index's exact duration so the
// seekbar is right regardless of the container's internal headers.
function sourceDescriptor(url, container, duration) {
    if ((container || "").split("/")[0] === "audio") {
        return new UnMuxVideoSourceDescriptor([], [
            new AudioUrlSource({
                name: "filestr",
                url,
                container,
                duration,
                bitrate: 0,
                codec: "",
                language: Language.UNKNOWN,
            }),
        ]);
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
// Fetch and parse a gateway listing endpoint (shape: {files, peers}). A failed
// request almost always means the filestr app isn't running on this device, so
// say that plainly instead of surfacing a raw HTTP status.
function fetchIndex(pathAndQuery) {
    const res = http.GET(`${BASE_URL}${pathAndQuery}`, {}, false);
    if (!res.isOk) {
        throw new ScriptException("filestr isn't reachable — open the filestr app on this device and make sure it's running, then try again.");
    }
    return JSON.parse(res.body) || {};
}
// Everything this node can serve (its shares + a one-hop browse of peers).
function fetchFiles() {
    return fetchIndex("/files").files || [];
}
// The granted peers and whether each answered the latest browse, so we can tell
// an offline peer apart from one that simply has nothing to share.
function fetchPeers() {
    return fetchIndex("/files").peers || [];
}
// The daemon's federated grant-graph search: reaches the whole reachable graph,
// not just direct peers, and matches the tag metadata (title/artist/album), not
// just the filename. The gateway records sources so the results are playable.
function fetchSearch(query) {
    if (!query)
        return [];
    return fetchIndex(`/search?q=${encodeURIComponent(query)}`).files || [];
}
// Shared pipeline: keep only files Grayjay can play (audio/video — anything that
// maps to the generic octet-stream container is hidden), optionally restrict to
// the selected media classes, then map to Grayjay videos.
function toVideos(files, mimeClasses) {
    let out = files.filter(isPlayable);
    if (mimeClasses && mimeClasses.length) {
        out = out.filter((f) => mimeClasses.indexOf(mimeClassOf(f)) !== -1);
    }
    return out.map(fileToVideo);
}
// The content type for a file: the type sniffed at index time (correct even for
// a misnamed file), else inferred from the filename extension.
function contentType(f) {
    const ct = f.media && f.media.content_type;
    return ct || containerOf(f.name);
}
function fileToVideo(f) {
    return new PlatformVideo({
        id: new PlatformID(PLATFORM, f.hash, PLUGIN_ID),
        name: displayName(f),
        thumbnails: thumbsFor(f),
        author: authorOf(f.source),
        datetime: nowSeconds(),
        duration: durationOf(f),
        viewCount: -1,
        url: contentUrl(f.hash, f.name),
        shareUrl: contentUrl(f.hash, f.name),
        isLive: false,
    });
}
// Prefer the embedded tag title (optionally "Artist — Title") over the raw
// filename, so the feed reads like a library, not a directory listing.
function displayName(f) {
    const m = f.media || {};
    if (m.title) {
        return m.artist ? `${m.artist} — ${m.title}` : m.title;
    }
    return baseName(f.name);
}
// Whole-second duration for Grayjay's UI (0 when unknown).
function durationOf(f) {
    const d = f.media && f.media.duration_secs;
    return typeof d === "number" && d > 0 ? Math.round(d) : 0;
}
// Cover-art thumbnail (served by the gateway at /thumb/{hash}) when one was
// extracted; otherwise an empty set.
function thumbsFor(f) {
    if (!f.thumb)
        return new Thumbnails([]);
    return new Thumbnails([new Thumbnail(`${BASE_URL}/thumb/${f.hash}`, 0)]);
}
function authorOf(sourceLabel) {
    const label = sourceLabel || "local";
    // the author link points at the source's channel, so tapping it opens that
    // peer's (or this node's) library
    return new PlatformAuthorLink(new PlatformID(PLATFORM, `peer:${label}`, PLUGIN_ID), label === "local" ? "This node" : label, channelUrl(label), "");
}
// A channel URL identifies a source ("local" or a peer label). It's interpreted
// only by this plugin; Grayjay treats it as opaque and passes it back.
function channelUrl(source) {
    return `${BASE_URL}/channel/${encodeURIComponent(source)}`;
}
function parseChannelUrl(url) {
    const m = /\/channel\/([^/?#]+)/.exec(url || "");
    return m ? decodeURIComponent(m[1]) : "local";
}
// The folder a file lives in (its visible path minus the final segment).
function dirOf(name) {
    const i = (name || "").lastIndexOf("/");
    return i === -1 ? "" : name.slice(0, i);
}
// Display name of a folder/playlist: its last path segment.
function folderName(folder) {
    return baseName(folder) || folder || "files";
}
// A playlist URL identifies a grouping — a source's folder, an album, or an
// artist — tagged with its kind, and (optionally) scoped to a source so
// getPlaylist resolves the right files and identical names across peers stay
// distinct. Opaque to Grayjay; interpreted only here.
//   folder:  key = folder path,  source = the owning source
//   album:   key = album name,   source = "" (whole library) or a source label
//   artist:  key = artist name,  source = "" or a source label
function playlistUrl(kind, key, source) {
    const s = source || "";
    const enc = kind === "folder" ? `folder\t${s}\t${key}` : `${kind}\t${key}\t${s}`;
    return `${BASE_URL}/playlist/${encodeURIComponent(enc)}`;
}
function parsePlaylistUrl(url) {
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
// Build a playlist stub (name + cover + count) for the channel Playlists tab.
// Grayjay resolves the contents lazily via getPlaylist when one is opened.
function playlistStub(kind, display, urlKey, source, files) {
    const cover = files.find((f) => f.thumb);
    return new PlatformPlaylist({
        id: new PlatformID(PLATFORM, `playlist:${kind}:${source}:${urlKey}`, PLUGIN_ID),
        name: display,
        thumbnails: cover ? thumbsFor(cover) : new Thumbnails([]),
        thumbnail: cover ? `${BASE_URL}/thumb/${cover.hash}` : "",
        author: authorOf(source),
        datetime: nowSeconds(),
        url: playlistUrl(kind, urlKey, source),
        videoCount: files.length,
    });
}
function contentUrl(hash, name) {
    return `${BASE_URL}/file/${hash}?name=${encodeURIComponent(name || "")}`;
}
function parseFileUrl(url) {
    const m = /\/file\/([0-9a-fA-F]+)/.exec(url);
    const hash = m ? m[1] : "";
    let name = "";
    const q = url.indexOf("name=");
    if (q !== -1) {
        name = decodeURIComponent(url.substring(q + 5).split("&")[0]);
    }
    return { hash, name };
}
function baseName(path) {
    if (!path)
        return "";
    const parts = path.split("/");
    return parts[parts.length - 1] || path;
}
function isPlayable(f) {
    return contentType(f) !== "application/octet-stream";
}
// "audio" or "video" (or "" for non-media) from the file's content type.
function mimeClassOf(f) {
    return contentType(f).split("/")[0];
}
function containerOf(name) {
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
function describe(f) {
    const m = f.media || {};
    const parts = [];
    if (m.artist)
        parts.push(m.artist);
    if (m.album)
        parts.push(m.album);
    parts.push(humanSize(f.size));
    parts.push(`source: ${f.source}`);
    return parts.join(" · ");
}
function humanSize(n) {
    const u = ["B", "KiB", "MiB", "GiB", "TiB"];
    let v = n, i = 0;
    while (v >= 1024 && i < u.length - 1) {
        v /= 1024;
        i++;
    }
    return `${i === 0 ? v : v.toFixed(1)} ${u[i]}`;
}
function nowSeconds() {
    try {
        return Math.floor(Date.now() / 1000);
    }
    catch (e) {
        return 0;
    }
}
class FilestrVideoPager extends VideoPager {
    constructor(results) {
        super(results, false);
    }
    nextPage() {
        return new FilestrVideoPager([]);
    }
}
class FilestrPlaylistPager extends PlaylistPager {
    constructor(results) {
        super(results, false);
    }
    nextPage() {
        return new FilestrPlaylistPager([]);
    }
}
console.log("filestr plugin loaded");
