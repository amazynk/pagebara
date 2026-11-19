#!/bin/sh
# Checks the app with a real screen reader's view of it: runs the window on a virtual X
# display with its own D-Bus session and AT-SPI bus, and walks the accessibility tree from
# an AT-SPI client (check.py), as Orca would.
# usage: run.sh OUT_DIR   (set PAGEBARA to the binary; exits 77 when a tool is missing)
# Keyboard checks need xdotool; without it they are skipped.
set -eu
unset WAYLAND_DISPLAY
here=$(cd "$(dirname "$0")" && pwd)
out=${1:?usage: run.sh OUT_DIR}
bin=${PAGEBARA:-$here/../../../../target/debug/pagebara}

skip() {
    echo "skip  a11y ($1)"
    exit 77
}
launcher=
for l in /usr/libexec/at-spi-bus-launcher /usr/lib/at-spi2-core/at-spi-bus-launcher \
    /usr/lib/at-spi2/at-spi-bus-launcher; do
    [ -x "$l" ] && launcher=$l && break
done
[ -n "$launcher" ] || skip "at-spi-bus-launcher not found (at-spi2-core)"
for t in Xvfb dbus-daemon python3; do
    command -v "$t" >/dev/null || skip "$t not found"
done
python3 -I -c "import gi; gi.require_version('Atspi', '2.0'); from gi.repository import Atspi" \
    2>/dev/null || skip "python3 AT-SPI bindings not found (python3-pyatspi)"

case $out in /*) ;; *) out=$(pwd)/$out ;; esac
rm -rf "$out"
mkdir -p "$out/samples" "$out/runtime"
chmod 700 "$out/runtime"
"$bin" --make-samples "$out/samples" >/dev/null

pids=
cleanup() {
    # Only the processes started here, newest first.
    for p in $pids; do kill "$p" 2>/dev/null || true; done
}
trap cleanup EXIT INT TERM

# A display of our own.
exec 3>"$out/display"
Xvfb -displayfd 3 -screen 0 1600x1000x24 -nolisten tcp >"$out/xvfb.log" 2>&1 &
pids="$! $pids"
exec 3>&-
for _ in $(seq 50); do [ -s "$out/display" ] && break; sleep 0.1; done
[ -s "$out/display" ] || { echo "Xvfb did not start"; exit 1; }
DISPLAY=:$(cat "$out/display")
export DISPLAY
# Labels are checked in English.
PAGEBARA_LANG=${PAGEBARA_LANG-en}
export PAGEBARA_LANG

# A private session bus; the AT-SPI bus launcher starts the accessibility bus and the
# registry on it (with dbus-daemon: dbus-broker needs systemd), and IsEnabled tells toolkits (AccessKit here) that an AT is listening.
XDG_RUNTIME_DIR=$out/runtime
export XDG_RUNTIME_DIR
dbus-daemon --session --fork --nopidfile --print-address=4 --print-pid=5 \
    4>"$out/bus" 5>"$out/bus.pid"
pids="$(cat "$out/bus.pid") $pids"
DBUS_SESSION_BUS_ADDRESS=$(head -n1 "$out/bus")
export DBUS_SESSION_BUS_ADDRESS
ATSPI_DBUS_IMPLEMENTATION=dbus-daemon "$launcher" --launch-immediately >"$out/atspi.log" 2>&1 &
pids="$! $pids"
python3 -I "$here/check.py" --enable || { echo "the AT-SPI bus did not start"; exit 1; }

PAGEBARA_SPEECH=${PAGEBARA_SPEECH:-stub} "$bin" --profile "$out/profile" --size 1280x800 --light "$out/samples/tagged.pdf" \
    >"$out/app.log" 2>&1 &
app=$!
pids="$app $pids"
status=0
# libatspi warns about AccessKit's cache signals, which it does not use: kept apart.
python3 -I "$here/check.py" --pid "$app" 2>"$out/client.log" || status=$?
if [ $status -eq 0 ]; then echo "ok    a11y"; else echo "FAIL  a11y (see $out)"; fi
exit $status
