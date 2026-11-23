# AGENTS.md

## What this is

BitChord Desktop Revived (`bitchord`) is a native desktop music client in
Rust — `egui`/`eframe` 0.37 for the UI, `rodio`/`symphonia` for audio, an
opt-in `wgpu` renderer, embedded `wry` WebViews for sign-ins. It plays from
YouTube Music and the fork's own sources (JioSaavn, MonoServers, addons),
Dolby Atmos from hi-res addons, and has Listen Together / jam sync through
its own relay server (`server/`).

It is a **daughter fork of [ytm-rs](https://github.com/grapedjuice/ytm-rs)**
and a sister fork of [BitChord-PC-Continued](https://github.com/jackychan4891/BitChord-PC-Continued) the style of the fork has to match Bitchord-PC-Continued rather than ytm-rs

most code comes from upstream, and new upstream commits get ported in
regularly. The fork's own features sit on top and must never be lost.

**Commit as:** `LoserDenji <loserdenji@proton.me>`

Work is handed over as patches: commit on top of the branch, then
`git format-patch -1 <sha>` (applied on the receiving end with `git am`).
One logical change per commit; the repo's message style is
`feat:`/`fix:`/`perf:`/`port:` lowercase prefixes, ported commits titled
`Port ytm-rs <sha>…`.

---

## Build & test

### System dependencies (Linux)

CI (ubuntu-24.04) installs:
```
libgtk-3-dev libwebkit2gtk-4.1-dev libasound2-dev \
libayatana-appindicator3-dev libdbus-1-dev libssl-dev pkg-config
```

With sudo, that's all you need. The rest of this section is the no-sudo
case (a container, a hosted workspace, a bare runner), from nothing:

1. the one-paste bootstrap below — a local prefix, `~/gtk-local`
2. the exports below it — every shell that builds or runs the app
3. `cargo check --tests` — the quick loop
4. to see it run: *Running the app headless*, two sections down

### The no-sudo bootstrap (one paste, a few minutes)

```bash
mkdir -p ~/gtk-local /tmp/gtk-debs && cd /tmp/gtk-debs
apt-get download libgtk-3-dev libwebkit2gtk-4.1-dev libasound2-dev \
  libwebkit2gtk-4.1-0 libjavascriptcoregtk-4.1-0 libjavascriptcoregtk-4.1-dev \
  libsoup-3.0-0 libsoup-3.0-dev libepoxy-dev libatk1.0-dev \
  libatk-bridge2.0-dev libpango1.0-dev libcairo2-dev libgdk-pixbuf-2.0-dev \
  libharfbuzz-dev libglib2.0-dev libdbus-1-dev libssl-dev \
  libfreetype-dev libfontconfig1-dev libffi-dev libpcre2-dev \
  libwayland-dev libxkbcommon-dev libx11-dev libxext-dev libxi-dev \
  libxrandr-dev libxcursor-dev libxfixes-dev libxcomposite-dev \
  libxdamage-dev libxinerama-dev libxtst-dev libxrender-dev libxcb1-dev \
  libxcb-render0-dev libxcb-shm0-dev libxau-dev libxdmcp-dev \
  libpixman-1-dev libpng-dev zlib1g-dev
for d in *.deb; do dpkg-deb -x "$d" ~/gtk-local/; done
cd ~/gtk-local/usr/lib/x86_64-linux-gnu
# the linker wants a plain libfoo.so for every libfoo.so.N
for f in *.so.*; do b=$(echo "$f" | sed -E 's/\.so\..*$//'); [ -e "$b.so" ] || ln -sf "$f" "$b.so"; done
# the deb list skips the runtime packages: stand the system's own .so.N in
for lib in gtk-3 atk-1.0 gdk-3 asound epoxy harfbuzz pango-1.0 cairo \
           gdk_pixbuf-2.0 gio-2.0 gobject-2.0 glib-2.0; do
  s=$(ls /usr/lib/x86_64-linux-gnu/lib$lib.so.* 2>/dev/null | head -1)
  [ -e "lib$lib.so" ] || [ -z "$s" ] || ln -sf "$s" "lib$lib.so"
done
# a symlink left dangling points at a version this prefix doesn't carry
for l in $(find . -maxdepth 1 -type l); do
  [ -e "$l" ] || { s=$(ls /usr/lib/x86_64-linux-gnu/$(basename "$l").* 2>/dev/null | head -1); [ -n "$s" ] && ln -sf "$s" "$l"; }
done
rm -f ./*.a   # static archives out, so the linker takes the shared ones
# stub .pc files for the packages several GTK .pc files Require but no deb
# here provides (versions only need to satisfy the Requires lines). The
# webkit/jsc .pc files come from their -dev debs above: don't stub those,
# their -I cflags are needed to compile against the webview headers.
PC=~/gtk-local/usr/lib/x86_64-linux-gnu/pkgconfig
for spec in "egl 1.4" "gl 1.2" "cloudproviders 0.3" "atspi-2 2.56" "libsystemd 250"; do
  set -- $spec
  printf 'prefix=/usr\nName: %s\nDescription: stub for the local prefix\nVersion: %s\nLibs:\nCflags:\n' "$1" "$2" > "$PC/$1.pc"
done
```

If the built app later misses a runtime library at launch, the loader names
it: `apt-get download <the package that ships it>`, `dpkg-deb -x` it into
`~/gtk-local`, re-run the `.so` loop above.

### The exports (every shell)

Drop these in a file (`env.sh`, outside the repo) and `source` it in each
shell that builds or runs the app:

```bash
export PKG_CONFIG_PATH=$HOME/gtk-local/usr/lib/x86_64-linux-gnu/pkgconfig:$HOME/gtk-local/usr/share/pkgconfig
export PKG_CONFIG_SYSROOT_DIR=$HOME/gtk-local
export RUSTFLAGS="-L $HOME/gtk-local/usr/lib/x86_64-linux-gnu -L /usr/lib/x86_64-linux-gnu \
  -C link-arg=-Wl,-rpath,$HOME/gtk-local/usr/lib/x86_64-linux-gnu"
export LD_LIBRARY_PATH=$HOME/gtk-local/usr/lib/x86_64-linux-gnu:/usr/lib/x86_64-linux-gnu
```

Two ways these quietly break, both worth knowing up front:

- `$HOME`, never `~` — a tilde inside double quotes doesn't expand, and the
  flags end up pointing at a directory literally named `~`.
- Keep them **byte-identical in every shell** — cargo hashes `RUSTFLAGS`
  into its fingerprints, so any change recompiles the world.

### Commands

```bash
cargo check              # type-check — fast, needs no .so files
cargo check --tests      # also type-check the tests
cargo test --no-run      # compile tests — needs the .so files above
cargo test               # compile and run everything
cargo test module_name:: # one module's tests
cargo build              # target/debug/bitchord — run with the exports' LD_LIBRARY_PATH
```

`cargo check --tests` is the quick loop for fixing compile errors: it
skips linking, so it works even without the runtime libraries.

Tests that need the network, a signed-in account, or `YTM_*` env vars are
`#[ignore]`d — plain `cargo test` is offline and hermetic. Run one with
`cargo test -- --ignored <name> --nocapture` (several want `--release`);
each one's doc comment says what it needs.

### CI

`.github/workflows/check.yml` runs `cargo test` and `cargo build --release`
on Linux, Windows and macOS. Warnings never fail the build — only errors
do. Windows and macOS need no system packages, so a change that compiles
locally on Linux is nearly always fine there too; what differs is
`#[cfg(windows)]`/`#[cfg(target_os = "macos")]` code, which the compiler
still type-checks for the local target only.

---

## Running the app headless

The app needs a GL context, not a desktop. Xvfb (the usual `xvfb` package)
provides one; Mesa's llvmpipe draws it in software, so no GPU either.

```bash
Xvfb :99 -screen 0 1920x1080x24 &   # if one isn't already up; screen ≥ window
DISPLAY=:99 YTM_BACKGROUND=1 YTM_DATA_DIR=/tmp/profile YTM_SIZE=1400x900 \
  YTM_MUTE=1 ./target/debug/bitchord
```

- `YTM_BACKGROUND` opens the window off-screen, unfocused, off the taskbar,
  and stops eframe from persisting window state — the run leaves no trace
  in the profile.
- `YTM_DATA_DIR` is a separate profile: settings in `<dir>/app.ron`, the
  log in `<dir>/bitchord.log` (rotated to `bitchord.prev.log`), renderer
  choice in `<dir>/renderer`, caches and sign-ins beside them. A fresh
  empty dir is a signed-out first launch — the state screenshots want.
  Without it, everything lands in `~/.local/share/ytm-rs`.
- `YTM_SIZE=WxH` sets the window in points (background mode defaults to
  1920x1057; first-launch default is 1200x780, minimum 760x480). The Xvfb
  screen must be at least as big as the window.
- **One copy per user.** The single-instance socket is per-user, not
  per-profile: if any copy is already running, a new launch hands over and
  exits silently — check `pgrep -f target/debug/bitchord` and kill strays
  first. This bites even with different `YTM_DATA_DIR`s.
- Renderer: OpenGL by default; the choice is cached in `<dir>/renderer`
  (`opengl`/`wgpu`). Under Xvfb the log's `gl:` line names llvmpipe —
  that's normal. A failed wgpu start falls back to OpenGL and rewrites the
  file itself.
- Log warnings that are normal on a headless box, not breakage: no D-Bus
  (media controls), no AppIndicator (tray icon), XRandR 0mm, XSETTINGS.
- CLI flags: `--show` (raise the running copy), `--no-tray`. The
  `login`/`turnstile` first-arg forms are internal webview child processes.
- No sound device only logs. `YTM_MUTE=1` starts silent and never persists
  the volume.

---

## Screenshots

The app shoots **its own framebuffer** (`egui` screenshots) — no OS window
capture, no compositor; works headless under Xvfb.

- `YTM_SCREENSHOT=out.png` — one shot after `YTM_SCREENSHOT_AT` seconds
  (default 8; the wait lets shelves/covers load).
- `YTM_SCREENSHOT_BURST=<n>` (with it) — keeps the n frames from that
  moment in memory, then writes `<stem>_<i>_<ms>.png`, ms being each
  frame's time: a filmstrip of an animation and its pacing.
- `YTM_THEME_TOUR=<step-secs>` (with `YTM_SCREENSHOT`) — steps through
  every theme preset, saving `<stem>_<n>_<id>.png` for each, then quits;
  `YTM_TOUR_ONLY=id,id` limits the tour.
- Inside a `YTM_SCRIPT` (below), `<t>:s:name` saves
  `$YTM_SCRIPT_DIR/name.png` at that moment.

This exact sequence runs end to end — fresh profile, Settings page, one
shot, clean exit (with the exports in the shell):

```bash
Xvfb :99 -screen 0 1500x1000x24 &    # if not already up
mkdir -p /tmp/shots /tmp/profile
DISPLAY=:99 YTM_DATA_DIR=/tmp/profile YTM_BACKGROUND=1 YTM_SIZE=1400x900 \
  YTM_PAGE=settings YTM_SCRIPT_DIR=/tmp/shots YTM_SCRIPT='5:s:settings;5.5:q' \
  ./target/debug/bitchord     # exits 0; the shot is /tmp/shots/settings.png
```

---

## Driving the UI (no mouse, no keyboard)

`YTM_SCRIPT="<secs>:<step>;…"` plays a script against the window, times in
seconds from the first frame. Steps (see `src/ui/dev.rs`):

| step | does |
| --- | --- |
| `m:x,y` | move the pointer to x,y (points, from the window's top-left) |
| `c:x,y` | click at x,y |
| `r:w,h` | resize the window |
| `t:text` | type text |
| `f:n` | touchpad flick of n px down (scroll), sent the way Wayland sends it |
| `s:name` | screenshot to `$YTM_SCRIPT_DIR/name.png` |
| `q` | quit |

- `YTM_PROBE=1` logs, each time the pointer stops at an `m:x,y`, whether a
  press there would hit the title bar and how many widgets sit under it —
  the way to find coordinates for a script. Take a screenshot, measure,
  probe, click.
- Animations need time: shots land after the number-eases finish, so put
  `s:` steps a second or two after the `c:` that triggers them.

State set up before the first frame (read in `src/ui/mod.rs` / `main.rs`):

- `YTM_PAGE=home|new|explore|library|uploads|liked|history|settings|transfer|jam|album:<id>|mood:<id>|artist:<id>|playlist:<id>|search:<q>` — the page to open at launch.
- `YTM_SEARCH=<q>` only searches; `YTM_SMOKE=<q>` searches and plays the
  first song; `YTM_SMOKE=id:<videoId>,…` queues exactly those (no search);
  `YTM_VIDEO` shows them as video. Playing needs the network.
- `YTM_NOWPLAYING` opens the now-playing overlay; `YTM_NP_TAB=comments|upnext|art` its tab; `YTM_SCROLL=<y>` pre-scrolls a page.
- `YTM_APPEARANCE`/`YTM_FONT` override the theme settings;
  `YTM_NO_SHADER` replaces the animated background with the flat gradient
  (deterministic shots).
- `YTM_UPLOAD=<file>` uploads that file as if picked;
  `YTM_PROXY_INPUT=<address>` opens Settings › Network on Custom with it
  typed in.

The rest of the `YTM_*` hooks (fail injection `YTM_STREAM_FAIL`,
`YTM_LYRICS_FAIL`, `YTM_DEVICE_LOST`; pacing `YTM_FPS`; memory `YTM_MEM`;
jam `YTM_JAM_*`; transfer `YTM_TRANSFER_*`; updates `YTM_UPDATE_*`…):
`rg 'YTM_' src/` lists every one, documented where it's read. The
screenshot/script/probe machinery itself is `src/ui/dev.rs`.

---

## Repo map

- `src/ui/mod.rs` — `App`, the update loop, launch state and dev hooks.
- `src/ui/pages.rs` — the pages, including the Settings sections.
- `src/ui/` — `nowplaying.rs` (the big overlay), `mini.rs`, `karaoke.rs`
  (lyrics), `palette.rs` (command palette), `storage.rs` (Settings ›
  Storage), `dialogs.rs` (confirm cards), `widgets.rs`.
- `src/backend.rs` — network/backend state machine; `src/ytm.rs` /
  `src/ytmapi.rs` the YouTube Music API; `src/cache.rs` storage
  measure/clear; `src/downloads.rs` the download index;
  `src/audio.rs` rodio output; `src/theme.rs` theming and fonts;
  `src/preset.rs` theme presets; `src/login.rs` the webview sign-ins.
- `server/` the jam relay (separate crate, with its own smoke tests);
  `site/` the website; `docs/readme` the README images;
  `installer/`, `linux/`, `macos/` packaging; `flake.nix` the Nix build.
- `vendor/souvlaki` — a patched copy of the media-controls crate, wired in
  through `[patch]` in `Cargo.toml` (its Linux fixes live there, not on
  crates.io — see the notes in its `mpris/dbus/controls.rs`).

A new Settings section must be added to `SECTIONS` in
`src/ui/palette.rs` **and** drawn with `(ui, "Name")` in one of the files
the `sections_are_in_settings` test `include_str!`s — that test is the
checklist; it fails naming exactly what's missing.

---

## Porting from upstream ytm-rs

1. Read the upstream diff: `git -C <ytm-rs> show <commit> -- src/`.
2. Never copy a whole file over the fork's version — the fork's files carry
   BitChord features upstream doesn't have. Apply the change surgically.
3. A cherry-pick's conflicts resolved to the upstream side is the classic
   way features get silently lost; when in doubt, keep the fork's hunk and
   re-apply upstream's change on top of it.
4. Upstream commits assume every *earlier* upstream commit is already in.
   When a ported change references something the fork doesn't have
   (`crate::backdrop`, `find::ListFind`, a struct field…), port the
   supporting piece too, never leave a stub.
5. Prefer commit messages like `fix/feat/port: <upstream message>`
   for hand-ported work; a bare cherry-pick with the upstream message is a
   sign the conflicts weren't resolved by hand.

---

## Code style

**Match the surrounding code.** Follow the naming, structure and comment
style of the files you are touching. Don't reformat code you aren't
changing — no drive-by rewraps, no import sorting, no `cargo fmt` over
whole files. The diff should read as only the change that was asked for.

**Don't trade features for speed.** Performance work keeps every existing
animation and visual. If a change alters anything visible, A frame is only
"spared" when nothing the listener sees is lost.

**Comments say why, not what.** A comment earns its line by explaining
something the code can't: the reason for a wait, a guard, a reorder.
Read like this:

```rust
// wait until the cover is in, otherwise the open spring jumps
let cover = app.cover_loaded(&song);

// paused while the player covers the page, so the bars stop eating frames
let playing = is_playing && !obscured;
```

Not like this:

```rust
/// NOTE: This function is responsible for retrieving the `cover` resource
/// and subsequently performing the initialization routine.
```

The second kind restates the code and rots the moment the code moves;
the first kind is the reason the line exists.
