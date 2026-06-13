// Port of test-grayjay-peer-offline.sh — an unreachable peer stays visible as a
// subscription, its channel is marked Offline, and opening it raises a clear error.

const { test } = require("node:test");
const assert = require("node:assert");
const crypto = require("node:crypto");
const { Daemon, available, waitUntil } = require("./harness/daemon");
const { loadSource } = require("./harness/plugin");

const skip = available() ? false : "filestrd/filestrctl/grayjay scaffolding not present";

test("offline peer is reported, not silently dropped", { skip }, async () => {
	const a = await Daemon.start("A", { share: true });
	const g = await Daemon.start("G", { httpPort: 39207, extraConfig: "[search]\nbrowse_timeout_secs = 2" });
	try {
		a.writeShare("clip.mp4", crypto.randomBytes(131072));
		a.rescan();
		g.peerAdd(a.inviteCreate());
		await g.waitGateway();
		await g.waitPeerFiles(1);
		assert.ok(
			(await g.files()).peers.some((p) => p.reachable === true),
			"peer not reachable while up"
		);

		a.kill();
		await waitUntil("peer reported offline", async () =>
			(await g.files()).peers.some((p) => p.reachable === false)
		);

		const source = loadSource(g.baseUrl());
		const subs = source.getUserSubscriptions();
		assert.ok(subs.length >= 1, "offline peer dropped from subscriptions");
		const desc = source.getChannel(subs[0]).description;
		assert.match(desc, /Offline/, `channel not marked Offline: ${desc}`);

		let threw = false;
		let msg = "";
		try {
			source.getChannelContents(subs[0], null, null, {});
		} catch (e) {
			threw = true;
			msg = String(e);
		}
		assert.ok(threw, "opening an offline channel did not raise an error");
		assert.match(msg, /offline/, `offline channel error not descriptive: ${msg}`);
	} finally {
		g.stop();
		a.stop();
	}
});
