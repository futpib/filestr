// e2e harness: spawn a real filestrd, drive it over filestrctl + the HTTP
// gateway. The node:test port of scripts/autotests/lib.sh's daemon management
// (relay disabled, hermetic on localhost). Readable setup with condition-polling
// instead of bash `sleep`s.

import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { spawn, spawnSync, type ChildProcess } from "node:child_process";

const HERE = path.dirname(fileURLToPath(import.meta.url));
export const ROOT = path.resolve(HERE, "..", "..", "..");
const BIN = path.join(ROOT, "target", "debug");
const FILESTRD = path.join(BIN, "filestrd");
const FILESTRCTL = path.join(BIN, "filestrctl");
export const SCAFFOLD =
	process.env.GRAYJAY_SCRIPTS ||
	path.join(ROOT, "..", "grayjay-android", "app", "src", "main", "assets", "scripts");

/** Whether the prerequisites for the plugin tests are present; otherwise skip. */
export function available(): boolean {
	return (
		fs.existsSync(FILESTRD) &&
		fs.existsSync(FILESTRCTL) &&
		fs.existsSync(path.join(SCAFFOLD, "source.js"))
	);
}

export const sleep = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms));

/** Poll `cond` (sync or async) until truthy or the timeout elapses, then throw. */
export async function waitUntil(
	what: string,
	cond: () => boolean | Promise<boolean>,
	timeoutMs = 20000,
	stepMs = 100,
): Promise<void> {
	const deadline = Date.now() + timeoutMs;
	for (;;) {
		if (await cond()) return;
		if (Date.now() >= deadline) throw new Error(`timed out waiting for: ${what}`);
		await sleep(stepMs);
	}
}

export interface NodeOpts {
	share?: boolean;
	httpPort?: number;
	extraConfig?: string;
}

interface FilesResponse {
	files: { name: string; hash: string; size: number; source: string; media?: { artist?: string } }[];
	peers?: { label: string; node_id: string; reachable: boolean }[];
}

export class Daemon {
	readonly name: string;
	readonly dir: string;
	private readonly socket: string;
	private httpPort: number | null = null;
	private proc: ChildProcess | null = null;

	private constructor(name: string) {
		this.name = name;
		this.dir = fs.mkdtempSync(path.join(os.tmpdir(), `filestr-node-${name}-`));
		this.socket = path.join(this.dir, "ctl.sock");
	}

	/** Spawn a node and wait until its control socket answers and the scan settles. */
	static async start(name: string, opts: NodeOpts = {}): Promise<Daemon> {
		const d = new Daemon(name);
		fs.mkdirSync(path.join(d.dir, "data"), { recursive: true });
		d.httpPort = opts.httpPort ?? null;

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
			return s != null && s.indexing == null;
		});
		return d;
	}

	shareDir(): string {
		return path.join(this.dir, "share");
	}

	baseUrl(): string {
		if (!this.httpPort) throw new Error(`daemon ${this.name} has no http gateway`);
		return `http://127.0.0.1:${this.httpPort}`;
	}

	/** Run filestrctl; returns trimmed stdout (throws on non-zero unless allowFail). */
	ctl(args: string[], allowFail = false): string {
		const r = spawnSync(FILESTRCTL, ["--socket", this.socket, ...args], {
			encoding: "utf8",
			maxBuffer: 1 << 28,
		});
		if (r.status !== 0 && !allowFail) {
			throw new Error(`filestrctl ${args.join(" ")} failed: ${r.stderr || r.stdout}`);
		}
		return (r.stdout || "").trim();
	}

	ctlJson<T = any>(args: string[]): T {
		return JSON.parse(this.ctl(["--json", ...args])) as T;
	}

	private tryStatus(): { indexing?: unknown } | null {
		try {
			return this.ctlJson(["status"]);
		} catch {
			return null;
		}
	}

	nodeId(): string {
		return this.ctlJson<{ endpoint_id: string }>(["status"]).endpoint_id;
	}

	rescan(): void {
		this.ctl(["rescan"]);
	}

	inviteCreate(label?: string): string {
		const args = ["invite", "create"];
		if (label) args.push("--label", label);
		// the ticket is the last non-empty line of stdout
		return this.ctl(args).split("\n").filter(Boolean).pop() as string;
	}

	peerAdd(ticket: string): void {
		this.ctl(["peer", "add", ticket]);
	}

	writeShare(rel: string, buf: Buffer | string): string {
		const p = path.join(this.shareDir(), rel);
		fs.mkdirSync(path.dirname(p), { recursive: true });
		fs.writeFileSync(p, buf);
		return p;
	}

	/** Generate a tagged audio fixture with ffmpeg (1s sine). */
	ffmpegTrack(rel: string, tags: { artist?: string; album?: string; title?: string } = {}): void {
		const out = path.join(this.shareDir(), rel);
		fs.mkdirSync(path.dirname(out), { recursive: true });
		const args = ["-v", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=1"];
		if (tags.artist) args.push("-metadata", `artist=${tags.artist}`);
		if (tags.album) args.push("-metadata", `album=${tags.album}`);
		if (tags.title) args.push("-metadata", `title=${tags.title}`);
		args.push("-write_xing", "1", out);
		ffmpeg(args);
	}

	async waitGateway(): Promise<void> {
		await waitUntil("http gateway", async () => {
			try {
				return (await fetch(`${this.baseUrl()}/files`)).ok;
			} catch {
				return false;
			}
		});
	}

	async files(): Promise<FilesResponse> {
		return (await fetch(`${this.baseUrl()}/files`)).json() as Promise<FilesResponse>;
	}

	/** Poll until `/files` reports >= n peer-sourced (non-local) files. */
	async waitPeerFiles(n: number): Promise<void> {
		await waitUntil(`${n} peer files`, async () => {
			const f = await this.files();
			return f.files.filter((x) => x.source !== "local").length >= n;
		});
	}

	/** Poll until `/files` reports `n` files tagged with the given artist. */
	async waitTaggedFiles(artist: string, n: number): Promise<void> {
		await waitUntil(`${n} ${artist} tracks`, async () => {
			const f = await this.files();
			return f.files.filter((x) => x.media?.artist === artist).length === n;
		});
	}

	kill(): void {
		if (this.proc?.pid) {
			try {
				process.kill(this.proc.pid, "SIGKILL");
			} catch {
				/* already gone */
			}
		}
	}

	stop(): void {
		this.kill();
		try {
			fs.rmSync(this.dir, { recursive: true, force: true });
		} catch {
			/* best effort */
		}
	}
}

export function ffmpeg(args: string[]): void {
	const r = spawnSync("ffmpeg", args, { stdio: "ignore" });
	if (r.status !== 0) throw new Error(`ffmpeg failed: ${args.join(" ")}`);
}

export function haveFfmpeg(): boolean {
	return spawnSync("ffmpeg", ["-version"], { stdio: "ignore" }).status === 0;
}
