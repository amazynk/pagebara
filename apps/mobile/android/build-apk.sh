#!/bin/sh
# Builds the Android app as a debug-signed APK, without Gradle: the Rust library with
# cargo-ndk, the two Java classes with javac and d8, resources with aapt2.
#
# usage: build-apk.sh [OUT_DIR]
#   ABIS        Android ABIs to include (default: arm64-v8a; also x86_64 for emulators)
#   PROFILE     release (default) or debug
#   ANDROID_HOME, ANDROID_NDK_ROOT   the SDK (default ~/Android/Sdk) and NDK (its newest)
#   KEYSTORE    signing keystore (default: a debug key made once in OUT_DIR)
set -eu
here=$(cd "$(dirname "$0")" && pwd)
crate=$(dirname "$here")
target=${CARGO_TARGET_DIR:-$(cd "$crate/../.." && pwd)/target}
out=${1:-$target/android}
abis=${ABIS:-arm64-v8a}
profile=${PROFILE:-release}
sdk=${ANDROID_HOME:-$HOME/Android/Sdk}
ndk=${ANDROID_NDK_ROOT:-$(ls -d "$sdk"/ndk/* | sort -V | tail -1)}
tools=$(ls -d "$sdk"/build-tools/* | sort -V | tail -1)
jar=$(ls -d "$sdk"/platforms/android-*/android.jar | sort -V | tail -1)
min_api=26
export ANDROID_HOME="$sdk" ANDROID_NDK_ROOT="$ndk" ANDROID_NDK_HOME="$ndk"
# Slint's Android build script looks for the platform named by ANDROID_PLATFORM, which
# cargo-ndk sets to the minimum API level, and that platform is usually not installed.
export ANDROID_JAR="${ANDROID_JAR:-$jar}"

# The product name, kept in one place.
name=$(sed -n 's/^pub const APP_NAME: &str = "\(.*\)";$/\1/p' "$crate/src/lib.rs")
[ -n "$name" ] || { echo "build-apk: APP_NAME not found in src/lib.rs" >&2; exit 1; }

work=$out/build
rm -rf "$work"
mkdir -p "$work/classes" "$work/dex" "$work/res/values" "$work/apk/lib"

# The text detection and Latin recognition models go inside the library (live text works
# offline); debug builds skip them unless PAGEBARA_OCR_BUNDLE is set.
if [ "$profile" = release ] || [ -n "${PAGEBARA_OCR_BUNDLE:-}" ]; then
    PAGEBARA_OCR_BUNDLE=$(sh "$crate/../../packaging/ocr-bundle.sh")
    export PAGEBARA_OCR_BUNDLE
fi

echo "== Rust ($abis, $profile)"
release=""
[ "$profile" = release ] && release="--release"
for abi in $abis; do
    # A cdylib only for Android; desktop builds of the crate stay a plain library.
    (cd "$crate" && cargo ndk -t "$abi" --platform "$min_api" \
        rustc -p pagebara-mobile --lib --crate-type cdylib $release)
    case $abi in
        arm64-v8a) triple=aarch64-linux-android ;;
        x86_64) triple=x86_64-linux-android ;;
        armeabi-v7a) triple=armv7-linux-androideabi ;;
        *) echo "build-apk: unknown ABI $abi" >&2; exit 1 ;;
    esac
    mkdir -p "$work/apk/lib/$abi"
    cp "$target/$triple/$profile/libpagebara_mobile.so" "$work/apk/lib/$abi/"
done

echo "== Java"
javac -source 11 -target 11 -encoding UTF-8 -nowarn -Xlint:-options \
    -classpath "$jar" -d "$work/classes" \
    $(find "$here/java" -name '*.java')
"$tools/d8" --release --min-api "$min_api" --lib "$jar" --output "$work/dex" \
    $(find "$work/classes" -name '*.class')

echo "== Resources"
cat >"$work/res/values/strings.xml" <<END
<?xml version="1.0" encoding="utf-8"?>
<resources>
    <string name="app_name">$name</string>
</resources>
END
"$tools/aapt2" compile --dir "$here/res" -o "$work/res-base.zip"
"$tools/aapt2" compile --dir "$work/res" -o "$work/res-name.zip"
"$tools/aapt2" link -o "$work/unsigned.apk" -I "$jar" \
    --manifest "$here/AndroidManifest.xml" \
    --min-sdk-version "$min_api" \
    "$work/res-base.zip" "$work/res-name.zip"

echo "== APK"
cp "$work/dex/classes.dex" "$work/apk/"
(cd "$work/apk" && zip -q -r "$work/unsigned.apk" classes.dex lib)
"$tools/zipalign" -p -f 4 "$work/unsigned.apk" "$work/aligned.apk"

keystore=${KEYSTORE:-$out/debug.keystore}
if [ ! -f "$keystore" ]; then
    keytool -genkeypair -keystore "$keystore" -storepass android -keypass android \
        -alias debug -keyalg RSA -keysize 2048 -validity 10000 \
        -dname "CN=$name debug" >/dev/null 2>&1
fi
apk=$out/$name-$profile.apk
"$tools/apksigner" sign --ks "$keystore" --ks-pass pass:android --key-pass pass:android \
    --ks-key-alias debug --out "$apk" "$work/aligned.apk"
rm -rf "$work"
echo "$apk"
