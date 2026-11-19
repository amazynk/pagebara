#!/bin/sh
# Fetches public PDF test corpora into DIR (outside the repository; never commit them).
# usage: tools/corpora/fetch.sh DIR [NAME...]   (default: every corpus below)
# Each corpus is pinned to a commit. After fetching, the SHA-256 of its file manifest
# (sorted "sha256  path" lines) is compared with the pinned one; a mismatch is reported
# and the script exits with 1. Only PDF files are kept (the veraPDF corpus as is, and
# PDFium's expected renderings).
#
# Licences (files are used locally for testing only):
#   pdfjs        mozilla/pdf.js test/pdfs        Apache-2.0 (test files: various owners)
#   pdfium       pdfium testing/resources        BSD-3-Clause (.in templates expanded here,
#                                                 expected PNG renderings kept)
#   verapdf      veraPDF/veraPDF-corpus          CC-BY-4.0 (includes Isartor, Bavaria, TWG)
#   qpdf         qpdf/qpdf qpdf/qtest/qpdf       Apache-2.0
#   differences  pdf-association/pdf-differences Apache-2.0
#   horrors      openpreserve/format-corpus pdfCabinetOfHorrors  CC0-1.0
set -eu
[ $# -ge 1 ] || { sed -n 3p "$0"; exit 2; }
dir=$1
shift
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$dir"
dir=$(cd "$dir" && pwd)
names=${*:-"pdfjs pdfium verapdf qpdf differences horrors"}
status=0

manifest() {
  (cd "$1" && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 -r sha256sum | sha256sum | cut -d' ' -f1)
}

# fetch_repo NAME URL COMMIT [SUBDIR...]: a shallow, sparse checkout of one commit.
fetch_repo() {
  name=$1 url=$2 commit=$3
  shift 3
  tmp="$dir/.$name.tmp"
  rm -rf "$tmp"
  git init -q "$tmp"
  git -C "$tmp" remote add origin "$url"
  if [ $# -gt 0 ]; then
    git -C "$tmp" sparse-checkout set --no-cone "$@"
  fi
  git -C "$tmp" fetch -q --depth 1 --filter=blob:none origin "$commit"
  git -C "$tmp" -c advice.detachedHead=false checkout -q FETCH_HEAD
  rm -rf "$tmp/.git" "$dir/$name"
  mv "$tmp" "$dir/$name"
}

only_pdfs() {
  find "$1" -type f ! -iname '*.pdf' -delete
  find "$1" -type d -empty -delete
}

fetch() {
  case $1 in
  pdfjs)
    fetch_repo pdfjs https://github.com/mozilla/pdf.js bb1539a2c9b5b76d070284cca9091717133d8bf1 '/test/pdfs/'
    only_pdfs "$dir/pdfjs"
    ;;
  pdfium)
    tmp="$dir/.pdfium.tmp"
    rm -rf "$tmp" "$dir/pdfium"
    mkdir -p "$tmp/resources"
    curl -sSfL https://pdfium.googlesource.com/pdfium/+archive/d87a8615e382cf84245e4634b4c43a44c3ae5cae/testing/resources.tar.gz |
      tar -xz -C "$tmp/resources"
    python3 -I "$here/pdfium_in.py" "$tmp/resources"
    # The expected renderings (default AGG build) are kept as a reference for pixel-diff.
    find "$tmp" -type f ! -iname '*.pdf' ! -name '*_expected.pdf.*.png' -delete
    find "$tmp" -type d -empty -delete
    mv "$tmp" "$dir/pdfium"
    ;;
  verapdf)
    fetch_repo verapdf https://github.com/veraPDF/veraPDF-corpus bb75f4f0073d9350dfd058c0162a367e6fadf25e
    ;;
  qpdf)
    fetch_repo qpdf https://github.com/qpdf/qpdf 4eba95899886e851cc41d76886483b347612f2a8 '/qpdf/qtest/qpdf/'
    only_pdfs "$dir/qpdf"
    ;;
  differences)
    fetch_repo differences https://github.com/pdf-association/pdf-differences 907fe96e52b73e491489eee545c47b119bf9989b
    only_pdfs "$dir/differences"
    ;;
  horrors)
    fetch_repo horrors https://github.com/openpreserve/format-corpus 366f068cec399d0cdfd61fa473de3ab6dc858098 '/pdfCabinetOfHorrors/'
    only_pdfs "$dir/horrors"
    ;;
  *)
    echo "unknown corpus: $1" >&2
    return 1
    ;;
  esac
}

pinned() {
  case $1 in
  pdfjs) echo 2a79426cc2a676059d6f88e8fbcdbbfec3bbceef5b25da70ffaf9b0d6a49ed87 ;;
  pdfium) echo aa2e6960f1ddc26df622e84a110f08876fbc922b0ae17b41a0a291f657e7c787 ;;
  verapdf) echo bd4f359dcf4f8cc08beecc89b788628e40c60dd4717ee695b3cbf651dd1d97e9 ;;
  qpdf) echo 3bfc3b94ad37005e8e083285e78b6c4d27e10639c04ac0de352975935610da44 ;;
  differences) echo 80ee371441a08dc1faf4e5d48415d3d0f585556eac6f2b276be5860ff07c4525 ;;
  horrors) echo e3f9474212b2665083ed41a6a98f817de6a8835955e2c7a0c4c50678138c497f ;;
  esac
}

for n in $names; do
  if [ ! -d "$dir/$n" ] || [ -n "${REFETCH:-}" ]; then
    echo "fetching $n"
    fetch "$n"
  fi
  got=$(manifest "$dir/$n")
  want=$(pinned "$n")
  count=$(find "$dir/$n" -type f | wc -l)
  if [ "$got" = "$want" ]; then
    echo "$n: $count files, manifest ok"
  else
    echo "$n: $count files, manifest $got does not match the pinned $want" >&2
    status=1
  fi
done
exit $status
