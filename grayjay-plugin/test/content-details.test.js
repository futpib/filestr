// Port of test-grayjay-content-details.sh — audio is handed back as an unmuxed
// AudioUrlSource with an authoritative duration (the -12:-55 regression), video
// stays a muxed VideoUrlSource.

const { test } = require("node:test");
const assert = require("node:assert");
const path = require("node:path");
const { Daemon, available, haveFfmpeg, ffmpeg } = require("./harness/daemon");
const { loadSource } = require("./harness/plugin");

const skip = !available()
	? "filestrd/filestrctl/grayjay scaffolding not present"
	: !haveFfmpeg()
		? "ffmpeg not installed"
		: false;

function descOf(details) {
	const d = details.video || {};
	const audio = (d.audioSources || [])[0] || null;
	const video = (d.videoSources || [])[0] || null;
	return {
		duration: details.duration,
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
		const find = (ext) => home.find((v) => String(v.url).indexOf(ext) !== -1);
		const audio = descOf(source.getContentDetails(find("song.mp3").url));
		const video = descOf(source.getContentDetails(find("clip.mp4").url));

		// audio: unmuxed AudioUrlSource carrying a positive duration on both levels
		assert.strictEqual(audio.descType, "UnMuxVideoSourceDescriptor", "audio not unmuxed (the -12:-55 bug)");
		assert.strictEqual(audio.isUnMuxed, true, "audio descriptor not flagged unmuxed");
		assert.strictEqual(audio.nVideo, 0, "audio descriptor has a video source");
		assert.strictEqual(audio.audio.type, "AudioUrlSource", "not an AudioUrlSource");
		assert.match(audio.audio.container, /^audio\//, "audio container not audio/*");
		assert.ok(audio.duration > 1 && audio.duration < 4, `audio details duration ${audio.duration} (~2)`);
		assert.ok(audio.audio.duration > 1 && audio.audio.duration < 4, `audio source duration ${audio.audio.duration} (~2)`);
		assert.strictEqual(audio.audio.duration, audio.duration, "audio source/details duration mismatch");

		// video: a normal muxed VideoUrlSource
		assert.strictEqual(video.descType, "MuxVideoSourceDescriptor", "video not a mux descriptor");
		assert.strictEqual(video.video.type, "VideoUrlSource", "not a VideoUrlSource");
		assert.ok(video.video.duration > 2 && video.video.duration < 4, `video duration ${video.video.duration} (~3)`);
	} finally {
		a.stop();
	}
});
