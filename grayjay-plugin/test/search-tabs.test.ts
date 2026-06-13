// Port of test-grayjay-search-tabs.sh — the search screen's Playlists tab
// (searchPlaylists) and Creators tab (searchChannels), plus the /peers endpoint
// creator search uses.

import { test } from "node:test";
import assert from "node:assert/strict";
import { Daemon, available, haveFfmpeg } from "./harness/daemon.ts";
import { loadSource } from "./harness/plugin.ts";

const skip = !available()
	? "filestrd/filestrctl/grayjay scaffolding not present"
	: !haveFfmpeg()
		? "ffmpeg not installed"
		: false;

interface PeersResponse {
	peers: { label: string; node_id: string }[];
}

test("search Playlists and Creators tabs", { skip }, async () => {
	const a = await Daemon.start("A", { share: true, httpPort: 39208 });
	const b = await Daemon.start("B", {});
	try {
		a.ffmpegTrack("gh1.mp3", { artist: "Tester", album: "Greatest Hits", title: "Hit One" });
		a.ffmpegTrack("gh2.mp3", { artist: "Tester", album: "Greatest Hits", title: "Hit Two" });
		a.ffmpegTrack("bs1.mp3", { artist: "Tester", album: "B Sides", title: "Rarity" });
		a.rescan();
		a.peerAdd(b.inviteCreate()); // a peer for /peers + creator search
		await a.waitGateway();
		await a.waitTaggedFiles("Tester", 3);

		// /peers: the granted peer is present without a browse
		const peers = (await (await fetch(`${a.baseUrl()}/peers`)).json()) as PeersResponse;
		assert.ok(peers.peers.length >= 1, "/peers returned no granted peer");
		const peerSub = peers.peers[0].label.slice(0, 6);

		const source = loadSource(a.baseUrl());

		// Playlists tab
		const plArtist = source.searchPlaylists("tester").results.map((p) => ({ name: p.name, count: p.videoCount, url: p.url }));
		const tester = plArtist.find((p) => p.name === "Tester");
		assert.ok(tester, "searchPlaylists(tester) missing artist");
		assert.equal(tester.count, 3, "searchPlaylists artist count");
		assert.match(String(tester.url), /\/playlist\//, "searchPlaylists result has no playlist url");
		assert.ok(
			source.searchPlaylists("greatest").results.some((p) => p.name === "Greatest Hits"),
			"searchPlaylists(greatest) missing album",
		);
		assert.equal(source.searchPlaylists("zzqqxx-nomatch").results.length, 0, "nonsense query not empty");

		// Creators tab
		assert.ok(
			source.searchChannels("this").results.some((c) => c.name === "This node"),
			"searchChannels(this) should match This node",
		);
		assert.ok(source.searchChannels("").results.length >= 2, "searchChannels() should return local + peer");
		assert.equal(source.searchChannels("zzqq-nomatch").results.length, 0, "nonsense query not empty");
		assert.ok(source.searchChannels(peerSub).results.length >= 1, "searchChannels by peer-label missed the peer");
	} finally {
		b.stop();
		a.stop();
	}
});
