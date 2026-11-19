#!/bin/sh
# Records demo.pdfs running in the real app on a virtual display, as an MP4.
# usage: record.sh OUT.mp4   (needs Xvfb and ffmpeg; set PAGEBARA to the binary, release preferred)
set -eu
unset WAYLAND_DISPLAY
here=$(cd "$(dirname "$0")" && pwd)
video=${1:?usage: record.sh OUT.mp4}
bin=${PAGEBARA:-$here/../../../target/release/pagebara}
# A 1280x800 window drawn at 1.5x: a sharp 1920x1200 video.
size=1280x800
scale=1.5
screen=1920x1200
work=$(mktemp -d)
trap 'kill $xvfb 2>/dev/null; rm -rf "$work"' EXIT
"$bin" --make-samples "$work/samples"

# A free display number.
n=99
while [ -e /tmp/.X11-unix/X$n ] || [ -e /tmp/.X$n-lock ]; do n=$((n + 1)); done
Xvfb ":$n" -screen 0 "${screen}x24" -nolisten tcp >/dev/null 2>&1 &
xvfb=$!
sleep 1
export DISPLAY=":$n"

# The app first, so the recording starts on a drawn window.
SAMPLES="$work/samples" OUT="$work" SLINT_BACKEND=${SLINT_BACKEND:-winit} \
    SLINT_SCALE_FACTOR=$scale WINIT_X11_SCALE_FACTOR=$scale \
    "$bin" --profile "$work/profile" --size "$size" --light --script "$here/demo.pdfs" \
    >"$work/app.log" 2>&1 &
app=$!
sleep 1.5
# Capture losslessly with little CPU, so the app keeps its frame rate; compress afterwards.
# The script draws its own pointer, so the X one is left out.
ffmpeg -loglevel error -y -f x11grab -draw_mouse 0 -framerate 30 -video_size "$screen" \
    -i ":$n" -c:v libx264rgb -preset ultrafast -qp 0 "$work/raw.mkv" &
rec=$!
status=0
wait $app || status=$?
kill -INT $rec
wait $rec || true
[ $status -eq 0 ] || { echo "record.sh: the script failed:" >&2; grep ERROR "$work/app.log" >&2; exit $status; }
# Drop the frames after the window closed.
end=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$work/raw.mkv")
ffmpeg -loglevel error -y -i "$work/raw.mkv" -t "$(echo "$end - 0.8" | bc)" \
    -c:v libx264 -preset slow -crf 18 -pix_fmt yuv420p -movflags +faststart "$video"
