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
	return new FilestrVideoPager(listVideos(null));
};

source.searchSuggestions = function (query) {
	return [];
};

source.getSearchCapabilities = function () {
	return {
		types: [Type.Feed.Mixed],
		sorts: [],
		filters: [],
	};
};

source.search = function (query, type, order, filters, continuationToken) {
	return new FilestrVideoPager(listVideos(query));
};

source.isContentDetailsUrl = function (url) {
	return typeof url === "string" && url.indexOf("/file/") !== -1;
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
	const display = item ? baseName(item.name) : name || hash;
	const sourceLabel = item ? item.source : "filestr";

	return new PlatformVideoDetails({
		id: new PlatformID(PLATFORM, hash, PLUGIN_ID),
		name: display,
		thumbnails: new Thumbnails([]),
		author: authorOf(sourceLabel),
		datetime: nowSeconds(),
		duration: 0,
		viewCount: -1,
		url: contentUrl(hash, item ? item.name : display),
		isLive: false,
		description: item ? `${humanSize(item.size)} · source: ${item.source}` : "",
		video: new VideoSourceDescriptor([
			new VideoUrlSource({
				name: "filestr",
				url: contentUrl(hash, item ? item.name : display),
				container: containerOf(display),
				duration: 0,
				width: 0,
				height: 0,
			}),
		]),
	});
};

// --- helpers ---------------------------------------------------------------

function fetchFiles() {
	const res = http.GET(`${BASE_URL}/files`, {});
	if (!res.isOk) {
		throw new ScriptException(`filestr gateway ${res.code} at ${BASE_URL}/files`);
	}
	const data = JSON.parse(res.body);
	return data.files || [];
}

function listVideos(query) {
	let files = fetchFiles();
	if (query) {
		const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
		files = files.filter((f) => {
			const hay = f.name.toLowerCase();
			return terms.every((t) => hay.indexOf(t) !== -1);
		});
	}
	return files.map(fileToVideo);
}

function fileToVideo(f) {
	return new PlatformVideo({
		id: new PlatformID(PLATFORM, f.hash, PLUGIN_ID),
		name: baseName(f.name),
		thumbnails: new Thumbnails([]),
		author: authorOf(f.source),
		datetime: nowSeconds(),
		duration: 0,
		viewCount: -1,
		url: contentUrl(f.hash, f.name),
		isLive: false,
	});
}

function authorOf(sourceLabel) {
	const label = sourceLabel || "filestr";
	return new PlatformAuthorLink(
		new PlatformID(PLATFORM, `peer:${label}`, PLUGIN_ID),
		label,
		"",
		""
	);
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
