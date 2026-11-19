#!/bin/sh
# Runs the phone UI's scripts in this folder on a virtual X display, in a phone-sized
# window (a script's "# size: WxH" line picks another size, e.g. a tablet's).
# usage: run.sh OUT_DIR [SCRIPT...]   (needs Xvfb; set PAGEBARA_MOBILE to the binary)
# OUT_DIR is wiped first; screenshots go to OUT_DIR/shots.
set -eu
unset WAYLAND_DISPLAY
here=$(cd "$(dirname "$0")" && pwd)
[ $# -gt 0 ] || { echo "usage: run.sh OUT_DIR [SCRIPT...]" >&2; exit 2; }
out=$1
shift
case $out in *.pdfs) echo "run.sh: the first argument is the output folder" >&2; exit 2 ;; esac
[ -e "$out" ] && [ ! -d "$out" ] && { echo "run.sh: $out is not a folder" >&2; exit 2; }
bin=${PAGEBARA_MOBILE:-$here/../../../target/debug/pagebara-mobile}
rm -rf "$out"
mkdir -p "$out/shots"
scripts=${*:-$(ls "$here"/*.pdfs)}
failed=0
for s in $scripts; do
    name=$(basename "$s" .pdfs)
    # Live text needs the models on disk; the tests never download them.
    case $name in *-live-text*)
        if [ -z "${PAGEBARA_OCR_MODELS:-}" ]; then
            echo "skip  $name (set PAGEBARA_OCR_MODELS to a folder with the OCR models to run it)"
            continue
        fi
    esac
    # Offers of other scripts' models: the app has only the Latin ones, and "downloads"
    # copy from the full folder.
    models=${PAGEBARA_OCR_MODELS:-}
    fetch=""
    case $name in *-live-text-offer|*-live-text-decline)
        models=$out/latin-models
        fetch=$PAGEBARA_OCR_MODELS
        rm -rf "$models"
        mkdir -p "$models"
        for f in ppocrv5-mobile-det.onnx ppocrv5-latin-rec.onnx ppocrv5-latin-rec.yml; do
            ln -s "$PAGEBARA_OCR_MODELS/$f" "$models/$f"
        done
    esac
    size=$(sed -n 's/^# size: \([0-9]*x[0-9]*\)$/\1/p' "$s" | head -1)
    # Fresh samples (scripts save into them) and profile per script.
    rm -rf "$out/profile" "$out/samples"
    "$bin" --make-samples "$out/samples"
    if PAGEBARA_OCR_MODELS=$models PAGEBARA_OCR_FETCH_DIR=$fetch \
        SAMPLES="$out/samples" OUT="$out" PAGEBARA_SHOTS="$out/shots" SLINT_BACKEND=${SLINT_BACKEND:-winit} PAGEBARA_LANG=${PAGEBARA_LANG-en} \
        xvfb-run -a -s "-screen 0 1400x1000x24" \
        "$bin" --profile "$out/profile" --size "${size:-390x844}" --light --script "$s" >"$out/$name.log" 2>&1; then
        echo "ok    $name"
    else
        echo "FAIL  $name (see $out/$name.log)"
        grep -E "ERROR" "$out/$name.log" | head -5
        failed=1
    fi
done
exit $failed
