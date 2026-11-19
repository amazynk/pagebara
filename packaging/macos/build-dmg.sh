#!/bin/sh
# Builds <name>.app and <name>-<version>-<arch>.dmg from a release binary (macOS only).
#
#   packaging/macos/build-dmg.sh BIN OUT_DIR
#
# BIN may be a universal binary (lipo -create of the x86_64 and aarch64 builds).
#
# Signing and notarisation run only when their CI secrets are set; no certificate exists yet:
#   MACOS_CERT_P12, MACOS_CERT_PASSWORD   base64 Developer ID Application .p12 and its password
#   MACOS_SIGN_IDENTITY                   e.g. "Developer ID Application: NAME (TEAMID)"
#   MACOS_NOTARY_APPLE_ID, MACOS_NOTARY_TEAM_ID, MACOS_NOTARY_PASSWORD (app-specific password)
set -eu
. "$(dirname "$0")/../identity.sh"
bin=$1
out=$2
arch=$(lipo -archs "$bin" 2>/dev/null | tr ' ' '-' || uname -m)
mkdir -p "$out"
app=$out/$APP_NAME.app
rm -rf "$app"

install -d "$app/Contents/MacOS" "$app/Contents/Resources/licences"
install -m755 "$bin" "$app/Contents/MacOS/$APP_BIN"
fill_template "$ROOT/packaging/macos/Info.plist.in" "$app/Contents/Info.plist"
printf 'APPL????' >"$app/Contents/PkgInfo"
cp "$ROOT/packaging/icons/app.icns" "$app/Contents/Resources/app.icns"
for f in LICENSE-MIT LICENSE-APACHE THIRD-PARTY.md THIRD-PARTY-LICENSES.md; do
    cp "$ROOT/$f" "$app/Contents/Resources/licences/"
done
cp "$ROOT/apps/desktop/ui/fonts/OFL.txt" "$app/Contents/Resources/licences/Inter-OFL.txt"
cp "$ROOT/crates/content/LICENSE-adobe-cmap" "$app/Contents/Resources/licences/"
cp "$ROOT/crates/typeset/LICENSE-hyphenation" "$app/Contents/Resources/licences/"
plutil -lint "$app/Contents/Info.plist"

if [ -n "${MACOS_CERT_P12:-}" ]; then
    kc=$RUNNER_TEMP/signing.keychain-db
    kcpass=$(uuidgen)
    echo "$MACOS_CERT_P12" | base64 --decode >"$RUNNER_TEMP/cert.p12"
    security create-keychain -p "$kcpass" "$kc"
    security set-keychain-settings -lut 3600 "$kc"
    security unlock-keychain -p "$kcpass" "$kc"
    security import "$RUNNER_TEMP/cert.p12" -P "$MACOS_CERT_PASSWORD" -A -t cert -f pkcs12 -k "$kc"
    security list-keychain -d user -s "$kc"
    codesign --force --options runtime --timestamp --sign "$MACOS_SIGN_IDENTITY" "$app"
else
    # Ad-hoc signature: required on Apple silicon, but Gatekeeper will still warn.
    codesign --force --sign - "$app"
fi

dmg=$out/$APP_NAME-$APP_VERSION-$arch.dmg
stage=$(mktemp -d)
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
hdiutil create -volname "$APP_NAME" -srcfolder "$stage" -fs HFS+ -format UDZO -ov "$dmg"
rm -rf "$stage"

if [ -n "${MACOS_CERT_P12:-}" ]; then
    codesign --force --timestamp --sign "$MACOS_SIGN_IDENTITY" "$dmg"
fi
if [ -n "${MACOS_NOTARY_APPLE_ID:-}" ]; then
    xcrun notarytool submit "$dmg" --apple-id "$MACOS_NOTARY_APPLE_ID" \
        --team-id "$MACOS_NOTARY_TEAM_ID" --password "$MACOS_NOTARY_PASSWORD" --wait
    xcrun stapler staple "$dmg"
fi
echo "$dmg"
