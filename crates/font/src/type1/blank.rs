//! Blanking glyphs of a Type 1 program: their charstrings become `hsbw endchar` (or
//! `sbw endchar`) with the original metrics and their names neutral ones, subroutines no
//! remaining glyph calls become `return`, and the private part is encrypted again.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Range;

use super::charstring::{self, Seac};
use super::{CHARSTRING_KEY, EEXEC_KEY, Lexer, Private, Tok, Type1, decrypt, find, is_hex};
use crate::data::BaseEncoding;
use crate::outline::{NullSink, Pen};

/// A Type 1 program with glyphs blanked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Blanked {
    pub data: Vec<u8>,
    pub glyphs: Vec<u16>,
    /// `Length1`, `Length2` and `Length3` of the new program: clear text, encrypted part
    /// and trailer.
    pub lengths: [usize; 3],
}

/// The trailer of 512 zeros (and `cleartomark`) that ends a Type 1 program.
const TRAILER_ZEROS: usize = 512;

impl Type1 {
    /// Runs glyph `gid`, marking the subroutines it calls.
    fn trace(&self, gid: u16, used: &RefCell<Vec<bool>>) -> Option<charstring::Result> {
        let cs = self
            .bytes
            .get(self.glyphs.get(usize::from(gid))?.1.clone())?;
        let subrs = |i: usize| {
            if let Some(u) = used.borrow_mut().get_mut(i) {
                *u = true;
            }
            self.bytes.get(self.subrs.get(i)?.clone())
        };
        let mut sink = NullSink;
        let mut pen = Pen::new(&mut sink, None);
        charstring::run(cs, &subrs, &mut pen, (0.0, 0.0))
    }

    fn seac_parts(&self, s: &Seac) -> impl Iterator<Item = u16> + '_ {
        [s.base, s.accent].into_iter().filter_map(|c| {
            let name = BaseEncoding::Standard.glyph_name(c)?;
            self.glyph_by_name(name.as_bytes())
        })
    }
}

/// Blanks `gids` of a Type 1 program (PFA or binary eexec). `lengths` are `Length1` and
/// `Length2` of a PDF `FontFile`, used when they fit. `None` when the program does not
/// parse or a remaining glyph cannot be run.
pub(crate) fn blank(
    data: &[u8],
    gids: &BTreeSet<u16>,
    lengths: Option<[usize; 2]>,
) -> Option<Blanked> {
    let data = super::unwrap_pfb(data);
    let data = &data[..];
    let font = Type1::parse(data)?;
    let n = font.num_glyphs();

    // What the remaining glyphs need.
    let used = RefCell::new(vec![false; font.subrs.len()]);
    // Subrs 0 to 3 are the flex and hint replacement mechanism.
    for u in used.borrow_mut().iter_mut().take(4) {
        *u = true;
    }
    let mut kept: BTreeSet<u16> = (0..n).filter(|g| !gids.contains(g)).collect();
    kept.insert(0);
    let mut todo: Vec<u16> = kept.iter().copied().collect();
    while let Some(g) = todo.pop() {
        let r = font.trace(g, &used)?;
        if let Some(s) = r.seac {
            for part in font.seac_parts(&s) {
                if kept.insert(part) {
                    todo.push(part);
                }
            }
        }
    }
    let kept_names: HashSet<&str> = kept.iter().filter_map(|&g| font.glyph_name(g)).collect();
    let mut gone: Vec<u16> = Vec::new();
    // Name → (new name, new charstring).
    let mut blanks: HashMap<&[u8], (Vec<u8>, Vec<u8>)> = HashMap::new();
    for &g in gids {
        if g >= n || kept.contains(&g) {
            continue;
        }
        let Some(name) = font.glyph_name(g) else {
            continue;
        };
        // A name a remaining glyph also has cannot be told apart from it.
        if kept_names.contains(name) {
            continue;
        }
        let Some(r) = font.trace(g, &RefCell::new(Vec::new())) else {
            continue;
        };
        let mut new = format!("g{g}");
        while font.glyph_by_name(new.as_bytes()).is_some() {
            new.push('_');
        }
        blanks.insert(name.as_bytes(), (new.into_bytes(), empty_glyph(&r)));
        gone.push(g);
    }
    if gone.is_empty() {
        let len = data.len();
        return Some(Blanked {
            data: data.to_vec(),
            glyphs: gone,
            lengths: [len, 0, 0],
        });
    }

    // The encrypted part.
    let eexec = find(data, b"eexec")?;
    let mut start = eexec + 5;
    while data.get(start).is_some_and(|b| b" \t\r\n".contains(b)) {
        start += 1;
    }
    let rest = &data[start..];
    let hex = rest.len() >= 4 && rest[..4].iter().all(|&b| is_hex(b));
    let end = match lengths.map(|[a, b]| a.saturating_add(b)) {
        Some(e) if e > start + 4 && e <= data.len() => e,
        _ => start + encrypted_end(rest, hex),
    };
    let cipher = if hex {
        unhex(&data[start..end])
    } else {
        data[start..end].to_vec()
    };
    let plain = decrypt(&cipher, EEXEC_KEY, 0);

    let private = Private::scan(&plain, 4.min(plain.len()));
    let skip = usize::try_from(private.len_iv).ok();
    let used = used.into_inner();
    let mut edits: Vec<(Range<usize>, Vec<u8>)> = Vec::new();
    let entry = |name: &[u8], rd: &[u8], cs: &[u8]| {
        let cs = match skip {
            Some(k) => encrypt(cs, CHARSTRING_KEY, k),
            None => cs.to_vec(),
        };
        let mut e = name.to_vec();
        e.extend_from_slice(format!("{} ", cs.len()).as_bytes());
        e.extend_from_slice(rd);
        e.push(b' ');
        e.extend(cs);
        e
    };
    for e in &private.glyphs {
        if let Some((name, cs)) = blanks.get(&plain[e.name.clone()]) {
            let mut head = b"/".to_vec();
            head.extend_from_slice(name);
            head.push(b' ');
            edits.push((e.head..e.bin.end, entry(&head, &plain[e.rd.clone()], cs)));
        }
    }
    for e in &private.subrs {
        if !used.get(e.index).copied().unwrap_or(true) {
            edits.push((e.head..e.bin.end, entry(b"", &plain[e.rd.clone()], &[11])));
        }
    }
    let plain = splice(&plain, edits);

    let clear = rename_encoding(&data[..eexec], &blanks);
    let mut out = clear;
    out.extend_from_slice(&data[eexec..start]);
    let enc = encrypt(&plain, EEXEC_KEY, 0);
    let enc = if hex { to_hex(&enc) } else { enc };
    let length1 = out.len();
    out.extend_from_slice(&enc);
    out.extend_from_slice(&data[end..]);
    Some(Blanked {
        lengths: [length1, enc.len(), data.len() - end],
        data: out,
        glyphs: gone,
    })
}

/// Where the encrypted part starting `rest` ends: before the trailer of zeros.
fn encrypted_end(rest: &[u8], hex: bool) -> usize {
    let mut end = match rest.windows(11).rposition(|w| w == b"cleartomark") {
        Some(p) => p,
        None if hex => rest
            .iter()
            .position(|b| !is_hex(*b) && !b.is_ascii_whitespace())
            .unwrap_or(rest.len()),
        None => return rest.len(),
    };
    // Back over the trailer's zeros (only its 512 when there are more: the rest may be
    // encrypted bytes that happen to be zeros).
    let mut zeros = 0;
    while end > 0 && zeros < TRAILER_ZEROS {
        match rest[end - 1] {
            b'0' => zeros += 1,
            b if b.is_ascii_whitespace() => {}
            _ => break,
        }
        end -= 1;
    }
    while end > 0 && rest[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    end
}

fn unhex(d: &[u8]) -> Vec<u8> {
    let digits: Vec<u8> = d
        .iter()
        .filter_map(|&b| (b as char).to_digit(16).map(|v| v as u8))
        .collect();
    digits
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[h, l]| h << 4 | l)
        .collect()
}

fn to_hex(d: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(d.len() * 2 + d.len() / 32);
    for (i, b) in d.iter().enumerate() {
        out.extend_from_slice(format!("{b:02x}").as_bytes());
        if i % 32 == 31 {
            out.push(b'\n');
        }
    }
    out
}

/// Encrypts `plain` after `skip` leading bytes (the inverse of [`decrypt`]).
pub(super) fn encrypt(plain: &[u8], key: u16, skip: usize) -> Vec<u8> {
    let mut r = key;
    std::iter::repeat_n(&0u8, skip)
        .chain(plain)
        .map(|&p| {
            let c = p ^ (r >> 8) as u8;
            r = (u16::from(c).wrapping_add(r))
                .wrapping_mul(52845)
                .wrapping_add(22719);
            c
        })
        .collect()
}

fn splice(d: &[u8], mut edits: Vec<(Range<usize>, Vec<u8>)>) -> Vec<u8> {
    edits.sort_by_key(|e| e.0.start);
    let mut out = Vec::with_capacity(d.len());
    let mut at = 0;
    for (r, new) in edits {
        if r.start < at {
            continue;
        }
        out.extend_from_slice(&d[at..r.start]);
        out.extend(new);
        at = r.end;
    }
    out.extend_from_slice(&d[at..]);
    out
}

/// A charstring drawing nothing with the metrics of `r`.
fn empty_glyph(r: &charstring::Result) -> Vec<u8> {
    let mut cs = Vec::new();
    match r.sbw {
        Some(v) => {
            for x in v {
                number(&mut cs, x);
            }
            cs.extend([12, 7]);
        }
        None => {
            number(&mut cs, r.sbx);
            number(&mut cs, r.width);
            cs.push(13);
        }
    }
    cs.push(14);
    cs
}

/// A charstring number; fractions as `a 64 div`.
fn number(out: &mut Vec<u8>, v: f32) {
    let int = |out: &mut Vec<u8>, v: i32| match v {
        -107..=107 => out.push((v + 139) as u8),
        108..=1131 => out.extend([((v - 108) / 256 + 247) as u8, ((v - 108) % 256) as u8]),
        -1131..=-108 => out.extend([((-v - 108) / 256 + 251) as u8, ((-v - 108) % 256) as u8]),
        _ => {
            out.push(255);
            out.extend(v.to_be_bytes());
        }
    };
    if v.fract() == 0.0 {
        int(out, v as i32);
    } else {
        int(out, (v * 64.0).round() as i32);
        int(out, 64);
        out.extend([12, 12]);
    }
}

/// The clear text with blanked glyph names in its `/Encoding` renamed.
fn rename_encoding(clear: &[u8], blanks: &HashMap<&[u8], (Vec<u8>, Vec<u8>)>) -> Vec<u8> {
    let mut edits = Vec::new();
    let mut lx = Lexer {
        d: clear,
        pos: 0,
        start: 0,
    };
    let mut in_encoding = false;
    while let Some(t) = lx.next() {
        match t {
            Tok::Name(b"Encoding") => in_encoding = true,
            Tok::Op(b"def") | Tok::Op(b"readonly") => in_encoding = false,
            Tok::Name(n) if in_encoding => {
                if let Some((new, _)) = blanks.get(n) {
                    edits.push((lx.start + 1..lx.pos, new.clone()));
                }
            }
            _ => {}
        }
    }
    splice(clear, edits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outline::Path;

    /// A charstring of numbers then an operator (negative: escaped `12 x`).
    fn cs(ops: &[&[i32]]) -> Vec<u8> {
        let mut out = Vec::new();
        for o in ops {
            let (&code, args) = o.split_last().unwrap();
            for &a in args {
                number(&mut out, a as f32);
            }
            if code < 0 {
                out.extend([12, (-code) as u8]);
            } else {
                out.push(code as u8);
            }
        }
        out
    }

    /// A Type 1 font: `A` and `B` call subroutines 4 and 5, `C` is `seac` of `A` and `B`.
    fn font(hex: bool) -> Vec<u8> {
        let subrs = [
            cs(&[&[3, 0, -16], &[11]]),
            cs(&[&[0, 1, -16], &[11]]),
            cs(&[&[0, 2, -16], &[11]]),
            cs(&[&[11]]),
            cs(&[&[0, 0, 21], &[0, 700, 5], &[300, 0, 5], &[9], &[11]]),
            cs(&[&[0, 0, 21], &[0, 500, 5], &[200, 0, 5], &[9], &[11]]),
        ];
        let glyphs = [
            (".notdef", cs(&[&[0, 250, 13], &[14]])),
            ("A", cs(&[&[20, 500, 13], &[4, 10], &[14]])),
            ("B", cs(&[&[30, 400, 13], &[5, 10], &[14]])),
            ("C", cs(&[&[20, 500, 13], &[20, 300, 0, 65, 66, -6]])),
        ];
        let mut p = b"dup /Private 8 dict dup begin\n/RD{string currentfile exch readstring pop}executeonly def\n/ND{noaccess def}executeonly def\n/NP{noaccess put}executeonly def\n/lenIV 4 def\n".to_vec();
        p.extend(format!("/Subrs {} array\n", subrs.len()).bytes());
        for (i, s) in subrs.iter().enumerate() {
            let e = encrypt(s, CHARSTRING_KEY, 4);
            p.extend(format!("dup {i} {} RD ", e.len()).bytes());
            p.extend(e);
            p.extend(b" NP\n");
        }
        p.extend(b"ND\n2 index /CharStrings 4 dict dup begin\n");
        for (n, g) in &glyphs {
            let e = encrypt(g, CHARSTRING_KEY, 4);
            p.extend(format!("/{n} {} RD ", e.len()).bytes());
            p.extend(e);
            p.extend(b" ND\n");
        }
        p.extend(b"end\nend\nreadonly put\nnoaccess put\ndup /FontName get exch definefont pop\nmark currentfile closefile\n");
        let enc = encrypt(&p, EEXEC_KEY, 4);
        let mut f = b"%!PS-AdobeFont-1.0: Test\n/FontName /Test def\n/FontMatrix [0.001 0 0 0.001 0 0] readonly def\n/Encoding 256 array\n0 1 255 {1 index exch /.notdef put} for\ndup 65 /A put\ndup 66 /B put\ndup 67 /C put\nreadonly def\ncurrentfile eexec\n".to_vec();
        if hex {
            f.extend(to_hex(&enc));
        } else {
            f.extend(enc);
        }
        f.push(b'\n');
        for _ in 0..8 {
            f.extend([b'0'; 64]);
            f.push(b'\n');
        }
        f.extend(b"cleartomark\n");
        f
    }

    fn outline(f: &Type1, g: u16) -> Path {
        let mut p = Path::new();
        f.outline(g, &mut p);
        p
    }

    fn check(hex: bool) {
        let data = font(hex);
        let before = Type1::parse(&data).unwrap();
        let [a, b, c] = [b"A", b"B", b"C"].map(|n| before.glyph_by_name(n).unwrap());
        assert!(!outline(&before, c).is_empty());

        // `B` stays while `C` draws it.
        let r = blank(&data, &BTreeSet::from([b]), None).unwrap();
        assert!(r.glyphs.is_empty());

        let r = blank(&data, &BTreeSet::from([b, c]), None).unwrap();
        assert_eq!(r.glyphs, [b, c]);
        let [l1, l2, l3] = r.lengths;
        assert_eq!(l1 + l2 + l3, r.data.len());
        assert!(r.data[l1 + l2..].trim_ascii_start().starts_with(b"0000"));
        let after = Type1::parse(&r.data).unwrap();
        assert_eq!(after.num_glyphs(), before.num_glyphs());
        assert_eq!(outline(&after, a), outline(&before, a));
        assert!(outline(&after, b).is_empty());
        assert!(outline(&after, c).is_empty());
        assert_eq!(after.advance(b), Some(400.0));
        assert_eq!(after.advance(c), Some(500.0));
        // Neutral names, in the charstrings and the encoding.
        assert_eq!(after.glyph_by_name(b"B"), None);
        assert_eq!(after.glyph_by_name(b"C"), None);
        assert_eq!(after.glyph_name(b), Some(&*format!("g{b}")));
        assert_eq!(after.encoding_glyph(66), Some(b));
        assert_eq!(after.encoding_glyph(65), Some(a));
        // Subroutine 5 only `B` called; the private part holds nothing of it.
        let subr = |f: &Type1, i: usize| f.bytes[f.subrs[i].clone()].to_vec();
        assert_eq!(subr(&after, 5), [11]);
        assert_eq!(subr(&after, 4), subr(&before, 4));
        let (_, private) = super::super::split(&r.data).unwrap();
        assert!(find(&private, b"/B ").is_none() && find(&private, b"/C ").is_none());
        assert!(find(&r.data, b"/B put").is_none());
    }

    #[test]
    fn binary_program_loses_glyphs() {
        check(false);
    }

    #[test]
    fn hex_program_loses_glyphs() {
        check(true);
    }

    #[test]
    fn pdf_lengths_bound_the_encrypted_part() {
        let data = font(false);
        let l1 = find(&data, b"eexec").unwrap() + 6;
        let l2 = data.len() - l1 - (8 * 65 + 13);
        let c = Type1::parse(&data).unwrap().glyph_by_name(b"C").unwrap();
        let r = blank(&data, &BTreeSet::from([c]), Some([l1, l2])).unwrap();
        assert_eq!(r.glyphs, [c]);
        assert_eq!(r.lengths[2], 8 * 65 + 13);
        assert!(Type1::parse(&r.data).is_some());
    }
}
