//! CFF subsetting to a CID-keyed font whose CIDs are the original glyph ids, as PDF
//! `CIDFontType0C` needs. Unused subroutines are blanked (indices stay valid) and only the
//! font DICTs in use are kept.

use std::collections::BTreeSet;

use crate::cff::charstring::{self, Ctx, Trace};
use crate::cff::{Cff, Index, Private};
use crate::outline::{NullSink, Pen};
use crate::reader::Write;

/// A raw DICT entry: the operator and the bytes of the whole entry.
pub(crate) fn dict_entries(d: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < d.len() {
        let b = d[i];
        i += match b {
            28 => 3,
            29 => 5,
            30 => {
                let mut n = 1;
                while let Some(&x) = d.get(i + n) {
                    n += 1;
                    if x & 0x0F == 0x0F || x & 0xF0 == 0xF0 {
                        break;
                    }
                }
                n
            }
            32..=246 => 1,
            247..=254 => 2,
            12 => {
                let op = 1200 + u16::from(d.get(i + 1).copied().unwrap_or(0));
                out.push((op, d.get(start..i + 2).unwrap_or_default()));
                i += 2;
                start = i;
                continue;
            }
            0..=27 => {
                out.push((u16::from(b), &d[start..=i]));
                i += 1;
                start = i;
                continue;
            }
            _ => 1,
        };
    }
    out
}

pub(crate) fn int5(out: &mut Vec<u8>, v: usize) {
    out.u8(29);
    out.u32(v as u32);
}

pub(crate) fn op(out: &mut Vec<u8>, op: u16) {
    if op >= 1200 {
        out.u8(12);
        out.u8((op - 1200) as u8);
    } else {
        out.u8(op as u8);
    }
}

pub(crate) fn index(items: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    out.u16(items.len() as u16);
    if items.is_empty() {
        return out;
    }
    let total: usize = items.iter().map(|i| i.len()).sum::<usize>() + 1;
    let size: u8 = match total {
        0..=0xFF => 1,
        0x100..=0xFFFF => 2,
        0x1_0000..=0xFF_FFFF => 3,
        _ => 4,
    };
    out.u8(size);
    let mut off = 1usize;
    let put = |out: &mut Vec<u8>, v: usize| {
        let b = (v as u32).to_be_bytes();
        out.extend_from_slice(&b[4 - usize::from(size)..]);
    };
    put(&mut out, off);
    for i in items {
        off += i.len();
        put(&mut out, off);
    }
    for i in items {
        out.extend_from_slice(i);
    }
    out
}

/// Copies INDEX `ix`, blanking objects not marked in `used`.
fn pruned_index(d: &[u8], ix: &Index, used: &[bool]) -> Vec<u8> {
    let items: Vec<&[u8]> = (0..ix.count)
        .map(|i| {
            if used.get(i as usize).copied().unwrap_or(false) {
                ix.get(d, i).unwrap_or(&[11])
            } else {
                &[11]
            }
        })
        .collect();
    index(&items)
}

/// Subsets a CFF (not CFF2) font to `gids` plus `.notdef`. Returns `None` for fonts using
/// `seac`, which CID-keyed fonts cannot express.
pub fn subset(cff: &Cff, gids: impl IntoIterator<Item = u16>) -> Option<Vec<u8>> {
    if cff.cff2 {
        return None;
    }
    let d = cff.table_data();
    let n = cff.num_glyphs();
    let mut glyphs: BTreeSet<u16> = gids.into_iter().filter(|&g| g < n).collect();
    glyphs.insert(0);
    let glyphs: Vec<u16> = glyphs.into_iter().collect();

    // Font DICTs in use, renumbered.
    let cid_keyed = !cff.fds.is_empty();
    let mut fd_used: Vec<usize> = if cid_keyed {
        glyphs.iter().map(|&g| cff.fd_index(g)).collect()
    } else {
        vec![0]
    };
    fd_used.sort_unstable();
    fd_used.dedup();
    let privates: Vec<&Private> = if cid_keyed {
        fd_used.iter().map(|&i| &cff.fds[i].private).collect()
    } else {
        vec![&cff.private]
    };

    // Trace subroutine use.
    let mut global = vec![false; cff.gsubrs.count as usize];
    let mut locals: Vec<Vec<bool>> = privates
        .iter()
        .map(|p| vec![false; p.subrs.count as usize])
        .collect();
    for &g in &glyphs {
        let fd = if cid_keyed {
            fd_used.binary_search(&cff.fd_index(g)).unwrap_or(0)
        } else {
            0
        };
        let p = privates[fd];
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
        let cs = cff.charstrings.get(d, u32::from(g)).unwrap_or(&[14]);
        let mut sink = NullSink;
        let mut pen = Pen::new(&mut sink, None);
        let r = charstring::run_traced(&ctx, cs, &mut pen, (0.0, 0.0), Some(&mut trace));
        locals[fd] = trace.local;
        global = trace.global;
        if r.is_some_and(|r| r.seac.is_some()) {
            return None;
        }
    }

    let mut top_extra = Vec::new();
    let mut top_matrix = None;
    for (o, e) in dict_entries(d.get(cff.top_dict.clone())?) {
        match o {
            1207 => top_matrix = Some(e.to_vec()),
            5 => top_extra.extend_from_slice(e),
            _ => {}
        }
    }
    if let Some(m) = top_matrix {
        top_extra.extend(m);
    }

    // Strings: "Adobe", "Identity" and the font DICT names.
    let strings: Vec<&[u8]> = vec![b"Adobe", b"Identity"];
    let name = cff.name().unwrap_or("Font").as_bytes().to_vec();

    let header = [1u8, 0, 4, 4];
    let name_index = index(&[&name]);
    let string_index = index(&strings);
    let gsubr_index = pruned_index(d, &cff.gsubrs, &global);

    let mut charset = vec![0u8];
    let mut max_cid = 0u16;
    for &g in &glyphs[1..] {
        let cid = if cid_keyed {
            cff.charset_id(g).unwrap_or(g)
        } else {
            g
        };
        max_cid = max_cid.max(cid);
        charset.u16(cid);
    }
    let mut fdselect = vec![3u8];
    let mut ranges: Vec<(u16, u8)> = Vec::new();
    for (new, &g) in glyphs.iter().enumerate() {
        let fd = if cid_keyed {
            fd_used.binary_search(&cff.fd_index(g)).unwrap_or(0) as u8
        } else {
            0
        };
        if ranges.last().is_none_or(|r| r.1 != fd) {
            ranges.push((new as u16, fd));
        }
    }
    fdselect.u16(ranges.len() as u16);
    for (first, fd) in &ranges {
        fdselect.u16(*first);
        fdselect.u8(*fd);
    }
    fdselect.u16(glyphs.len() as u16);
    let cs: Vec<&[u8]> = glyphs
        .iter()
        .map(|&g| cff.charstrings.get(d, u32::from(g)).unwrap_or(&[14]))
        .collect();
    let charstrings = index(&cs);

    // Private DICTs (without Subrs) followed by their local subrs.
    let mut private_bodies: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for (k, p) in privates.iter().enumerate() {
        let mut body = Vec::new();
        for (o, e) in dict_entries(d.get(p.range.clone()).unwrap_or_default()) {
            if o != 19 {
                body.extend_from_slice(e);
            }
        }
        let subrs = if p.subrs.count > 0 {
            pruned_index(d, &p.subrs, &locals[k])
        } else {
            Vec::new()
        };
        private_bodies.push((body, subrs));
    }
    let private_len = |b: &(Vec<u8>, Vec<u8>)| b.0.len() + if b.1.is_empty() { 0 } else { 6 };

    let fd_dicts = |offsets: &[usize]| -> Vec<u8> {
        let dicts: Vec<Vec<u8>> = privates
            .iter()
            .enumerate()
            .map(|(k, _)| {
                let mut fd = Vec::new();
                if cid_keyed
                    && let Some(src) = cff.fds.get(fd_used[k]).and_then(|f| d.get(f.dict.clone()))
                {
                    for (o, e) in dict_entries(src) {
                        if o == 1207 {
                            fd.extend_from_slice(e);
                        }
                    }
                }
                int5(&mut fd, private_len(&private_bodies[k]));
                int5(&mut fd, offsets[k]);
                op(&mut fd, 18);
                fd
            })
            .collect();
        let refs: Vec<&[u8]> = dicts.iter().map(|v| &v[..]).collect();
        index(&refs)
    };

    let top = |charset_off: usize, fdselect_off: usize, cs_off: usize, fdarray_off: usize| {
        let mut t = Vec::new();
        // ROS: Adobe Identity 0
        int5(&mut t, 391);
        int5(&mut t, 392);
        int5(&mut t, 0);
        op(&mut t, 1230);
        t.extend_from_slice(&top_extra);
        int5(&mut t, usize::from(max_cid) + 1);
        op(&mut t, 1234);
        int5(&mut t, charset_off);
        op(&mut t, 15);
        int5(&mut t, cs_off);
        op(&mut t, 17);
        int5(&mut t, fdarray_off);
        op(&mut t, 1236);
        int5(&mut t, fdselect_off);
        op(&mut t, 1237);
        index(&[&t])
    };

    // Sizes do not depend on offset values (fixed-width integers).
    let top_len = top(0, 0, 0, 0).len();
    let fdarray_len = fd_dicts(&vec![0; privates.len()]).len();
    let mut pos =
        header.len() + name_index.len() + top_len + string_index.len() + gsubr_index.len();
    let charset_off = pos;
    pos += charset.len();
    let fdselect_off = pos;
    pos += fdselect.len();
    let cs_off = pos;
    pos += charstrings.len();
    let fdarray_off = pos;
    pos += fdarray_len;
    let mut private_offs = Vec::new();
    for b in &private_bodies {
        private_offs.push(pos);
        pos += private_len(b) + b.1.len();
    }

    let mut out = Vec::with_capacity(pos);
    out.extend_from_slice(&header);
    out.extend(name_index);
    out.extend(top(charset_off, fdselect_off, cs_off, fdarray_off));
    out.extend(string_index);
    out.extend(gsubr_index);
    out.extend(charset);
    out.extend(fdselect);
    out.extend(charstrings);
    out.extend(fd_dicts(&private_offs));
    for b in private_bodies {
        let len = private_len(&b);
        out.extend_from_slice(&b.0);
        if !b.1.is_empty() {
            int5(&mut out, len);
            op(&mut out, 19);
        }
        out.extend(b.1);
    }
    debug_assert_eq!(out.len(), pos);
    Some(out)
}
