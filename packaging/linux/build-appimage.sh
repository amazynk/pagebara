#!/bin/sh
# Builds <name>-<version>-<arch>.AppImage from a release binary.
#
#   packaging/linux/build-appimage.sh BIN OUT_DIR
#
# Needs appimagetool on PATH (https://github.com/AppImage/appimagetool); set
# APPIMAGE_RUNTIME to a type-2 runtime file to build offline. The AppImage needs a glibc at
# least as new as the build machine's, so release builds run on the oldest supported distro.
# GPU drivers, libwayland, libxkbcommon and the X11 libraries are loaded from the host at
# run time (winit and the renderer dlopen them); nothing else needs bundling.
set -eu
. "$(dirname "$0")/../identity.sh"
bin=$1
out=$2
arch=$(uname -m)
work=$(mktemp -d "${TMPDIR:-/tmp}/appdir.XXXXXX")
trap 'rm -rf "$work"' EXIT
app=$work/AppDir

sh "$ROOT/packaging/linux/stage.sh" "$bin" "$app"
ln -s "usr/bin/$APP_BIN" "$app/AppRun"
cp "$app/usr/share/applications/$APP_ID.desktop" "$app/"
cp "$ROOT/packaging/icons/app-256.png" "$app/$APP_ID.png"
ln -s "$APP_ID.png" "$app/.DirIcon"
# appimagetool looks for the metainfo under the legacy name too.
cp "$app/usr/share/metainfo/$APP_ID.metainfo.xml" "$app/usr/share/metainfo/$APP_ID.appdata.xml"

mkdir -p "$out"
file=$out/$APP_NAME-$APP_VERSION-$arch.AppImage
set -- --no-appstream
[ -n "${APPIMAGE_RUNTIME:-}" ] && set -- "$@" --runtime-file "$APPIMAGE_RUNTIME"
ARCH=$arch appimagetool "$@" "$app" "$file"
echo "$file"
