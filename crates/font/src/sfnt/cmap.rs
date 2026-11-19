//! The `cmap` table: formats 0, 2, 4, 6, 10, 12, 13 and 14.

use crate::reader::{Reader, u8_at, u16_at, u24_at, u32_at};

/// One encoding subtable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Subtable {
    pub platform: u16,
    pub encoding: u16,
    pub format: u16,
    /// Offset of the subtable within the `cmap` table.
    pub offset: u32,
}

/// What a variation sequence maps to (format 14).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variation {
    /// The base character's default glyph.
    Default,
    Glyph(u16),
}

pub fn subtables(cmap: &[u8]) -> Vec<Subtable> {
    let n = u16_at(cmap, 2).unwrap_or(0);
    let mut out = Vec::with_capacity(usize::from(n));
    for i in 0..usize::from(n) {
        let rec = 4 + i * 8;
        let (Some(platform), Some(encoding), Some(offset)) = (
            u16_at(cmap, rec),
            u16_at(cmap, rec + 2),
            u32_at(cmap, rec + 4),
        ) else {
            break;
        };
        let Some(format) = u16_at(cmap, offset as usize) else {
            continue;
        };
        out.push(Subtable {
            platform,
            encoding,
            format,
            offset,
        });
    }
    out
}

/// The glyph of `code` in subtable `s`.
pub fn lookup(cmap: &[u8], s: &Subtable, code: u32) -> Option<u16> {
    let t = cmap.get(s.offset as usize..)?;
    let gid = match s.format {
        0 => u16::from(u8_at(
            t,
            6 + usize::try_from(code).ok().filter(|&c| c < 256)?,
        )?),
        2 => format2(t, code)?,
        4 => format4(t, code)?,
        6 => {
            let first = u32::from(u16_at(t, 6)?);
            let count = u32::from(u16_at(t, 8)?);
            let i = code.checked_sub(first).filter(|&i| i < count)?;
            u16_at(t, 10 + 2 * i as usize)?
        }
        10 => {
            let first = u32_at(t, 12)?;
            let count = u32_at(t, 16)?;
            let i = code.checked_sub(first).filter(|&i| i < count)?;
            u16_at(t, 20 + 2 * i as usize)?
        }
        12 | 13 => {
            let n = u32_at(t, 12)? as usize;
            let n = n.min(t.len().saturating_sub(16) / 12);
            let (mut lo, mut hi) = (0, n);
            let mut found = None;
            while lo < hi {
                let mid = (lo + hi) / 2;
                let g = 16 + mid * 12;
                let (start, end) = (u32_at(t, g)?, u32_at(t, g + 4)?);
                if code < start {
                    hi = mid;
                } else if code > end {
                    lo = mid + 1;
                } else {
                    let v = u32_at(t, g + 8)?;
                    found = Some(if s.format == 12 {
                        v.wrapping_add(code - start)
                    } else {
                        v
                    });
                    break;
                }
            }
            u16::try_from(found?).ok()?
        }
        _ => return None,
    };
    (gid != 0).then_some(gid)
}

fn format2(t: &[u8], code: u32) -> Option<u16> {
    let (hi, lo) = if code < 256 {
        (code as usize, None)
    } else if code < 0x10000 {
        ((code >> 8) as usize, Some((code & 0xFF) as u16))
    } else {
        return None;
    };
    let key = usize::from(u16_at(t, 6 + hi * 2)? / 8);
    // Sub-header 0 maps single bytes; others need a second byte.
    let lo = match (key, lo) {
        (0, None) => hi as u16,
        (0, Some(_)) | (_, None) => return None,
        (_, Some(lo)) => lo,
    };
    let sh = 6 + 512 + key * 8;
    let first = u16_at(t, sh)?;
    let count = u16_at(t, sh + 2)?;
    let delta = u16_at(t, sh + 4)?;
    let range_off = usize::from(u16_at(t, sh + 6)?);
    let i = lo.checked_sub(first).filter(|&i| i < count)?;
    let g = u16_at(t, sh + 6 + range_off + 2 * usize::from(i))?;
    (g != 0).then(|| g.wrapping_add(delta))
}

fn format4(t: &[u8], code: u32) -> Option<u16> {
    let code = u16::try_from(code).ok()?;
    let segs = usize::from(u16_at(t, 6)? / 2);
    let ends = 14;
    let starts = ends + segs * 2 + 2;
    let deltas = starts + segs * 2;
    let ranges = deltas + segs * 2;
    // First segment whose end >= code.
    let (mut lo, mut hi) = (0, segs);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if u16_at(t, ends + mid * 2)? < code {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    let i = lo;
    if i >= segs {
        return None;
    }
    let start = u16_at(t, starts + i * 2)?;
    if code < start {
        return None;
    }
    let delta = u16_at(t, deltas + i * 2)?;
    let ro = usize::from(u16_at(t, ranges + i * 2)?);
    if ro == 0 {
        return Some(code.wrapping_add(delta)).filter(|&g| g != 0);
    }
    let at = ranges + i * 2 + ro + 2 * usize::from(code - start);
    let g = u16_at(t, at)?;
    (g != 0).then(|| g.wrapping_add(delta))
}

/// Calls `f(code, gid)` for every mapping of subtable `s` (bounded work on corrupt tables).
pub fn for_each(cmap: &[u8], s: &Subtable, f: &mut dyn FnMut(u32, u16)) {
    let Some(t) = cmap.get(s.offset as usize..) else {
        return;
    };
    match s.format {
        0 => {
            for c in 0..256u32 {
                if let Some(g) = u8_at(t, 6 + c as usize).filter(|&g| g != 0) {
                    f(c, u16::from(g));
                }
            }
        }
        2 => {
            for c in 0..0x10000u32 {
                if let Some(g) = format2(t, c) {
                    f(c, g);
                }
            }
        }
        4 => {
            // Each code once: overlapping or unordered segments of a corrupt table would
            // otherwise be walked again and again (32767 segments of 65536 codes).
            let segs = usize::from(u16_at(t, 6).unwrap_or(0) / 2);
            let mut next = 0u32;
            for i in 0..segs {
                let (Some(end), Some(start)) =
                    (u16_at(t, 14 + i * 2), u16_at(t, 14 + segs * 2 + 2 + i * 2))
                else {
                    break;
                };
                for c in u32::from(start).max(next)..=u32::from(end) {
                    if let Some(g) = format4(t, c) {
                        f(c, g);
                    }
                }
                next = next.max(u32::from(end) + 1);
            }
        }
        6 | 10 => {
            let (first, count, data) = if s.format == 6 {
                (
                    u32::from(u16_at(t, 6).unwrap_or(0)),
                    u32::from(u16_at(t, 8).unwrap_or(0)),
                    10,
                )
            } else {
                (u32_at(t, 12).unwrap_or(0), u32_at(t, 16).unwrap_or(0), 20)
            };
            let count = count.min((t.len().saturating_sub(data) / 2) as u32);
            for i in 0..count {
                if let Some(g) = u16_at(t, data + 2 * i as usize).filter(|&g| g != 0) {
                    f(first.wrapping_add(i), g);
                }
            }
        }
        12 | 13 => {
            let n = u32_at(t, 12).unwrap_or(0) as usize;
            let n = n.min(t.len().saturating_sub(16) / 12);
            let mut budget = 0x11_0000u32 * 2;
            for k in 0..n {
                let g = 16 + k * 12;
                let (Some(start), Some(end), Some(v)) =
                    (u32_at(t, g), u32_at(t, g + 4), u32_at(t, g + 8))
                else {
                    break;
                };
                let end = end.min(0x10_FFFF);
                for c in start..=end {
                    if budget == 0 {
                        return;
                    }
                    budget -= 1;
                    let gid = if s.format == 12 {
                        v.wrapping_add(c - start)
                    } else {
                        v
                    };
                    if let Ok(gid) = u16::try_from(gid)
                        && gid != 0
                    {
                        f(c, gid);
                    }
                }
            }
        }
        _ => {}
    }
}

/// Format 14 lookup of the variation sequence `c` + `selector`.
pub fn variation(cmap: &[u8], s: &Subtable, c: u32, selector: u32) -> Option<Variation> {
    if s.format != 14 {
        return None;
    }
    let t = cmap.get(s.offset as usize..)?;
    let n = u32_at(t, 6)? as usize;
    let n = n.min(t.len().saturating_sub(10) / 11);
    let mut rec = None;
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let vs = u24_at(t, 10 + mid * 11)?;
        match vs.cmp(&selector) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => {
                rec = Some(10 + mid * 11);
                break;
            }
        }
    }
    let rec = rec?;
    let default_off = u32_at(t, rec + 3)? as usize;
    let non_default_off = u32_at(t, rec + 7)? as usize;
    if default_off != 0 {
        let mut r = Reader::at(t, default_off);
        let count = r.u32()?;
        for _ in 0..count.min(0x10_0000) {
            let start = r.u24()?;
            let add = u32::from(r.u8()?);
            if c < start {
                break;
            }
            if c <= start + add {
                return Some(Variation::Default);
            }
        }
    }
    if non_default_off != 0 {
        let mut r = Reader::at(t, non_default_off);
        let count = r.u32()?;
        for _ in 0..count.min(0x10_0000) {
            let u = r.u24()?;
            let g = r.u16()?;
            if u == c {
                return Some(Variation::Glyph(g));
            }
            if u > c {
                break;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::Write;

    /// A cmap table holding (platform, encoding, subtable bytes).
    fn table(subs: &[(u16, u16, Vec<u8>)]) -> Vec<u8> {
        let mut t = Vec::new();
        t.u16(0);
        t.u16(subs.len() as u16);
        let mut off = 4 + 8 * subs.len();
        for (p, e, s) in subs {
            t.u16(*p);
            t.u16(*e);
            t.u32(off as u32);
            off += s.len();
        }
        for (_, _, s) in subs {
            t.extend(s);
        }
        t
    }

    fn all(t: &[u8], s: &Subtable) -> Vec<(u32, u16)> {
        let mut v = Vec::new();
        for_each(t, s, &mut |c, g| v.push((c, g)));
        v
    }

    fn words(v: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        for &w in v {
            out.u16(w);
        }
        out
    }

    #[test]
    fn formats() {
        let mut f0 = words(&[0, 262, 0]);
        f0.extend((0..=255u8).map(|c| if c == b'A' { 7 } else { 0 }));
        let f4 =
            crate::subset::truetype::cmap_table(&[(0x41, 3), (0x42, 4), (0x43, 9), (0x20AC, 5)])
                [12..]
                .to_vec();
        let f6 = words(&[6, 0, 0, 0x30, 3, 10, 0, 12]);
        let mut f10 = words(&[10, 0]);
        for v in [0u32, 0, 0x1_F600, 2] {
            f10.u32(v);
        }
        f10.extend(words(&[20, 21]));
        let mut f12 = words(&[12, 0]);
        for v in [0u32, 0, 2, 0x41, 0x43, 30, 0x1_0000, 0x1_0001, 40] {
            f12.u32(v);
        }
        let mut f13 = f12.clone();
        f13[1] = 13;
        let mut f14 = words(&[14]);
        f14.u32(0);
        f14.u32(1);
        f14.extend([0, 0xFE, 0x00]);
        f14.u32(21);
        f14.u32(29);
        // Default: U+4E00..=U+4E01; non-default: U+4E08 → 77.
        f14.u32(1);
        f14.extend([0, 0x4E, 0x00, 1]);
        f14.u32(1);
        f14.extend([0, 0x4E, 0x08, 0, 77]);
        // Format 2: single bytes through subheader 0, 0x81 0x40 through subheader 1.
        let mut keys = vec![2u16, 0, 0];
        keys.extend((0..256u16).map(|hi| if hi == 0x81 { 8 } else { 0 }));
        keys.extend([0x41, 2, 0, 10, 0x40, 1, 100, 6, 5, 6, 1]);
        let f2 = words(&keys);
        let t = table(&[
            (1, 0, f0),
            (3, 1, f4),
            (1, 1, f6),
            (0, 4, f10),
            (3, 10, f12),
            (3, 99, f13),
            (0, 5, f14),
            (3, 2, f2),
        ]);
        let subs = subtables(&t);
        assert_eq!(subs.len(), 8);
        let get = |i: usize, c: u32| lookup(&t, &subs[i], c);
        assert_eq!(
            (get(0, 0x41), get(0, 0x42), get(0, 300)),
            (Some(7), None, None)
        );
        assert_eq!(
            (get(1, 0x41), get(1, 0x43), get(1, 0x20AC), get(1, 0x44)),
            (Some(3), Some(9), Some(5), None)
        );
        assert_eq!(
            all(&t, &subs[1]),
            vec![(0x41, 3), (0x42, 4), (0x43, 9), (0x20AC, 5)]
        );
        assert_eq!(
            (get(2, 0x30), get(2, 0x31), get(2, 0x32), get(2, 0x33)),
            (Some(10), None, Some(12), None)
        );
        assert_eq!(
            (get(3, 0x1_F600), get(3, 0x1_F601), get(3, 0x1_F602)),
            (Some(20), Some(21), None)
        );
        assert_eq!(
            (get(4, 0x42), get(4, 0x1_0001), get(4, 0x44)),
            (Some(31), Some(41), None)
        );
        assert_eq!((get(5, 0x42), get(5, 0x1_0001)), (Some(30), Some(40)));
        assert_eq!(all(&t, &subs[4]).len(), 5);
        assert_eq!(
            variation(&t, &subs[6], 0x4E01, 0xFE00),
            Some(Variation::Default)
        );
        assert_eq!(
            variation(&t, &subs[6], 0x4E08, 0xFE00),
            Some(Variation::Glyph(77))
        );
        assert_eq!(variation(&t, &subs[6], 0x4E09, 0xFE00), None);
        assert_eq!(variation(&t, &subs[6], 0x4E08, 0xFE01), None);
        assert_eq!(
            (
                get(7, 0x41),
                get(7, 0x42),
                get(7, 0x8140),
                get(7, 0x43),
                get(7, 0x81)
            ),
            (Some(5), Some(6), Some(101), None, None)
        );
    }

    /// A fuzzer find: 4096 segments each spanning every code made the enumeration walk
    /// 268 million codes; each code is visited once now.
    #[test]
    fn overlapping_format4_segments() {
        let segs = 4096u16;
        let mut f4 = words(&[4, 0, 0, segs * 2, 0, 0, 0]);
        f4.extend(words(&vec![0xFFFF; segs as usize]));
        f4.extend(words(&[0]));
        f4.extend(words(&vec![0; segs as usize]));
        f4.extend(words(&vec![1; segs as usize]));
        f4.extend(words(&vec![0; segs as usize]));
        let t = table(&[(3, 1, f4)]);
        let s = Subtable {
            platform: 3,
            encoding: 1,
            format: 4,
            offset: 12,
        };
        let start = std::time::Instant::now();
        let n = all(&t, &s).len();
        assert!(n <= 0x10000, "{n}");
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }
}
