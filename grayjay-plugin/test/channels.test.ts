// Port of test-grayjay-channels.sh — peers map to channels: getUserSubscriptions
// lists them, getChannel/getChannelContents browse a peer's library.

import { test } from "node:test";
import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { Daemon, available } from "./harness/daemon.ts";
import { loadSource } from "./harness/plugin.ts";

const skip = available() ? false : "filestrd/filestrctl/grayjay scaffolding not present";

test("peers surface as channels with browsable contents", { skip }, async () => {
	const a = await Daemon.start("A", { share: true });
	const g = await Daemon.start("G", { httpPort: 39201 });
	try {
		a.writeShare("song.mp3", randomBytes(131072));
		a.writeShare("clip.mp4", randomBytes(131072));
		a.rescan();
		g.peerAdd(a.inviteCreate());
		await g.waitGateway();
		await g.waitPeerFiles(2);

		const source = loadSource(g.baseUrl());
		const subs = source.getUserSubscriptions();
		assert.ok(subs.length >= 1, "no peer channels");
		assert.equal(source.isChannelUrl(subs[0]), true, "isChannelUrl false for a channel url");

		const ch = source.getChannel(subs[0]);
		assert.ok(ch.name && ch.name.length > 0, "getChannel returned no name");

		const contents = source.getChannelContents(subs[0], null, null, {});
		assert.equal(contents.results.length, 2, "wrong channel content count");
		assert.match(String(contents.results[0].url), /\/file\//, "channel content not playable");
	} finally {
		g.stop();
		a.stop();
	}
});
