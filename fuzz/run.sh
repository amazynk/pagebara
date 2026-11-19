#!/bin/bash
# Builds every target once, then runs each for SECS seconds (default 300), PAR at a time
# (default 3). Crashes land in artifacts/<target>/, logs in logs/<target>.log.
# Binaries are taken from $CARGO_TARGET_DIR when set.
cd "$(dirname "$0")" || exit 1
secs=${SECS:-300}
par=${PAR:-3}
targets=${TARGETS:-"syntax_open syntax_unlock objstm filters flate ccitt jpeg jp2 jbig2 png font cmap
  render_pdf render_content image shading fdf spell i18n optimize"}
bin=${CARGO_TARGET_DIR:-target}/x86_64-unknown-linux-gnu/release
cargo fuzz build || exit 1
mkdir -p logs
run() {
  mkdir -p "artifacts/$1" "corpus/$1"
  ASAN_OPTIONS=detect_odr_violation=0 nice "$bin/$1" \
    -artifact_prefix="artifacts/$1/" -max_total_time="$secs" -rss_limit_mb=2048 \
    -malloc_limit_mb=1024 -timeout=10 -max_len=262144 -print_final_stats=1 "corpus/$1" > "logs/$1.log" 2>&1
  echo "$1: exit $? $(grep -E '^stat::number_of_executed_units' "logs/$1.log")"
}
for t in $targets; do
  run "$t" &
  while [ "$(jobs -rp | wc -l)" -ge "$par" ]; do wait -n; done
done
wait
