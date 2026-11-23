//! Local music: songs from this computer, played straight from their files — nothing
//! is uploaded, and a FLAC plays as the FLAC it is (Library › Uploads › Local music).
//!
//! `local.json` in the data dir lists them in the order they were added. A file that
//! was moved or deleted keeps its place: like a download on a drive that's away, it
//! comes back when the file does, and until then playing it just fails.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use sha1::Digest;

use crate::model::Thumb;
use crate::model::Track;
use crate::stream::{Format, Shared};

/// The id every local song plays under, with the file's digest after it.
pub const PREFIX: &str = "local:";

/// The URI scheme a local song's cover loads by (the image loaders in `images.rs` read
/// it): the rest is a file name in the data dir's `local-covers`, written when the
/// cover was found.
pub const COVER: &str = "local-cover:";

/// What rodio opens with a hint (see `Format::File`): the extensions local music takes.
/// WMA is missing on purpose — YouTube Music takes it for upload, nothing here decodes it.
const EXTS: &[&str] = &["mp3", "m4a", "mp4", "m4b", "aac", "flac", "ogg", "oga", "wav", "webm", "mka", "mkv"];

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    path: PathBuf,
    /// The song as its tags told it, for lists and the player.
    track: Track,
    /// A look for a cover found none, so later launches don't look again. Removing the
    /// song and adding it back is the way to look once more.
    #[serde(default)]
    no_cover: bool,
}

struct State {
    /// The songs in the order they were added, the way the page lists them.
    index: Vec<Entry>,
    /// Where the index is saved (the data dir's local.json).
    file: PathBuf,
    /// Where the covers found are kept (the data dir's local-covers).
    covers: PathBuf,
}

static STATE: LazyLock<RwLock<State>> =
    LazyLock::new(|| RwLock::new(State { index: Vec::new(), file: PathBuf::new(), covers: PathBuf::new() }));

/// Bumped whenever the index changes, so the page can pick up files added on the
/// picker's thread without reading them every frame.
static REVISED: AtomicU64 = AtomicU64::new(0);

/// How many changes the index has had: a number the page hasn't seen means new files.
pub fn revision() -> u64 {
    REVISED.load(Ordering::Relaxed)
}

/// Load the index.
pub fn init(data_dir: &Path) {
    let file = data_dir.join("local.json");
    let index = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let mut st = STATE.write().unwrap();
    st.index = index;
    st.file = file;
    st.covers = data_dir.join("local-covers");
    // Songs added before the app read covers (an older version) get theirs now: one
    // header read each, off the UI thread, the page picking them up as they land.
    if st.index.iter().any(|e| e.track.thumbs.is_empty() && !e.no_cover) {
        std::thread::Builder::new().name("local-covers".into()).spawn(backfill).ok();
    }
}

/// The covers for songs already in the index whose look hadn't happened or found
/// nothing yet. One entry at a time, so a big add isn't kept waiting on the lock.
fn backfill() {
    let todo: Vec<(String, PathBuf)> = {
        let st = STATE.read().unwrap();
        st.index.iter().filter(|e| e.track.thumbs.is_empty() && !e.no_cover).map(|e| (e.track.id.clone(), e.path.clone())).collect()
    };
    let mut found = false;
    for (id, path) in todo {
        let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
        let cover = read_tags(&path, &ext).2.and_then(|p| {
            let covers = STATE.read().unwrap().covers.clone();
            save_cover(&covers, &p.bytes).map(|name| Thumb { url: format!("{COVER}{name}"), width: p.width, height: p.height })
        });
        let mut st = STATE.write().unwrap();
        // Added again while this one was being read: its own cover stands.
        if let Some(e) = st.index.iter_mut().find(|e| e.track.id == id && e.track.thumbs.is_empty()) {
            match cover {
                Some(thumb) => {
                    e.track.thumbs.push(thumb);
                    found = true;
                }
                None => e.no_cover = true,
            }
        }
    }
    if found {
        REVISED.fetch_add(1, Ordering::Relaxed);
    }
    write_index();
}

/// Save the index whole or not at all: a crash mid-write doesn't lose it. Called after
/// a change, with `STATE` let go, so the file is written without holding anyone up.
fn write_index() {
    static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let (json, file) = {
        let st = STATE.read().unwrap();
        (serde_json::to_vec(&st.index), st.file.clone())
    };
    let Ok(json) = json else { return };
    let tmp = file.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, &file)) {
        log::warn!("couldn't save the local music index: {e}");
    }
}

/// Whether `id` is one of this computer's songs (anything after the prefix is opaque).
pub fn is_local(id: &str) -> bool {
    id.starts_with(PREFIX)
}

/// The songs as the page lists them, in the order they were added.
pub fn tracks() -> Vec<Track> {
    STATE.read().unwrap().index.iter().map(|e| e.track.clone()).collect()
}

/// One song's file, for Show in folder.
pub fn path(id: &str) -> Option<PathBuf> {
    STATE.read().unwrap().index.iter().find(|e| e.track.id == id).map(|e| e.path.clone())
}

/// A song's audio, as a complete stream buffer for playback. Nothing is transcoded:
/// the bytes go in as they are on disk, and the decoder reads the format from them.
pub fn open(id: &str) -> Option<Arc<Shared>> {
    let entry = STATE.read().unwrap().index.iter().find(|e| e.track.id == id).cloned()?;
    let data = std::fs::read(&entry.path).ok()?;
    let duration_ms = entry.track.duration.map(|s| s as u64 * 1000).unwrap_or(0);
    let ext = entry.path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    log::info!("local {}: playing {}", entry.track.title, entry.path.display());
    Some(Shared::from_bytes_with_format(data, duration_ms, None, Format::File { hint: hint(&ext), mime: None }, None))
}

/// Take files in: each is read for its tags and duration (a header's worth of disk
/// work, so this runs off the UI thread), skipping what isn't audio. Returns the
/// number added; a file already in the list by the same path isn't added twice.
pub fn add(paths: Vec<PathBuf>) -> usize {
    let mut added = 0;
    {
        let mut st = STATE.write().unwrap();
        for path in paths {
            let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
            if !EXTS.contains(&ext.as_str()) || !path.is_file() {
                continue;
            }
            if st.index.iter().any(|e| e.path == path) {
                continue;
            }
            let (mut track, duration, cover) = read_tags(&path, &ext);
            track.id = format!("{PREFIX}{:x}", sha1::Sha1::digest(path.to_string_lossy().as_bytes()));
            track.duration = duration;
            let no_cover = match cover.and_then(|p| save_cover(&st.covers, &p.bytes).map(|name| (name, p))) {
                Some((name, p)) => {
                    track.thumbs.push(Thumb { url: format!("{COVER}{name}"), width: p.width, height: p.height });
                    false
                }
                None => true,
            };
            st.index.push(Entry { path, track, no_cover });
            added += 1;
        }
    }
    if added > 0 {
        REVISED.fetch_add(1, Ordering::Relaxed);
        write_index();
    }
    added
}

/// Take a song out of the list. Its file is left alone.
pub fn remove(id: &str) {
    let changed = {
        let mut st = STATE.write().unwrap();
        let before = st.index.len();
        st.index.retain(|e| e.track.id != id);
        before != st.index.len()
    };
    if changed {
        REVISED.fetch_add(1, Ordering::Relaxed);
        write_index();
    }
}

/// A cover picture found for a song: its bytes, and the size its header tells.
struct Picture {
    bytes: Vec<u8>,
    width: u32,
    height: u32,
}

/// One file's song: its tags where they say something, the file's name where they
/// don't, its length in seconds (unknown when the header doesn't tell), and its cover
/// picture — the one in its tags, else the folder's own.
fn read_tags(path: &Path, ext: &str) -> (Track, Option<u32>, Option<Picture>) {
    let mut track = Track { id: String::new(), title: String::new(), artists: Vec::new(), album: None, duration: None, thumbs: Vec::new(), track_nr: None, plays: None, music_video: false, video: false, explicit: false };
    let mut duration = None;
    let mut embedded = None;
    // The tags and the length both sit in the container's header: one open reads both.
    match std::fs::File::open(path) {
        Ok(file) => {
            let mss = symphonia::core::io::MediaSourceStream::new(Box::new(file), Default::default());
            let mut hint = symphonia::core::probe::Hint::new();
            hint.with_extension(ext);
            match symphonia::default::get_probe().format(&hint, mss, &Default::default(), &Default::default()) {
                Ok(mut probed) => {
                    let params = probed.format.tracks().iter().find(|t| t.codec_params.sample_rate.is_some()).map(|t| t.codec_params.clone());
                    // A timebase in ticks makes frames into seconds; without one (or a
                    // length), the header just doesn't say how long the song is.
                    if let Some(p) = params
                        && let (Some(frames), Some(base)) = (p.n_frames, p.time_base)
                    {
                        let t = base.calc_time(frames);
                        duration = Some(t.seconds.min(u32::MAX as u64) as u32);
                    }
                    // The container's own tags, or the ones the probe read ahead of it.
                    let tags = probed.format.metadata().current().map(|m| m.tags().to_vec());
                    let tags = tags.or_else(|| probed.metadata.get().and_then(|m| m.current().map(|r| r.tags().to_vec())));
                    // The pictures in the tags come from the same read: a cover first,
                    // and anything attached in its place when there isn't one.
                    let visuals = probed.format.metadata().current().map(|m| m.visuals().to_vec());
                    let visuals = visuals.or_else(|| probed.metadata.get().and_then(|m| m.current().map(|r| r.visuals().to_vec())));
                    embedded = visuals.as_deref().and_then(|v| {
                        v.iter().find(|i| i.usage == Some(symphonia::core::meta::StandardVisualKey::FrontCover)).or_else(|| v.first()).and_then(|i| picture(&i.data))
                    });
                    if let Some(tags) = tags {
                        let tag = |key: symphonia::core::meta::StandardTagKey| {
                            tags.iter().find_map(|t| match (t.std_key == Some(key), &t.value) {
                                // A WAV's INFO tags can carry their NUL terminator; trim it.
                                (true, symphonia::core::meta::Value::String(v)) => Some(v.trim_matches(|c: char| c.is_whitespace() || c == '\0')).filter(|v| !v.is_empty()).map(str::to_owned),
                                _ => None,
                            })
                        };
                        track.title = tag(symphonia::core::meta::StandardTagKey::TrackTitle).unwrap_or_default();
                        if let Some(artist) = tag(symphonia::core::meta::StandardTagKey::Artist) {
                            track.artists.push(crate::model::Link { id: None, name: artist });
                        }
                        track.album = tag(symphonia::core::meta::StandardTagKey::Album).map(|name| crate::model::Link { id: None, name });
                    }
                }
                Err(e) => log::warn!("local {}: couldn't read the header: {e}", path.display()),
            }
        }
        Err(e) => log::warn!("local {}: couldn't open: {e}", path.display()),
    }
    // A tagless file still has its name: "Artist - Title" splits, the rest is the title.
    if track.title.is_empty() {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().trim();
        match stem.split_once(" - ") {
            Some((artist, title)) if !title.trim().is_empty() => {
                track.artists.push(crate::model::Link { id: None, name: artist.trim().to_owned() });
                track.title = title.trim().to_owned();
            }
            _ => track.title = stem.to_owned(),
        }
    }
    if track.title.is_empty() {
        track.title = path.file_name().and_then(|s| s.to_str()).unwrap_or("Local file").to_owned();
    }
    // Nothing in the tags: the folder's own picture stands in, the way music is
    // usually filed — one cover beside the songs it belongs to.
    let picture = embedded.or_else(|| sibling_picture(path));
    (track, duration, picture)
}

/// The names a folder's cover goes by, best known first.
const FOLDER_COVERS: [&str; 6] = ["cover.jpg", "cover.jpeg", "cover.png", "folder.jpg", "folder.png", "front.jpg"];

fn sibling_picture(song: &Path) -> Option<Picture> {
    let dir = song.parent()?;
    FOLDER_COVERS.iter().find_map(|name| std::fs::read(dir.join(name)).ok().and_then(|b| picture(&b)))
}

/// Which of the image formats this app carries the bytes are ("jpg", "png", "webp"),
/// or `None` for anything else.
fn image_kind(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("png")
    } else if bytes.len() > 12 && bytes[..4] == *b"RIFF" && bytes[8..12] == *b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

/// A picture this app can show, with the size its header tells. Anything the image
/// decoders don't carry isn't a cover it can use.
fn picture(bytes: &[u8]) -> Option<Picture> {
    image_kind(bytes)?;
    let (width, height) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_dimensions().ok()?;
    Some(Picture { bytes: bytes.to_vec(), width, height })
}

/// Write the picture under a digest of its bytes, so one image shared by an album's
/// files is stored once. The name is what a [`COVER`] URI carries.
fn save_cover(covers: &Path, bytes: &[u8]) -> Option<String> {
    let name = format!("{:x}.{}", sha1::Sha1::digest(bytes), image_kind(bytes)?);
    let file = covers.join(&name);
    if !file.is_file() && std::fs::create_dir_all(covers).and_then(|_| std::fs::write(&file, bytes)).is_err() {
        log::warn!("couldn't save a local cover to {}", file.display());
        return None;
    }
    Some(name)
}

/// The file a cover name names, inside `covers`. Names come from [`save_cover`] (a
/// digest); one that isn't a plain file name is refused, so a hand-edited index can't
/// point anywhere outside that directory.
pub fn cover_in(covers: &Path, name: &str) -> Option<PathBuf> {
    let plain = (1..=64).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    plain.then(|| covers.join(name))
}

/// A local song's cover file, for the places that read a file rather than load a
/// texture: the media controls and the OS notification.
pub fn cover_file(id: &str) -> Option<PathBuf> {
    let st = STATE.read().unwrap();
    let name = st.index.iter().find(|e| e.track.id == id)?.track.thumbs.iter().find_map(|t| t.url.strip_prefix(COVER))?;
    cover_in(&st.covers, name)
}

/// The extension as rodio's hint wants it (a file name's worth of format names).
fn hint(ext: &str) -> &'static str {
    match ext {
        "m4a" | "m4b" | "mp4" => "m4a",
        "oga" => "ogg",
        "mka" | "mkv" => "webm",
        other => EXTS.iter().find(|e| **e == other).copied().unwrap_or("mp3"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_ids_are_marked() {
        assert!(is_local("local:1234"));
        assert!(!is_local("dQw4w9WgXcQ"));
    }

    #[test]
    fn hints_match_the_extensions_taken() {
        for ext in EXTS {
            assert!(EXTS.contains(&hint(ext)), "{ext} has a hint");
        }
        assert_eq!(hint("m4b"), "m4a");
        assert_eq!(hint("oga"), "ogg");
    }

    #[test]
    fn cover_names_stay_inside_the_covers_dir() {
        let covers = Path::new("/data/local-covers");
        assert_eq!(cover_in(covers, "e5321c.jpg"), Some(covers.join("e5321c.jpg")));
        assert_eq!(cover_in(covers, ""), None);
        assert_eq!(cover_in(covers, "../local.json"), None);
        assert_eq!(cover_in(covers, "sub/dir.jpg"), None);
        assert_eq!(cover_in(covers, &"x".repeat(65)), None);
    }

    #[test]
    fn pictures_are_sniffed_and_sized_by_their_header() {
        let mut png = Vec::new();
        image::RgbImage::from_pixel(3, 2, image::Rgb([12, 34, 56])).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let p = picture(&png).expect("a PNG decodes");
        assert_eq!((p.width, p.height), (3, 2));
        assert_eq!(image_kind(&png), Some("png"));
        assert!(picture(b"neither jpg nor png nor webp").is_none());
    }

    /// A tiny MP3: one MPEG-1 Layer III frame of nothing, with an ID3v2.3 tag in front
    /// carrying a front-cover picture when there is one. Enough for the header read
    /// that `add` does; nothing plays it.
    fn mp3(cover: Option<&[u8]>) -> Vec<u8> {
        let mut tag = b"ID3".to_vec();
        tag.extend_from_slice(&[3, 0, 0]);
        let mut frames = Vec::new();
        if let Some(png) = cover {
            let mut apic = vec![0x00];
            apic.extend_from_slice(b"image/png\0");
            apic.push(0x03);
            apic.push(0x00);
            apic.extend_from_slice(png);
            frames.extend_from_slice(b"APIC");
            frames.extend_from_slice(&(apic.len() as u32).to_be_bytes());
            frames.extend_from_slice(&[0, 0]);
            frames.extend_from_slice(&apic);
        }
        let len = frames.len();
        // The tag's size is syncsafe: seven bits a byte, most significant first.
        tag.extend_from_slice(&[(len >> 21) as u8 & 0x7f, (len >> 14) as u8 & 0x7f, (len >> 7) as u8 & 0x7f, len as u8 & 0x7f]);
        tag.extend_from_slice(&frames);
        // 128 kbps at 44100 Hz makes a 417-byte frame.
        tag.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
        tag.extend(std::iter::repeat_n(0, 413));
        tag
    }

    #[test]
    fn an_added_song_carries_its_cover() {
        let mut png = Vec::new();
        image::RgbImage::from_pixel(5, 4, image::Rgb([200, 40, 90])).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let dir = std::env::temp_dir().join("bitchord-local-covers");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("song.mp3"), mp3(Some(&png))).unwrap();
        // The folder's own picture, for the song whose tags carry none.
        let mut jpg = Vec::new();
        image::RgbImage::from_pixel(6, 6, image::Rgb([90, 200, 40])).write_to(&mut std::io::Cursor::new(&mut jpg), image::ImageFormat::Jpeg).unwrap();
        std::fs::write(dir.join("cover.jpg"), &jpg).unwrap();
        std::fs::write(dir.join("plain.mp3"), mp3(None)).unwrap();
        init(&dir);
        assert_eq!(add(vec![dir.join("song.mp3"), dir.join("plain.mp3")]), 2);
        let added = tracks();
        let embedded = added.iter().find(|t| t.title == "song").expect("the titled one");
        assert_eq!(embedded.thumbs.len(), 1);
        assert_eq!((embedded.thumbs[0].width, embedded.thumbs[0].height), (5, 4));
        assert_eq!(std::fs::read(cover_file(&embedded.id).unwrap()).unwrap(), png, "the picture from the tags");
        let sibling = added.iter().find(|t| t.title == "plain").expect("the untitled one");
        assert_eq!(std::fs::read(cover_file(&sibling.id).unwrap()).unwrap(), jpg, "the folder's own picture");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
