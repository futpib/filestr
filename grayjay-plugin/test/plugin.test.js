// Port of test-grayjay-plugin.sh — getHome lists only playable media (hides the
// .txt), the type filter partitions search, and stream URLs serve correct bytes.

const { test } = require("node:test");
const assert = require("node:assert");
const crypto = require("node:crypto");
const { Daemon, available } = require("./harness/daemon");
const { loadSource } = require("./harness/plugin");

const skip = available() ? false : "filestrd/filestrctl/grayjay scaffolding not present";

test("getHome hides non-media; type filter partitions search; streams serve bytes", { skip }, async () => {
	const a = await Daemon.start("A", { share: true, httpPort: 39200 });
	try {
		const song = crypto.randomBytes(131072);
		const movie = crypto.randomBytes(131072);
		a.writeShare("song.mp3", song);
		a.writeShare("movie.mp4", movie);
		a.writeShare("readme.txt", Buffer.from("not playable by a media player\n"));
		a.rescan();
		await a.waitGateway();

		const source = loadSource(a.baseUrl());

		const home = source.getHome();
		const names = home.results.map((v) => v.name);
		assert.strictEqual(home.results.length, 2, `only the 2 media files, got ${names}`);
		assert.ok(!names.some((n) => n.includes("readme")), "non-media file must be hidden");

		// type filter: audio-only + video-only == unfiltered, for the same query
		const term = "mp"; // matches song.mp3 / movie.mp4 filenames
		const all = source.search(term, null, null, {}).results.length;
		const audio = source.search(term, null, null, { mime: ["audio"] }).results.length;
		const video = source.search(term, null, null, { mime: ["video"] }).results.length;
		assert.strictEqual(audio + video, all, "audio+video partitions should sum to all");

		// every home item hands back a /file/ stream url that serves the real bytes
		for (const item of home.results) {
			assert.match(item.url, /\/file\//, "home item not playable");
			const got = Buffer.from(await (await fetch(item.url)).arrayBuffer());
			const want = item.name.includes("song") ? song : movie;
			assert.ok(got.equals(want), `streamed bytes for ${item.name} differ`);
		}
	} finally {
		a.stop();
	}
});
