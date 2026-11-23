//! Resolves both presence artwork slots. Slow work shares one worker and cached results,
//! so skipping tracks or moving around Settings cannot start a backlog of GIF encodes.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, ensure};
use base64::Engine;
use image::codecs::gif::{GifDecoder, GifEncoder, Repeat};
use image::{AnimationDecoder, Delay, Frame, ImageFormat, Rgba, RgbaImage, imageops};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::backend::Backend;
use crate::discord::{ICON, ICON_BYTES, ICON_SOURCE, Prefs, Presence, web_url};
use crate::model::Track;

const CACHE: &str = "discord-artwork-cache";
const PICTURES: &str = "discord-artwork";
const MAX_CACHE: u64 = 80 * 1024 * 1024;
const MAX_GIF: usize = 4 * 1024 * 1024;
const MAX_IMAGE: usize = 16 * 1024 * 1024;
const MAX_CLIP: usize = 24 * 1024 * 1024;
const RETRY: Duration = Duration::from_secs(600);
const ASSETS: &str = "https://raw.githubusercontent.com/LoserDenji/Bitchord-Desktop-Revived/main/site/discord";
const STATUS_RENDERER: &str = "https://kroki.io/graphviz/png";
// Material icons (Apache-2.0) keep play and pause distinct during an asset outage.
const PLAY_FALLBACK: &str =
    "https://raw.githubusercontent.com/google/material-design-icons/master/png/av/play_circle/materialicons/48dp/2x/baseline_play_circle_black_48dp.png";
const PAUSE_FALLBACK: &str =
    "https://raw.githubusercontent.com/google/material-design-icons/master/png/av/pause_circle/materialicons/48dp/2x/baseline_pause_circle_black_48dp.png";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Album,
    Artist,
    Logo,
    Status,
    SpinAlbum,
    SpinArtist,
    Custom,
    Spotify,
    Apple,
    Either,
    #[default]
    #[serde(other)]
    Off,
}

impl Mode {
    pub const ALL: [Self; 11] = [
        Self::Off,
        Self::Album,
        Self::Artist,
        Self::Logo,
        Self::Status,
        Self::SpinAlbum,
        Self::SpinArtist,
        Self::Custom,
        Self::Spotify,
        Self::Apple,
        Self::Either,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Album => "Album artwork",
            Self::Artist => "Artist artwork",
            Self::Logo => "BitChord icon",
            Self::Status => "Playback status",
            Self::SpinAlbum => "Rotating album artwork",
            Self::SpinArtist => "Rotating artist artwork",
            Self::Custom => "Custom artwork",
            Self::Spotify => "Spotify Canvas",
            Self::Apple => "Apple Music animated cover",
            Self::Either => "Spotify or Apple animation",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImagePrefs {
    pub mode: Mode,
    pub hover: String,
    pub url: String,
    pub file: Option<String>,
}

impl ImagePrefs {
    pub fn album() -> Self {
        Self { mode: Mode::Album, hover: "{album}".into(), ..Default::default() }
    }

    pub fn logo() -> Self {
        Self { mode: Mode::Logo, hover: "{quality}".into(), ..Default::default() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub host: bool,
    pub spin_seconds: u8,
    pub spin_fps: u8,
    pub canvas_fps: u8,
    pub canvas_size: u16,
}

pub const SPIN_FPS: &[u8] = &[10, 12, 14, 17, 20, 25, 33, 50];
pub const CANVAS_FPS: &[u8] = &[10, 15, 20, 24, 30, 40, 50];
pub const CANVAS_SIZE: &[u16] = &[96, 128, 160, 192, 240];

impl Default for Options {
    fn default() -> Self {
        Self { host: false, spin_seconds: 8, spin_fps: 25, canvas_fps: 30, canvas_size: 128 }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Picture {
    pub url: Option<String>,
    /// Art to use if Discord refuses the preferred URL.
    pub fallback: Option<String>,
    pub badge: Option<(u8, bool)>,
    pub preview: Option<Arc<Preview>>,
    pub rotation: Option<Rotation>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rotation {
    pub source: String,
    pub frames: u32,
    pub length_ms: u32,
}

impl Picture {
    pub fn web(url: &str) -> Option<Self> {
        Some(Self { url: Some(web_url(url, 256)?), ..Default::default() })
    }

    pub fn addresses(&self) -> (Option<&str>, Option<&str>) {
        (self.url.as_deref(), self.fallback.as_deref())
    }

    pub fn ipc_url(&self, still: bool) -> Option<&str> {
        if still { self.fallback.as_deref().or(self.url.as_deref()) } else { self.url.as_deref() }
    }
}

#[derive(Debug, PartialEq)]
pub struct Preview {
    pub key: String,
    pub frames: Vec<(u32, egui::ColorImage)>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum Job {
    StatusAssets,
    Artist(String),
    Still(String),
    Spin(String, u8, u8),
    Canvas(Mode, String, u8, u16),
}

#[derive(Clone, Default)]
struct ResultImage {
    url: Option<String>,
    preview: Option<Arc<Preview>>,
}

struct Entry {
    result: ResultImage,
    until: Instant,
    used: Instant,
}

#[derive(Default)]
struct State {
    entries: HashMap<(Job, bool), Entry>,
    busy: bool,
    import: Option<(usize, anyhow::Result<String>)>,
    importing: bool,
}

impl State {
    fn store(&mut self, key: (Job, bool), result: ResultImage, now: Instant, ttl: Duration) {
        self.entries.remove(&key);
        // Keep GPU/CPU preview pixels for just the last few pictures.
        if result.preview.is_some() {
            let mut by_age: Vec<_> = self.entries.iter().filter(|(_, e)| e.result.preview.is_some()).map(|(k, e)| (e.used, k.clone())).collect();
            by_age.sort_by_key(|(at, _)| *at);
            let excess = by_age.len().saturating_sub(3);
            for (_, old) in by_age.into_iter().take(excess) {
                self.entries.remove(&old);
            }
        }
        if self.entries.len() >= 128
            && let Some(old) = self.entries.iter().min_by_key(|(_, e)| e.used).map(|(k, _)| k.clone())
        {
            self.entries.remove(&old);
        }
        self.entries.insert(key, Entry { result, until: now + ttl, used: now });
    }
}

#[derive(Clone, Default)]
pub struct Artwork {
    state: Arc<Mutex<State>>,
    publish: Arc<AtomicBool>,
}

impl Artwork {
    pub fn allow_publish(&self, allow: bool) {
        self.publish.store(allow, Ordering::Release);
    }

    /// Generate previews locally. Only the playback path can send bytes to a file host.
    pub fn pictures(&self, backend: &Backend, p: &Presence, prefs: &Prefs, track: Option<&Track>, preview: bool) -> [Option<Picture>; 2] {
        if prefs.hide_details || !prefs.art {
            return [None, None];
        }
        let large = self.pick(backend, p, prefs, track, 0, preview);
        let small = large.as_ref().and_then(|_| self.pick(backend, p, prefs, track, 1, preview));
        [large, small]
    }

    fn pick(&self, backend: &Backend, p: &Presence, prefs: &Prefs, track: Option<&Track>, slot: usize, preview: bool) -> Option<Picture> {
        let options = &prefs.artwork;
        let img = &prefs.images[slot];
        let host = options.host && track.is_some() && !preview;
        let logo = || Some(Picture { url: Some(ICON.into()), fallback: Some(ICON_SOURCE.into()), ..Default::default() });
        let album = p.art.as_deref().and_then(Picture::web);
        let mut source = p.art.as_deref().and_then(|url| web_url(url, 2048));
        if (host || preview) && source.is_none() {
            source = track.and_then(crate::backend::art_uri).filter(|s| s.starts_with(crate::local::COVER));
        }
        match img.mode {
            Mode::Off => return None,
            Mode::Logo => return logo(),
            Mode::Status => {
                let source = if preview { None } else { self.get(backend, Job::StatusAssets, false, false, None).and_then(|a| a.url) };
                return Some(status_badge(p.position_ms, p.duration_ms, p.paused, source.as_deref()));
            }
            Mode::Artist | Mode::SpinArtist => {
                if let Some(url) = p.artist_art.as_deref().and_then(|url| web_url(url, 2048)) {
                    source = Some(url);
                } else if let Some(id) = &p.artist_id
                    && let Some(url) = self.get(backend, Job::Artist(id.clone()), false, false, None).and_then(|art| art.url)
                {
                    source = Some(url);
                }
            }
            _ => {}
        }
        let fallback = source.as_deref().and_then(Picture::web).or_else(logo);
        match img.mode {
            Mode::Album | Mode::Artist => {
                if source.as_deref().is_some_and(|s| s.starts_with(crate::local::COVER)) {
                    let job = Job::Still(source?);
                    return self.generated(backend, job, host, preview, track, fallback);
                }
                // Keep BitChord's existing large album slot absent when there is no cover.
                if slot == 0 && img.mode == Mode::Album && prefs.images[1].mode == Mode::Off { album } else { fallback }
            }
            Mode::SpinAlbum | Mode::SpinArtist => {
                let seconds = options.spin_seconds.clamp(2, 20);
                let fps = choice(options.spin_fps, SPIN_FPS, 25);
                if preview {
                    let (frames, centis) = spin_timing(seconds, fps);
                    let mut picture = fallback.unwrap_or_default();
                    picture.rotation = Some(Rotation { source: source.unwrap_or_else(|| ICON.to_owned()), frames, length_ms: centis * 10 });
                    return Some(picture);
                }
                let Some(source) = source else { return fallback };
                self.generated(backend, Job::Spin(source, seconds, fps), host, false, track, fallback)
            }
            Mode::Custom => {
                if let Some(file) = img.file.as_deref().filter(|f| valid_key(f)) {
                    return self.generated(backend, Job::Still(format!("custom:{slot}:{file}")), host, preview, track, None);
                }
                Picture::web(&img.url)
            }
            Mode::Spotify | Mode::Apple | Mode::Either => {
                if track.is_none() {
                    return fallback;
                }
                let fps = choice(options.canvas_fps, CANVAS_FPS, 30);
                let side = choice(options.canvas_size, CANVAS_SIZE, 128);
                self.generated(backend, Job::Canvas(img.mode, p.video_id.clone(), fps, side), host, preview, track, fallback)
            }
            _ => unreachable!("handled above"),
        }
    }

    fn generated(&self, backend: &Backend, job: Job, host: bool, preview: bool, track: Option<&Track>, fallback: Option<Picture>) -> Option<Picture> {
        if !host && !preview {
            return fallback;
        }
        let Some(result) = self.get(backend, job, host, preview, track) else { return fallback };
        let mut picture = match result.url.as_deref().and_then(Picture::web) {
            Some(mut picture) => {
                picture.fallback = fallback.and_then(|p| p.url).filter(|url| Some(url.as_str()) != picture.url.as_deref());
                picture
            }
            None if result.preview.is_some() => fallback.unwrap_or_default(),
            None => fallback?,
        };
        picture.preview = result.preview;
        Some(picture)
    }

    /// Look up the normalized asset without touching disk or doing network work here.
    fn get(&self, backend: &Backend, job: Job, host: bool, preview: bool, track: Option<&Track>) -> Option<ResultImage> {
        let key = (job, host);
        let now = Instant::now();
        let mut state = self.state.lock().unwrap();
        let existing = state.entries.get_mut(&key).filter(|e| now < e.until).map(|e| {
            e.used = now;
            e.result.clone()
        });
        if state.busy || existing.is_some() {
            return existing;
        }
        state.busy = true;
        drop(state);

        let this = self.clone();
        let worker = backend.clone();
        let track = track.cloned();
        backend.spawn(async move {
            let (job, host) = &key;
            let outcome = run(&worker, job, *host, preview, track.as_ref(), &this.publish).await;
            let (result, ttl) = match outcome {
                Ok(x) => x,
                Err(err) => {
                    log::debug!("discord artwork: {err:#}");
                    (ResultImage::default(), RETRY)
                }
            };
            let now = Instant::now();
            let mut state = this.state.lock().unwrap();
            state.busy = false;
            state.store(key, result, now, ttl);
            drop(state);
            worker.wake();
        });
        None
    }

    pub fn importing(&self) -> bool {
        self.state.lock().unwrap().importing
    }

    pub fn import(&self, backend: &Backend, slot: usize) {
        let mut state = self.state.lock().unwrap();
        if state.importing {
            return;
        }
        state.importing = true;
        drop(state);
        let this = self.clone();
        let worker = backend.clone();
        backend.spawn(async move {
            let picked =
                rfd::AsyncFileDialog::new().set_title("Choose Discord artwork").add_filter("Image", &["png", "jpg", "jpeg", "webp", "gif"]).pick_file().await;
            let result = if let Some(file) = picked {
                let dir = worker.data_dir().to_owned();
                let file = file.path().to_owned();
                Some(tokio::task::spawn_blocking(move || save_custom(&dir, slot, &file)).await.unwrap_or_else(|e| Err(e.into())))
            } else {
                None
            };
            let mut state = this.state.lock().unwrap();
            state.import = result.map(|result| {
                (
                    slot,
                    result.map(|(file, preview)| {
                        let now = Instant::now();
                        state.store((Job::Still(format!("custom:{slot}:{file}")), false), ResultImage { url: None, preview: Some(preview) }, now, RETRY);
                        file
                    }),
                )
            });
            state.importing = false;
            drop(state);
            worker.wake();
        });
    }

    pub fn take_import(&self) -> Option<(usize, anyhow::Result<String>)> {
        self.state.lock().unwrap().import.take()
    }
}

fn status_badge(position_ms: u64, duration_ms: u64, paused: bool, source: Option<&str>) -> Picture {
    let fraction = position_ms as f64 / duration_ms.max(1) as f64;
    let step = (fraction * 20.0).round().clamp(0.0, 20.0) as u8;
    let (state, fallback) = if paused { ("paused", PAUSE_FALLBACK) } else { ("playing", PLAY_FALLBACK) };
    let url = match source {
        Some(ASSETS) => Some(format!("{ASSETS}/{state}-{step:02}.png")),
        Some(STATUS_RENDERER) => web_url(rendered_badge(step, paused), 300),
        _ => None,
    };
    let backup = url.as_ref().map(|_| fallback.to_owned());
    Picture { url: url.or_else(|| Some(fallback.to_owned())), fallback: backup, badge: Some((step, paused)), ..Default::default() }
}

fn rendered_badge(step: u8, paused: bool) -> &'static str {
    static URLS: [[OnceLock<String>; 21]; 2] = [const { [const { OnceLock::new() }; 21] }; 2];
    URLS[usize::from(paused)][usize::from(step)].get_or_init(|| {
        let (color, glyph) = if paused { ("#ffcc77", "Ⅱ") } else { ("#55dd99", "▶") };
        let ring = match step {
            0 => "#444455".to_owned(),
            20 => color.to_owned(),
            _ => format!("{color};0.{:02}:#444455", step * 5),
        };
        let diagram = format!(
            concat!(
                r#"graph{{layout=neato;bgcolor=transparent;node[fixedsize=true,shape=circle,pos="0,0!"];"#,
                r##"a[style=wedged,fillcolor="{}",label="",width=1];"##,
                r##"b[style=filled,fillcolor="#171922",width=.83,label="{}",fontcolor=white,fontname=sans,fontsize=35]}}"##,
            ),
            ring, glyph,
        );
        // Discord's media proxy can forward a plain path without having to preserve a chart query.
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(diagram.as_bytes(), 9);
        let path = base64::engine::general_purpose::URL_SAFE.encode(compressed);
        format!("{STATUS_RENDERER}/{path}")
    })
}

fn choice<T: Copy + PartialEq>(value: T, choices: &[T], default: T) -> T {
    if choices.contains(&value) { value } else { default }
}

fn valid_key(key: &str) -> bool {
    key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn custom_dir(dir: &Path, slot: usize) -> PathBuf {
    dir.join(PICTURES).join(slot.to_string())
}

fn save_custom(dir: &Path, slot: usize, path: &Path) -> anyhow::Result<(String, Arc<Preview>)> {
    let img = read_file(path, MAX_IMAGE)?;
    let bytes = png(square(decode_image(&img)?, 512))?;
    let key = digest(&bytes);
    let folder = custom_dir(dir, slot);
    std::fs::create_dir_all(&folder)?;
    let preview = make_preview(key.clone(), &bytes, false)?;
    std::fs::write(folder.join(format!("{key}.png")), bytes)?;
    Ok((key, preview))
}

/// Keep the files the last completed save points at when the profile opens again.
pub fn prune_custom(dir: &Path, images: &[ImagePrefs; 2]) {
    for (slot, image) in images.iter().enumerate() {
        let keep = image.file.as_ref().map(|f| format!("{f}.png"));
        if let Ok(files) = std::fs::read_dir(custom_dir(dir, slot)) {
            for file in files.flatten() {
                let name = file.file_name();
                if name.to_str().is_some_and(|n| n.strip_suffix(".png").is_some_and(valid_key) && Some(n) != keep.as_deref()) {
                    let _ = std::fs::remove_file(file.path());
                }
            }
        }
    }
}

fn read_file(path: &Path, max: usize) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(max as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= max, "artwork file exceeds its size limit");
    Ok(bytes)
}

async fn download(http: &reqwest::Client, url: &str, max: usize) -> anyhow::Result<Vec<u8>> {
    let mut reply = http.get(url).timeout(Duration::from_secs(25)).send().await?.error_for_status()?;
    ensure!(reply.content_length().is_none_or(|n| n <= max as u64), "artwork response exceeds its size limit");
    let mut bytes = Vec::new();
    while let Some(chunk) = reply.chunk().await? {
        ensure!(bytes.len() + chunk.len() <= max, "artwork response exceeds its size limit");
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn decode_image(bytes: &[u8]) -> anyhow::Result<RgbaImage> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(80 * 1024 * 1024);
    reader.limits(limits);
    Ok(reader.decode()?.into_rgba8())
}

fn square(img: RgbaImage, side: u32) -> RgbaImage {
    let (w, h) = img.dimensions();
    let edge = w.min(h);
    let crop = imageops::crop_imm(&img, (w - edge) / 2, (h - edge) / 2, edge, edge);
    imageops::resize(&*crop, side, side, imageops::FilterType::Triangle)
}

fn png(img: RgbaImage) -> anyhow::Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png)?;
    Ok(out.into_inner())
}

async fn image_bytes(backend: &Backend, source: &str) -> anyhow::Result<Vec<u8>> {
    let dir = backend.data_dir();
    if let Some(name) = source.strip_prefix(crate::local::COVER) {
        let path = crate::local::cover_in(&dir.join("local-covers"), name).context("local cover is missing")?;
        return tokio::task::spawn_blocking(move || read_file(&path, MAX_IMAGE)).await?;
    }
    if let Some((slot, key)) = source.strip_prefix("custom:").and_then(|s| s.split_once(':')) {
        ensure!(valid_key(key), "invalid custom artwork");
        let path = custom_dir(dir, slot.parse()?).join(format!("{key}.png"));
        return tokio::task::spawn_blocking(move || read_file(&path, MAX_IMAGE)).await?;
    }
    if source == ICON {
        return Ok(ICON_BYTES.to_vec());
    }
    let url = web_url(source, 2048).context("invalid artwork URL")?;
    download(&backend.http(), &url, MAX_IMAGE).await
}

fn spin_timing(seconds: u8, fps: u8) -> (u32, u32) {
    let length = u32::from(seconds) * 100;
    let delay = ((100.0 / f32::from(fps)).round() as u32).max(length.div_ceil(400)).max(2);
    (length.div_ceil(delay), length)
}

fn spin(bytes: &[u8], seconds: u8, fps: u8) -> anyhow::Result<Vec<u8>> {
    const SIDE: u32 = 96;
    let img = square(decode_image(bytes)?, SIDE);
    let (count, length) = spin_timing(seconds, fps);
    let mut bytes = Vec::new();
    {
        let mut gif = GifEncoder::new_with_speed(&mut bytes, 22);
        gif.set_repeat(Repeat::Infinite)?;
        let center = SIDE as f64 / 2.0;
        for i in 0..count {
            let (sin, cos) = (i as f64 / count as f64 * std::f64::consts::TAU).sin_cos();
            let frame = RgbaImage::from_fn(SIDE, SIDE, |x, y| {
                let (x, y) = (x as f64 + 0.5 - center, y as f64 + 0.5 - center);
                let r = x.hypot(y);
                if r >= center || r < 3.5 {
                    return Rgba([0, 0, 0, 0]);
                }
                let sx = (center + cos * x + sin * y) as u32;
                let sy = (center - sin * x + cos * y) as u32;
                *img.get_pixel(sx.min(SIDE - 1), sy.min(SIDE - 1))
            });
            let ms = (length * (i + 1) / count - length * i / count) * 10;
            gif.encode_frame(Frame::from_parts(frame, 0, 0, Delay::from_numer_denom_ms(ms, 1)))?;
        }
    }
    ensure!(bytes.len() <= MAX_GIF, "rotating art exceeds Discord's GIF budget");
    Ok(bytes)
}

fn canvas_gif(video: &[u8], fps: u8, side: u16) -> anyhow::Result<Vec<u8>> {
    // Retry a heavy clip at lower resolution and sampling before falling back to the still.
    for (fps, side) in [(fps, side), (fps.min(20), side.min(128)), (fps.min(12), side.min(96))] {
        let mut bytes = Vec::new();
        {
            let mut gif = GifEncoder::new_with_speed(&mut bytes, 22);
            gif.set_repeat(Repeat::Infinite)?;
            crate::motion::visit_clip(video, u32::from(side), fps, |w, h, rgba, ms| {
                let frame = RgbaImage::from_raw(w as u32, h as u32, rgba).context("invalid video frame")?;
                let frame = square(frame, side.into());
                gif.encode_frame(Frame::from_parts(frame, 0, 0, Delay::from_numer_denom_ms(ms, 1)))?;
                Ok(())
            })?;
        }
        if bytes.len() <= MAX_GIF {
            return Ok(bytes);
        }
    }
    anyhow::bail!("animated cover exceeds Discord's GIF budget")
}

fn make_preview(key: String, bytes: &[u8], animated: bool) -> anyhow::Result<Arc<Preview>> {
    let rgba = |img: RgbaImage, side| {
        let img = square(img, side);
        egui::ColorImage::from_rgba_unmultiplied([side as usize; 2], img.as_raw())
    };
    let mut frames = Vec::new();
    if animated {
        let mut at: u32 = 0;
        let mut next: u32 = 0;
        for frame in GifDecoder::new(Cursor::new(bytes))?.into_frames() {
            let frame = frame?;
            let delay: Duration = frame.delay().into();
            let end = at.saturating_add(delay.as_millis() as u32);
            if at >= next {
                let step = if frames.len() < 40 { 90 } else { 260 };
                frames.push((end, rgba(frame.into_buffer(), 64)));
                next = at + step;
            } else if let Some((last, _)) = frames.last_mut() {
                *last = end;
            }
            at = end;
        }
    } else {
        frames.push((0, rgba(decode_image(bytes)?, 96)));
    }
    ensure!(!frames.is_empty(), "artwork has no frames");
    Ok(Arc::new(Preview { key, frames }))
}

#[derive(Deserialize, Serialize)]
struct Hosted {
    url: String,
    expires: u64,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

async fn upload(http: &reqwest::Client, bytes: &[u8], gif: bool) -> anyhow::Result<Hosted> {
    use reqwest::multipart::{Form, Part};
    let (name, mime) = if gif { ("bit-chord.gif", "image/gif") } else { ("bit-chord.png", "image/png") };
    let part = || Part::bytes(bytes.to_vec()).file_name(name).mime_str(mime);
    // Both hosts expire files. A permanent public upload would outlive Reset.
    let uguu = async {
        let reply = http
            .post("https://uguu.se/upload")
            .timeout(Duration::from_secs(25))
            .multipart(Form::new().part("files[]", part()?))
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;
        let url = reply["files"][0]["url"].as_str().and_then(|url| hosted_url(url, "uguu.se")).context("Uguu returned no artwork URL")?;
        Ok::<_, anyhow::Error>(Hosted { url, expires: now_secs() + 2 * 3600 })
    };
    if let Ok(hosted) = uguu.await {
        return Ok(hosted);
    }
    let reply = http
        .post("https://litterbox.catbox.moe/resources/internals/api.php")
        .timeout(Duration::from_secs(25))
        .multipart(Form::new().text("reqtype", "fileupload").text("time", "72h").part("fileToUpload", part()?))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let url = hosted_url(reply.trim(), "catbox.moe").context("Litterbox returned no artwork URL")?;
    Ok(Hosted { url, expires: now_secs() + 66 * 3600 })
}

fn hosted_url(url: &str, domain: &str) -> Option<String> {
    let url = web_url(url, 256)?;
    let parsed = url::Url::parse(&url).ok()?;
    let host = parsed.host_str()?;
    (parsed.scheme() == "https" && (host == domain || host.strip_suffix(domain).is_some_and(|p| p.ends_with('.')))).then_some(url)
}

async fn render(backend: &Backend, job: &Job, track: Option<&Track>) -> anyhow::Result<Vec<u8>> {
    match job {
        Job::Still(source) => {
            let bytes = image_bytes(backend, source).await?;
            Ok(tokio::task::spawn_blocking(move || png(square(decode_image(&bytes)?, 512))).await??)
        }
        Job::Spin(source, seconds, fps) => {
            let bytes = image_bytes(backend, source).await?;
            let (seconds, fps) = (*seconds, *fps);
            Ok(tokio::task::spawn_blocking(move || spin(&bytes, seconds, fps)).await??)
        }
        Job::Canvas(mode, id, fps, side) => {
            let track = track.context("no song to look up")?;
            let http = backend.http();
            let (fps, side) = (*fps, *side);
            if *mode != Mode::Apple
                && let Some(url) = backend.spotify_canvas(id, &track.title, &track.artist_line(), track.duration.map(|d| u64::from(d) * 1000)).await
                && let Ok(path) = crate::motion::cache_mp4(&http, backend.data_dir(), &url, MAX_CLIP).await
                && let Ok(Ok(gif)) = tokio::task::spawn_blocking(move || canvas_gif(&read_file(&path, MAX_CLIP)?, fps, side)).await
            {
                return Ok(gif);
            }
            ensure!(*mode != Mode::Spotify, "Spotify Canvas is unavailable");
            let url = crate::canvas::find(&http, backend.data_dir(), track).await.context("Apple Music animated cover is unavailable")?;
            let clip = crate::canvas::load_bounded(&http, &url, side.into(), MAX_CLIP).await?;
            ensure!(clip.init.len() + clip.segments.iter().map(Vec::len).sum::<usize>() <= MAX_CLIP, "animated cover clip is too large");
            Ok(tokio::task::spawn_blocking(move || {
                let mut video = clip.init.clone();
                for segment in &clip.segments {
                    video.extend_from_slice(segment);
                }
                canvas_gif(&video, fps, side)
            })
            .await??)
        }
        Job::Artist(_) | Job::StatusAssets => unreachable!("remote assets are resolved before rendering"),
    }
}

async fn run(backend: &Backend, job: &Job, host: bool, preview: bool, track: Option<&Track>, publish: &AtomicBool) -> anyhow::Result<(ResultImage, Duration)> {
    if *job == Job::StatusAssets {
        let probe = format!("{ASSETS}/playing-00.png");
        let available = backend.http().head(probe.as_str()).timeout(Duration::from_secs(3)).send().await.is_ok_and(|response| response.status().is_success());
        let url = if available {
            Some(ASSETS)
        } else {
            backend
                .http()
                .get(rendered_badge(10, false))
                .timeout(Duration::from_secs(3))
                .send()
                .await
                .is_ok_and(|response| {
                    response.status().is_success() && response.headers().get(reqwest::header::CONTENT_TYPE).is_some_and(|value| value == "image/png")
                })
                .then_some(STATUS_RENDERER)
        };
        return Ok((ResultImage { url: url.map(str::to_owned), preview: None }, RETRY));
    }
    if let Job::Artist(id) = job {
        let api = backend.api();
        let page = tokio::time::timeout(Duration::from_secs(20), crate::ytm::artist(&api, id, api.account().is_some())).await??;
        let url = crate::model::pick(&page.banner, 160).and_then(|u| web_url(&crate::model::sized(u, 512), 256));
        return Ok((ResultImage { url, preview: None }, Duration::from_secs(3600)));
    }
    let animated = !matches!(job, Job::Still(_));
    let key = digest(format!("v1:{job:?}").as_bytes());
    let path = backend.data_dir().join(CACHE).join(&key);
    let location = path.join("host.json");
    let cached = || -> Option<Hosted> {
        let value: Hosted = serde_json::from_slice(&read_file(&location, 2048).ok()?).ok()?;
        (value.expires > now_secs() && web_url(&value.url, 256).is_some()).then_some(value)
    };
    let hosted = host.then(cached).flatten();
    if !preview && let Some(h) = hosted.as_ref() {
        return Ok((ResultImage { url: Some(h.url.clone()), preview: None }, Duration::from_secs(h.expires.saturating_sub(now_secs()))));
    }
    let (bytes, new) = if let Ok(bytes) = read_file(&path.join("image"), MAX_GIF) { (bytes, false) } else { (render(backend, job, track).await?, true) };
    if new && std::fs::create_dir_all(&path).is_ok() {
        let temp = path.join("image.part");
        if std::fs::write(&temp, &bytes).is_ok() {
            let _ = std::fs::rename(&temp, path.join("image"));
        }
        let _ = std::fs::remove_file(&temp);
    }
    let hosted = match hosted {
        Some(value) => Some(value),
        None if host && publish.load(Ordering::Acquire) => match upload(&backend.http(), &bytes, animated).await {
            Ok(value) => {
                if let Ok(json) = serde_json::to_vec(&value) {
                    let _ = std::fs::write(location, json);
                }
                Some(value)
            }
            Err(err) => {
                log::debug!("discord artwork hosting: {err:#}");
                None
            }
        },
        None => None,
    };
    let _ = std::fs::File::options().write(true).open(path.join("image")).and_then(|f| f.set_modified(SystemTime::now()));
    if new {
        trim(path.parent().expect("cache entry has a parent"));
    }
    let ttl = hosted.as_ref().map_or(RETRY, |h| Duration::from_secs(h.expires.saturating_sub(now_secs())));
    let preview = if preview { tokio::task::spawn_blocking(move || make_preview(key, &bytes, animated)).await?.ok() } else { None };
    Ok((ResultImage { url: hosted.map(|h| h.url), preview }, ttl))
}

fn trim(dir: &Path) {
    let Ok(items) = std::fs::read_dir(dir) else { return };
    let mut entries = Vec::new();
    for item in items.flatten() {
        let path = item.path();
        if let Ok(meta) = std::fs::metadata(path.join("image")) {
            entries.push((meta.modified().unwrap_or(UNIX_EPOCH), meta.len(), path));
        } else {
            let _ = std::fs::remove_dir_all(path);
        }
    }
    entries.sort();
    let mut total: u64 = entries.iter().map(|(_, size, _)| size).sum();
    let excess = entries.len().saturating_sub(256);
    for (i, (_, size, path)) in entries.into_iter().enumerate() {
        if i >= excess && total <= MAX_CACHE {
            break;
        }
        if std::fs::remove_dir_all(path).is_ok() {
            total -= size
        }
    }
}

/// Locally embedded copies let the preview show exactly the same status badge offline.
pub fn badge_bytes(step: u8, paused: bool) -> &'static [u8] {
    macro_rules! badges {
        ($state:literal; $($step:literal),+) => {
            [$(include_bytes!(concat!("../site/discord/", $state, "-", $step, ".png")) as &[u8]),+]
        };
    }
    const PLAYING: [&[u8]; 21] =
        badges!("playing"; "00", "01", "02", "03", "04", "05", "06", "07", "08", "09", "10", "11", "12", "13", "14", "15", "16", "17", "18", "19", "20");
    const PAUSED: [&[u8]; 21] =
        badges!("paused"; "00", "01", "02", "03", "04", "05", "06", "07", "08", "09", "10", "11", "12", "13", "14", "15", "16", "17", "18", "19", "20");
    if paused { PAUSED[usize::from(step)] } else { PLAYING[usize::from(step)] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rotation_preserves_its_period_and_bounds_frame_count() {
        assert_eq!(spin_timing(8, 25), (200, 800));
        assert_eq!(spin_timing(20, 50), (400, 2000));
        let art = RgbaImage::from_fn(96, 96, |x, y| Rgba([x as u8, y as u8, 30, 255]));
        let gif = spin(&png(art).unwrap(), 2, 10).unwrap();
        let frames = GifDecoder::new(Cursor::new(&gif)).unwrap().into_frames().collect_frames().unwrap();
        assert_eq!(frames.len(), 20);
        assert_eq!(frames[0].delay().numer_denom_ms(), (100, 1));
        assert_eq!(frames[0].buffer().get_pixel(0, 0).0[3], 0);
        assert_ne!(frames[0].buffer(), frames[5].buffer());
        let p = make_preview("sample".into(), &gif, true).unwrap();
        assert_eq!(p.frames.last().unwrap().0, 2000);
    }

    #[test]
    fn spotify_and_apple_clip_containers_convert_to_a_timed_loop() {
        let flat = include_bytes!("../tests/fixtures/discord-canvas.mp4");
        let fragmented = include_bytes!("../tests/fixtures/discord-canvas-fragmented.mp4");
        for clip in [flat.as_slice(), fragmented.as_slice()] {
            let gif = canvas_gif(clip, 30, 96).unwrap();
            let p = make_preview("sample".into(), &gif, true).unwrap();
            assert!((6..=12).contains(&p.frames.len()));
            assert_eq!(p.frames.last().unwrap().0, 1000);
            assert_ne!(p.frames.first().unwrap().1, p.frames.last().unwrap().1);
        }
        assert!(canvas_gif(b"a bad clip", 30, 96).is_err());
    }

    #[test]
    fn cache_pruning_removes_old_entries_and_partial_work() {
        let dir = std::env::temp_dir().join(format!("bitchord-presence-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (name, size) in [("old", MAX_CACHE), ("new", 100), ("partial", 0)] {
            let path = dir.join(name);
            std::fs::create_dir_all(&path).unwrap();
            if size > 0 {
                let file = std::fs::File::create(path.join("image")).unwrap();
                file.set_len(size).unwrap();
                if name == "old" {
                    file.set_modified(UNIX_EPOCH).unwrap();
                }
            }
        }
        trim(&dir);
        assert!(!dir.join("old").exists());
        assert!(!dir.join("partial").exists());
        assert!(dir.join("new").join("image").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn playback_badges_keep_their_state_and_progress_while_the_progress_set_is_unpublished() {
        for step in 0..=20 {
            let playing = status_badge(step, 20, false, Some(STATUS_RENDERER));
            let paused = status_badge(step, 20, true, Some(STATUS_RENDERER));
            assert_eq!(playing.badge, Some((step as u8, false)));
            assert_eq!(paused.badge, Some((step as u8, true)));
            for picture in [&playing, &paused] {
                let url = url::Url::parse(picture.ipc_url(false).unwrap()).unwrap();
                assert!(url.as_str().starts_with(STATUS_RENDERER) && url.as_str().len() <= 300);
                assert!(url.query().is_none());
                let path = url.path_segments().unwrap().next_back().unwrap();
                let compressed = base64::engine::general_purpose::URL_SAFE.decode(path).unwrap();
                let diagram = String::from_utf8(miniz_oxide::inflate::decompress_to_vec_zlib(&compressed).unwrap()).unwrap();
                let paused = picture.badge.unwrap().1;
                assert!(diagram.contains(if paused { "Ⅱ" } else { "▶" }));
                if (1..20).contains(&step) {
                    assert!(diagram.contains(&format!(";0.{:02}:", step * 5)));
                }
            }
            assert_ne!(playing.ipc_url(false), paused.ipc_url(false));
            assert_eq!(playing.ipc_url(true), Some(PLAY_FALLBACK));
            assert_eq!(paused.ipc_url(true), Some(PAUSE_FALLBACK));
        }

        let published = status_badge(5, 10, true, Some(ASSETS));
        assert!(published.ipc_url(false).unwrap().ends_with("/paused-10.png"));
        assert_eq!(published.ipc_url(true), Some(PAUSE_FALLBACK));
        assert_eq!(status_badge(5, 10, true, None).ipc_url(false), Some(PAUSE_FALLBACK));
    }

    #[test]
    fn selected_file_is_saved_and_decoded_for_an_immediate_preview() {
        let dir = std::env::temp_dir().join(format!("bitchord-presence-custom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("selected.png");
        let pixels = RgbaImage::from_pixel(128, 96, Rgba([43, 196, 110, 255]));
        std::fs::write(&file, png(pixels).unwrap()).unwrap();
        let (key, preview) = save_custom(&dir, 1, &file).unwrap();
        assert_eq!(preview.key, key);
        assert_eq!(preview.frames.len(), 1);
        assert_eq!(preview.frames[0].1.pixels[0], egui::Color32::from_rgb(43, 196, 110));
        assert!(custom_dir(&dir, 1).join(format!("{key}.png")).is_file());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn all_progress_badges_are_embedded_and_play_pause_are_distinct() {
        for step in 0..=20 {
            assert_eq!(decode_image(badge_bytes(step, false)).unwrap().dimensions(), (96, 96));
            assert_ne!(badge_bytes(step, false), badge_bytes(step, true));
        }
        assert_ne!(badge_bytes(0, false), badge_bytes(20, false));
    }
}
