#!/bin/sh
# Builds a Flatpak bundle (<name>-<version>.flatpak) from a release binary.
#
#   packaging/linux/build-flatpak.sh BIN OUT_DIR
#
# Needs flatpak-builder and the runtimes:
#   flatpak install flathub org.freedesktop.Platform//25.08 org.freedesktop.Sdk//25.08
set -eu
. "$(dirname "$0")/../identity.sh"
bin=$1
out=$(mkdir -p "$2" && cd "$2" && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/flatpak.XXXXXX")
trap 'rm -rf "$work"' EXIT
sh "$ROOT/packaging/linux/stage.sh" "$bin" "$work/stage"
fill_template "$ROOT/packaging/linux/flatpak.yml.in" "$work/$APP_ID.yml"
flatpak-builder --force-clean --state-dir="$work/state" \
    --repo="$work/repo" \
    "$work/build" "$work/$APP_ID.yml"
flatpak build-bundle "$work/repo" "$out/$APP_NAME-$APP_VERSION.flatpak" "$APP_ID"
echo "$out/$APP_NAME-$APP_VERSION.flatpak"
