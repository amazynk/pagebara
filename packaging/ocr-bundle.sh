#!/bin/sh
# Prints a folder holding the OCR models that release builds carry inside the program
# (text detection and Latin recognition, about 13 MB, Apache-2.0: see THIRD-PARTY.md), for
# the build to read through PAGEBARA_OCR_BUNDLE. Uses $PAGEBARA_OCR_BUNDLE when it is set;
# otherwise downloads the pinned files once into $CARGO_TARGET_DIR/ocr-bundle and checks
# their SHA-256 (the same pins as crates/ocr/src/model.rs).
#
#   PAGEBARA_OCR_BUNDLE=$(sh packaging/ocr-bundle.sh) cargo build --release -p pagebara-app
set -eu
if [ -n "${PAGEBARA_OCR_BUNDLE:-}" ]; then
    echo "$PAGEBARA_OCR_BUNDLE"
    exit 0
fi
root=$(cd "$(dirname "$0")/.." && pwd)
dir=${CARGO_TARGET_DIR:-$root/target}/ocr-bundle
mkdir -p "$dir"
hf=https://huggingface.co/PaddlePaddle
fetch() {
    name=$1 url=$2 sum=$3
    if [ -f "$dir/$name" ] && echo "$sum  $dir/$name" | sha256sum -c --status; then
        return
    fi
    curl -fsSL --retry 3 -o "$dir/$name.part" "$url" >&2
    echo "$sum  $dir/$name.part" | sha256sum -c --status || {
        echo "ocr-bundle: checksum mismatch for $name" >&2
        exit 1
    }
    mv "$dir/$name.part" "$dir/$name"
}
fetch ppocrv5-mobile-det.onnx \
    "$hf/PP-OCRv5_mobile_det_onnx/resolve/e6f4fa85f00e168c862bc462aebca69eef9b3d3d/inference.onnx" \
    a431985659dc921974177a95adcfbb90fd9e51989a5e04d70d0b75f597b6e61d
fetch ppocrv5-latin-rec.onnx \
    "$hf/latin_PP-OCRv5_mobile_rec_onnx/resolve/89d3a50e2c27e2e7cceeab0e944c25c807d5db4f/inference.onnx" \
    7888113072263cb471b93f66dd5e2ad70548dc526fa1ace760d0d973dd121498
fetch ppocrv5-latin-rec.yml \
    "$hf/latin_PP-OCRv5_mobile_rec_onnx/resolve/89d3a50e2c27e2e7cceeab0e944c25c807d5db4f/inference.yml" \
    0bbe984570f597af3638e50bdf2e8276f3ab26a61966096538b3b0d1849f5c84
echo "$dir"
