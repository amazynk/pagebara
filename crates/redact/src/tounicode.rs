//! Removing codes from a `/ToUnicode` CMap, leaving the rest of it as written.

use std::collections::BTreeSet;
use std::fmt::Write as _;

/// A CMap token: its bytes and where it starts.
#[derive(Clone, Copy)]
struct Tok<'a> {
    at: usize,
    text: &'a [u8],
}

fn tokens(src: &[u8]) -> Vec<Tok<'_>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < src.len() {
        let b = src[i];
        let start = i;
        match b {
            b'%' => {
                while i < src.len() && src[i] != b'\n' && src[i] != b'\r' {
                    i += 1;
                }
                continue;
            }
            b if b.is_ascii_whitespace() => {
                i += 1;
                continue;
            }
            b'<' if src.get(i + 1) == Some(&b'<') => i += 2,
            b'>' if src.get(i + 1) == Some(&b'>') => i += 2,
            b'<' => {
                while i < src.len() && src[i] != b'>' {
                    i += 1;
                }
                i = (i + 1).min(src.len());
            }
            b'(' => {
                let mut depth = 0;
                while i < src.len() {
                    match src[i] {
                        b'\\' => i += 1,
                        b'(' => depth += 1,
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                i += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
                i = i.min(src.len());
            }
            b'[' | b']' | b'{' | b'}' => i += 1,
            _ => {
                i += 1;
                while i < src.len()
                    && !src[i].is_ascii_whitespace()
                    && !b"<>[](){}/%".contains(&src[i])
                {
                    i += 1;
                }
            }
        }
        out.push(Tok {
            at: start,
            text: &src[start..i],
        });
    }
    out
}

/// The bytes of a hex string token.
fn hex(t: &[u8]) -> Option<Vec<u8>> {
    let inner = t.strip_prefix(b"<")?.strip_suffix(b">")?;
    let digits: Vec<u8> = inner
        .iter()
        .filter(|b| !b.is_ascii_whitespace())
        .map(|&b| (b as char).to_digit(16).map(|d| d as u8))
        .collect::<Option<_>>()?;
    Some(
        digits
            .chunks(2)
            .map(|c| c[0] << 4 | c.get(1).copied().unwrap_or(0))
            .collect(),
    )
}

fn code(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0, |a, &b| a << 8 | u32::from(b))
}

fn hex_of(out: &mut String, bytes: &[u8]) {
    out.push('<');
    for b in bytes {
        let _ = write!(out, "{b:02X}");
    }
    out.push('>');
}

/// One mapping kept: source code bytes and the destination as written.
enum Entry {
    /// Source code and destination token.
    Char(Vec<u8>, String),
    /// A range kept whole: low, high and the destination as written.
    Range(Vec<u8>, Vec<u8>, String),
}

/// `src` without mappings for `codes`; `None` when nothing changes or it does not parse.
pub(crate) fn without(src: &[u8], codes: &BTreeSet<u32>) -> Option<Vec<u8>> {
    let toks = tokens(src);
    let mut out = Vec::with_capacity(src.len());
    let mut at = 0;
    let mut changed = false;
    let mut i = 0;
    while i < toks.len() {
        let kind = match toks[i].text {
            b"beginbfchar" => 2,
            b"beginbfrange" => 3,
            _ => {
                i += 1;
                continue;
            }
        };
        let end_kw: &[u8] = if kind == 2 {
            b"endbfchar"
        } else {
            b"endbfrange"
        };
        let end = (i + 1..toks.len()).find(|&k| toks[k].text == end_kw)?;
        // The count before the keyword is rewritten with the section.
        let start = if i > 0 && toks[i - 1].text.iter().all(u8::is_ascii_digit) {
            toks[i - 1].at
        } else {
            toks[i].at
        };
        let body = &toks[i + 1..end];
        let mut entries = Vec::new();
        let mut dropped = false;
        let mut k = 0;
        let raw = |t: &Tok| String::from_utf8_lossy(t.text).into_owned();
        while k < body.len() {
            if kind == 2 {
                let (Some(s), Some(d)) = (body.get(k), body.get(k + 1)) else {
                    break;
                };
                let sb = hex(s.text)?;
                if codes.contains(&code(&sb)) {
                    dropped = true;
                } else {
                    entries.push(Entry::Char(sb, raw(d)));
                }
                k += 2;
                continue;
            }
            let (Some(lo), Some(hi)) = (body.get(k), body.get(k + 1)) else {
                break;
            };
            let (lob, hib) = (hex(lo.text)?, hex(hi.text)?);
            let (l, h) = (code(&lob), code(&hib));
            // The destination: a string, or an array of strings up to `]`.
            let array = body.get(k + 2).is_some_and(|t| t.text == b"[");
            let (dst, next): (Vec<Tok>, usize) = if array {
                let close = (k + 3..body.len()).find(|&j| body[j].text == b"]")?;
                (body[k + 3..close].to_vec(), close + 1)
            } else {
                (body.get(k + 2).into_iter().copied().collect(), k + 3)
            };
            k = next;
            let hit = codes.range(l..=h).next().is_some();
            if !hit || h < l || h - l > 0xFFFF {
                let parts: Vec<String> = dst.iter().map(raw).collect();
                let text = if array {
                    format!("[{}]", parts.join(" "))
                } else {
                    parts.join(" ")
                };
                entries.push(Entry::Range(lob, hib, text));
                continue;
            }
            dropped = true;
            let base = if array {
                None
            } else {
                dst.first().and_then(|t| hex(t.text))
            };
            for (n, c) in (l..=h).enumerate() {
                if codes.contains(&c) {
                    continue;
                }
                let sb = c.to_be_bytes()[4 - lob.len().clamp(1, 4)..].to_vec();
                let d = if array {
                    dst.get(n).map(raw)
                } else {
                    base.as_ref().map(|b| {
                        let mut b = b.clone();
                        if let Some(last) = b.last_mut() {
                            *last = last.wrapping_add(n as u8);
                        }
                        let mut s = String::new();
                        hex_of(&mut s, &b);
                        s
                    })
                };
                if let Some(d) = d {
                    entries.push(Entry::Char(sb, d));
                }
            }
        }
        if dropped {
            changed = true;
            out.extend_from_slice(&src[at..start]);
            let mut s = String::new();
            let (chars, ranges): (Vec<_>, Vec<_>) =
                entries.iter().partition(|e| matches!(e, Entry::Char(..)));
            for chunk in chars.chunks(100) {
                let _ = writeln!(s, "{} beginbfchar", chunk.len());
                for e in chunk {
                    if let Entry::Char(c, d) = e {
                        hex_of(&mut s, c);
                        let _ = writeln!(s, " {d}");
                    }
                }
                s.push_str("endbfchar\n");
            }
            for chunk in ranges.chunks(100) {
                let _ = writeln!(s, "{} beginbfrange", chunk.len());
                for e in chunk {
                    if let Entry::Range(l, h, d) = e {
                        hex_of(&mut s, l);
                        s.push(' ');
                        hex_of(&mut s, h);
                        let _ = writeln!(s, " {d}");
                    }
                }
                s.push_str("endbfrange\n");
            }
            out.extend_from_slice(s.as_bytes());
            at = toks[end].at + toks[end].text.len();
        }
        i = end + 1;
    }
    if !changed {
        return None;
    }
    out.extend_from_slice(&src[at..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unicode(src: &[u8], c: u32) -> String {
        let m = pagebara_content::font::cmap::CMap::parse(src);
        let mut s = String::new();
        m.unicode(c, &mut s);
        s
    }

    #[test]
    fn drops_chars_and_splits_ranges() {
        let src = b"/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
            1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n\
            2 beginbfchar\n<0001> <004A>\n<0002> <0061>\nendbfchar\n\
            2 beginbfrange\n<0010> <0013> <0041>\n<0020> <0021> [<0078> <0079>]\nendbfrange\n\
            endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n";
        let codes = BTreeSet::from([1, 0x11, 0x21]);
        let out = without(src, &codes).unwrap();
        assert_eq!(unicode(&out, 1), "");
        assert_eq!(unicode(&out, 2), "a");
        assert_eq!(unicode(&out, 0x10), "A");
        assert_eq!(unicode(&out, 0x11), "");
        assert_eq!(unicode(&out, 0x12), "C");
        assert_eq!(unicode(&out, 0x13), "D");
        assert_eq!(unicode(&out, 0x20), "x");
        assert_eq!(unicode(&out, 0x21), "");
        assert!(out.ends_with(b"end\nend\n"));
        assert!(without(src, &BTreeSet::from([0x99])).is_none());
    }
}
