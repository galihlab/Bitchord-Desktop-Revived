//! What Settings › Storage shows and clears: what the app fetched and keeps only to be
//! quicker, all of which it fetches again when needed. Sign-in, settings, downloads,
//! the helpers playback runs on, and logs stay.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Folders in the data folder that are only a cache.
const DIRS: &[&str] = &[
    // Covers and thumbnails, kept so the next launch shows them at once.
    "thumbs",
    // Romanized and translated lyrics.
    "translations",
    // The browser profile the lyrics check runs in (its token, `lyrics_jwt`, stays).
    "webview-lyrics",
    // Animated covers and Spotify canvas clips.
    "motion",
    "discord-artwork-cache",
    // Downloaded updates: an installer, an unpacked build, the last install's log.
    "updates",
    // Error reports from versions up to 0.2.11.
    "rustypipe_reports",
];

/// The folder of downloaded updates, left alone while one downloads or waits to install.
const UPDATES: &str = "updates";

/// Bytes the cache takes up.
pub fn size(data_dir: &Path) -> u64 {
    DIRS.iter().map(|d| dir_size(&data_dir.join(d))).sum()
}

/// Delete the cache, keeping downloaded updates when `keep_updates`. Files in use (the
/// lyrics check's window, if it's open) are skipped. Returns the bytes freed.
pub fn clear(data_dir: &Path, keep_updates: bool) -> u64 {
    DIRS.iter().filter(|d| !(keep_updates && **d == UPDATES)).map(|d| clear_dir(&data_dir.join(d))).sum()
}

fn dir_size(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map_or(0, |m| m.len()),
            _ => 0,
        })
        .sum()
}

/// Everything in `dir`, but `dir` itself stays: what's running writes into it.
fn clear_dir(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    let mut freed = 0;
    for e in rd.flatten() {
        let path = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => {
                freed += clear_dir(&path);
                let _ = std::fs::remove_dir(&path);
            }
            Ok(_) => {
                let len = e.metadata().map_or(0, |m| m.len());
                if std::fs::remove_file(&path).is_ok() {
                    freed += len;
                }
            }
            Err(_) => {}
        }
    }
    freed
}

/// One disk the app keeps things on, for its own card at the top of the page. The data
/// folder's comes first; the downloads folder's follows when it sits on another one.
#[derive(Clone)]
pub struct Drive {
    /// What its card is called: the downloads folder's path, which is all the app
    /// keeps on that disk.
    pub label: String,
    /// Bytes on the disk, and what's free of them, as the OS reports it.
    pub total: u64,
    pub free: u64,
    /// What the app takes on this disk.
    pub app: u64,
}

impl Drive {
    /// The used room that isn't the app's, for the grey part of the overall bar.
    pub fn others(&self) -> u64 {
        self.total.saturating_sub(self.free).saturating_sub(self.app)
    }
}

/// How much room the app takes, in the parts the Storage page shows (Mochy's
/// `StorageUsage`, without the database it has and this app doesn't).
#[derive(Clone)]
pub struct Usage {
    /// Every disk the parts sit on.
    pub drives: Vec<Drive>,
    /// Covers, translations, the lyrics check's browser and downloaded updates: what
    /// clearing the cache empties.
    pub cache: u64,
    /// The download folder's songs.
    pub downloads: u64,
    /// Everything else in the data folder: sign-in, settings, logs, the solver.
    pub app_data: u64,
    /// The downloads folder this was measured with, so a new one is noticed.
    pub folder: PathBuf,
}

impl Usage {
    /// Everything the app takes, across the disks.
    pub fn total(&self) -> u64 {
        self.cache + self.downloads + self.app_data
    }
}

/// Whether both paths sit on the same disk, so the page shows one card.
fn same_disk(a: &Path, b: &Path) -> bool {
    // The two inodes' device ids say it outright.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (std::fs::metadata(a), std::fs::metadata(b)) {
            (Ok(a), Ok(b)) => a.dev() == b.dev(),
            _ => false,
        }
    }
    // No volume ids in std: the prefix a canonical path starts with (a drive, or a
    // share) is the disk, near enough to tell the two apart.
    #[cfg(windows)]
    {
        let root = |p: &Path| std::fs::canonicalize(p).ok().and_then(|c| c.components().next().map(|c| c.as_os_str().to_owned()));
        match (root(a), root(b)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }
}

/// Read the whole page's numbers. Disk work, for the measuring thread.
fn read_usage(data_dir: &Path, downloads: &Path) -> Usage {
    let cache = size(data_dir);
    let songs = dir_size(downloads);
    // A downloads folder picked inside the data folder is neither cache nor settings:
    // it has its own part of the breakdown.
    let inside = downloads.starts_with(data_dir);
    let app_data = dir_size(data_dir).saturating_sub(cache).saturating_sub(u64::from(inside) * songs);
    let (total, free) = (fs4::total_space(data_dir).unwrap_or(0), fs4::available_space(data_dir).unwrap_or(0));
    // The downloads folder gets the second card when it's on its own disk; its path is
    // all that names that disk from here.
    let elsewhere = std::fs::metadata(downloads).is_ok_and(|m| m.is_dir()) && !same_disk(data_dir, downloads);
    let drives = if elsewhere {
        let (t, f) = (fs4::total_space(downloads).unwrap_or(0), fs4::available_space(downloads).unwrap_or(0));
        vec![
            Drive { label: String::new(), total, free, app: cache + app_data },
            Drive { label: downloads.display().to_string(), total: t, free: f, app: songs },
        ]
    } else {
        vec![Drive { label: String::new(), total, free, app: cache + app_data + songs }]
    };
    Usage { drives, cache, downloads: songs, app_data, folder: downloads.to_owned() }
}

/// `bytes` the way Android's own storage screen writes them: powers of 1000, one
/// decimal past 10 (Mochy's `formatSize`).
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let (mut value, mut unit) = (bytes as f64, 0);
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    let number = if value >= 10.0 || unit == 0 { format!("{}", value.round() as u64) } else { format!("{value:.1}") };
    format!("{number} {}", UNITS[unit])
}

/// The page's numbers, measured and cleared off the UI thread.
#[derive(Default)]
pub struct Storage {
    inner: Arc<Mutex<Inner>>,
    /// When it was last measured (app time, s).
    measured: Option<f64>,
}

#[derive(Default)]
struct Inner {
    usage: Option<Usage>,
    /// While the cache or the downloads are being cleared, so nothing starts twice.
    busy: bool,
    /// What the last cache clear freed, until it's said.
    freed: Option<u64>,
    /// The last download removal's songs and bytes, until they're said.
    removed: Option<(usize, u64)>,
}

/// Measured again this often while Settings shows it: covers keep coming in.
const REMEASURE: f64 = 10.0;

impl Storage {
    /// The page's numbers, `None` until they're measured.
    pub fn usage(&mut self, ctx: &egui::Context, data_dir: &Path, downloads: &Path, now: f64) -> Option<Usage> {
        // A folder picked since the last measure is measured at once.
        let folder_moved = self.inner.lock().unwrap().usage.as_ref().is_some_and(|u| u.folder != downloads);
        if (self.measured.is_none_or(|at| now - at > REMEASURE) || folder_moved) && !self.busy() {
            self.measured = Some(now);
            let (inner, dir, folder, ctx) = (self.inner.clone(), data_dir.to_owned(), downloads.to_owned(), ctx.clone());
            std::thread::spawn(move || {
                let usage = read_usage(&dir, &folder);
                inner.lock().unwrap().usage = Some(usage);
                ctx.request_repaint();
            });
        }
        self.inner.lock().unwrap().usage.clone()
    }

    /// The numbers last measured, without measuring again.
    pub fn known(&self) -> Option<Usage> {
        self.inner.lock().unwrap().usage.clone()
    }

    pub fn busy(&self) -> bool {
        self.inner.lock().unwrap().busy
    }

    /// Delete the cache, then measure again. An update that's downloading or waiting
    /// to install keeps its files.
    pub fn clear(&mut self, ctx: &egui::Context, data_dir: &Path, downloads: &Path, keep_updates: bool) {
        let mut g = self.inner.lock().unwrap();
        if g.busy {
            return;
        }
        g.busy = true;
        drop(g);
        let (inner, dir, folder, ctx) = (self.inner.clone(), data_dir.to_owned(), downloads.to_owned(), ctx.clone());
        std::thread::spawn(move || {
            let freed = clear(&dir, keep_updates);
            let left = size(&dir);
            log::info!("storage: cleared the cache, {} freed, {} left", format_size(freed), format_size(left));
            let mut g = inner.lock().unwrap();
            (g.usage, g.freed, g.busy) = (Some(read_usage(&dir, &folder)), Some(freed), false);
            drop(g);
            ctx.request_repaint();
        });
    }

    /// Remove every downloaded song, then measure again.
    pub fn remove_downloads(&mut self, ctx: &egui::Context, data_dir: &Path, downloads: &Path) {
        let mut g = self.inner.lock().unwrap();
        if g.busy {
            return;
        }
        g.busy = true;
        drop(g);
        let (inner, dir, folder, ctx) = (self.inner.clone(), data_dir.to_owned(), downloads.to_owned(), ctx.clone());
        std::thread::spawn(move || {
            let (songs, bytes) = crate::downloads::remove_all_files();
            log::info!("storage: removed {songs} downloads, {} freed", format_size(bytes));
            let mut g = inner.lock().unwrap();
            (g.usage, g.removed, g.busy) = (Some(read_usage(&dir, &folder)), Some((songs, bytes)), false);
            drop(g);
            ctx.request_repaint();
        });
    }

    /// What the last cache clear freed, once.
    pub fn take_freed(&self) -> Option<u64> {
        self.inner.lock().unwrap().freed.take()
    }

    /// The last download removal's songs and bytes, once.
    pub fn take_removed(&self) -> Option<(usize, u64)> {
        self.inner.lock().unwrap().removed.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clears_the_cache_and_keeps_the_rest() {
        let dir = std::env::temp_dir().join("bitchord-cache-test");
        let _ = std::fs::remove_dir_all(&dir);
        for (path, len) in [
            ("thumbs/a", 100),
            ("webview-lyrics/EBWebView/Default/Cookies", 50),
            ("updates/bitchord-setup.exe", 1000),
            ("lyrics/song.lrc", 7),
            ("webview/Cookies", 9),
            ("app.ron", 3),
            ("lyrics_jwt", 2),
        ] {
            let p = dir.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, vec![0u8; len]).unwrap();
        }
        assert_eq!(size(&dir), 1150);
        assert_eq!(clear(&dir, true), 150);
        assert!(dir.join("updates/bitchord-setup.exe").exists());
        assert!(dir.join("thumbs").is_dir() && !dir.join("thumbs/a").exists());
        assert!(!dir.join("webview-lyrics/EBWebView").exists());
        assert_eq!(clear(&dir, false), 1000);
        assert_eq!(size(&dir), 0);
        for kept in ["lyrics/song.lrc", "webview/Cookies", "app.ron", "lyrics_jwt"] {
            assert!(dir.join(kept).exists(), "{kept}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sizes_read_plainly() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(999), "999 B");
        assert_eq!(format_size(1_500), "1.5 KB");
        // Just under and just over a rounding boundary, as the page writes them.
        assert_eq!(format_size(102_400_000), "102 MB");
        assert_eq!(format_size(102_760_448), "103 MB");
        assert_eq!(format_size(1_610_612_736), "1.6 GB");
    }

    #[test]
    fn usage_splits_into_its_parts() {
        let root = std::env::temp_dir().join(format!("bitchord-usage-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (data, songs) = (root.join("data"), root.join("music"));
        for (path, len) in [
            ("data/thumbs/a", 100),
            ("data/translations/t", 50),
            ("data/app.ron", 3),
            ("data/lyrics_jwt", 2),
            ("data/webview/Cookies", 9),
            ("music/song.opus", 1000),
        ] {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, vec![0u8; len]).unwrap();
        }
        let usage = read_usage(&data, &songs);
        assert_eq!(usage.cache, 150);
        assert_eq!(usage.downloads, 1000);
        assert_eq!(usage.app_data, 14);
        assert_eq!(usage.total(), 1164);
        // Both folders sit on the one disk the temp dir is on, so one card.
        assert_eq!(usage.drives.len(), 1);
        assert!(usage.drives[0].total > 0 && usage.drives[0].free > 0);
        assert_eq!(usage.drives[0].app, usage.total());
        assert_eq!(usage.folder, songs);
        let _ = std::fs::remove_dir_all(&root);
    }
}
