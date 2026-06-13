// Port of test-grayjay-playlists.sh — shared folders map to playlists:
// getUserPlaylists lists them, getPlaylist resolves one to its files.

const { test } = require("node:test");
const assert = require("node:assert");
const crypto = require("node:crypto");
const { Daemon, available } = require("./harness/daemon");
const { loadSource } = require("./harness/plugin");

const skip = available() ? false : "filestrd/filestrctl/grayjay scaffolding not present";

test("folders surface as playlists resolvable to their tracks", { skip }, async () => {
	const a = await Daemon.start("A", { share: true, httpPort: 39202 });
	try {
		for (const i of [1, 2, 3]) a.writeShare(`album/track${i}.mp3`, crypto.randomBytes(65536));
		a.rescan();
		await a.waitGateway();

		const source = loadSource(a.baseUrl());
		const pls = source.getUserPlaylists().map((u) => {
			const p = source.getPlaylist(u);
			return {
				name: p.name,
				count: p.videoCount,
				isPl: source.isPlaylistUrl(u),
				first: p.contents && p.contents.results[0] ? p.contents.results[0].url : null,
			};
		});
		const album = pls.find((p) => p.name === "album");
		assert.ok(album, `no 'album' playlist in ${JSON.stringify(pls)}`);
		assert.strictEqual(album.count, 3, "album playlist wrong count");
		assert.strictEqual(album.isPl, true, "isPlaylistUrl false");
		assert.match(album.first, /\/file\//, "playlist item not playable");
	} finally {
		a.stop();
	}
});
