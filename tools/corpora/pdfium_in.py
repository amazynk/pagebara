"""Expands PDFium's .in test templates under DIR into .pdf files next to them.

usage: python3 -I pdfium_in.py DIR

A re-implementation of the template syntax (not PDFium's script): {{header [V]}},
{{object N G}}, {{streamlen}}, {{include FILE}}, {{xref}}, {{trailer}}, {{trailersize}},
{{startxref}}, {{endxref}}, {{startxrefobj N G}}.
"""

import os
import re
import sys

TOKEN = re.compile(rb"\{\{(\w+)((?: [^}]*)?)\}\}")


def expand_includes(path, depth=0):
    data = open(path, "rb").read()
    if depth > 8:
        return data

    def inc(m):
        if m.group(1) != b"include":
            return m.group(0)
        name = m.group(2).strip().decode()
        target = os.path.join(os.path.dirname(path), name)
        if not os.path.isfile(target):
            return b""
        return expand_includes(target, depth + 1)

    return TOKEN.sub(inc, data)


def stream_lengths(data):
    """The length of the stream after each {{streamlen}}, in order."""
    out = []
    for m in re.finditer(rb"\{\{stre(?:am|an)len\}\}", data):
        start = data.find(b"stream", m.end())
        if start < 0:
            out.append(0)
            continue
        start += len(b"stream")
        if data[start:start + 2] == b"\r\n":
            start += 2
        elif data[start:start + 1] in (b"\n", b"\r"):
            start += 1
        end = data.find(b"endstream", start)
        if end < 0:
            end = len(data)
        if data[end - 1:end] == b"\n":
            end -= 1
        out.append(end - start)
    return out


def expand(data):
    lengths = iter(stream_lengths(data))
    out = bytearray()
    offsets = {}
    xref_at = 0
    pos = 0
    for m in TOKEN.finditer(data):
        out += data[pos:m.start()]
        pos = m.end()
        name, args = m.group(1), m.group(2).split()
        if name == b"header":
            version = args[0].decode() if args else "1.7"
            out += b"%PDF-" + version.encode() + b"\n%\xa0\xf2\xa4\xf4"
        elif name == b"object":
            num, gen = int(args[0]), int(args[1])
            offsets[num] = (len(out), gen)
            out += b"%d %d obj" % (num, gen)
        elif name in (b"streamlen", b"streanlen"):
            out += b"/Length %d" % next(lengths, 0)
        elif name == b"xref":
            xref_at = len(out)
            size = max(offsets, default=0) + 1
            out += b"xref\n0 %d\n" % size
            for n in range(size):
                if n in offsets:
                    out += b"%010d %05d n \n" % offsets[n]
                else:
                    out += b"0000000000 65535 f \n"
            if out.endswith(b"\n") and data[pos:pos + 1] == b"\n":
                out = out[:-1]
        elif name == b"trailer":
            size = max(offsets, default=0) + 1
            out += b"trailer <<\n  /Root 1 0 R\n  /Size %d\n>>" % size
        elif name == b"trailersize":
            out += b"/Size %d" % (max(offsets, default=0) + 1)
        elif name in (b"startxref", b"endxref"):
            out += b"startxref\n%d" % xref_at
        elif name == b"startxrefobj":
            out += b"startxref\n%d" % offsets.get(int(args[0]), (0, 0))[0]
        else:
            out += m.group(0)
    out += data[pos:]
    return bytes(out)


def main(root):
    n = 0
    for base, _, files in os.walk(root):
        for f in files:
            if f.endswith(".in"):
                src = os.path.join(base, f)
                with open(src[:-3] + ".pdf", "wb") as w:
                    w.write(expand(expand_includes(src)))
                n += 1
    print(f"expanded {n} templates")


if __name__ == "__main__":
    main(sys.argv[1])
