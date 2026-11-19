//! TrueType (`glyf`) subsetting: composite closure, renumbering and minimal tables.

use std::collections::BTreeSet;

use crate::reader::{Write, u16_at, u32_at};
use crate::sfnt::{Sfnt, glyf};

/// A subset font and its glyph numbering.
#[derive(Clone, Debug)]
pub struct Subset {
    pub data: Vec<u8>,
    /// Old glyph id of each new glyph id (new id 0 is `.notdef`).
    pub glyphs: Vec<u16>,
}

impl Subset {
    /// The new id of an old glyph.
    pub fn new_id(&self, old: u16) -> Option<u16> {
        self.glyphs.binary_search(&old).ok().map(|i| i as u16)
    }
}

/// The glyphs plus `.notdef` and every composite component, sorted.
pub fn closure(f: &Sfnt, gids: impl IntoIterator<Item = u16>) -> Vec<u16> {
    let n = f.num_glyphs();
    let mut set: BTreeSet<u16> = gids.into_iter().filter(|&g| g < n).collect();
    set.insert(0);
    if let Some(loca) = f.loca() {
        let mut todo: Vec<u16> = set.iter().copied().collect();
        while let Some(g) = todo.pop() {
            if let Some(data) = loca.glyph(g) {
                for c in glyf::components(data) {
                    if c < n && set.insert(c) {
                        todo.push(c);
                    }
                }
            }
        }
    }
    set.into_iter().collect()
}

/// Subsets a TrueType font to `gids` (plus composite components), renumbering glyphs in
/// ascending old order. `cmap` lists (Unicode, old gid) pairs for a (3,1) cmap.
pub fn subset(
    f: &Sfnt,
    gids: impl IntoIterator<Item = u16>,
    cmap: &[(char, u16)],
) -> Option<Subset> {
    let loca = f.loca()?;
    let glyphs = closure(f, gids);
    let new_id = |old: u16| glyphs.binary_search(&old).ok().map(|i| i as u16);

    let mut glyf_out = Vec::new();
    let mut loca_out = Vec::with_capacity((glyphs.len() + 1) * 4);
    for &g in &glyphs {
        loca_out.u32(glyf_out.len() as u32);
        let mut data = loca.glyph(g).unwrap_or_default().to_vec();
        remap_components(&mut data, &new_id);
        glyf_out.extend_from_slice(&data);
        while glyf_out.len() % 4 != 0 {
            glyf_out.push(0);
        }
    }
    loca_out.u32(glyf_out.len() as u32);

    let mut hmtx = Vec::with_capacity(glyphs.len() * 4);
    for &g in &glyphs {
        hmtx.u16(f.advance(g).unwrap_or(0));
        hmtx.i16(f.left_side_bearing(g).unwrap_or(0));
    }
    let n = glyphs.len() as u16;

    let mut head = f.table(b"head").filter(|h| h.len() >= 54)?.to_vec();
    head[8..12].fill(0);
    head[50..52].copy_from_slice(&1u16.to_be_bytes());
    let mut hhea = f.table(b"hhea").filter(|h| h.len() >= 36)?.to_vec();
    hhea[34..36].copy_from_slice(&n.to_be_bytes());
    let mut maxp = f.table(b"maxp").filter(|m| m.len() >= 6)?.to_vec();
    maxp[4..6].copy_from_slice(&n.to_be_bytes());
    // Format 3 (no glyph names), keeping the italic angle and underline metrics.
    let mut post = f
        .table(b"post")
        .and_then(|p| p.get(..32))
        .map_or_else(|| vec![0; 32], <[u8]>::to_vec);
    post[0..4].copy_from_slice(&0x0003_0000u32.to_be_bytes());

    let mut map: Vec<(u16, u16)> = cmap
        .iter()
        .filter_map(|&(c, g)| Some((u16::try_from(c as u32).ok()?, new_id(g)?)))
        .collect();
    map.sort_unstable();
    map.dedup_by_key(|m| m.0);

    let mut tables: Vec<([u8; 4], Vec<u8>)> = vec![
        (*b"cmap", cmap_table(&map)),
        (*b"glyf", glyf_out),
        (*b"head", head),
        (*b"hhea", hhea),
        (*b"hmtx", hmtx),
        (*b"loca", loca_out),
        (*b"maxp", maxp),
        (*b"post", post),
    ];
    for tag in [b"OS/2", b"cvt ", b"fpgm", b"prep"] {
        if let Some(t) = f.table(tag) {
            tables.push((*tag, t.to_vec()));
        }
    }
    let data = write_sfnt(0x0001_0000, tables);
    Some(Subset { data, glyphs })
}

fn remap_components(g: &mut [u8], new_id: &dyn Fn(u16) -> Option<u16>) {
    if u16_at(g, 0).is_none_or(|c| (c as i16) >= 0) {
        return;
    }
    let mut p = 10;
    loop {
        let (Some(flags), Some(old)) = (u16_at(g, p), u16_at(g, p + 2)) else {
            return;
        };
        let new = new_id(old).unwrap_or(0);
        g[p + 2..p + 4].copy_from_slice(&new.to_be_bytes());
        p += 4 + if flags & 1 != 0 { 4 } else { 2 };
        p += if flags & 8 != 0 {
            2
        } else if flags & 0x40 != 0 {
            4
        } else if flags & 0x80 != 0 {
            8
        } else {
            0
        };
        if flags & 0x20 == 0 {
            return;
        }
    }
}

/// A (3,1) format 4 cmap.
pub(crate) fn cmap_table(map: &[(u16, u16)]) -> Vec<u8> {
    let mut t = Vec::new();
    t.u16(0);
    t.u16(1);
    t.u16(3);
    t.u16(1);
    t.u32(12);
    t.extend(format4(map));
    t
}

/// A format 4 cmap subtable of sorted `(code, gid)` pairs.
pub(crate) fn format4(map: &[(u16, u16)]) -> Vec<u8> {
    // Segments of consecutive codes with consecutive glyphs use idDelta only.
    let mut segs: Vec<(u16, u16, u16)> = Vec::new(); // start, end, first gid
    for &(c, g) in map {
        if c == 0xFFFF {
            continue;
        }
        match segs.last_mut() {
            Some(s) if s.1.checked_add(1) == Some(c) && s.2.wrapping_add(c - s.0) == g => s.1 = c,
            _ => segs.push((c, c, g)),
        }
    }
    // The required final segment maps U+FFFF to glyph 0 (idDelta 1).
    segs.push((0xFFFF, 0xFFFF, 0));
    let n = segs.len() as u16;
    let mut sub = Vec::new();
    let len = 16 + 8 * usize::from(n);
    sub.u16(4);
    sub.u16(len as u16);
    sub.u16(0);
    sub.u16(n * 2);
    let mut sr = 1u16;
    let mut es = 0u16;
    while sr * 2 <= n {
        sr *= 2;
        es += 1;
    }
    sub.u16(sr * 2);
    sub.u16(es);
    sub.u16(n * 2 - sr * 2);
    for s in &segs {
        sub.u16(s.1);
    }
    sub.u16(0);
    for s in &segs {
        sub.u16(s.0);
    }
    for s in &segs {
        sub.u16(s.2.wrapping_sub(s.0));
    }
    for _ in &segs {
        sub.u16(0);
    }
    sub
}

fn checksum(d: &[u8]) -> u32 {
    d.chunks(4).fold(0u32, |s, c| {
        let mut b = [0u8; 4];
        b[..c.len()].copy_from_slice(c);
        s.wrapping_add(u32::from_be_bytes(b))
    })
}

/// Writes an sfnt with tables sorted by tag and `head.checkSumAdjustment` set.
pub(crate) fn write_sfnt(version: u32, mut tables: Vec<([u8; 4], Vec<u8>)>) -> Vec<u8> {
    tables.sort_by_key(|a| a.0);
    let n = tables.len() as u16;
    let mut out = Vec::new();
    out.u32(version);
    out.u16(n);
    let mut sr = 1u16;
    let mut es = 0u16;
    while sr * 2 <= n {
        sr *= 2;
        es += 1;
    }
    out.u16(sr * 16);
    out.u16(es);
    out.u16(n * 16 - sr * 16);
    let mut off = 12 + 16 * tables.len();
    for (tag, d) in &tables {
        out.extend_from_slice(tag);
        out.u32(checksum(d));
        out.u32(off as u32);
        out.u32(d.len() as u32);
        off += d.len().div_ceil(4) * 4;
    }
    let mut head_at = None;
    for (tag, d) in &tables {
        if tag == b"head" {
            head_at = Some(out.len());
        }
        out.extend_from_slice(d);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    if let Some(h) = head_at {
        let adj = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
        out[h + 8..h + 12].copy_from_slice(&adj.to_be_bytes());
    }
    let _ = u32_at;
    out
}
