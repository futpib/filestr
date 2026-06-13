// Port of test-grayjay-author-link.sh — every emitted item carries an author
// whose url resolves as a channel (the "No source enabled to support this
// channel ()" regression), and tapping it round-trips to a real channel.

import { test } from "node:test";
import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { Daemon, available } from "./harness/daemon.ts";
import { loadSource, type PlatformItem } from "./harness/plugin.ts";

const skip = available() ? false : "filestrd/filestrctl/grayjay scaffolding not present";

test("author links are resolvable channel urls", { skip }, async () => {
	const a = await Daemon.start("A", { share: true, httpPort: 39206 });
	try {
		a.writeShare("clip.mp4", randomBytes(131072));
		a.rescan();
		await a.waitGateway();

		const source = loadSource(a.baseUrl());
		const authorOk = (x: PlatformItem | undefined): boolean =>
			!!(x && x.author && x.author.url && source.isChannelUrl(x.author.url));

		const home = source.getHome().results;
		assert.ok(authorOk(home[0]), "home author url empty/unresolvable (the '()' bug)");
		assert.ok(authorOk(source.getContentDetails(String(home[0].url))), "details author url unresolvable");
		const hits = source.search("clip", null, null, []).results;
		if (hits.length) assert.ok(authorOk(hits[0]), "search hit author url unresolvable");

		// the actual "tap the channel name under a video" round-trip
		const au = String(home[0].author!.url);
		assert.equal(source.isChannelUrl(au), true, "author url not a channel url");
		const ch = source.getChannel(au);
		assert.ok(ch.name && ch.name.length > 0, "getChannel returned no channel");
		assert.equal(source.isChannelUrl(String(ch.url)), true, "resolved channel has no usable url");
		assert.ok(source.getChannelContents(au, null, null, {}).results.length >= 1, "channel has no contents");
	} finally {
		a.stop();
	}
});
