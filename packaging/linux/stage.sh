#!/bin/sh
# Installs the app into a staging root as /usr: binary, desktop file (PDF types, "Open
# with"), AppStream metainfo, icons and licence notices. Nothing else: thumbnails, menus
# and the default app are the user's choice in Settings > System integration. The
# AppImage, .deb and Flatpak are built from this tree.
#
#   packaging/linux/stage.sh BIN DESTDIR
set -eu
. "$(dirname "$0")/../identity.sh"
bin=$1
dest=$2
here=$ROOT/packaging/linux
icons=$ROOT/packaging/icons

install -Dm755 "$bin" "$dest/usr/bin/$APP_BIN"
mkdir -p "$dest/usr/share/applications" "$dest/usr/share/metainfo"
fill_template "$here/app.desktop.in" "$dest/usr/share/applications/$APP_ID.desktop"
fill_template "$here/app.metainfo.xml.in" "$dest/usr/share/metainfo/$APP_ID.metainfo.xml"
for s in 16 24 32 48 64 128 256 512; do
    install -Dm644 "$icons/app-$s.png" "$dest/usr/share/icons/hicolor/${s}x$s/apps/$APP_ID.png"
done
install -Dm644 "$icons/app.svg" "$dest/usr/share/icons/hicolor/scalable/apps/$APP_ID.svg"
doc=$dest/usr/share/doc/$APP_BIN
for f in README.md LICENSE-MIT LICENSE-APACHE THIRD-PARTY.md THIRD-PARTY-LICENSES.md; do
    install -Dm644 "$ROOT/$f" "$doc/$f"
done
install -Dm644 "$ROOT/apps/desktop/ui/fonts/OFL.txt" "$doc/Inter-OFL.txt"
install -Dm644 "$ROOT/crates/content/LICENSE-adobe-cmap" "$doc/LICENSE-adobe-cmap"
install -Dm644 "$ROOT/crates/typeset/LICENSE-hyphenation" "$doc/LICENSE-hyphenation"
