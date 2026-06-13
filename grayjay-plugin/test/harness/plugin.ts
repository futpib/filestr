// Loads the filestr Grayjay plugin into Grayjay's OWN runtime scaffolding
// (polyfil.js + source.js) inside a vm, with a synchronous `http` shim (Grayjay's
// http.GET is blocking) backed by curl. Returns the live `source` object so a
// node:test can call source.getHome()/search()/... directly and assert on the
// results — no string drivers, no jq.

import * as fs from "node:fs";
import * as vm from "node:vm";
import * as path from "node:path";
import { execFileSync } from "node:child_process";
import { SCAFFOLD, ROOT } from "./daemon.ts";

/** Grayjay returns dynamic content objects; tests only read a few fields. */
export interface PlatformItem {
	name?: string;
	url?: string;
	videoCount?: number;
	description?: string;
	author?: { name?: string; url?: string };
	contents?: Pager;
	video?: any;
	duration?: number;
	[k: string]: unknown;
}
export interface Pager {
	results: PlatformItem[];
}

/** The subset of the Grayjay Source surface these tests drive. */
export interface PluginSource {
	getHome(continuationToken?: unknown): Pager;
	search(query: string, type: null, order: null, filters: unknown): Pager;
	searchPlaylists(query: string): Pager;
	searchChannels(query: string): Pager;
	getUserPlaylists(): string[];
	getUserSubscriptions(): string[];
	getPlaylist(url: string): PlatformItem;
	getChannel(url: string): PlatformItem;
	getChannelContents(url: string, type: null, order: null, filters: unknown): Pager;
	getChannelPlaylists(url: string): Pager;
	getContentDetails(url: string): PlatformItem;
	isChannelUrl(url: string): boolean;
	isPlaylistUrl(url: string): boolean;
}

interface HttpResponse {
	isOk: boolean;
	code: number;
	body: string;
	headers: Record<string, string>;
}

// Synchronous HTTP via curl, mirroring Grayjay's blocking http.GET contract.
function curl(method: string, url: string, headers?: Record<string, string>): HttpResponse {
	const args = ["-s", "-X", method, "-w", "\n__CODE__%{http_code}", url];
	for (const k of Object.keys(headers || {})) args.push("-H", `${k}: ${headers![k]}`);
	let out: string;
	try {
		out = execFileSync("curl", args, { maxBuffer: 1 << 28 }).toString("utf8");
	} catch (e) {
		return { isOk: false, code: 0, body: String(e), headers: {} };
	}
	const i = out.lastIndexOf("__CODE__");
	const code = parseInt(out.slice(i + "__CODE__".length), 10);
	return { isOk: code >= 200 && code < 300, code, body: out.slice(0, i).replace(/\n$/, ""), headers: {} };
}

/** Load the plugin against `baseUrl` and return its enabled `source` object. */
export function loadSource(baseUrl: string): PluginSource {
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

	const ctx: Record<string, unknown> = {
		console: { log() {}, error() {} },
		bridge: { log() {}, setTimeout, clearTimeout },
		http: {
			GET: (url: string, headers?: Record<string, string>) => curl("GET", url, headers),
			POST: (url: string, _body: unknown, headers?: Record<string, string>) => curl("POST", url, headers),
		},
	};
	vm.createContext(ctx);
	vm.runInContext(code, ctx, { timeout: 60000 });
	return ctx.__pluginSource as PluginSource;
}
