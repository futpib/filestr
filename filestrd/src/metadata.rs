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

/// Extract metadata (and cover art) for `path`, dispatched by extension. Never
/// fails: an unreadable or unsupported file just yields empty results.
pub fn probe(path: &Path) -> Probed {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "mp3" | "flac" | "ogg" | "oga" | "opus" | "wav" | "m4a" | "aac" | "aiff" | "alac" => {
            probe_audio(path).unwrap_or_default()
        }
        "mp4" | "m4v" | "mov" => Probed {
            meta: probe_mp4(path).unwrap_or_default(),
            cover: None,
        },
        _ => Probed::default(),
    }
}

fn probe_audio(path: &Path) -> Option<Probed> {
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

    // Duration from the default audio track. Prefer the container's stated
    // duration (in timebase units); fall back to the playable frame count.
    // Some streams (e.g. CBR MP3 without a Xing header) report neither — then
    // we leave the duration unset rather than guess.
    if let Some(track) = reader.default_track(TrackType::Audio) {
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
