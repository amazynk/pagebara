#!/bin/sh
# Runs the UI scripts in this folder against the real window on a virtual X display.
# usage: run.sh [OUT_DIR] [SCRIPT...]   (needs Xvfb; set PAGEBARA to the binary)
set -eu
# On the virtual display only: winit would prefer a Wayland session it finds.
unset WAYLAND_DISPLAY
here=$(cd "$(dirname "$0")" && pwd)
out=${1:-${TMPDIR:-/tmp}/pagebara-e2e}
[ $# -gt 0 ] && shift
# OUT is wiped below: refuse a script or any other file passed there by mistake.
case $out in *.pdfs) echo "run.sh: the first argument is the output folder" >&2; exit 2 ;; esac
[ -e "$out" ] && [ ! -d "$out" ] && { echo "run.sh: $out is not a folder" >&2; exit 2; }
bin=${PAGEBARA:-$here/../../../../target/debug/pagebara}
rm -rf "$out"
mkdir -p "$out/samples" "$out/shots"
"$bin" --make-samples "$out/samples"
# A stand-in print system: `lpstat` lists one printer and `lp` records each job in
# $out/lp, so the scripts never reach a real printer. The stubs sit next to the binary,
# where programs can run (temp folders are often mounted noexec).
stubs=$(cd "$(dirname "$bin")" && pwd)/e2e-stubs-$$
trap 'rm -rf "$stubs"' EXIT
mkdir -p "$stubs" "$out/lp"
cat >"$stubs/lpstat" <<'END'
#!/bin/sh
echo "printer pagebara-test is idle.  enabled since Thu 01 Jan 1970 00:00:00"
echo "system default destination: pagebara-test"
END
cat >"$stubs/lp" <<END
#!/bin/sh
echo "\$*" >"$out/lp/args"
for last; do :; done
cp "\$last" "$out/lp/job.pdf"
echo "request id is pagebara-test-1 (1 file(s))"
END
# A file manager the system integration page finds (it is never run).
printf '#!/bin/sh\nexit 0\n' >"$stubs/nautilus"
chmod +x "$stubs/lpstat" "$stubs/lp" "$stubs/nautilus"
PATH="$stubs:$PATH"
export PATH
scripts=${*:-$(ls "$here"/*.pdfs)}
failed=0
for s in $scripts; do
    name=$(basename "$s" .pdfs)
    # Text recognition needs the models on disk; the tests never download them.
    if { [ "$name" = "13-ocr" ] || [ "$name" = "20-live-text" ]; } && [ -z "${PAGEBARA_OCR_MODELS:-}" ]; then
        echo "skip  $name (set PAGEBARA_OCR_MODELS to a folder with the OCR models to run it)"
        continue
    fi
    # A fresh profile per script: settings, recent files and signatures start empty.
    rm -rf "$out/profile"
    # The read-aloud test engine records instead of speaking; PAGEBARA_SPEECH set but empty
    # uses the system's engine. The test key store stands in for the system's certificates.
    if SAMPLES="$out/samples" OUT="$out" PAGEBARA_SHOTS="$out/shots" SLINT_BACKEND=${SLINT_BACKEND:-winit} PAGEBARA_LANG=${PAGEBARA_LANG-en} \
        PAGEBARA_SPEECH=${PAGEBARA_SPEECH-stub} PAGEBARA_KEYSTORE=${PAGEBARA_KEYSTORE-test} \
        xvfb-run -a -s "-screen 0 1600x1000x24" \
        "$bin" --profile "$out/profile" --size 1440x900 --light --script "$s" >"$out/$name.log" 2>&1; then
        echo "ok    $name"
    else
        echo "FAIL  $name (see $out/$name.log)"
        grep -E "ERROR" "$out/$name.log" | head -5
        failed=1
    fi
done
exit $failed
