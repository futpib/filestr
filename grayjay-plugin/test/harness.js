// Test harness for the filestr Grayjay plugin.
//
// Loads Grayjay's OWN injected scaffolding (polyfil.js + source.js) so the
// plugin runs against the real runtime contracts, mocks the `http` package
// (synchronous, via curl) and `bridge`/`console`, then exercises the plugin
// against a LIVE filestr HTTP gateway and verifies that the stream URLs it
// hands back actually return the right bytes (full + Range).
//
// Usage: node harness.js [BASE_URL] [GRAYJAY_SCRIPTS_DIR]

const fs = require("fs");
const vm = require("vm");
const path = require("path");
const { execFileSync } = require("child_process");

const BASE = (process.argv[2] || "http://127.0.0.1:11780").replace(/\/+$/, "");
const SCRIPTS =
	process.argv[3] ||
	"/home/claude/code/grayjay-android/app/src/main/assets/scripts";
const PLUGIN_DIR = path.join(__dirname, "..");

let failures = 0;
function check(name, cond, detail) {
	if (cond) {
		console.log(`  PASS  ${name}`);
	} else {
		failures++;
		console.log(`  FAIL  ${name}${detail ? " — " + detail : ""}`);
	}
}

// --- synchronous HTTP via curl (mirrors Grayjay's blocking http.GET) --------
function curl(method, url, headers) {
	const args = ["-s", "-X", method, "-w", "\n__HTTP_CODE__%{http_code}", url];
	for (const k of Object.keys(headers || {})) {
		args.push("-H", `${k}: ${headers[k]}`);
	}
	let out;
	try {
		out = execFileSync("curl", args, { maxBuffer: 256 * 1024 * 1024 }).toString(
			"latin1"
		);
	} catch (e) {
		return { isOk: false, code: 0, body: String(e) };
	}
	const idx = out.lastIndexOf("__HTTP_CODE__");
	const code = parseInt(out.slice(idx + "__HTTP_CODE__".length), 10);
	const body = out.slice(0, idx).replace(/\n$/, "");
	return { isOk: code >= 200 && code < 300, code, body, headers: {} };
}

const httpMock = {
	GET: (url, headers) => curl("GET", url, headers),
	POST: (url, _body, headers) => curl("POST", url, headers),
};

// --- build a single combined script so class/const decls are shared ---------
const polyfil = fs.readFileSync(path.join(SCRIPTS, "polyfil.js"), "utf8");
const sourceJs = fs.readFileSync(path.join(SCRIPTS, "source.js"), "utf8");
const plugin = fs.readFileSync(path.join(PLUGIN_DIR, "FilestrScript.js"), "utf8");

const driver = `
out.errors = [];
try {
	source.enable({ id: "test-plugin-id" }, { serverUrl: ${JSON.stringify(BASE)} }, null);

	const home = source.getHome(null);
	out.home = { type: home.plugin_type, count: home.results.length,
		items: home.results.map(v => ({ t: v.plugin_type, name: v.name, url: v.url, author: v.author ? v.author.name : null })) };

	const search = source.search("clip", null, null, {}, null);
	out.search = { count: search.results.length, names: search.results.map(v => v.name) };

	// pick the mp4 item from home
	const mp4 = home.results.find(v => v.url.toLowerCase().indexOf("clip.mp4") !== -1) || home.results[0];
	out.pickedUrl = mp4.url;
	out.isDetailsUrl = source.isContentDetailsUrl(mp4.url);

	const d = source.getContentDetails(mp4.url);
	const vs = d.video && d.video.videoSources ? d.video.videoSources : [];
	out.details = {
		type: d.plugin_type,
		name: d.name,
		description: d.description,
		sourceCount: vs.length,
		streamUrl: vs[0] ? vs[0].url : null,
		container: vs[0] ? vs[0].container : null,
	};
} catch (e) {
	out.errors.push(String(e && e.stack ? e.stack : e));
}
`;

const combined = [polyfil, sourceJs, plugin, driver].join("\n;\n");

const out = {};
const ctx = {
	console: { log: (...a) => {} }, // silence plugin's own logging
	bridge: { log: () => {}, setTimeout: setTimeout, clearTimeout: clearTimeout },
	http: httpMock,
	out,
};
vm.createContext(ctx);

console.log(`filestr Grayjay plugin harness  (gateway: ${BASE})`);
console.log(`scaffolding: ${SCRIPTS}`);
console.log("");

try {
	vm.runInContext(combined, ctx, { filename: "combined.js", timeout: 30000 });
} catch (e) {
	console.log("HARNESS ERROR running plugin:", e.stack || e);
	process.exit(2);
}

if (out.errors && out.errors.length) {
	console.log("PLUGIN THREW:");
	for (const e of out.errors) console.log(e);
	process.exit(2);
}

// --- assertions on plugin output -------------------------------------------
console.log("[scaffolding + plugin]");
check("getHome returns a VideoPager", out.home.type === "VideoPager", out.home.type);
check("getHome returns items", out.home.count > 0, `count=${out.home.count}`);
check(
	"home items are PlatformVideo",
	out.home.items.every((i) => i.t === "PlatformVideo"),
	JSON.stringify(out.home.items.map((i) => i.t))
);
check(
	"home items carry a /file/ stream url",
	out.home.items.every((i) => /\/file\/[0-9a-f]+/.test(i.url)),
	out.home.items.map((i) => i.url).join(", ")
);
check(
	"search('clip') narrows results",
	out.search.count >= 1 && out.search.count <= out.home.count,
	`search=${out.search.count} home=${out.home.count}`
);
check("isContentDetailsUrl(true) for a file url", out.isDetailsUrl === true);
check(
	"getContentDetails returns PlatformVideoDetails",
	out.details.type === "PlatformVideoDetails",
	out.details.type
);
check(
	"details has a video source",
	out.details.sourceCount >= 1,
	`count=${out.details.sourceCount}`
);
check(
	"details stream url is a /file/ url",
	out.details.streamUrl && /\/file\/[0-9a-f]+/.test(out.details.streamUrl),
	out.details.streamUrl
);

// --- end-to-end: the stream url the plugin produced really serves bytes -----
console.log("");
console.log("[playback: fetch the plugin's stream url]");
const streamUrl = out.details.streamUrl;

// full fetch via the plugin's url
const full = execFileSync("curl", ["-s", streamUrl], {
	maxBuffer: 256 * 1024 * 1024,
});
// reference: pull the same hash straight from the gateway
const hash = /\/file\/([0-9a-f]+)/.exec(streamUrl)[1];
const ref = execFileSync("curl", ["-s", `${BASE}/file/${hash}`], {
	maxBuffer: 256 * 1024 * 1024,
});
check("plugin stream url returns bytes", full.length > 0, `len=${full.length}`);
check(
	"plugin stream matches direct gateway fetch",
	Buffer.compare(full, ref) === 0,
	`plugin=${full.length} direct=${ref.length}`
);

// range request through the plugin's url
const head = execFileSync("curl", [
	"-s",
	"-D",
	"-",
	"-o",
	"/dev/null",
	"-H",
	"Range: bytes=0-99",
	streamUrl,
]).toString();
check(
	"range request on plugin url -> 206",
	/206 Partial Content/i.test(head),
	head.split("\n")[0]
);
check(
	"range request reports content-range",
	/content-range:\s*bytes 0-99\//i.test(head),
	(head.match(/content-range:[^\r\n]*/i) || [""])[0]
);

console.log("");
if (failures === 0) {
	console.log(`ALL PASS (${out.home.count} files served via plugin)`);
	process.exit(0);
} else {
	console.log(`${failures} CHECK(S) FAILED`);
	process.exit(1);
}
