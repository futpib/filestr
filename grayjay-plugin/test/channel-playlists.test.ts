// Port of test-grayjay-channel-playlists.sh — getChannelPlaylists returns lazy
// stubs (name+count, no contents) scoped to a source; getPlaylist resolves one.

import { test } from "node:test";
import assert from "node:assert/strict";
import { Daemon, available, haveFfmpeg } from "./harness/daemon.ts";
import { loadSource } from "./harness/plugin.ts";

const skip = !available()
	? "filestrd/filestrctl/grayjay scaffolding not present"
	: !haveFfmpeg()
		? "ffmpeg not installed"
		: false;

test("channel Playlists tab returns lazy stubs that resolve", { skip }, async () => {
	const a = await Daemon.start("A", { share: true, httpPort: 39204 });
	try {
		a.ffmpegTrack("gh1.mp3", { artist: "Tester", album: "Greatest Hits", title: "Hit One" });
		a.ffmpegTrack("gh2.mp3", { artist: "Tester", album: "Greatest Hits", title: "Hit Two" });
		a.ffmpegTrack("bs1.mp3", { artist: "Tester", album: "B Sides", title: "Rarity" });
		a.rescan();
		await a.waitGateway();
		await a.waitTaggedFiles("Tester", 3);

		const source = loadSource(a.baseUrl());
		const pager = source.getChannelPlaylists(`${a.baseUrl()}/channel/local`);
		const stubs = pager.results.map((p) => ({
			name: p.name,
			count: p.videoCount,
			url: p.url,
			hasContents: !!p.contents,
		}));

		assert.ok(stubs.length >= 3, "expected folder+album+artist stubs");
		assert.ok(stubs.every((s) => s.hasContents === false), "stub carried contents (should be lazy)");
		assert.ok(stubs.every((s) => /\/playlist\//.test(String(s.url))), "stub missing playlist url");
		assert.equal(stubs.filter((s) => s.name === "Tester").length, 1, "no single artist stub");
		assert.equal(stubs.find((s) => s.name === "Tester")?.count, 3, "artist stub count");
		assert.equal(stubs.find((s) => s.name === "Greatest Hits")?.count, 2, "album count");
		assert.equal(stubs.find((s) => s.name === "B Sides")?.count, 1, "B Sides count");

		const art = pager.results.find((p) => p.name === "Tester");
		const det = source.getPlaylist(String(art?.url));
		assert.equal(det.videoCount, 3, "resolved artist playlist count");
		assert.match(String(det.contents?.results[0].url), /\/file\//, "resolved track not playable");
	} finally {
		a.stop();
	}
});
