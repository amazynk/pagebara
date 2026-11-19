#!/bin/sh
# Fails when BIN needs a newer glibc than VERSION: the newest GLIBC_x.y symbol version it
# references (objdump -T) must not be above it.
#
#   packaging/linux/check-glibc.sh BIN [VERSION]   (default 2.35, the README's promise)
set -eu
bin=$1
max=${2:-2.35}
need=$(objdump -T "$bin" | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/GLIBC_//' | sort -uV | tail -n1)
[ -n "$need" ] || { echo "$bin: no glibc symbol versions found" >&2; exit 1; }
if [ "$(printf '%s\n%s\n' "$need" "$max" | sort -V | tail -n1)" != "$max" ]; then
    echo "$bin needs glibc $need, newer than $max:" >&2
    objdump -T "$bin" | grep -E "GLIBC_($(echo "$need" | sed 's/\./\\./g'))[^.0-9]" >&2 || true
    exit 1
fi
echo "$bin: needs glibc $need (at most $max)"
