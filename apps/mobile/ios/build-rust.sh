#!/bin/sh
# Xcode build phase: builds the Rust app for the platform and architecture Xcode builds
# for, and puts the binary where the app bundle's executable goes.
set -eu
crate=$(cd "$(dirname "$0")/.." && pwd)
# Xcode's environment lacks the user's PATH.
export PATH="$HOME/.cargo/bin:$PATH"

case "${PLATFORM_NAME:-iphonesimulator}" in
    iphoneos) triple=aarch64-apple-ios ;;
    iphonesimulator)
        case "${NATIVE_ARCH_ACTUAL:-${ARCHS:-arm64}}" in
            x86_64) triple=x86_64-apple-ios ;;
            *) triple=aarch64-apple-ios-sim ;;
        esac
        ;;
    *) echo "build-rust: unsupported platform $PLATFORM_NAME" >&2; exit 1 ;;
esac
if [ "${CONFIGURATION:-Debug}" = Release ]; then
    profile=release
    flag=--release
else
    profile=debug
    flag=
fi
target=${CARGO_TARGET_DIR:-$(cd "$crate/../.." && pwd)/target}
rustup target add "$triple" >/dev/null 2>&1 || true
(cd "$crate" && cargo build -p pagebara-mobile --bin pagebara-mobile --target "$triple" $flag)
mkdir -p "$(dirname "$TARGET_BUILD_DIR/$EXECUTABLE_PATH")"
cp "$target/$triple/$profile/pagebara-mobile" "$TARGET_BUILD_DIR/$EXECUTABLE_PATH"
