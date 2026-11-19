#!/bin/bash
# Fills corpus/<target> with seeds from the crates' test fixtures and a few generated files.
# With CORPORA=DIR (see tools/corpora/fetch.sh), also small PDFs of the public corpora, and
# the fonts and images MuPDF extracts from some of them.
set -e
cd "$(dirname "$0")"
root=..
mk() { mkdir -p "corpus/$1"; }
all="syntax_open syntax_unlock filters flate ccitt jpeg jp2 jbig2 png objstm font cmap render_pdf
  render_content image shading fdf spell i18n optimize"
for t in $all; do mk "$t"; done
cp "$root"/crates/syntax/tests/fixtures/crypt/*.pdf corpus/syntax_open/
cp "$root"/crates/pages/tests/fixtures/*.pdf corpus/syntax_open/
cp "$root"/crates/pages/tests/fixtures/*.pdf corpus/render_pdf/
cp "$root"/crates/pages/tests/fixtures/*.pdf corpus/optimize/
cp "$root"/po/*.po corpus/i18n/
for f in "$root"/crates/syntax/tests/fixtures/crypt/*.pdf; do
  # Password-length byte, then the password, then the file.
  { printf '\x04user'; cat "$f"; } > "corpus/syntax_unlock/$(basename "$f")"
done
cp "$root"/crates/codec/jpeg/tests/fixtures/*.jpg corpus/jpeg/ 2>/dev/null || true
find "$root"/crates/codec/jp2/tests/data -type f -size -64k \( -name '*.j2k' -o -name '*.jp2' -o -name '*.jpx' -o -name '*.jph' -o -name '*.j2c' -o -name '*.jhc' \) -exec cp {} corpus/jp2/ \;
for f in "$root"/crates/codec/jbig2/tests/fixtures/*.jbig2; do
  { printf '\x00'; cat "$f"; } > "corpus/jbig2/$(basename "$f")"
done
python3 -I - <<'EOF'
import struct, zlib, os
def chunk(t, d):
    return struct.pack('>I', len(d)) + t + d + struct.pack('>I', zlib.crc32(t + d) & 0xffffffff)
def png(w, h, ct, depth, rows, interlace=0, extra=b''):
    ihdr = struct.pack('>IIBBBBB', w, h, depth, ct, 0, 0, interlace)
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', ihdr) + extra + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b'')
out = 'corpus/png'
for ct, ch in [(0, 1), (2, 3), (4, 2), (6, 4)]:
    w, h = 13, 7
    rows = b''.join(bytes([y % 5]) + bytes((x * 7 + y * ch) % 256 for x in range(w * ch)) for y in range(h))
    open(f'{out}/ct{ct}.png', 'wb').write(png(w, h, ct, 8, rows))
    open(f'{out}/ct{ct}-i.png', 'wb').write(png(w, h, ct, 8, rows * 2, 1))
rows = b''.join(b'\x00' + bytes([0x1b, 0xe4]) for _ in range(5))
open(f'{out}/pal2.png', 'wb').write(png(8, 5, 3, 2, rows, 0, chunk(b'PLTE', bytes(range(12)))))
fl = 'corpus/flate'
for i, d in enumerate([b'', b'hello hello hello', bytes(range(256)) * 9, b'\0' * 5000]):
    for lvl in (0, 1, 9):
        open(f'{fl}/z{i}-{lvl}', 'wb').write(zlib.compress(d, lvl))
fi = 'corpus/filters'
data = bytes((x * 3) % 251 for x in range(3000))
open(f'{fi}/flate-png', 'wb').write(bytes([0, 3, 2, 3, 40, 1]) + zlib.compress(b''.join(b'\x02' + data[i:i+120] for i in range(0, 2400, 120))))
open(f'{fi}/hex', 'wb').write(bytes([0, 0, 0, 0, 0, 0]) + data[:200].hex().encode() + b'>')
open(f'{fi}/a85-flate', 'wb').write(bytes([1, 1, 0, 0, 0, 0, 3, 0, 0, 0, 0]) + b'<~9jqo^BlbD-BleB1DJ+*+F(f,q/0JhKF<GL>Cj@.4Gp$d7F!,L7@<6@)/0JDEF<G%<+EV:2F!,O<DJ+*.@<*K0@<6L(Df-\\0Ec5e;DffZ(EZee.Bl.9pF"AGXBPCsi+DGm>@3BB/F*&OCAfu2/AKYi(DIb:@FD,*)+C]U=@3BN#EcYf8ATD3s@q?d$AftVqCh[NqF<G:8+EV:.+Cf>-FD5W8ARlolDIal(DId<j@<?3r@:F%a+D58\'ATD4$Bl@l3De:,-DJs`8ARoFb/0JMK@qB4^F!,R<AKZ&-DfTqBG%G>uD.RTpAKYo\'+CT/5+Cei#DII?(E,9)oF*2M7/c~>')
open(f'{fi}/rl', 'wb').write(bytes([2, 4, 0, 0, 0, 0]) + bytes([2, 65, 66, 67, 0xfe, 68, 128]))
cc = 'corpus/ccitt'
open(f'{cc}/g4-white', 'wb').write(bytes([0xff, 26, 3, 0]) + b'\xff' * 8 + b'\x00\x10\x01')
open(f'{cc}/g3', 'wb').write(bytes([0, 26, 3, 0]) + b'\x00\x10\x9a\x80' * 4)

def put(target, name, data):
    open(f'corpus/{target}/{name}', 'wb').write(data)

# Object stream: three objects, then N and First (little-endian) in front.
objs = [b'<</Type/Catalog/Pages 2 0 R>>', b'<</Type/Pages/Kids[3 0 R]/Count 1>>', b'<</Type/Page/Parent 2 0 R>>']
head, body = b'', b''
for i, o in enumerate(objs):
    head += b'%d %d ' % (i + 1, len(body))
    body += o + b' '
put('objstm', 'three', bytes([3]) + struct.pack('<H', len(head)) + head + body)
put('cmap', 'basic', b'/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n'
    b'1 begincodespacerange <0000> <FFFF> endcodespacerange\n'
    b'2 beginbfchar <0003> <0020> <0024> <0041> endbfchar\n'
    b'1 beginbfrange <0010> <0020> <0061> endbfrange\n'
    b'1 begincidrange <0100> <01ff> 500 endcidrange\n'
    b'endcmap CMapName currentdict /CMap defineresource pop end end\n')
put('render_content', 'mixed', b'BT /F1 12 Tf 10 40 Td (Hello) Tj /F3 8 Tf (ab) Tj ET q /G1 gs /Im Do Q '
    b'/Pattern cs /P1 scn 0 0 20 20 re f /Sh sh /Cs1 cs 1 sc 5 5 m 30 50 l 60 5 l h f '
    b'q /G2 gs /Fm Do Q /Mk Do 3 Tr BT /F2 9 Tf 0 0 Td [(W)-200(x)] TJ ET '
    b'q /G3 gs 1 0 0 RG 2 w 5 5 50 50 re S Q /Pattern cs /P2 scn 30 30 30 30 re f /Sh2 sh '
    b'BI /W 2 /H 2 /CS /G /BPC 8 ID \x00\x80\xff\x40 EI')
put('image', 'rgb', bytes([3, 3, 3, 1, 0, 0]) + bytes(range(48)))
put('image', 'mask', bytes([7, 7, 0, 0, 0, 9]) + bytes(range(64)))
put('shading', 'axial', bytes([1, 1, 1, 3, 1]))
put('shading', 'mesh', bytes([3, 1, 3, 3, 1]) + bytes([0, 0, 0, 0, 255, 0, 255, 0, 0, 0, 0, 128, 255, 128]))
put('fdf', 'basic.fdf', b'%FDF-1.2\n1 0 obj\n<</FDF<</Fields[<</T(name)/V(Bob)>><</T(agree)/V/Yes>>'
    b'<</T(size)/V(L)>><</T(pick)/V[(a)(b)]>>]>>>>\nendobj\ntrailer\n<</Root 1 0 R>>\n%%EOF\n')
put('fdf', 'basic.xfdf', b'<?xml version="1.0"?><xfdf xmlns="http://ns.adobe.com/xfdf/"><fields>'
    b'<field name="name"><value>Eve</value></field><field name="pick"><value>a</value><value>b</value></field>'
    b'</fields><annots><text page="0" rect="1,1,20,20" color="#FF0000"><contents>hi</contents></text>'
    b'</annots></xfdf>')
put('spell', 'basic', b'SET UTF-8\nTRY esianrtolcdugmphbyfvkwz\nPFX A Y 1\nPFX A 0 re .\nSFX B Y 2\n'
    b'SFX B 0 ed [^y]\nSFX B y ied y\nCOMPOUNDFLAG C\nREP 1\nREP f ph\n\x004\nwork/AB\ntry/B\nfoot/C\nball/C\n')
EOF
if [ -n "${CORPORA:-}" ]; then
  # Small files only: the fuzzer's inputs stay short.
  list=$(mktemp)
  find "$CORPORA" -name '*.pdf' -size -64k | head -n 1500 > "$list"
  while read -r f; do
    n=$(printf '%s' "$f" | sha256sum | cut -c1-16)
    cp "$f" "corpus/syntax_open/$n.pdf"
    cp "$f" "corpus/render_pdf/$n.pdf"
  done < "$list"
  head -n 300 "$list" | while read -r f; do
    cp "$f" "corpus/optimize/$(printf '%s' "$f" | sha256sum | cut -c1-16).pdf"
  done
  rm -f "$list"
  ex=$(mktemp -d)
  find "$CORPORA" -name '*.pdf' -size -2M | head -n 400 | while read -r f; do
    (cd "$ex" && timeout 10 mutool extract "$f" > /dev/null 2>&1) || true
  done
  for f in "$ex"/font-*; do
    [ -s "$f" ] && [ "$(stat -c %s "$f")" -lt 200000 ] || continue
    { printf '\001'; cat "$f"; } > "corpus/font/$(sha256sum < "$f" | cut -c1-16)"
  done
  for f in "$ex"/image-*.jpg "$ex"/image-*.jpx; do
    [ -s "$f" ] && [ "$(stat -c %s "$f")" -lt 100000 ] || continue
    case $f in *.jpg) cp "$f" corpus/jpeg/ ;; *) cp "$f" corpus/jp2/ ;; esac
  done
  rm -rf "$ex"
fi
echo seeded: $(find corpus -type f | wc -l) files
