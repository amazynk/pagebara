# Generates the predefined CMaps (code -> CID) and the CID -> Unicode tables of the Adobe
# character collections from Adobe's cmap-resources (BSD-3-Clause, see LICENSE-adobe-cmap).
# Usage: gen_cmaps.py <cMap dir> <out dir>
#   <cMap dir>: cmap-resources' per-collection directories (Adobe-Japan1/, …), e.g. poppler-data's
#   /usr/share/poppler/cMap. Writes <out dir>/data.rs and <out dir>/cmaps.bin; then
#   rustfmt --edition 2024 <out dir>/data.rs
#
# Each table is a raw DEFLATE stream of varint records over sorted, disjoint ranges:
#   cid table:     gap, hi - lo, zigzag(cid - expected)        (expected: previous range's next CID)
#   unicode table: gap, hi - lo, k, then zigzag(scalar - expected) if k == 1 else k scalars
# where gap = lo - (previous hi + 1). Unicode-coded CMaps (UCS2/UTF16/UTF8/UTF32) are keyed by
# scalar value so the encoding forms of one character set share a table.
import os, re, sys, zlib

SRC, OUT = sys.argv[1], sys.argv[2]
ORDERINGS = ['Japan1', 'GB1', 'CNS1', 'Korea1', 'KR']
TOKEN = re.compile(rb'%[^\n\r]*|<([0-9A-Fa-f\s]*)>|/([^\s/<>\[\]()%{}]+)|\(([^)]*)\)|(\[)|(\])|([^\s/<>\[\]()%{}]+)')


def tokens(data):
    for m in TOKEN.finditer(data):
        hexs, name, string, lb, rb, word = m.groups()
        if hexs is not None:
            yield ('hex', bytes.fromhex(re.sub(rb'\s', b'', hexs).decode()))
        elif name is not None:
            yield ('name', name.decode())
        elif string is not None:
            yield ('str', string.decode('latin-1'))
        elif lb:
            yield ('[', None)
        elif rb:
            yield (']', None)
        elif word is not None:
            w = word.decode()
            yield ('num', int(w)) if re.fullmatch(r'-?\d+', w) else ('kw', w)


def parse(path):
    m = {'codespace': [], 'maps': [], 'uni': {}, 'usecmap': None, 'wmode': 0, 'ordering': None}
    stack, arr = [], None
    for kind, v in tokens(open(path, 'rb').read()):
        if kind == '[':
            arr = []
            continue
        if kind == ']':
            stack.append(('arr', arr)); arr = None
            continue
        if arr is not None:
            arr.append((kind, v)); continue
        if kind != 'kw':
            stack.append((kind, v)); continue
        vals = [x[1] for x in stack]
        if v == 'endcodespacerange':
            for lo, hi in zip(vals[0::2], vals[1::2]):
                m['codespace'].append((lo, hi))
        elif v == 'endcidrange':
            for lo, hi, cid in zip(vals[0::3], vals[1::3], vals[2::3]):
                m['maps'].append((lo, hi, cid, 1))
        elif v == 'endnotdefrange':
            # Every code of a notdef range maps to the same CID, below all other mappings.
            for lo, hi, cid in zip(vals[0::3], vals[1::3], vals[2::3]):
                m['maps'].insert(0, (lo, hi, cid, 0))
        elif v == 'endcidchar':
            for c, cid in zip(vals[0::2], vals[1::2]):
                m['maps'].append((c, c, cid, 1))
        elif v == 'endbfchar':
            for c, dst in zip(vals[0::2], vals[1::2]):
                m['uni'][int.from_bytes(c, 'big')] = utf16(dst)
        elif v == 'endbfrange':
            for lo, hi, dst in zip(stack[0::3], stack[1::3], stack[2::3]):
                lo, hi = int.from_bytes(lo[1], 'big'), int.from_bytes(hi[1], 'big')
                for i in range(hi - lo + 1):
                    if dst[0] == 'arr':
                        m['uni'][lo + i] = utf16(dst[1][i][1])
                    else:
                        b = bytearray(dst[1])
                        last = int.from_bytes(b[-2:], 'big') + i
                        m['uni'][lo + i] = utf16(bytes(b[:-2]) + last.to_bytes(2, 'big'))
        elif v == 'usecmap' and stack and stack[-1][0] == 'name':
            m['usecmap'] = stack[-1][1]
        elif v == 'def' and len(stack) >= 2 and stack[-2][0] == 'name':
            k, val = stack[-2][1], stack[-1][1]
            if k == 'WMode':
                m['wmode'] = val
            elif k == 'Ordering':
                m['ordering'] = val
        stack = []
    return m


def utf16(b):
    units = [int.from_bytes(b[i:i + 2], 'big') for i in range(0, len(b) - 1, 2)]
    return tuple(ord(c) for c in b''.join(u.to_bytes(2, 'big') for u in units).decode('utf-16-be', 'surrogatepass'))


def form_of(name):
    for f in ('UCS2', 'UTF16', 'UTF8', 'UTF32'):
        if '-' + f + '-' in name:
            return f
    return 'Bytes'


def key(code, form):
    if form == 'UTF16':
        return ord(code.decode('utf-16-be'))
    if form == 'UTF8':
        return ord(code.decode('utf-8'))
    return int.from_bytes(code, 'big')


def codes(lo, hi):
    """The codes of a cidrange: bytes vary within each position's own lo..hi."""
    if len(lo) == 1 or lo[:-1] == hi[:-1]:
        for c in range(lo[-1], hi[-1] + 1):
            yield lo[:-1] + bytes([c])
        return
    n = int.from_bytes(lo, 'big')
    for c in range(n, int.from_bytes(hi, 'big') + 1):
        yield c.to_bytes(len(lo), 'big')


def varint(out, v):
    assert v >= 0
    while v >= 0x80:
        out.append(v & 0x7F | 0x80); v >>= 7
    out.append(v)


def zigzag(v):
    return v * 2 if v >= 0 else -v * 2 - 1


def ranges(mapping):
    """Sorted (lo, hi, value) runs of a key -> int mapping with consecutive values."""
    out = []
    for k in sorted(mapping):
        v = mapping[k]
        if out and out[-1][1] + 1 == k and out[-1][2] + (k - out[-1][0]) == v:
            out[-1][1] = k
        else:
            out.append([k, k, v])
    return out


def cid_table(mapping):
    out, end, nxt = bytearray(), 0, 0
    rs = ranges(mapping)
    varint(out, len(rs))
    for lo, hi, cid in rs:
        varint(out, lo - end); varint(out, hi - lo); varint(out, zigzag(cid - nxt))
        end, nxt = hi + 1, cid + hi - lo + 1
    return bytes(out)


def unicode_table(uni):
    single = {k: v[0] for k, v in uni.items() if len(v) == 1}
    rs = [(lo, hi, (v,)) for lo, hi, v in ranges(single)]
    rs += [(k, k, v) for k, v in uni.items() if len(v) != 1]
    rs.sort()
    out, end, nxt = bytearray(), 0, 0
    varint(out, len(rs))
    for lo, hi, v in rs:
        varint(out, lo - end); varint(out, hi - lo); varint(out, len(v))
        if len(v) == 1:
            varint(out, zigzag(v[0] - nxt)); nxt = v[0] + hi - lo + 1
        else:
            for s in v:
                varint(out, s)
        end = hi + 1
    return bytes(out)


def wanted(name):
    if name.startswith(('Adobe-', 'UCS2-')) or name.endswith(('-UCS2', '-UCS2C')):
        return False
    return not name.startswith('Identity')


blobs, blob_index = [], {}


def add_blob(raw):
    if raw not in blob_index:
        c = zlib.compressobj(9, zlib.DEFLATED, -15)
        blob_index[raw] = len(blobs)
        blobs.append(c.compress(raw) + c.flush())
    return blob_index[raw]


entries = []
for coll in sorted(os.listdir(SRC)):
    d = os.path.join(SRC, coll)
    if not os.path.isdir(d):
        continue
    for name in sorted(os.listdir(d)):
        if not wanted(name):
            continue
        m = parse(os.path.join(d, name))
        form = form_of(name)
        mapping = {}
        for lo, hi, cid, step in m['maps']:
            for i, c in enumerate(codes(lo, hi)):
                mapping[key(c, form)] = cid + i * step
        entries.append((name, m, form, add_blob(cid_table(mapping))))

ucs2 = []
for o in ORDERINGS:
    p = os.path.join(SRC, f'Adobe-{o}', f'Adobe-{o}-UCS2')
    ucs2.append((o, add_blob(unicode_table(parse(p)['uni']))))

entries.sort(key=lambda e: e[0])
names = [e[0] for e in entries]
offs, pos = [], 0
for b in blobs:
    offs.append((pos, pos + len(b))); pos += len(b)
open(os.path.join(OUT, 'cmaps.bin'), 'wb').write(b''.join(blobs))


def cs(lo, hi):
    pad = lambda b: '[' + ', '.join(f'0x{x:02X}' for x in b + bytes(4 - len(b))) + ']'
    return f'Codespace::of({len(lo)}, {pad(lo)}, {pad(hi)})'


def ordering(o):
    return o if o in ORDERINGS else 'Other'


o = ['// @generated by tools/gen_cmaps.py from Adobe cmap-resources (BSD-3-Clause, see',
     '// LICENSE-adobe-cmap). Do not edit.\n',
     'use super::Codespace;',
     'use super::predefined::{Entry, Form, Ordering};\n',
     'pub(super) static CMAPS: &[u8] = include_bytes!("cmaps.bin");\n',
     f'/// Byte ranges of [`CMAPS`] holding each table.\npub(super) static TABLES: [(u32, u32); {len(offs)}] = [']
o += [f'    ({a}, {b}),' for a, b in offs]
o.append('];\n')
o.append(f'pub(super) const ENTRY_COUNT: usize = {len(entries)};\n')
o.append('/// Sorted by name.\npub(super) static ENTRIES: [Entry; ENTRY_COUNT] = [')
for name, m, form, t in entries:
    parent = names.index(m['usecmap']) if m['usecmap'] else 'u16::MAX'
    space = ', '.join(cs(lo, hi) for lo, hi in m['codespace'])
    o.append(f'    Entry {{ name: "{name}", parent: {parent}, vertical: {str(m["wmode"] == 1).lower()}, '
             f'form: Form::{form.title() if form != "Bytes" else "Bytes"}, ordering: Ordering::{ordering(m["ordering"])}, '
             f'codespace: &[{space}], table: {t} }},')
o.append('];\n')
o.append(f'pub(super) const UNICODE_COUNT: usize = {len(ucs2)};\n')
o.append('/// CID to Unicode tables of the character collections.\npub(super) static UNICODE: [(Ordering, u16); UNICODE_COUNT] = [')
o += [f'    (Ordering::{n}, {t}),' for n, t in ucs2]
o.append('];')
open(os.path.join(OUT, 'data.rs'), 'w').write('\n'.join(o) + '\n')
raw = sum(len(k) for k in blob_index)
print(f'{len(entries)} cmaps, {len(blobs)} tables, {raw} raw bytes, {pos} compressed', file=sys.stderr)
