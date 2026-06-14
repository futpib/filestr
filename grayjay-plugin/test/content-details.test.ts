// Port of test-grayjay-content-details.sh — audio is handed back as an unmuxed
// AudioUrlSource with an authoritative duration (the -12:-55 regression), video
// stays a muxed VideoUrlSource.

import { test } from "node:test";
import assert from "node:assert/strict";
import * as path from "node:path";
import * as fs from "node:fs";
import { Daemon, available, haveFfmpeg, ffmpeg, waitUntil } from "./harness/daemon.ts";
import { loadSource, type PlatformItem } from "./harness/plugin.ts";

const skip = !available()
	? "filestrd/filestrctl/grayjay scaffolding not present"
	: !haveFfmpeg()
		? "ffmpeg not installed"
		: false;

interface SourceDump {
	duration: number;
	descType: string;
	isUnMuxed: boolean;
	nVideo: number;
	audio: { type: string; container: string; duration: number } | null;
	video: { type: string; container: string; duration: number } | null;
}

function descOf(details: PlatformItem): SourceDump {
	const d = details.video || {};
	const audio = (d.audioSources || [])[0] || null;
	const video = (d.videoSources || [])[0] || null;
	return {
		duration: details.duration as number,
		descType: d.plugin_type,
		isUnMuxed: d.isUnMuxed,
		nVideo: (d.videoSources || []).length,
		audio: audio && { type: audio.plugin_type, container: audio.container, duration: audio.duration },
		video: video && { type: video.plugin_type, container: video.container, duration: video.duration },
	};
}

test("audio is unmuxed with a real duration; video is muxed", { skip }, async () => {
	const a = await Daemon.start("A", { share: true, httpPort: 39205 });
	try {
		const sd = a.shareDir();
		ffmpeg(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=2", "-write_xing", "1", path.join(sd, "song.mp3")]);
		ffmpeg(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=duration=3:size=320x240:rate=10", "-pix_fmt", "yuv420p", path.join(sd, "clip.mp4")]);
		a.rescan();
		await a.waitGateway();

		const source = loadSource(a.baseUrl());
		const home = source.getHome().results;
		const find = (ext: string): PlatformItem => home.find((v) => String(v.url).indexOf(ext) !== -1)!;
		const audio = descOf(source.getContentDetails(String(find("song.mp3").url)));
		const video = descOf(source.getContentDetails(String(find("clip.mp4").url)));

		// audio: unmuxed AudioUrlSource carrying a positive duration on both levels
		assert.equal(audio.descType, "UnMuxVideoSourceDescriptor", "audio not unmuxed (the -12:-55 bug)");
		assert.equal(audio.isUnMuxed, true, "audio descriptor not flagged unmuxed");
		assert.equal(audio.nVideo, 0, "audio descriptor has a video source");
		assert.equal(audio.audio!.type, "AudioUrlSource", "not an AudioUrlSource");
		assert.match(audio.audio!.container, /^audio\//, "audio container not audio/*");
		assert.ok(audio.duration > 1 && audio.duration < 4, `audio details duration ${audio.duration} (~2)`);
		assert.ok(audio.audio!.duration > 1 && audio.audio!.duration < 4, `audio source duration ${audio.audio!.duration} (~2)`);
		assert.equal(audio.audio!.duration, audio.duration, "audio source/details duration mismatch");

		// video: a normal muxed VideoUrlSource
		assert.equal(video.descType, "MuxVideoSourceDescriptor", "video not a mux descriptor");
		assert.equal(video.video!.type, "VideoUrlSource", "not a VideoUrlSource");
		assert.ok(video.video!.duration > 2 && video.video!.duration < 4, `video duration ${video.video!.duration} (~3)`);
	} finally {
		a.stop();
	}
});

// The crux of the "-12:-55" fix: getContentDetails must recover the duration
// from the URL it's handed, NOT by re-downloading the whole library to find the
// row. We prove it by deleting the file so every lookup (/files, /search) misses
// — the duration must survive purely from the metadata embedded in the URL.
test("content detail keeps duration from the URL with no library lookup", { skip }, async () => {
	const a = await Daemon.start("A", { share: true, httpPort: 39215 });
	try {
		const sd = a.shareDir();
		ffmpeg(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=2", "-write_xing", "1", path.join(sd, "lonely.mp3")]);
		a.rescan();
		await a.waitGateway();

		const source = loadSource(a.baseUrl());
		const url = String(source.getHome().results.find((v) => String(v.url).indexOf("lonely.mp3") !== -1)!.url);
		assert.match(url, /[?&]m=/, "content URL is missing the embedded metadata blob");

		// remove the file so a library lookup can no longer resolve it
		fs.unlinkSync(path.join(sd, "lonely.mp3"));
		a.rescan();
		await waitUntil("file gone from the gateway", async () =>
			!(await a.files()).files.some((f) => String(f.name).endsWith("lonely.mp3")),
		);

		// duration is still correct — recovered from the URL alone, no fetch
		const d = source.getContentDetails(url);
		const dur = d.duration as number;
		assert.ok(dur > 1 && dur < 4, `duration lost without a library lookup: ${dur} (the -12:-55 path)`);
		const audioDur = (d.video?.audioSources?.[0]?.duration) as number;
		assert.equal(audioDur, dur, "audio source/details duration mismatch from the embedded URL");
	} finally {
		a.stop();
	}
});
