// Port of test-grayjay-album-artist.sh — getUserPlaylists groups by embedded
// album/artist tags (across the library), getPlaylist resolves each.

import { test } from "node:test";
import assert from "node:assert/strict";
import { Daemon, available, haveFfmpeg } from "./harness/daemon.ts";
import { loadSource } from "./harness/plugin.ts";

const skip = !available()
	? "filestrd/filestrctl/grayjay scaffolding not present"
	: !haveFfmpeg()
		? "ffmpeg not installed"
		: false;

test("library groups by album/artist tags", { skip }, async () => {
	const a = await Daemon.start("A", { share: true, httpPort: 39203 });
	try {
		a.ffmpegTrack("gh1.mp3", { artist: "Tester", album: "Greatest Hits", title: "Hit One" });
		a.ffmpegTrack("gh2.mp3", { artist: "Tester", album: "Greatest Hits", title: "Hit Two" });
		a.ffmpegTrack("bs1.mp3", { artist: "Tester", album: "B Sides", title: "Rarity" });
		a.rescan();
		await a.waitGateway();
		await a.waitTaggedFiles("Tester", 3);

		const source = loadSource(a.baseUrl());
		const pls = source.getUserPlaylists().map((u) => {
			const p = source.getPlaylist(u);
			return {
				name: p.name,
				count: p.videoCount,
				isPl: source.isPlaylistUrl(u),
				first: p.contents?.results[0] ? p.contents.results[0].url : null,
			};
		});

		const tester = pls.filter((p) => p.name === "Tester");
		assert.equal(tester.length, 1, "expected one 'Tester' artist playlist");
		assert.equal(tester[0].count, 3, "artist playlist wrong count");
		assert.equal(tester[0].isPl, true, "isPlaylistUrl false for artist");
		assert.match(String(tester[0].first), /\/file\//, "artist track not playable");

		assert.equal(pls.find((p) => p.name === "Greatest Hits")?.count, 2, "Greatest Hits count");
		assert.equal(pls.find((p) => p.name === "B Sides")?.count, 1, "B Sides count");
	} finally {
		a.stop();
	}
});
