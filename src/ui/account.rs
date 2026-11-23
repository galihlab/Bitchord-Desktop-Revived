//! The Google and Spotify sign-in windows, uploading songs to the account, and the
//! local music files picked (or dropped) for the Library's Uploads page.

use std::sync::atomic::Ordering;

use crate::backend::Req;

use super::App;

impl App {
    /// Open the system file picker for songs to upload; it runs on its own thread.
    pub(super) fn pick_uploads(&self, ctx: &egui::Context) {
        let (slot, ctx) = (self.picked_uploads.clone(), ctx.clone());
        std::thread::spawn(move || {
            let files = rfd::FileDialog::new().set_title("Upload music").add_filter("Audio", UPLOAD_EXTS).pick_files();
            if let Some(files) = files {
                *slot.lock().unwrap() = Some(files);
                ctx.request_repaint();
            }
        });
    }

    pub(super) fn upload_files(&mut self, files: Vec<std::path::PathBuf>) {
        let (ok, skipped): (Vec<_>, Vec<_>) = files.into_iter().partition(|f| {
            f.extension().and_then(|e| e.to_str()).is_some_and(|e| UPLOAD_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        });
        if !self.logged_in {
            self.toast("Sign in to upload music");
        } else if ok.is_empty() {
            if !skipped.is_empty() {
                self.toast("YouTube Music only takes MP3, M4A, FLAC, OGG and WMA files");
            }
        } else {
            self.toast(if ok.len() == 1 { "Uploading 1 song…".to_owned() } else { format!("Uploading {} songs…", ok.len()) });
            self.backend.request(Req::Upload(ok));
        }
    }

    /// Open the system file picker for local music. The files are read in (their tags
    /// and lengths) on the picker's own thread, so a big pick never holds a frame.
    pub(super) fn pick_local(&self, ctx: &egui::Context) {
        let (ctx, added) = (ctx.clone(), self.local_added.clone());
        std::thread::spawn(move || {
            if let Some(files) = rfd::FileDialog::new().set_title("Add local music").add_filter("Audio", LOCAL_EXTS).pick_files() {
                *added.lock().unwrap() = Some(crate::local::add(files));
                ctx.request_repaint();
            }
        });
    }

    /// The same for a whole folder: every audio file right in it (a folder a level deep
    /// says "this album"; walking a whole disk is what the file manager is for).
    pub(super) fn pick_local_folder(&self, ctx: &egui::Context) {
        let (ctx, added) = (ctx.clone(), self.local_added.clone());
        std::thread::spawn(move || {
            if let Some(dir) = rfd::FileDialog::new().set_title("Add a folder of music").pick_folder() {
                // `local::add` skips what isn't audio, so the folder is handed over as is.
                let files = std::fs::read_dir(&dir).map(|d| d.filter_map(|e| e.ok().and_then(|e| e.path().is_file().then_some(e.path()))).collect()).unwrap_or_default();
                *added.lock().unwrap() = Some(crate::local::add(files));
                ctx.request_repaint();
            }
        });
    }

    /// Files dropped on the window: local music, read in on their own thread. They play
    /// from where they are — which is what dropping them in asks for, and what keeps a
    /// FLAC a FLAC rather than a YouTube Music upload of one.
    pub(super) fn add_local(&self, ctx: &egui::Context, files: Vec<std::path::PathBuf>) {
        let (ctx, added) = (ctx.clone(), self.local_added.clone());
        std::thread::spawn(move || {
            *added.lock().unwrap() = Some(crate::local::add(files));
            ctx.request_repaint();
        });
    }

    /// The local-add toast, set by whichever thread finished reading files in.
    pub(super) fn take_local_added(&self) -> Option<usize> {
        self.local_added.lock().unwrap().take()
    }

    pub(super) fn start_spotify_sign_in(&self, ctx: &egui::Context) {
        let xfer = self.transfer.clone();
        if xfer.signing_in.swap(true, Ordering::Relaxed) {
            return;
        }
        let (backend, ctx) = (self.backend.clone(), ctx.clone());
        crate::login::spawn(crate::login::Site::Spotify, xfer.spotify_profile(), move |outcome| {
            match outcome {
                crate::login::Outcome::Cookie(c) => xfer.set_cookie(Some(c), &backend),
                // A token is the Discord window's outcome; this one never returns one.
                crate::login::Outcome::Token(_) => {}
                crate::login::Outcome::Cancelled => {}
                crate::login::Outcome::Failed(e) => backend.report_error(format!("Spotify sign-in window failed: {e}")),
            }
            xfer.signing_in.store(false, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    pub(super) fn start_sign_in(&self, ctx: &egui::Context) {
        if self.signing_in.swap(true, Ordering::Relaxed) {
            return;
        }
        let (backend, busy, ctx) = (self.backend.clone(), self.signing_in.clone(), ctx.clone());
        crate::login::spawn(crate::login::Site::Google, self.backend.login_profile(), move |outcome| {
            match outcome {
                crate::login::Outcome::Cookie(c) => backend.request(Req::SetCookie(c)),
                // A token is the Discord window's outcome; this one never returns one.
                crate::login::Outcome::Token(_) => {}
                crate::login::Outcome::Cancelled => {}
                crate::login::Outcome::Failed(e) => backend.report_error(format!("Sign-in window failed: {e}")),
            }
            busy.store(false, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }
}

/// File types YouTube Music accepts for upload.
const UPLOAD_EXTS: &[&str] = &["mp3", "m4a", "flac", "ogg", "wma"];

/// What local music plays (see [`crate::local`]): the audio rodio opens, which is one
/// list shorter than YouTube Music's — nothing here decodes WMA.
const LOCAL_EXTS: &[&str] = &["mp3", "m4a", "mp4", "m4b", "aac", "flac", "ogg", "oga", "wav", "webm", "mka", "mkv"];
