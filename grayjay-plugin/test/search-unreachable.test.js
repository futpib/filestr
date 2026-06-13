// Port of test-grayjay-search-unreachable.sh — a federated search must not hang
// on a dead peer; it returns the local hit promptly (well within the old 10s
// connect hang).

const { test } = require("node:test");
const assert = require("node:assert");
const crypto = require("node:crypto");
const { Daemon, available, waitUntil } = require("./harness/daemon");
const { loadSource } = require("./harness/plugin");

const skip = available() ? false : "filestrd/filestrctl/grayjay scaffolding not present";

test("search returns promptly with a dead granted peer", { skip }, async () => {
	const a = await Daemon.start("A", { share: true }); // soon unreachable
	const g = await Daemon.start("G", {
		share: true,
		httpPort: 39209,
		extraConfig: "[search]\nconnect_timeout_secs = 1\ntimeout_secs = 8",
	});
	try {
		a.writeShare("unrelated.mp4", crypto.randomBytes(131072));
		a.rescan();
		g.writeShare("tekwars-clip.mp4", crypto.randomBytes(131072));
		g.rescan();
		g.peerAdd(a.inviteCreate());
		await g.waitGateway();
		await waitUntil("G indexed its own file", async () =>
			(await g.files()).files.some((f) => f.source === "local")
		);

		a.kill(); // grant now points at a dead peer

		const source = loadSource(g.baseUrl());
		const t0 = Date.now();
		const r = source.search("tekwars", null, null, {});
		const elapsed = Date.now() - t0;
		const names = r.results.map((v) => v.name);

		assert.ok(elapsed < 6000, `search took ${elapsed}ms — a dead peer is stalling it`);
		assert.ok(r.results.length >= 1, `no results (local hit lost): ${names}`);
		assert.ok(names.some((n) => /tekwars/.test(n)), `matching local file missing: ${names}`);
	} finally {
		g.stop();
		a.stop();
	}
});
