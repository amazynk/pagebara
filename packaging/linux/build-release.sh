#!/bin/sh
# Builds the Linux release binary for glibc 2.35 (Ubuntu 22.04, Debian 12 and newer) on any
# build machine, with cargo-zigbuild (zig links against that glibc's symbol versions), checks
# it, and packages it as .deb and AppImage.
#
#   packaging/linux/build-release.sh OUT_DIR
#
# Needs cargo-zigbuild and zig, plus what build-deb.sh and build-appimage.sh need. The
# release workflow builds on an ubuntu-22.04 runner instead, whose own glibc is 2.35.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
out=$1
glibc=2.35
target=x86_64-unknown-linux-gnu
cd "$root"
# The text detection and Latin recognition models go inside the binary (live text works
# offline without a download).
PAGEBARA_OCR_BUNDLE=$(sh "$root/packaging/ocr-bundle.sh")
export PAGEBARA_OCR_BUNDLE
cargo zigbuild --release --locked -p pagebara-app --target "$target.$glibc"
bin=${CARGO_TARGET_DIR:-$root/target}/$target/release/pagebara
sh "$here/check-glibc.sh" "$bin" "$glibc"
sh "$here/build-deb.sh" "$bin" "$out"
sh "$here/build-appimage.sh" "$bin" "$out"
