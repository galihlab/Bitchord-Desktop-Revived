//! The user's own music sources — the addon links above all — in a file of their
//! own beside `app.ron`, written by temp-and-rename. `app.ron` is eframe's, and
//! eframe rewrites it whole every half-minute and at exit; a process caught mid-
//! write (the updater's installer closing the app among them) left it half-written,
//! which the next launch read as the sources wiped. A file that only ever appears
//! under its whole name can't be caught half-written.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The settings that name the user's sources, as the strings `app.ron` kept them:
/// one link per line, so the file stays what a text editor can fix.
#[derive(Default, Clone, Serialize, Deserialize)]
pub struct Sources {
    /// The addon links, as Settings lists them.
    pub hires_source: String,
    /// The links switched off, that survive reordering and removals.
    pub hires_disabled: String,
}

const FILE: &str = "sources.json";

/// What's on disk, if it's there and reads.
pub fn load(dir: &Path) -> Option<Sources> {
    let bytes = std::fs::read(dir.join(FILE)).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(s) => Some(s),
        Err(e) => {
            log::warn!("sources: {FILE} is unreadable ({e}); falling back to app.ron's copy");
            None
        }
    }
}

/// Write it so a kill can't catch it half-written: the whole file lands under a
/// temp name, and the rename over the real one is atomic on every platform.
pub fn store(dir: &Path, sources: &Sources) {
    let path = dir.join(FILE);
    let json = serde_json::to_vec(sources).unwrap_or_default();
    let tmp = dir.join(format!("{FILE}.tmp"));
    if let Err(e) = std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, &path)) {
        log::warn!("sources: couldn't save {FILE}: {e}");
    }
}
