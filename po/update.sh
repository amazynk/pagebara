#!/bin/sh
# Collects the strings into pagebara.pot, then merges it into every language's catalog
# (new strings come in untranslated; removed ones are dropped).
# usage: sh po/update.sh   (needs gettext's msgmerge and msgattrib)
set -eu
here=$(cd "$(dirname "$0")" && pwd)
cargo run -q -p tr-extract
for po in "$here"/*.po; do
    [ -e "$po" ] || continue
    msgmerge --quiet --update --backup=none --no-wrap --no-fuzzy-matching "$po" "$here/pagebara.pot"
    msgattrib --no-obsolete --no-wrap -o "$po" "$po"
done
