//! Blanking glyphs in place: the outlines of chosen glyphs are removed while glyph ids,
//! metrics and the other tables stay, so whatever refers to the remaining glyphs (codes,
//! CIDs, glyph names) keeps working. Redaction uses it so a font subset no longer holds the
//! shapes of removed text.
//!
//! - TrueType: the glyphs become empty in `glyf` (zero length in `loca`).
//! - CFF (bare, or the `CFF ` table of OpenType): the charstrings become `endchar` and
//!   subroutines no remaining glyph calls become `return`, both at their original sizes;
//!   the glyphs of name-keyed fonts get neutral names (`g<id>`) in the charset and in the
//!   custom encoding's supplements.
//! - Type 1: the charstrings become `hsbw endchar` with the original metrics and the
//!   glyphs neutral names (in `/CharStrings` and the built-in encoding), unused
//!   subroutines become `return`, and the private part is encrypted again.
//!
//! Glyphs that remaining glyphs draw (composite components, `seac` parts) are kept.

use std::collections::BTreeSet;

use crate::cff::charstring::{self, Ctx, Trace};
use crate::cff::{Cff, Index};
use crate::data::BaseEncoding;
use crate::font::Font;
use crate::outline::{NullSink, Pen};
use crate::reader::{Write, u16_at, u32_at};
use crate::sfnt::Sfnt;
use crate::subset::truetype::{closure, format4, write_sfnt};

const ENDCHAR: u8 = 14;
const RETURN: u8 = 11;

/// A font program with glyphs blanked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blanked {
    pub data: Vec<u8>,
    /// The glyphs whose outlines are gone (those asked for, less the ones still needed).
    pub glyphs: Vec<u16>,
    /// For Type 1: the lengths of the clear text, encrypted part and trailer (a PDF
    /// `FontFile`'s `Length1`, `Length2` and `Length3`).
    pub lengths: Option<[usize; 3]>,
}

/// Blanks `gids` of a TrueType, OpenType-CFF, bare CFF or Type 1 program. `None` when the
/// format is not supported (CFF2, collections) or the program does not parse.
pub fn blank(data: &[u8], gids: &BTreeSet<u16>) -> Option<Blanked> {
    match Font::parse(data.to_vec())? {
        Font::Sfnt(s) => blank_sfnt(&s, gids),
        Font::Cff(c) => {
            let (data, glyphs) = blank_cff(&c, gids)?;
            Some(Blanked {
                data,
                glyphs,
                lengths: None,
            })
        }
        Font::Type1(_) => blank_type1(data, gids, None),
    }
}

/// Blanks `gids` of a Type 1 program; `lengths` are the `Length1` and `Length2` of its PDF
/// stream, which say where the encrypted part ends.
pub fn blank_type1(
    data: &[u8],
    gids: &BTreeSet<u16>,
    lengths: Option<[usize; 2]>,
) -> Option<Blanked> {
    let b = crate::type1::blank::blank(data, gids, lengths)?;
    Some(Blanked {
        data: b.data,
        glyphs: b.glyphs,
        lengths: Some(b.lengths),
    })
}

fn blank_sfnt(s: &Sfnt, gids: &BTreeSet<u16>) -> Option<Blanked> {
    let d = s.data();
    let version = u32::from_be_bytes(d.get(..4)?.try_into().ok()?);
    if &d[..4] == b"ttcf" {
        return None;
    }
    let mut replaced: Vec<([u8; 4], Vec<u8>)> = Vec::new();
    let glyphs = if s.has_glyf() {
        let loca = s.loca()?;
        let n = s.num_glyphs();
        let keep = closure(s, (0..n).filter(|g| !gids.contains(g)));
        let gone: Vec<u16> = gids
            .iter()
            .copied()
            .filter(|&g| g != 0 && g < n && keep.binary_search(&g).is_err())
            .collect();
        if gone.is_empty() {
            return Some(Blanked {
                data: d.to_vec(),
                glyphs: gone,
                lengths: None,
            });
        }
        let mut glyf = Vec::new();
        let mut offsets = Vec::with_capacity((usize::from(n) + 1) * 4);
        for g in 0..n {
            offsets.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
            if gone.binary_search(&g).is_err() {
                glyf.extend_from_slice(loca.glyph(g).unwrap_or_default());
                glyf.resize(glyf.len().div_ceil(4) * 4, 0);
            }
        }
        offsets.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
        let mut head = s.table(b"head").filter(|h| h.len() >= 54)?.to_vec();
        head[50..52].copy_from_slice(&1u16.to_be_bytes());
        replaced.push((*b"glyf", glyf));
        replaced.push((*b"loca", offsets));
        replaced.push((*b"head", head));
        gone
    } else {
        let cff = s.cff()?;
        let (table, gone) = blank_cff(cff, gids)?;
        if gone.is_empty() {
            return Some(Blanked {
                data: d.to_vec(),
                glyphs: gone,
                lengths: None,
            });
        }
        replaced.push((*b"CFF ", table));
        gone
    };
    // Character maps and glyph names would still say what the glyphs were.
    if let Some(cmap) = cmap_without(s, &glyphs) {
        replaced.push((*b"cmap", cmap));
    }
    if let Some(post) = post_without(s, &glyphs) {
        replaced.push((*b"post", post));
    }
    let mut tables = replaced;
    for tag in s.table_tags() {
        // A digital signature would no longer match.
        if tag == b"DSIG" || tables.iter().any(|t| &t.0 == tag) {
            continue;
        }
        tables.push((*tag, s.table(tag)?.to_vec()));
    }
    if let Some(h) = tables.iter_mut().find(|t| &t.0 == b"head")
        && h.1.len() >= 12
    {
        h.1[8..12].fill(0);
    }
    Some(Blanked {
        data: write_sfnt(version, tables),
        glyphs,
        lengths: None,
    })
}

/// `(code, glyph)` pairs of a cmap subtable.
type Mappings = Vec<(u32, u16)>;

/// The `cmap` table without mappings to `gone` glyphs, rebuilt as format 4 (or 12)
/// subtables; `None` when no mapping leads to them.
fn cmap_without(s: &Sfnt, gone: &[u16]) -> Option<Vec<u8>> {
    let mut subs: Vec<(u16, u16, Mappings)> = Vec::new();
    let mut hit = false;
    for t in s.cmap_subtables() {
        if t.format == 14 {
            continue;
        }
        let mut map = Vec::new();
        s.cmap_for_each(t, &mut |c, g| {
            if gone.binary_search(&g).is_ok() {
                hit = true;
            } else if g != 0 {
                map.push((c, g));
            }
        });
        map.sort_unstable();
        map.dedup_by_key(|m| m.0);
        subs.push((t.platform, t.encoding, map));
    }
    if !hit {
        return None;
    }
    let bodies: Vec<Vec<u8>> = subs
        .iter()
        .map(|(_, _, map)| {
            let small: Option<Vec<(u16, u16)>> = map
                .iter()
                .map(|&(c, g)| Some((u16::try_from(c).ok()?, g)))
                .collect();
            match small.map(|m| format4(&m)) {
                Some(b) if b.len() <= 0xFFFF => b,
                _ => format12(map),
            }
        })
        .collect();
    let mut t = Vec::new();
    t.u16(0);
    t.u16(subs.len() as u16);
    let mut off = 4 + 8 * subs.len();
    for ((p, e, _), b) in subs.iter().zip(&bodies) {
        t.u16(*p);
        t.u16(*e);
        t.u32(off as u32);
        off += b.len();
    }
    for b in bodies {
        t.extend(b);
    }
    Some(t)
}

fn format12(map: &[(u32, u16)]) -> Vec<u8> {
    let mut groups: Vec<(u32, u32, u32)> = Vec::new();
    for &(c, g) in map {
        match groups.last_mut() {
            Some(gr) if gr.1 + 1 == c && gr.2 + (c - gr.0) == u32::from(g) => gr.1 = c,
            _ => groups.push((c, c, u32::from(g))),
        }
    }
    let mut t = Vec::new();
    t.u16(12);
    t.u16(0);
    t.u32((16 + 12 * groups.len()) as u32);
    t.u32(0);
    t.u32(groups.len() as u32);
    for (a, b, g) in groups {
        t.u32(a);
        t.u32(b);
        t.u32(g);
    }
    t
}

/// A format 2 `post` table without the names of `gone` glyphs; `None` when it has none.
fn post_without(s: &Sfnt, gone: &[u16]) -> Option<Vec<u8>> {
    let p = s.table(b"post")?;
    if u32_at(p, 0)? != 0x20000 {
        return None;
    }
    let n = usize::from(u16_at(p, 32)?);
    let mut idx: Vec<u16> = (0..n)
        .map(|i| u16_at(p, 34 + 2 * i))
        .collect::<Option<_>>()?;
    let mut names: Vec<&[u8]> = Vec::new();
    let mut pos = 34 + 2 * n;
    while let Some(&len) = p.get(pos) {
        let Some(name) = p.get(pos + 1..pos + 1 + usize::from(len)) else {
            break;
        };
        names.push(name);
        pos += 1 + usize::from(len);
    }
    let named = |g: &u16| idx.get(usize::from(*g)).is_some_and(|&i| i != 0);
    if !gone.iter().any(named) {
        return None;
    }
    for &g in gone {
        if let Some(i) = idx.get_mut(usize::from(g)) {
            *i = 0;
        }
    }
    // Custom names still referred to, renumbered in order.
    let mut kept: Vec<u16> = idx.iter().copied().filter(|&i| i >= 258).collect();
    kept.sort_unstable();
    kept.dedup();
    let mut out = p[..32].to_vec();
    out.u16(n as u16);
    for i in &idx {
        out.u16(match kept.binary_search(i) {
            Ok(k) if *i >= 258 => 258 + k as u16,
            _ => *i,
        });
    }
    for i in kept {
        let name = names.get(usize::from(i - 258)).copied().unwrap_or_default();
        out.u8(name.len() as u8);
        out.extend_from_slice(name);
    }
    Some(out)
}

/// The CFF table with `gids` blanked, and the glyphs blanked.
fn blank_cff(cff: &Cff, gids: &BTreeSet<u16>) -> Option<(Vec<u8>, Vec<u16>)> {
    if cff.cff2 {
        return None;
    }
    let d = cff.table_data();
    let n = cff.num_glyphs();
    let cid_keyed = !cff.fds.is_empty();
    let private = |g: u16| {
        if cid_keyed {
            &cff.fds[cff.fd_index(g)].private
        } else {
            &cff.private
        }
    };
    let mut global = vec![false; cff.gsubrs.count as usize];
    let mut locals: Vec<Vec<bool>> = if cid_keyed {
        cff.fds
            .iter()
            .map(|f| vec![false; f.private.subrs.count as usize])
            .collect()
    } else {
        vec![vec![false; cff.private.subrs.count as usize]]
    };
    // Trace what the kept glyphs use, adding `seac` parts to them.
    let mut kept: BTreeSet<u16> = (0..n).filter(|g| !gids.contains(g)).collect();
    kept.insert(0);
    let mut todo: Vec<u16> = kept.iter().copied().collect();
    while let Some(g) = todo.pop() {
        let fd = if cid_keyed { cff.fd_index(g) } else { 0 };
        let p = private(g);
        let ctx = Ctx {
            data: d,
            gsubrs: &cff.gsubrs,
            lsubrs: &p.subrs,
            cff2: false,
            regions: &[],
            vsindex: 0,
        };
        let mut trace = Trace {
            local: std::mem::take(&mut locals[fd]),
            global: std::mem::take(&mut global),
        };
        let cs = cff.charstrings.get(d, u32::from(g)).unwrap_or(&[ENDCHAR]);
        let mut sink = NullSink;
        let mut pen = Pen::new(&mut sink, None);
        let r = charstring::run_traced(&ctx, cs, &mut pen, (0.0, 0.0), Some(&mut trace));
        locals[fd] = trace.local;
        global = trace.global;
        // A glyph that does not run cannot be traced: leave the font alone.
        let r = r?;
        if let Some(s) = r.seac {
            for code in [s.base, s.accent] {
                let part = BaseEncoding::Standard
                    .glyph_name(code)
                    .and_then(|name| cff.glyph_by_name(name.as_bytes()));
                if let Some(part) = part
                    && kept.insert(part)
                {
                    todo.push(part);
                }
            }
        }
    }
    let gone: Vec<u16> = gids
        .iter()
        .copied()
        .filter(|&g| g < n && !kept.contains(&g))
        .collect();
    let mut out = d.to_vec();
    if gone.is_empty() {
        return Some((out, gone));
    }
    for &g in &gone {
        fill(&mut out, &cff.charstrings, d, u32::from(g), ENDCHAR);
    }
    let unused = |out: &mut Vec<u8>, ix: &Index, used: &[bool]| {
        for (i, &u) in used.iter().enumerate() {
            if !u {
                fill(out, ix, d, i as u32, RETURN);
            }
        }
    };
    unused(&mut out, &cff.gsubrs, &global);
    if cid_keyed {
        for (f, used) in cff.fds.iter().zip(&locals) {
            unused(&mut out, &f.private.subrs, used);
        }
    } else {
        unused(&mut out, &cff.private.subrs, &locals[0]);
        if let Some(renamed) = rename(cff, &out, &gone) {
            out = renamed;
        }
    }
    Some((out, gone))
}

/// Top DICT operators whose operand is a SID.
const SID_OPS: [u16; 8] = [0, 1, 2, 3, 4, 1200, 1221, 1222];

/// The name-keyed CFF `d` (`cff` with glyphs blanked) with `gone` glyphs named `g<id>`
/// in a new charset; their old names leave the String INDEX unless something else uses
/// them, and the old charset is zeroed. `None` when the layout is not the usual one.
fn rename(cff: &Cff, d: &[u8], gone: &[u16]) -> Option<Vec<u8>> {
    use crate::data::CFF_STANDARD_STRINGS as STD;
    use crate::subset::cff::{dict_entries, index, int5, op};
    let n = cff.num_glyphs();
    let names = Index::parse(d, usize::from(*d.get(2)?), false)?;
    let tops = Index::parse(d, names.end, false)?;
    let strings = Index::parse(d, tops.end, false)?;
    let top = d.get(tops.range(d, 0)?)?;
    let mut dict_sids = BTreeSet::new();
    let mut offsets: Vec<(u16, Vec<f64>)> = Vec::new();
    crate::cff::parse_dict(top, |o, a| {
        if SID_OPS.contains(&o) {
            dict_sids.extend(a.first().map(|&v| v as u16));
        }
        if matches!(o, 15..=18 | 1236 | 1237 | 24) {
            offsets.push((o, a.to_vec()));
        }
    });
    let mut charset_off = None;
    let mut encoding_off = None;
    for (o, a) in &offsets {
        let off = match o {
            18 => a.get(1),
            _ => a.first(),
        }
        .copied()
        .unwrap_or(0.0);
        match o {
            // Predefined charsets and encodings.
            15 | 16 if off <= 2.0 => {}
            1236 | 1237 | 24 => return None,
            _ if off < strings.end as f64 => return None,
            _ => {}
        }
        if *o == 15 && off > 2.0 {
            charset_off = Some(off as usize);
        }
        if *o == 16 && off > 2.0 {
            encoding_off = Some(off as usize);
        }
    }

    let kept_sids: BTreeSet<u16> = (0..n)
        .filter(|g| gone.binary_search(g).is_err())
        .filter_map(|g| cff.charset_id(g))
        .collect();
    let mut items: Vec<Vec<u8>> = (0..strings.count)
        .map(|i| strings.get(d, i).unwrap_or_default().to_vec())
        .collect();
    let mut sids: Vec<u16> = (0..n).map(|g| cff.charset_id(g).unwrap_or(0)).collect();
    let mut claimed = BTreeSet::new();
    // Old names of glyphs that got a new one, and the new name.
    let mut renamed = Vec::new();
    for &g in gone {
        let mut name = format!("g{g}");
        while cff.glyph_by_name(name.as_bytes()).is_some() {
            name.push('_');
        }
        let old = sids[usize::from(g)];
        let own = old >= STD
            && usize::from(old - STD) < items.len()
            && !kept_sids.contains(&old)
            && !dict_sids.contains(&old)
            && claimed.insert(old);
        if own {
            items[usize::from(old - STD)] = name.into_bytes();
        } else {
            sids[usize::from(g)] = u16::try_from(usize::from(STD) + items.len()).ok()?;
            items.push(name.into_bytes());
            if !kept_sids.contains(&old) {
                renamed.push((old, sids[usize::from(g)]));
            }
        }
    }
    let refs: Vec<&[u8]> = items.iter().map(|i| &i[..]).collect();
    let string_index = index(&refs);

    let entries = dict_entries(top);
    let new_top = |shift: isize, charset: usize| {
        let mut t = Vec::new();
        let moved = |v: f64| (v as isize + shift) as usize;
        let mut wrote_charset = false;
        for (o, e) in &entries {
            let a = offsets.iter().find(|x| x.0 == *o).map(|x| &x.1[..]);
            match (o, a) {
                (15, _) => {
                    int5(&mut t, charset);
                    wrote_charset = true;
                }
                (16, Some([v])) if *v > 2.0 => int5(&mut t, moved(*v)),
                (17, Some([v])) => int5(&mut t, moved(*v)),
                (18, Some([size, v])) => {
                    int5(&mut t, *size as usize);
                    int5(&mut t, moved(*v));
                }
                _ => {
                    t.extend_from_slice(e);
                    continue;
                }
            }
            op(&mut t, *o);
        }
        if !wrote_charset {
            int5(&mut t, charset);
            op(&mut t, 15);
        }
        index(&[&t])
    };
    let old_len = strings.end - names.end;
    let new_len = new_top(0, 0).len() + string_index.len();
    let shift = new_len as isize - old_len as isize;
    let mut rest = d[strings.end..].to_vec();
    if let Some(off) = charset_off
        && let Some(end) = charset_end(d, off, n)
        && let Some(old) = rest.get_mut(off - strings.end..end - strings.end)
    {
        old.fill(0);
    }
    if let Some(off) = encoding_off {
        rename_supplements(&mut rest, off - strings.end, &renamed);
    }
    let charset_at = (d.len() as isize + shift) as usize;
    let mut out = d[..names.end].to_vec();
    out.extend(new_top(shift, charset_at));
    out.extend(string_index);
    out.extend(rest);
    debug_assert_eq!(out.len(), charset_at);
    out.u8(0);
    for &sid in &sids[1..] {
        out.u16(sid);
    }
    Some(out)
}

/// Points the supplements of the custom encoding at `off` of `d` that name glyphs by an
/// old name (SID) to their new one.
fn rename_supplements(d: &mut [u8], off: usize, renamed: &[(u16, u16)]) {
    let Some(&format) = d.get(off) else { return };
    if format & 0x80 == 0 {
        return;
    }
    let n = usize::from(d.get(off + 1).copied().unwrap_or(0));
    let mut pos = off
        + 2
        + match format & 0x7F {
            0 => n,
            1 => 2 * n,
            _ => return,
        };
    let Some(&sups) = d.get(pos) else { return };
    pos += 1;
    for _ in 0..sups {
        let Some(sid) = u16_at(d, pos + 1) else {
            return;
        };
        if let Some(&(_, new)) = renamed.iter().find(|r| r.0 == sid) {
            d[pos + 1..pos + 3].copy_from_slice(&new.to_be_bytes());
        }
        pos += 3;
    }
}

/// Where the charset at `off` for `n` glyphs ends.
fn charset_end(d: &[u8], off: usize, n: u16) -> Option<usize> {
    let format = *d.get(off)?;
    let mut pos = off + 1;
    let mut covered = 1usize;
    while covered < usize::from(n) {
        match format {
            0 => {
                pos += 2;
                covered += 1;
            }
            1 | 2 => {
                let left = if format == 1 {
                    usize::from(*d.get(pos + 2)?)
                } else {
                    usize::from(u16_at(d, pos + 2)?)
                };
                pos += if format == 1 { 3 } else { 4 };
                covered += left + 1;
            }
            _ => return None,
        }
    }
    (pos <= d.len()).then_some(pos)
}

/// Overwrites object `i` of an INDEX with `op` bytes (the first one ends it).
fn fill(out: &mut [u8], ix: &Index, d: &[u8], i: u32, op: u8) {
    if let Some(r) = ix.range(d, i)
        && let Some(s) = out.get_mut(r)
    {
        s.fill(op);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outline::{OutlineSink, Path};

    fn outline(f: &Font, gid: u16) -> Path {
        let mut p = Path::default();
        let sink: &mut dyn OutlineSink = &mut p;
        match f {
            Font::Sfnt(s) => {
                s.outline(gid, sink);
            }
            Font::Cff(c) => {
                c.outline(gid, sink);
            }
            Font::Type1(t) => {
                t.outline(gid, sink);
            }
        }
        p
    }

    fn system_font(ext: &str) -> Option<Vec<u8>> {
        let db = crate::fallback::FontDb::system();
        db.faces()
            .iter()
            .filter(|f| {
                f.path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case(ext))
            })
            .find_map(|f| {
                let data = std::fs::read(&f.path).ok()?;
                let font = Font::parse(data.clone())?;
                let s = font.as_sfnt()?;
                // Two glyphs of their own: a placeholder font (macOS's LastResort) draws
                // every character with the same few.
                let (j, e) = (s.glyph_for_char('J')?, s.glyph_for_char('e')?);
                (j != e).then_some(data)
            })
    }

    /// Checks blanking `J` of `data`, made a subset first when it has `glyf` outlines (a
    /// whole font may use `J` in composites).
    fn check(data: Vec<u8>) {
        let f = Font::parse(data.clone()).unwrap();
        let s = f.as_sfnt().unwrap();
        let data = if s.has_glyf() {
            let cmap: Vec<(char, u16)> = "Jane e"
                .chars()
                .map(|c| (c, s.glyph_for_char(c).unwrap()))
                .collect();
            let gids = cmap.iter().map(|c| c.1);
            crate::subset::truetype::subset(s, gids, &cmap)
                .unwrap()
                .data
        } else {
            data
        };
        let f = Font::parse(data.clone()).unwrap();
        let s = f.as_sfnt().unwrap();
        let j = s.glyph_for_char('J').unwrap();
        let e = s.glyph_for_char('e').unwrap();
        assert!(!outline(&f, j).is_empty());
        let b = blank(&data, &BTreeSet::from([j])).unwrap();
        assert_eq!(b.glyphs, [j]);
        let g = Font::parse(b.data.clone()).unwrap();
        let t = g.as_sfnt().unwrap();
        assert_eq!(t.num_glyphs(), s.num_glyphs());
        assert!(outline(&g, j).is_empty());
        assert_eq!(outline(&g, e), outline(&f, e));
        assert_eq!(t.advance(j), s.advance(j));
        assert_eq!(t.glyph_for_char('J'), None);
        assert_eq!(t.glyph_for_char('e'), Some(e));
        if t.has_glyf() && t.glyph_name(e).is_some() {
            assert!(t.glyph_name(j).is_none_or(|n| n == ".notdef"));
        }
        // A name-keyed CFF names the glyph neutrally; other glyphs keep names and shapes.
        if let (Some(a), Some(b)) = (s.cff(), t.cff())
            && !a.is_cid()
        {
            assert_eq!(b.glyph_name(j), Some(&*format!("g{j}")));
            assert_eq!(b.glyph_by_name(b"J"), None);
            assert_eq!(b.name(), a.name());
            for k in (0..s.num_glyphs()).filter(|&k| k != j) {
                assert_eq!(b.glyph_name(k), a.glyph_name(k));
                assert_eq!(outline(&g, k), outline(&f, k), "glyph {k}");
            }
        }
    }

    #[test]
    fn truetype_glyph_is_emptied() {
        let data = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../apps/desktop/ui/fonts/InterVariable.ttf"
        ))
        .unwrap();
        check(data);
    }

    #[test]
    fn opentype_cff_glyph_is_emptied() {
        match system_font("otf") {
            Some(d) => check(d),
            None => eprintln!("skipped: no OpenType-CFF font with Latin glyphs installed"),
        }
    }

    /// A Type 1 font from the system (TeX fonts) loses `J`; every other glyph draws the same.
    #[test]
    fn type1_glyph_is_emptied() {
        fn pfb(dir: &std::path::Path, depth: u32) -> Option<std::path::PathBuf> {
            for e in std::fs::read_dir(dir).ok()?.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "pfb") {
                    return Some(p);
                }
                if depth > 0
                    && p.is_dir()
                    && let Some(f) = pfb(&p, depth - 1)
                {
                    return Some(f);
                }
            }
            None
        }
        let Some(path) = pfb(std::path::Path::new("/usr/share/fonts/type1"), 3) else {
            eprintln!("skipped: no Type 1 font installed");
            return;
        };
        let data = std::fs::read(&path).unwrap();
        let f = Font::parse(data.clone()).unwrap();
        let t = f.as_type1().unwrap();
        let Some(j) = t.glyph_by_name(b"J") else {
            return;
        };
        // With the accented forms built on it.
        let js: BTreeSet<u16> = (0..t.num_glyphs())
            .filter(|&g| t.glyph_name(g).is_some_and(|n| n.starts_with('J')))
            .collect();
        let b = blank(&data, &js).unwrap();
        assert_eq!(
            b.glyphs,
            Vec::from_iter(js.iter().copied()),
            "{}",
            path.display()
        );
        let g = Font::parse(b.data.clone()).unwrap();
        let u = g.as_type1().unwrap();
        assert!(outline(&g, j).is_empty());
        assert_eq!(u.advance(j), t.advance(j));
        assert_eq!(u.glyph_by_name(b"J"), None);
        for k in (0..t.num_glyphs()).filter(|k| !js.contains(k)) {
            assert_eq!(outline(&g, k), outline(&f, k), "glyph {k}");
            assert_eq!(u.glyph_name(k), t.glyph_name(k));
        }
        for c in (0..=255).filter(|&c| c != b'J') {
            assert_eq!(u.encoding_glyph(c), t.encoding_glyph(c));
        }
    }

    /// A bare name-keyed CFF: glyphs `A`, `B` (standard names) and `secret` drawing a
    /// square each, a custom encoding (codes 0x41 to 0x43) with supplements mapping 0x61
    /// to `A` and 0x63 to `secret`.
    fn cff_with_supplements() -> Vec<u8> {
        use crate::subset::cff::{index, int5, op};
        let square: &[u8] = &[139, 139, 21, 239, 139, 5, 139, 239, 5, 14];
        let charstrings = index(&[&[14], square, square, square]);
        let charset = [0u8, 0, 34, 0, 35, 1, 135];
        let encoding = [0x80u8, 3, 0x41, 0x42, 0x43, 2, 0x61, 0, 34, 0x63, 1, 135];
        let private = [139u8, 20];
        let top = |at: [usize; 4]| {
            let mut t = Vec::new();
            for (v, o) in at.into_iter().take(3).zip([15, 16, 17]) {
                int5(&mut t, v);
                op(&mut t, o);
            }
            int5(&mut t, private.len());
            int5(&mut t, at[3]);
            op(&mut t, 18);
            index(&[&t])
        };
        let mut head = vec![1, 0, 4, 1];
        head.extend(index(&[b"Test"]));
        let strings = index(&[b"secret"]);
        let start = head.len() + top([0; 4]).len() + strings.len() + 2;
        let at = [
            start,
            start + charset.len(),
            start + charset.len() + encoding.len(),
            start + charset.len() + encoding.len() + charstrings.len(),
        ];
        let mut d = head;
        d.extend(top(at));
        d.extend(strings);
        d.extend([0, 0]);
        d.extend(charset);
        d.extend(encoding);
        d.extend(charstrings);
        d.extend(private);
        d
    }

    /// Encoding supplements naming a blanked glyph by its old name follow the new one.
    #[test]
    fn cff_supplements_lose_old_names() {
        let data = cff_with_supplements();
        let f = Font::parse(data.clone()).unwrap();
        let c = f.as_cff().unwrap();
        assert_eq!(c.encoding_glyph(0x61), Some(1));
        assert_eq!(c.encoding_glyph(0x63), Some(3));
        let b = blank(&data, &BTreeSet::from([1, 3])).unwrap();
        assert_eq!(b.glyphs, [1, 3]);
        let g = Font::parse(b.data.clone()).unwrap();
        let u = g.as_cff().unwrap();
        assert!(outline(&g, 1).is_empty() && outline(&g, 3).is_empty());
        assert_eq!(outline(&g, 2), outline(&f, 2));
        assert_eq!(u.glyph_by_name(b"A"), None);
        assert_eq!(u.glyph_by_name(b"secret"), None);
        assert_eq!(u.glyph_name(2), Some("B"));
        for code in [0x41, 0x61] {
            assert_eq!(u.encoding_glyph(code), Some(1), "code {code:#x}");
        }
        assert_eq!(u.encoding_glyph(0x63), Some(3));
        assert_eq!(u.encoding_glyph(0x42), Some(2));
        // No supplement names a glyph `A` or `secret` any more.
        let at = b
            .data
            .windows(5)
            .position(|w| w[0] == 2 && w[1] == 0x61 && w[4] == 0x63)
            .unwrap();
        for sid in [at + 2, at + 5].map(|p| u16_at(&b.data, p).unwrap()) {
            let name = u.string(sid).unwrap_or_default();
            assert!(name != "A" && name != "secret", "{name}");
        }
        assert!(!b.data.windows(6).any(|w| w == b"secret"));
    }

    #[test]
    fn composite_components_stay() {
        let Some(data) = system_font("ttf") else {
            eprintln!("skipped: no TrueType font installed");
            return;
        };
        let f = Font::parse(data.clone()).unwrap();
        let s = f.as_sfnt().unwrap();
        // A composite glyph and one of its components.
        let loca = s.loca().unwrap();
        let Some((g, c)) = (1..s.num_glyphs()).find_map(|g| {
            let c = *crate::sfnt::glyf::components(loca.glyph(g)?).first()?;
            Some((g, c))
        }) else {
            eprintln!("skipped: no composite glyphs");
            return;
        };
        let b = blank(&data, &BTreeSet::from([c])).unwrap();
        assert!(b.glyphs.is_empty());
        let b = blank(&data, &BTreeSet::from([c, g])).unwrap();
        assert!(b.glyphs.contains(&g));
    }
}
