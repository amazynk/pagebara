#!/bin/sh
# Generates pagebara.xcodeproj with XcodeGen (brew install xcodegen), naming the app after
# APP_NAME in src/lib.rs. Then: open pagebara.xcodeproj, or
#   xcodebuild -project pagebara.xcodeproj -scheme pagebara -sdk iphonesimulator \
#       -destination 'generic/platform=iOS Simulator' CODE_SIGNING_ALLOWED=NO build
set -eu
here=$(cd "$(dirname "$0")" && pwd)
PAGEBARA_APP_NAME=$(sed -n 's/^pub const APP_NAME: &str = "\(.*\)";$/\1/p' "$here/../src/lib.rs")
[ -n "$PAGEBARA_APP_NAME" ] || { echo "generate: APP_NAME not found in src/lib.rs" >&2; exit 1; }
export PAGEBARA_APP_NAME
chmod +x "$here/build-rust.sh"
cd "$here" && xcodegen generate --spec project.yml
