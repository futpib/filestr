//! Best-effort media metadata extraction at index time: duration and the
//! common tags (title/artist/album). Audio goes through `symphonia` (pure
//! Rust, no system deps); mp4-family video gets its duration from the `mp4`
//! container header. Everything is best-effort — any failure yields empty
//! metadata, never an error that would fail a scan.

use std::path::Path;

use libfilestr::ctl::MediaMeta;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTag, StandardVisualKey};
use symphonia::core::units::Timestamp;

/// Result of probing a file: the tag/duration metadata plus, for audio, the
/// embedded cover image bytes (if any), which the caller caches as a thumbnail.
#[derive(Default)]
pub struct Probed {
    pub meta: MediaMeta,
    pub cover: Option<Vec<u8>>,
}

/// Extract metadata (and cover art) for `path`. The content type is sniffed
/// from the file's magic bytes (so a misnamed/extensionless file is still
/// recognised), and that drives which extractor runs. Never fails: an
/// unreadable or unsupported file just yields empty results.
pub fn probe(path: &Path) -> Probed {
    let content_type = sniff_content_type(path);
    let mut probed = match content_type.as_deref() {
        // audio containers + Matroska/WebM go through symphonia (duration, tags,
        // cover art; its mkv demuxer handles webm/mkv duration too)
        Some(t)
            if t.starts_with("audio/") || t == "video/x-matroska" || t == "video/webm" =>
        {
            probe_symphonia(path).unwrap_or_default()
        }
        // mp4-family video: duration from the container header
        Some("video/mp4") | Some("video/quicktime") => {
            Probed { meta: probe_mp4(path).unwrap_or_default(), cover: None }
        }
        _ => Probed::default(),
    };
    probed.meta.content_type = content_type;
    probed
}

/// Identify a file's container by its magic bytes (reads only the head). Returns
/// a MIME type for the formats we care about, else None.
fn sniff_content_type(path: &Path) -> Option<String> {
    use std::io::Read;
    // Files whose extension makes clear they aren't media are never sniffed: a
    // UTF-16 sidecar (EAC/CUETools logs, some .m3u) starts with the BOM 0xFF 0xFE,
    // which trips the raw MPEG frame-sync bits and used to be mis-tagged
    // audio/mpeg (surfacing logs/playlists as "tracks"). A genuinely misnamed
    // media file with one of these extensions is vanishingly rare; getting the
    // common log/playlist/text case right wins.
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if matches!(
            ext.to_ascii_lowercase().as_str(),
            "log" | "txt" | "nfo" | "cue" | "m3u" | "m3u8" | "pls" | "md" | "json"
                | "xml" | "ini" | "sfv" | "accurip" | "ffp"
        ) {
            return None;
        }
    }
    let mut head = [0u8; 64];
    let n = std::fs::File::open(path).ok()?.read(&mut head).ok()?;
    let b = &head[..n];
    let ct = if b.starts_with(b"ID3") || is_mpeg_frame(b) {
        "audio/mpeg"
    } else if b.starts_with(b"fLaC") {
        "audio/flac"
    } else if b.starts_with(b"OggS") {
        "audio/ogg"
    } else if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WAVE" {
        "audio/wav"
    } else if b.len() >= 12 && &b[0..4] == b"FORM" && &b[8..12] == b"AIFF" {
        "audio/aiff"
    } else if b.starts_with(b"\x1aE\xdf\xa3") {
        // EBML: Matroska or WebM. The DocType ("webm"/"matroska") sits a few
        // bytes in; scan the head for it.
        if b.windows(4).any(|w| w == b"webm") {
            "video/webm"
        } else {
            "video/x-matroska"
        }
    } else if b.len() >= 12 && &b[4..8] == b"ftyp" {
        // ISO-BMFF: the major brand at [8..12] tells audio (M4A) from video.
        match &b[8..11] {
            b"M4A" | b"M4B" => "audio/mp4",
            b"qt " => "video/quicktime",
            _ => "video/mp4",
        }
    } else if b.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if b.starts_with(b"GIF8") {
        "image/gif"
    } else if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        "image/webp"
    } else {
        return None;
    };
    Some(ct.to_string())
}

/// Whether the head looks like a real MPEG audio frame, not just the 11 sync
/// bits. Validating the version/layer/bitrate/sample-rate fields rejects
/// non-MP3 byte runs (e.g. a UTF-16 BOM 0xFF 0xFE) that share the sync bits.
fn is_mpeg_frame(b: &[u8]) -> bool {
    if b.len() < 3 || b[0] != 0xFF || (b[1] & 0xE0) != 0xE0 {
        return false;
    }
    let version = (b[1] >> 3) & 0x3; // 01 = reserved
    let layer = (b[1] >> 1) & 0x3; // 00 = reserved
    let bitrate = (b[2] >> 4) & 0xF; // 0000 = free, 1111 = bad
    let samplerate = (b[2] >> 2) & 0x3; // 11 = reserved
    version != 0x1 && layer != 0x0 && bitrate != 0xF && samplerate != 0x3
}

fn probe_symphonia(path: &Path) -> Option<Probed> {
    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut reader = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .ok()?;

    let mut meta = MediaMeta::default();

    // Duration from the default audio track, or — for video-only mkv/webm —
    // any track that states one. Prefer the container's stated duration (in
    // timebase units); fall back to the playable frame count. Some streams
    // (e.g. CBR MP3 without a Xing header) report neither, leaving it unset.
    let dur_track = reader.default_track(TrackType::Audio).or_else(|| {
        reader
            .tracks()
            .iter()
            .find(|t| t.time_base.is_some() && (t.duration.is_some() || t.num_frames.is_some()))
    });
    if let Some(track) = dur_track {
        let ticks = track.duration.map(|d| d.get()).or(track.num_frames);
        if let (Some(tb), Some(ticks)) = (track.time_base, ticks) {
            if let Some(time) = tb.calc_time(Timestamp::from(ticks as i64)) {
                let secs = time.as_nanos() as f64 / 1e9;
                if secs > 0.0 {
                    meta.duration_secs = Some(secs);
                }
            }
        }
    }

    // Media-level tags (ID3v2, Vorbis comments, mp4 ilst, …) and visuals
    // (embedded cover art). The probe appends any leading/trailing metadata to
    // the reader's log, so this is the one place to read them.
    let mut cover: Option<Vec<u8>> = None;
    let md = reader.metadata();
    if let Some(rev) = md.current() {
        for tag in &rev.media.tags {
            match &tag.std {
                Some(StandardTag::TrackTitle(s)) if meta.title.is_none() => {
                    meta.title = Some(s.to_string());
                }
                Some(StandardTag::Artist(s)) if meta.artist.is_none() => {
                    meta.artist = Some(s.to_string());
                }
                Some(StandardTag::AlbumArtist(s)) if meta.artist.is_none() => {
                    meta.artist = Some(s.to_string());
                }
                Some(StandardTag::Album(s)) if meta.album.is_none() => {
                    meta.album = Some(s.to_string());
                }
                _ => {}
            }
        }
        cover = pick_cover(&rev.media.visuals);
    }

    Some(Probed { meta, cover })
}

/// Choose the best embedded image to use as a thumbnail: a front cover if one
/// is tagged as such, otherwise the largest image (most likely the artwork).
fn pick_cover(visuals: &[symphonia::core::meta::Visual]) -> Option<Vec<u8>> {
    if visuals.is_empty() {
        return None;
    }
    let front = visuals
        .iter()
        .find(|v| v.usage == Some(StandardVisualKey::FrontCover));
    let chosen = front.or_else(|| visuals.iter().max_by_key(|v| v.data.len()))?;
    if chosen.data.is_empty() {
        return None;
    }
    Some(chosen.data.to_vec())
}

fn probe_mp4(path: &Path) -> Option<MediaMeta> {
    let file = std::fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let mp4 = mp4::Mp4Reader::read_header(std::io::BufReader::new(file), size).ok()?;
    let secs = mp4.duration().as_secs_f64();
    let mut meta = MediaMeta::default();
    if secs > 0.0 {
        meta.duration_secs = Some(secs);
    }
    Some(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mpeg_frame_rejects_utf16_bom() {
        // UTF-16 LE BOM shares the 11 sync bits but isn't a valid frame header.
        assert!(!is_mpeg_frame(&[0xFF, 0xFE, 0xFF, 0xFF]));
        // a real MPEG1 Layer III frame (128 kbps, 44.1 kHz) is accepted
        assert!(is_mpeg_frame(&[0xFF, 0xFB, 0x90, 0x00]));
        // reserved sample-rate / bad bitrate are rejected
        assert!(!is_mpeg_frame(&[0xFF, 0xFB, 0xFC, 0x00]));
    }

    #[test]
    fn sidecar_extensions_are_not_media() {
        // A UTF-16 log (EAC/auCDtect): magic bytes look MP3-ish, extension says no.
        let dir = std::env::temp_dir().join(format!("filestr-sniff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("rushchk.log");
        std::fs::write(&log, [0xFFu8, 0xFE, b'h', 0, b'i', 0]).unwrap();
        assert_eq!(sniff_content_type(&log), None, "a .log must not sniff as media");
        // a real PNG (non-media extension not in the deny list) is still detected
        let png = dir.join("cover.png");
        std::fs::write(&png, [0x89u8, b'P', b'N', b'G', b'\r', b'\n']).unwrap();
        assert_eq!(sniff_content_type(&png).as_deref(), Some("image/png"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
