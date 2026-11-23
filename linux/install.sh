#!/bin/sh
# Install bitchord for this user: the app in ~/.local/lib/bitchord, a `bitchord` command in
# ~/.local/bin, and a menu entry with its icon. Installed this way, the app updates
# itself. Libraries it needs that are missing get installed with the system's package
# manager (that step asks for your password).
#
# Run it from the extracted download, or straight from the web, which fetches the
# latest release first:
#   curl -fsSL https://raw.githubusercontent.com/LoserDenji/Bitchord-Desktop-Revived/main/linux/install.sh | sh
# `install.sh --uninstall` removes it again.
set -e

data=${XDG_DATA_HOME:-$HOME/.local/share}
lib=$HOME/.local/lib/bitchord
bin=$HOME/.local/bin
desktop=$data/applications/bitchord.desktop
icon=$data/icons/hicolor/256x256/apps/bitchord.png

refresh() {
    update-desktop-database -q "$data/applications" 2>/dev/null || true
    gtk-update-icon-cache -q -t "$data/icons/hicolor" 2>/dev/null || true
}

if [ "$1" = "--uninstall" ]; then
    rm -rf "$lib"
    rm -f "$desktop" "$icon"
    [ -L "$bin/bitchord" ] && rm -f "$bin/bitchord"
    refresh
    echo "bitchord is uninstalled. Settings are still in $data/ytm-rs, and sign-ins in your keyring."
    exit 0
fi

# The files to install: beside this script, or (run from the web) the latest release.
here=$(cd "$(dirname "$0")" 2>/dev/null && pwd || true)
if [ ! -f "$here/bitchord" ]; then
    repo=LoserDenji/Bitchord-Desktop-Revived
    arch=$(uname -m)
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    url=$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" | grep -o "https://[^\"]*/bitchord-[0-9][^\"/]*-linux-$arch\.tar\.gz" | head -n 1)
    [ -n "$url" ] || { echo "No Linux $arch download in the latest release."; exit 1; }
    echo "Downloading $(basename "$url")"
    curl -fL# "$url" | tar -xz -C "$tmp"
    here=$(dirname "$(find "$tmp" -name bitchord -type f | head -n 1)")
fi

mkdir -p "$lib" "$bin" "$data/applications" "$(dirname "$icon")"
# Put the new binary next to the old one and rename it over, so a copy that's
# running keeps going.
cp "$here/bitchord" "$lib/bitchord.new"
chmod 755 "$lib/bitchord.new"
mv -f "$lib/bitchord.new" "$lib/bitchord"
cp "$here/install.sh" "$lib/install.sh"
chmod 755 "$lib/install.sh"
echo "Installed by install.sh. This file lets bitchord update itself." > "$lib/install.txt"
ln -sf "$lib/bitchord" "$bin/bitchord"
sed "s|^Exec=bitchord|Exec=$lib/bitchord|" "$here/bitchord.desktop" > "$desktop"
cp "$here/bitchord.png" "$icon"
refresh

# Libraries: WebKitGTK (sign-in, lyrics), GTK, the tray's appindicator and ALSA. And
# desktop-file-utils: browsers find the app for `ytm-rs://` links in its MIME cache.
missing=$(ldd "$lib/bitchord" 2>/dev/null | awk '/not found/ { print $1 }' | tr '\n' ' ')
command -v update-desktop-database >/dev/null || missing="${missing}desktop-file-utils "
# (Loaded at run time, so ldd doesn't list it. ldconfig is in /sbin, off a user's PATH on Debian.)
ldconfig=$(command -v ldconfig || echo /sbin/ldconfig)
appindicator=$("$ldconfig" -p 2>/dev/null | grep -c 'libayatana-appindicator3\|libappindicator3' || true)
if [ "$appindicator" = 0 ]; then
    missing="${missing}libayatana-appindicator3.so.1 (the tray icon) "
fi
if [ -n "$missing" ]; then
    sudo=""
    [ "$(id -u)" = 0 ] || sudo=sudo
    echo "bitchord needs these, which aren't installed: $missing"
    installed=1
    if [ -e /run/ostree-booted ] && command -v rpm-ostree >/dev/null; then
        # Fedora Atomic (Silverblue, Kinoite, Bazzite...): dnf can't change the system,
        # packages are layered on it with rpm-ostree, which takes a restart.
        echo "On this system they're added with rpm-ostree, then a restart: for the tray icon,"
        echo "  rpm-ostree install libayatana-appindicator-gtk3"
        echo "(The others come from webkit2gtk4.1, gtk3, alsa-lib and desktop-file-utils.)"
    elif command -v apt-get >/dev/null; then
        $sudo apt-get install -y libwebkit2gtk-4.1-0 libgtk-3-0 libayatana-appindicator3-1 libasound2 desktop-file-utils || installed=0
    elif command -v dnf >/dev/null; then
        $sudo dnf install -y webkit2gtk4.1 gtk3 libayatana-appindicator-gtk3 alsa-lib desktop-file-utils || installed=0
    elif command -v pacman >/dev/null; then
        $sudo pacman -S --needed --noconfirm webkit2gtk-4.1 gtk3 libayatana-appindicator alsa-lib desktop-file-utils || installed=0
    elif command -v zypper >/dev/null; then
        $sudo zypper --non-interactive install libwebkit2gtk-4_1-0 libgtk-3-0 libayatana-appindicator3-1 alsa desktop-file-utils || installed=0
    else
        installed=0
    fi
    if [ "$installed" = 0 ]; then
        echo "Couldn't install them: install them with your package manager, then open bitchord."
    fi
    # The MIME cache, now that update-desktop-database may be there.
    refresh
fi

echo "bitchord is installed. Open BitChord from your app menu, or run bitchord."
echo "To uninstall: $lib/install.sh --uninstall"
