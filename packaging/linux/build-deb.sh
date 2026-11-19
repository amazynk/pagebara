#!/bin/sh
# Builds <bin>_<version>_<arch>.deb from a release binary with dpkg-deb. (cargo-deb would
# need the package name and file names repeated in Cargo.toml; this keeps them in
# crates/support/identity.)
#
#   packaging/linux/build-deb.sh BIN OUT_DIR
#
# Needs dpkg-deb, and dpkg-shlibdeps (dpkg-dev) for the library dependencies.
set -eu
. "$(dirname "$0")/../identity.sh"
bin=$1
out=$2
arch=$(dpkg --print-architecture)
work=$(mktemp -d "${TMPDIR:-/tmp}/deb.XXXXXX")
trap 'rm -rf "$work"' EXIT
root=$work/root

sh "$ROOT/packaging/linux/stage.sh" "$bin" "$root"
# dpkg-shlibdeps wants a debian/control next to the binary it inspects.
mkdir -p "$work/debian"
printf 'Source: %s\n\nPackage: %s\nArchitecture: any\n' "$APP_BIN" "$APP_BIN" >"$work/debian/control"
libs=$(cd "$work" && dpkg-shlibdeps -O "root/usr/bin/$APP_BIN" | sed -n 's/^shlibs:Depends=//p')
size=$(du -sk "$root" | cut -f1)

mkdir -p "$root/DEBIAN"
cat >"$root/DEBIAN/control" <<END
Package: $APP_BIN
Version: $APP_VERSION
Architecture: $arch
Maintainer: $APP_PUBLISHER <help@amazynk.com>
Installed-Size: $size
Depends: ${libs:-libc6}
Recommends: libxkbcommon0, libwayland-client0, libgl1, cups-client, speech-dispatcher
Section: text
Priority: optional
Homepage: $APP_HOMEPAGE
Description: $APP_SUMMARY
 A fast PDF viewer and editor that keeps documents on your computer: read,
 search, annotate, fill in forms, sign, redact and reorganise PDF files.
 No account, no cloud, no telemetry.
END
cat >"$root/DEBIAN/copyright" <<END
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: $APP_NAME
Source: $APP_REPOSITORY

Files: *
Copyright: $APP_PUBLISHER
License: MIT or Apache-2.0
 See /usr/share/doc/$APP_BIN/LICENSE-MIT and /usr/share/doc/$APP_BIN/LICENSE-APACHE.
 Third-party notices: /usr/share/doc/$APP_BIN/THIRD-PARTY.md and
 /usr/share/doc/$APP_BIN/THIRD-PARTY-LICENSES.md.
END
mv "$root/DEBIAN/copyright" "$root/usr/share/doc/$APP_BIN/copyright"

mkdir -p "$out"
file=$out/${APP_BIN}_${APP_VERSION}_$arch.deb
dpkg-deb --root-owner-group -Zxz --build "$root" "$file" >/dev/null
echo "$file"
