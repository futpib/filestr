// Loads the filestr Grayjay plugin into Grayjay's OWN runtime scaffolding
// (polyfil.js + source.js) inside a vm, with a synchronous `http` shim (Grayjay's
// http.GET is blocking) backed by curl. Returns the live `source` object so a
// node:test can call source.getHome()/search()/... directly and assert on the
// results — no string drivers, no jq.

const fs = require("fs");
const vm = require("vm");
const path = require("path");
const { execFileSync } = require("child_process");
const { SCAFFOLD, ROOT } = require("./daemon");

// Synchronous HTTP via curl, mirroring Grayjay's blocking http.GET contract:
// returns { isOk, code, body }.
function curl(method, url, headers) {
	const args = ["-s", "-X", method, "-w", "\n__CODE__%{http_code}", url];
	for (const k of Object.keys(headers || {})) args.push("-H", `${k}: ${headers[k]}`);
	let out;
	try {
		out = execFileSync("curl", args, { maxBuffer: 1 << 28 }).toString("utf8");
	} catch (e) {
		return { isOk: false, code: 0, body: String(e) };
	}
	const i = out.lastIndexOf("__CODE__");
	const code = parseInt(out.slice(i + "__CODE__".length), 10);
	return { isOk: code >= 200 && code < 300, code, body: out.slice(0, i).replace(/\n$/, ""), headers: {} };
}

/// Load the plugin against `baseUrl` and return its enabled `source` object.
function loadSource(baseUrl) {
	const base = baseUrl.replace(/\/+$/, "");
	const code = [
		fs.readFileSync(path.join(SCAFFOLD, "polyfil.js"), "utf8"),
		fs.readFileSync(path.join(SCAFFOLD, "source.js"), "utf8"),
		fs.readFileSync(path.join(ROOT, "grayjay-plugin", "FilestrScript.js"), "utf8"),
		`source.enable({ id: "test-plugin-id" }, { serverUrl: ${JSON.stringify(base)} }, null);`,
		// `source` is a const in the scaffolding, so it doesn't attach to the
		// context object — expose it explicitly for the host to drive.
		`globalThis.__pluginSource = source;`,
	].join("\n;\n");

	const ctx = {
		console: { log() {}, error() {} },
		bridge: { log() {}, setTimeout, clearTimeout },
		http: {
			GET: (url, headers) => curl("GET", url, headers),
			POST: (url, _body, headers) => curl("POST", url, headers),
		},
	};
	vm.createContext(ctx);
	vm.runInContext(code, ctx, { timeout: 60000 });
	return ctx.__pluginSource;
}

module.exports = { loadSource };
