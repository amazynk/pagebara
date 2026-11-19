//! Pair kerning: the `GPOS` `kern` feature (pair adjustment lookups, formats 1 and 2) and
//! the `kern` table.

use pagebara_font::sfnt::Sfnt;

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*d.get(at)?, *d.get(at + 1)?]))
}

fn i16_at(d: &[u8], at: usize) -> Option<i16> {
    u16_at(d, at).map(|v| v as i16)
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// Pair adjustments of one font.
#[derive(Debug, Default)]
pub(crate) struct Kerner {
    /// Offsets of `GPOS` pair adjustment subtables of the `kern` feature, by lookup.
    lookups: Vec<Vec<usize>>,
    gpos: bool,
}

impl Kerner {
    pub(crate) fn new(font: &Sfnt) -> Kerner {
        let lookups = font.table(b"GPOS").map(pair_lookups).unwrap_or_default();
        Kerner {
            gpos: !lookups.is_empty(),
            lookups,
        }
    }

    /// The advance adjustment between two glyphs in font units.
    pub(crate) fn kern(&self, font: &Sfnt, left: u16, right: u16) -> i32 {
        if self.gpos {
            let Some(t) = font.table(b"GPOS") else {
                return 0;
            };
            let mut total = 0i32;
            for subtables in &self.lookups {
                for &st in subtables {
                    if let Some(v) = pair_value(t, st, left, right) {
                        total += i32::from(v);
                        break;
                    }
                }
            }
            total
        } else {
            i32::from(font.kerning(left, right))
        }
    }
}

/// Subtable offsets (from the table start) of the `kern` feature's pair lookups.
fn pair_lookups(t: &[u8]) -> Vec<Vec<usize>> {
    let (Some(fl), Some(ll)) = (u16_at(t, 6), u16_at(t, 8)) else {
        return Vec::new();
    };
    let (fl, ll) = (usize::from(fl), usize::from(ll));
    let mut indices: Vec<u16> = Vec::new();
    let nf = u16_at(t, fl).unwrap_or(0);
    for i in 0..usize::from(nf) {
        let rec = fl + 2 + i * 6;
        if t.get(rec..rec + 4) != Some(b"kern") {
            continue;
        }
        let Some(off) = u16_at(t, rec + 4) else {
            continue;
        };
        let f = fl + usize::from(off);
        let n = u16_at(t, f + 2).unwrap_or(0);
        for j in 0..usize::from(n) {
            if let Some(ix) = u16_at(t, f + 4 + j * 2)
                && !indices.contains(&ix)
            {
                indices.push(ix);
            }
        }
    }
    indices.sort_unstable();
    let nl = u16_at(t, ll).unwrap_or(0);
    let mut out = Vec::new();
    for ix in indices {
        if ix >= nl {
            continue;
        }
        let Some(off) = u16_at(t, ll + 2 + usize::from(ix) * 2) else {
            continue;
        };
        let l = ll + usize::from(off);
        let (Some(kind), Some(n)) = (u16_at(t, l), u16_at(t, l + 4)) else {
            continue;
        };
        let mut subs = Vec::new();
        for k in 0..usize::from(n) {
            let Some(so) = u16_at(t, l + 6 + k * 2) else {
                continue;
            };
            let mut s = l + usize::from(so);
            let mut st_kind = kind;
            if kind == 9 {
                let (Some(ek), Some(eo)) = (u16_at(t, s + 2), u32_at(t, s + 4)) else {
                    continue;
                };
                st_kind = ek;
                s += eo as usize;
            }
            if st_kind == 2 {
                subs.push(s);
            }
        }
        if !subs.is_empty() {
            out.push(subs);
        }
    }
    out
}

fn coverage_index(t: &[u8], c: usize, g: u16) -> Option<u16> {
    match u16_at(t, c)? {
        1 => {
            let n = usize::from(u16_at(t, c + 2)?);
            let (mut lo, mut hi) = (0, n);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let v = u16_at(t, c + 4 + mid * 2)?;
                match v.cmp(&g) {
                    std::cmp::Ordering::Less => lo = mid + 1,
                    std::cmp::Ordering::Greater => hi = mid,
                    std::cmp::Ordering::Equal => return Some(mid as u16),
                }
            }
            None
        }
        2 => {
            let n = usize::from(u16_at(t, c + 2)?);
            let (mut lo, mut hi) = (0, n);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let r = c + 4 + mid * 6;
                let (s, e) = (u16_at(t, r)?, u16_at(t, r + 2)?);
                if g < s {
                    hi = mid;
                } else if g > e {
                    lo = mid + 1;
                } else {
                    return Some(u16_at(t, r + 4)? + (g - s));
                }
            }
            None
        }
        _ => None,
    }
}

fn class_of(t: &[u8], c: usize, g: u16) -> u16 {
    let f = || -> Option<u16> {
        match u16_at(t, c)? {
            1 => {
                let start = u16_at(t, c + 2)?;
                let n = u16_at(t, c + 4)?;
                let i = g.checked_sub(start).filter(|&i| i < n)?;
                u16_at(t, c + 6 + usize::from(i) * 2)
            }
            2 => {
                let n = usize::from(u16_at(t, c + 2)?);
                let (mut lo, mut hi) = (0, n);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    let r = c + 4 + mid * 6;
                    let (s, e) = (u16_at(t, r)?, u16_at(t, r + 2)?);
                    if g < s {
                        hi = mid;
                    } else if g > e {
                        lo = mid + 1;
                    } else {
                        return u16_at(t, r + 4);
                    }
                }
                None
            }
            _ => None,
        }
    };
    f().unwrap_or(0)
}

fn value_size(format: u16) -> usize {
    (format & 0xFF).count_ones() as usize * 2
}

/// `XAdvance` of a value record (0 if the format has none).
fn x_advance(t: &[u8], at: usize, format: u16) -> Option<i16> {
    if format & 4 == 0 {
        return Some(0);
    }
    i16_at(t, at + (format & 3).count_ones() as usize * 2)
}

/// The first glyph's `XAdvance` from a pair adjustment subtable, when it covers the pair.
fn pair_value(t: &[u8], s: usize, left: u16, right: u16) -> Option<i16> {
    let format = u16_at(t, s)?;
    let cov = s + usize::from(u16_at(t, s + 2)?);
    let ci = coverage_index(t, cov, left)?;
    let (vf1, vf2) = (u16_at(t, s + 4)?, u16_at(t, s + 6)?);
    let (s1, s2) = (value_size(vf1), value_size(vf2));
    match format {
        1 => {
            let n = u16_at(t, s + 8)?;
            if ci >= n {
                return None;
            }
            let ps = s + usize::from(u16_at(t, s + 10 + usize::from(ci) * 2)?);
            let count = usize::from(u16_at(t, ps)?);
            let rec = 2 + s1 + s2;
            let (mut lo, mut hi) = (0, count);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let r = ps + 2 + mid * rec;
                let g = u16_at(t, r)?;
                match g.cmp(&right) {
                    std::cmp::Ordering::Less => lo = mid + 1,
                    std::cmp::Ordering::Greater => hi = mid,
                    std::cmp::Ordering::Equal => return x_advance(t, r + 2, vf1),
                }
            }
            None
        }
        2 => {
            let cd1 = s + usize::from(u16_at(t, s + 8)?);
            let cd2 = s + usize::from(u16_at(t, s + 10)?);
            let (n1, n2) = (u16_at(t, s + 12)?, u16_at(t, s + 14)?);
            let (c1, c2) = (class_of(t, cd1, left), class_of(t, cd2, right));
            if c1 >= n1 || c2 >= n2 {
                return None;
            }
            let r = s + 16 + (usize::from(c1) * usize::from(n2) + usize::from(c2)) * (s1 + s2);
            x_advance(t, r, vf1)
        }
        _ => None,
    }
}
