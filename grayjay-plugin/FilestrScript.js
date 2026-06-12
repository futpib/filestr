// Grayjay source plugin for filestr.
//
// Talks to the filestr app's local HTTP gateway (see filestrd's [http] bridge)
// over loopback and presents every file the node can serve — its own shares
// plus everything reachable through its grant graph — as Grayjay content with
// a directly playable stream URL.

const PLATFORM = "filestr";
let PLUGIN_ID = "filestr";
let BASE_URL = "http://127.0.0.1:11780";

source.enable = function (conf, settings, savedState) {
	if (conf && conf.id) PLUGIN_ID = conf.id;
	if (settings && settings.serverUrl) {
		BASE_URL = String(settings.serverUrl).replace(/\/+$/, "");
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

source.search = function (query, type, order, filters, continuationToken) {
	return new FilestrVideoPager(toVideos(fetchSearch(query), mimeClassesFromFilters(filters)));
};

// Extract the selected media classes (e.g. ["audio"]) from Grayjay's filters
// map. Empty/absent means no restriction.
function mimeClassesFromFilters(filters) {
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
	return new PlatformChannel({
		id: new PlatformID(PLATFORM, `peer:${src}`, PLUGIN_ID),
		name: name,
		thumbnail: "",
		description: src === "local" ? "Files this node shares" : `Files reachable via ${src}`,
		url: channelUrl(src),
	});
};

source.getChannelContents = function (url, type, order, filters) {
	const src = parseChannelUrl(url);
	const files = fetchFiles().filter((f) => f.source === src);
	return new FilestrVideoPager(toVideos(files, mimeClassesFromFilters(filters)));
};

// Your peers are your subscriptions: one channel per source we can serve from,
// excluding this node itself.
source.getSubscriptionsUser = function () {
	const sources = {};
	for (const f of fetchFiles()) {
		if (f.source && f.source !== "local") sources[f.source] = true;
	}
	return Object.keys(sources).map(channelUrl);
};

source.getContentDetails = function (url) {
	const { hash, name } = parseFileUrl(url);
	// Find the matching file for accurate name/size; fall back to the URL.
	let item = null;
	try {
		item = fetchFiles().find((f) => f.hash === hash) || null;
	} catch (e) {
		// gateway may be momentarily unavailable; build from the URL instead
	}
	const display = item ? displayName(item) : name || hash;
	const sourceLabel = item ? item.source : "filestr";
	const dur = item ? durationOf(item) : 0;
	const fileName = item ? item.name : display;

	return new PlatformVideoDetails({
		id: new PlatformID(PLATFORM, hash, PLUGIN_ID),
		name: display,
		thumbnails: item ? thumbsFor(item) : new Thumbnails([]),
		author: authorOf(sourceLabel),
		datetime: nowSeconds(),
		duration: dur,
		viewCount: -1,
		url: contentUrl(hash, fileName),
		isLive: false,
		description: item ? describe(item) : "",
		video: new VideoSourceDescriptor([
			new VideoUrlSource({
				name: "filestr",
				url: contentUrl(hash, fileName),
				container: item ? contentType(item) : containerOf(fileName),
				duration: dur,
				width: 0,
				height: 0,
			}),
		]),
	});
};

// --- helpers ---------------------------------------------------------------

function fetchJson(pathAndQuery) {
	const res = http.GET(`${BASE_URL}${pathAndQuery}`, {});
	if (!res.isOk) {
		throw new ScriptException(`filestr gateway ${res.code} at ${BASE_URL}${pathAndQuery}`);
	}
	return JSON.parse(res.body).files || [];
}

// Everything this node can serve (its shares + a one-hop browse of peers).
function fetchFiles() {
	return fetchJson("/files");
}

// The daemon's federated grant-graph search: reaches the whole reachable graph,
// not just direct peers, and matches the tag metadata (title/artist/album), not
// just the filename. The gateway records sources so the results are playable.
function fetchSearch(query) {
	if (!query) return [];
	return fetchJson(`/search?q=${encodeURIComponent(query)}`);
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
	if (!f.thumb) return new Thumbnails([]);
	return new Thumbnails([new Thumbnail(`${BASE_URL}/thumb/${f.hash}`, 0)]);
}

function authorOf(sourceLabel) {
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
function channelUrl(source) {
	return `${BASE_URL}/channel/${encodeURIComponent(source)}`;
}

function parseChannelUrl(url) {
	const m = /\/channel\/([^/?#]+)/.exec(url || "");
	return m ? decodeURIComponent(m[1]) : "local";
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
	if (!path) return "";
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
	if (m.artist) parts.push(m.artist);
	if (m.album) parts.push(m.album);
	parts.push(humanSize(f.size));
	parts.push(`source: ${f.source}`);
	return parts.join(" · ");
}

function humanSize(n) {
	const u = ["B", "KiB", "MiB", "GiB", "TiB"];
	let v = n,
		i = 0;
	while (v >= 1024 && i < u.length - 1) {
		v /= 1024;
		i++;
	}
	return `${i === 0 ? v : v.toFixed(1)} ${u[i]}`;
}

function nowSeconds() {
	try {
		return Math.floor(Date.now() / 1000);
	} catch (e) {
		return 0;
	}
}

class FilestrVideoPager extends VideoPager {
	constructor(results) {
		super(results, false, {});
	}
	nextPage() {
		return new FilestrVideoPager([]);
	}
}

console.log("filestr plugin loaded");
