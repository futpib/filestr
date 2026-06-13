// e2e harness: spawn a real filestrd, drive it over filestrctl + the HTTP
// gateway. The node:test port of scripts/autotests/lib.sh's daemon management
// (relay disabled, hermetic on localhost). Readable setup with condition-polling
// instead of bash `sleep`s.

const fs = require("fs");
const os = require("os");
const path = require("path");
const { spawnSync, spawn, execFileSync } = require("child_process");

const ROOT = path.resolve(__dirname, "..", "..", "..");
const BIN = path.join(ROOT, "target", "debug");
const FILESTRD = path.join(BIN, "filestrd");
const FILESTRCTL = path.join(BIN, "filestrctl");
const SCAFFOLD =
	process.env.GRAYJAY_SCRIPTS ||
	path.join(ROOT, "..", "grayjay-android", "app", "src", "main", "assets", "scripts");

/// Whether the prerequisites for the plugin tests are present; otherwise skip.
function available() {
	return (
		fs.existsSync(FILESTRD) &&
		fs.existsSync(FILESTRCTL) &&
		fs.existsSync(path.join(SCAFFOLD, "source.js"))
	);
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/// Poll `cond` (sync or async) until truthy or the timeout elapses, then throw.
async function waitUntil(what, cond, timeoutMs = 20000, stepMs = 100) {
	const deadline = Date.now() + timeoutMs;
	for (;;) {
		if (await cond()) return;
		if (Date.now() >= deadline) throw new Error(`timed out waiting for: ${what}`);
		await sleep(stepMs);
	}
}

class Daemon {
	constructor(name) {
		this.name = name;
		this.dir = fs.mkdtempSync(path.join(os.tmpdir(), `filestr-node-${name}-`));
		this.socket = path.join(this.dir, "ctl.sock");
		this.httpPort = null;
		this.proc = null;
	}

	/// start(name, {share, httpPort, extraConfig})
	static async start(name, opts = {}) {
		const d = new Daemon(name);
		fs.mkdirSync(path.join(d.dir, "data"), { recursive: true });
		d.httpPort = opts.httpPort || null;

		let config =
			`socket = "${d.socket}"\n` +
			`data_dir = "${path.join(d.dir, "data")}"\n` +
			`relay = "disabled"\n`;
		if (opts.share) {
			fs.mkdirSync(d.shareDir(), { recursive: true });
			config += `[[share]]\nname = "files"\npath = "${d.shareDir()}"\n`;
		}
		if (d.httpPort) config += `[http]\nlisten = "127.0.0.1:${d.httpPort}"\n`;
		if (opts.extraConfig) config += opts.extraConfig + "\n";
		fs.writeFileSync(path.join(d.dir, "config.toml"), config);

		const log = fs.openSync(path.join(d.dir, "daemon.log"), "w");
		d.proc = spawn(FILESTRD, ["--config", path.join(d.dir, "config.toml"), "-vv"], {
			stdio: ["ignore", log, log],
		});
		await waitUntil(`daemon ${name} ready`, () => {
			const s = d.tryStatus();
			return s && s.indexing == null;
		});
		return d;
	}

	shareDir() {
		return path.join(this.dir, "share");
	}

	baseUrl() {
		if (!this.httpPort) throw new Error(`daemon ${this.name} has no http gateway`);
		return `http://127.0.0.1:${this.httpPort}`;
	}

	/// Run filestrctl; returns trimmed stdout (throws on non-zero unless allowFail).
	ctl(args, allowFail = false) {
		const r = spawnSync(FILESTRCTL, ["--socket", this.socket, ...args], {
			encoding: "utf8",
			maxBuffer: 1 << 28,
		});
		if (r.status !== 0 && !allowFail) {
			throw new Error(`filestrctl ${args.join(" ")} failed: ${r.stderr || r.stdout}`);
		}
		return (r.stdout || "").trim();
	}

	ctlJson(args) {
		return JSON.parse(this.ctl(["--json", ...args]));
	}

	tryStatus() {
		try {
			return this.ctlJson(["status"]);
		} catch {
			return null;
		}
	}

	nodeId() {
		return this.ctlJson(["status"]).endpoint_id;
	}

	rescan() {
		this.ctl(["rescan"]);
	}

	inviteCreate(label) {
		const args = ["invite", "create"];
		if (label) args.push("--label", label);
		// the ticket is the last non-empty line of stdout
		return this.ctl(args).split("\n").filter(Boolean).pop();
	}

	peerAdd(ticket) {
		this.ctl(["peer", "add", ticket]);
	}

	writeShare(rel, buf) {
		const p = path.join(this.shareDir(), rel);
		fs.mkdirSync(path.dirname(p), { recursive: true });
		fs.writeFileSync(p, buf);
		return p;
	}

	/// Generate a tagged audio fixture with ffmpeg (1s sine), if available.
	ffmpegTrack(rel, { artist, album, title } = {}) {
		const out = path.join(this.shareDir(), rel);
		fs.mkdirSync(path.dirname(out), { recursive: true });
		const args = ["-v", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=1"];
		if (artist) args.push("-metadata", `artist=${artist}`);
		if (album) args.push("-metadata", `album=${album}`);
		if (title) args.push("-metadata", `title=${title}`);
		args.push("-write_xing", "1", out);
		ffmpeg(args);
	}

	async waitGateway() {
		await waitUntil("http gateway", async () => {
			try {
				const r = await fetch(`${this.baseUrl()}/files`);
				return r.ok;
			} catch {
				return false;
			}
		});
	}

	async files() {
		return (await fetch(`${this.baseUrl()}/files`)).json();
	}

	/// Poll until `/files` reports >= n peer-sourced (non-local) files.
	async waitPeerFiles(n) {
		await waitUntil(`${n} peer files`, async () => {
			const f = await this.files();
			return (f.files || []).filter((x) => x.source !== "local").length >= n;
		});
	}

	/// Poll until `/files` reports `n` files tagged with the given artist.
	async waitTaggedFiles(artist, n) {
		await waitUntil(`${n} ${artist} tracks`, async () => {
			const f = await this.files();
			return (f.files || []).filter((x) => x.media && x.media.artist === artist).length === n;
		});
	}

	kill() {
		try {
			process.kill(this.proc.pid, "SIGKILL");
		} catch {}
	}

	stop() {
		this.kill();
		try {
			fs.rmSync(this.dir, { recursive: true, force: true });
		} catch {}
	}
}

function ffmpeg(args) {
	const r = spawnSync("ffmpeg", args, { stdio: "ignore" });
	if (r.status !== 0) throw new Error(`ffmpeg failed: ${args.join(" ")}`);
}

function haveFfmpeg() {
	return spawnSync("ffmpeg", ["-version"], { stdio: "ignore" }).status === 0;
}

module.exports = { Daemon, available, haveFfmpeg, ffmpeg, waitUntil, sleep, SCAFFOLD, ROOT };
