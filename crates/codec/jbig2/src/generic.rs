//! Generic region decoding (6.2) and generic refinement region decoding (6.3).
//!
//! The arithmetic generic decoder keeps the two rows above and the decoded part of the
//! current row in sliding 17-bit windows (bit `8 - dx` holds the pixel at column x + dx), so
//! each pixel's context is a few shifts and masks; adaptive pixels inside the windows are
//! one shift each, others are read from the bitmap.

use crate::bitmap::{Bitmap, Budget};
use crate::mq::Mq;
use crate::{Error, Result};

/// Number of contexts of generic template `t`.
pub(crate) fn contexts(t: u8) -> usize {
    match t {
        0 => 1 << 16,
        1 => 1 << 13,
        _ => 1 << 10,
    }
}

/// Number of contexts of refinement template `t`.
pub(crate) fn refine_contexts(t: u8) -> usize {
    if t == 0 { 1 << 13 } else { 1 << 10 }
}

/// Default adaptive pixels of generic template `t`.
pub(crate) fn default_at(t: u8) -> [(i32, i32); 4] {
    match t {
        0 => [(3, -1), (-3, -1), (2, -2), (-2, -2)],
        1 => [(3, -1), (0, 0), (0, 0), (0, 0)],
        _ => [(2, -1), (0, 0), (0, 0), (0, 0)],
    }
}

pub(crate) struct Generic<'a> {
    pub w: u32,
    pub h: u32,
    pub template: u8,
    pub tpgdon: bool,
    pub at: [(i32, i32); 4],
    /// Pixels set here are not coded and stay 0 (halftone grey-scale planes).
    pub skip: Option<&'a Bitmap>,
}

/// Where an adaptive pixel comes from.
#[derive(Clone, Copy)]
enum At {
    /// Window 0, 1 or 2 (current row, one up, two up) and the shift.
    Win(u8, u32),
    Direct(i64, i64),
}

fn at_source((dx, dy): (i32, i32)) -> At {
    let (dx, dy) = (dx as i64, dy as i64);
    if (-8..=8).contains(&dx) {
        match dy {
            0 if dx < 0 => return At::Win(0, (8 - dx) as u32),
            -1 => return At::Win(1, (8 - dx) as u32),
            -2 => return At::Win(2, (8 - dx) as u32),
            _ => {}
        }
    }
    At::Direct(dx, dy)
}

const MASK: u32 = 0x1FFFF;

#[inline]
fn px(row: Option<&[u8]>, x: usize) -> u32 {
    row.and_then(|r| r.get(x)).copied().unwrap_or(0) as u32
}

/// Decodes a generic region with the arithmetic coder (MMR is [`crate::mmr::decode`]).
pub(crate) fn decode(mq: &mut Mq, cx: &mut [u8], p: &Generic, budget: &Budget) -> Result<Bitmap> {
    if cx.len() < contexts(p.template) {
        return Err(Error::Malformed("generic region contexts".into()));
    }
    let n_at = if p.template == 0 { 4 } else { 1 };
    let mut ats = [At::Win(0, 0); 4];
    for (a, &at) in ats.iter_mut().zip(&p.at).take(n_at) {
        *a = at_source(at);
    }
    let mut win = [(0usize, 0u32); 4];
    for (wn, a) in win.iter_mut().zip(&ats).take(n_at) {
        match *a {
            At::Win(k, s) => *wn = (k as usize, s),
            At::Direct(..) => return decode_direct(mq, cx, p, &ats, budget),
        }
    }
    // The default adaptive pixels make each row above one contiguous run of context bits,
    // so the context is shifted along rather than rebuilt.
    if p.skip.is_none() && p.at[..n_at] == default_at(p.template)[..n_at] {
        let inc = match p.template {
            0 => Inc {
                keep: 0xF7EE,
                a1: (4, 4),
                a2: (3, 11),
            },
            1 => Inc {
                keep: 0x1DF6,
                a1: (4, 3),
                a2: (3, 9),
            },
            2 => Inc {
                keep: 0x037A,
                a1: (3, 2),
                a2: (2, 7),
            },
            _ => Inc {
                keep: 0x03EE,
                a1: (3, 4),
                a2: (0, 16),
            },
        };
        let first = move |w1: u32, w2: u32| match p.template {
            0 => (((w1 >> 5) & 0x7F) << 4) | (((w2 >> 6) & 0x1F) << 11),
            1 => (((w1 >> 5) & 0x3F) << 3) | (((w2 >> 6) & 0xF) << 9),
            2 => (((w1 >> 6) & 0x1F) << 2) | (((w2 >> 7) & 7) << 7),
            _ => ((w1 >> 6) & 0x3F) << 4,
        };
        // The bits entering from above, per pixel, gathered first in a loop that vectorises.
        let mut above = vec![0u16; p.w as usize];
        return rows(mq, cx, p, budget, move |mq, cx, cur, l1, l2, _| {
            let ahead = l1[inc.a1.0..].iter().zip(&l2[inc.a2.0..]);
            for (b, (&a1, &a2)) in above.iter_mut().zip(ahead) {
                *b = (((a1 as u32) << inc.a1.1) | (((a2 as u32) << inc.a2.1) & 0xFFFF)) as u16;
            }
            let mut ctx = first(window(l1), window(l2));
            // A local copy keeps the coder's registers out of memory in the loop.
            let mut m = mq.clone();
            for (c, &b) in cur.iter_mut().zip(&above) {
                let v = m.decode(&mut cx[ctx as usize]);
                *c = v as u8;
                ctx = ((ctx << 1) & inc.keep) | v | b as u32;
            }
            *mq = m;
        });
    }
    let at = move |w: [u32; 3], i: usize| (w[win[i].0] >> win[i].1) & 1;
    let t = p.template;
    let n = move |w0: u32, w1: u32, w2: u32| match t {
        0 => {
            let w = [w0, w1, w2];
            nominal(0, w0, w1, w2)
                | (at(w, 0) << 4)
                | (at(w, 1) << 10)
                | (at(w, 2) << 11)
                | (at(w, 3) << 15)
        }
        1 => nominal(1, w0, w1, w2) | (at([w0, w1, w2], 0) << 3),
        2 => nominal(2, w0, w1, w2) | (at([w0, w1, w2], 0) << 2),
        _ => nominal(3, w0, w1, w2) | (at([w0, w1, w2], 0) << 4),
    };
    rows(mq, cx, p, budget, |mq, cx, cur, l1, l2, skip| {
        let (mut w1, mut w2) = (window(l1), window(l2));
        let mut w0 = 0u32;
        let ahead = l1[9..].iter().zip(&l2[9..]);
        let skip = skip.into_iter().flatten().chain(std::iter::repeat(&0));
        for ((c, (&a1, &a2)), &s) in cur.iter_mut().zip(ahead).zip(skip) {
            let v = if s != 0 {
                0
            } else {
                mq.decode(&mut cx[n(w0, w1, w2) as usize])
            };
            *c = v as u8;
            w0 = ((w0 | (v << 8)) << 1) & MASK;
            w1 = ((w1 << 1) | a1 as u32) & MASK;
            w2 = ((w2 << 1) | a2 as u32) & MASK;
        }
    })
}

/// How the context of a nominal template moves one pixel right: the bits kept after the
/// shift, and (offset from the decoded pixel, context bit) of the pixels entering from the
/// two rows above.
#[derive(Clone, Copy)]
struct Inc {
    keep: u32,
    a1: (usize, u32),
    a2: (usize, u32),
}

/// The window of a padded row above at column 0.
#[inline(always)]
fn window(l: &[u8]) -> u32 {
    l[..9]
        .iter()
        .enumerate()
        .fold(0, |w, (c, &v)| w | ((v as u32) << (8 - c)))
}

/// The context bits of template `t` without its adaptive pixels.
#[inline(always)]
fn nominal(t: u8, w0: u32, w1: u32, w2: u32) -> u32 {
    match t {
        0 => (((w2 >> 7) & 7) << 12) | (((w1 >> 6) & 0x1F) << 5) | ((w0 >> 9) & 0xF),
        1 => (((w2 >> 6) & 0xF) << 9) | (((w1 >> 6) & 0x1F) << 4) | ((w0 >> 9) & 7),
        2 => (((w2 >> 7) & 7) << 7) | (((w1 >> 7) & 0xF) << 3) | ((w0 >> 9) & 3),
        _ => (((w1 >> 7) & 0x1F) << 5) | ((w0 >> 9) & 0xF),
    }
}

/// Typical prediction's context (6.2.5.7, SLTP).
fn sltp(t: u8) -> usize {
    match t {
        0 => 0x9B25,
        1 => 0x0795,
        2 => 0x00E5,
        _ => 0x0195,
    }
}

/// Row loop with typical prediction when every context pixel lies in the windows. `line`
/// decodes one row from the two rows above, copied into zero-padded lines so look-ahead
/// needs no bounds checks, and the row's skip mask.
#[inline(always)]
fn rows(
    mq: &mut Mq,
    cx: &mut [u8],
    p: &Generic,
    budget: &Budget,
    mut line: impl FnMut(&mut Mq, &mut [u8], &mut [u8], &[u8], &[u8], Option<&[u8]>),
) -> Result<Bitmap> {
    let mut bm = Bitmap::new(p.w, p.h, budget)?;
    let w = p.w as usize;
    let cx = &mut cx[..contexts(p.template)];
    let sltp = sltp(p.template);
    let mut l1 = vec![0u8; w + 9];
    let mut l2 = vec![0u8; w + 9];
    let mut ltp = 0u32;
    for y in 0..p.h as usize {
        std::mem::swap(&mut l1, &mut l2);
        if y > 0 {
            l1[..w].copy_from_slice(&bm.d[(y - 1) * w..y * w]);
        }
        if p.tpgdon {
            ltp ^= mq.decode(&mut cx[sltp]);
            if ltp == 1 {
                if y > 0 {
                    bm.d.copy_within((y - 1) * w..y * w, y * w);
                }
                continue;
            }
        }
        if mq.exhausted() {
            return Err(Error::Truncated("generic region data ends early".into()));
        }
        let skip = p.skip.map(|s| s.row(y as u32));
        line(mq, cx, &mut bm.d[y * w..(y + 1) * w], &l1, &l2, skip);
    }
    Ok(bm)
}

/// The general loop, for adaptive pixels outside the windows.
fn decode_direct(
    mq: &mut Mq,
    cx: &mut [u8],
    p: &Generic,
    ats: &[At; 4],
    budget: &Budget,
) -> Result<Bitmap> {
    let mut bm = Bitmap::new(p.w, p.h, budget)?;
    let w = p.w as usize;
    let sltp = sltp(p.template);
    let mut ltp = 0u32;
    for y in 0..p.h as usize {
        if p.tpgdon {
            ltp ^= mq.decode(&mut cx[sltp]);
            if ltp == 1 {
                if y > 0 {
                    bm.d.copy_within((y - 1) * w..y * w, y * w);
                }
                continue;
            }
        }
        if mq.exhausted() {
            return Err(Error::Truncated("generic region data ends early".into()));
        }
        let (above, cur) = bm.d.split_at_mut(y * w);
        let cur = &mut cur[..w];
        let r1 = (y >= 1).then(|| &above[(y - 1) * w..y * w]);
        let r2 = (y >= 2).then(|| &above[(y - 2) * w..(y - 1) * w]);
        let mut w1 = 0u32;
        let mut w2 = 0u32;
        for c in 0..=8usize {
            w1 |= px(r1, c) << (8 - c);
            w2 |= px(r2, c) << (8 - c);
        }
        let mut w0 = 0u32;
        let skip = p.skip.map(|s| s.row(y as u32));
        for x in 0..w {
            let at = |i: usize| -> u32 {
                match ats[i] {
                    At::Win(0, s) => (w0 >> s) & 1,
                    At::Win(1, s) => (w1 >> s) & 1,
                    At::Win(_, s) => (w2 >> s) & 1,
                    At::Direct(dx, dy) => {
                        let (xx, yy) = (x as i64 + dx, y as i64 + dy);
                        if yy < 0 || xx < 0 || xx >= w as i64 || yy > y as i64 {
                            0
                        } else if yy == y as i64 {
                            if xx < x as i64 {
                                cur[xx as usize] as u32
                            } else {
                                0
                            }
                        } else {
                            above[yy as usize * w + xx as usize] as u32
                        }
                    }
                }
            };
            let ctx = nominal(p.template, w0, w1, w2)
                | match p.template {
                    0 => (at(0) << 4) | (at(1) << 10) | (at(2) << 11) | (at(3) << 15),
                    1 => at(0) << 3,
                    2 => at(0) << 2,
                    _ => at(0) << 4,
                };
            let v = if skip.is_some_and(|s| s[x] != 0) {
                0
            } else {
                mq.decode(&mut cx[ctx as usize])
            };
            cur[x] = v as u8;
            w0 = ((w0 | (v << 8)) << 1) & MASK;
            w1 = ((w1 << 1) | px(r1, x + 9)) & MASK;
            w2 = ((w2 << 1) | px(r2, x + 9)) & MASK;
        }
    }
    Ok(bm)
}

pub(crate) struct Refine<'a> {
    pub w: u32,
    pub h: u32,
    pub template: u8,
    pub reference: &'a Bitmap,
    pub dx: i64,
    pub dy: i64,
    pub tpgron: bool,
    pub at: [(i8, i8); 2],
}

/// Decodes a generic refinement region (6.3.5).
pub(crate) fn refine(mq: &mut Mq, cx: &mut [u8], p: &Refine, budget: &Budget) -> Result<Bitmap> {
    let mut bm = Bitmap::new(p.w, p.h, budget)?;
    if cx.len() < refine_contexts(p.template) {
        return Err(Error::Malformed("refinement contexts".into()));
    }
    let r = p.reference;
    let (a1x, a1y) = (p.at[0].0 as i64, p.at[0].1 as i64);
    let (a2x, a2y) = (p.at[1].0 as i64, p.at[1].1 as i64);
    let sltp = if p.template == 0 { 0x100 } else { 0x080 };
    let mut ltp = 0;
    let w = p.w as i64;
    for y in 0..p.h as i64 {
        if p.tpgron {
            ltp ^= mq.decode(&mut cx[sltp]);
        }
        if mq.exhausted() {
            return Err(Error::Truncated("refinement data ends early".into()));
        }
        for x in 0..w {
            let (rx, ry) = (x - p.dx, y - p.dy);
            if ltp == 1 {
                let v = r.get(rx, ry);
                let same = (-1..=1).all(|j| (-1..=1).all(|i| r.get(rx + i, ry + j) == v));
                if same {
                    bm.d[(y * w + x) as usize] = v;
                    continue;
                }
            }
            let g = |xx: i64, yy: i64| bm.get(xx, yy) as usize;
            let f = |xx: i64, yy: i64| r.get(xx, yy) as usize;
            let ctx = if p.template == 0 {
                g(x - 1, y)
                    | (g(x + 1, y - 1) << 1)
                    | (g(x, y - 1) << 2)
                    | (g(x + a1x, y + a1y) << 3)
                    | (f(rx + 1, ry + 1) << 4)
                    | (f(rx, ry + 1) << 5)
                    | (f(rx - 1, ry + 1) << 6)
                    | (f(rx + 1, ry) << 7)
                    | (f(rx, ry) << 8)
                    | (f(rx - 1, ry) << 9)
                    | (f(rx + 1, ry - 1) << 10)
                    | (f(rx, ry - 1) << 11)
                    | (f(rx + a2x, ry + a2y) << 12)
            } else {
                g(x - 1, y)
                    | (g(x + 1, y - 1) << 1)
                    | (g(x, y - 1) << 2)
                    | (g(x - 1, y - 1) << 3)
                    | (f(rx + 1, ry + 1) << 4)
                    | (f(rx, ry + 1) << 5)
                    | (f(rx + 1, ry) << 6)
                    | (f(rx, ry) << 7)
                    | (f(rx - 1, ry) << 8)
                    | (f(rx, ry - 1) << 9)
            };
            let v = mq.decode(&mut cx[ctx]);
            bm.d[(y * w + x) as usize] = v as u8;
        }
    }
    Ok(bm)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shifted-context and window loops agree with the general one on every template,
    /// with default and moved adaptive pixels, with and without typical prediction.
    #[test]
    fn fast_paths_match_general_loop() {
        let mut seed = 0x1234_5678u32;
        let data: Vec<u8> = (0..4000)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        let budget = Budget::new();
        for template in 0..4 {
            let moved = [(-5, -1), (4, -2), (-8, 0), (1, -2)];
            for at in [default_at(template), moved] {
                for tpgdon in [false, true] {
                    let p = Generic {
                        w: 77,
                        h: 31,
                        template,
                        tpgdon,
                        at,
                        skip: None,
                    };
                    let mut cx = vec![0; contexts(template)];
                    let fast = decode(&mut Mq::new(&data), &mut cx, &p, &budget).unwrap();
                    let mut ats = [At::Win(0, 0); 4];
                    for (a, &v) in ats.iter_mut().zip(&at) {
                        *a = At::Direct(v.0 as i64, v.1 as i64);
                    }
                    let mut cx2 = vec![0; contexts(template)];
                    let slow = decode_direct(&mut Mq::new(&data), &mut cx2, &p, &ats, &budget);
                    assert_eq!(fast, slow.unwrap(), "template {template} at {at:?}");
                    assert_eq!(cx, cx2);
                    assert!(fast.d.contains(&1) && fast.d.contains(&0));
                }
            }
        }
    }
}
