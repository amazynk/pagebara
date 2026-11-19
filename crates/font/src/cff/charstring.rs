//! Type 2 charstrings (Adobe TN 5177) and the CFF2 additions (`blend`, `vsindex`).

use super::Index;
use crate::outline::Pen;
use crate::reader::{i16_at, u32_at};

const MAX_DEPTH: u32 = 10;
const MAX_OPS: u32 = 1 << 16;

/// Subroutine sets and CFF2 settings for one charstring run.
pub(crate) struct Ctx<'a> {
    pub data: &'a [u8],
    pub gsubrs: &'a Index,
    pub lsubrs: &'a Index,
    pub cff2: bool,
    /// Region counts by `vsindex` (CFF2).
    pub regions: &'a [u16],
    pub vsindex: u16,
}

/// A `seac`-style accented glyph request from `endchar`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Seac {
    pub adx: f32,
    pub ady: f32,
    pub base: u8,
    pub accent: u8,
}

/// Subroutines a charstring called (for subsetting).
#[derive(Default, Debug)]
pub(crate) struct Trace {
    pub local: Vec<bool>,
    pub global: Vec<bool>,
}

#[derive(Default, Debug)]
pub(crate) struct Result {
    /// The width operand, if the charstring had one (add nominalWidthX).
    pub width: Option<f32>,
    pub seac: Option<Seac>,
}

thread_local!(static STACK: std::cell::Cell<Vec<f32>> = std::cell::Cell::default());

pub(crate) fn bias(count: usize) -> i32 {
    if count < 1240 {
        107
    } else if count < 33900 {
        1131
    } else {
        32768
    }
}

struct Machine<'a, 'p> {
    ctx: &'a Ctx<'a>,
    pen: &'a mut Pen<'p>,
    stack: Vec<f32>,
    transient: [f32; 32],
    x: f32,
    y: f32,
    stems: usize,
    width_done: bool,
    res: Result,
    ops: u32,
    vsindex: u16,
    done: bool,
    trace: Option<&'a mut Trace>,
}

pub(crate) fn run(ctx: &Ctx, cs: &[u8], pen: &mut Pen, origin: (f32, f32)) -> Option<Result> {
    run_traced(ctx, cs, pen, origin, None)
}

pub(crate) fn run_traced(
    ctx: &Ctx,
    cs: &[u8],
    pen: &mut Pen,
    origin: (f32, f32),
    trace: Option<&mut Trace>,
) -> Option<Result> {
    let mut m = Machine {
        ctx,
        pen,
        stack: STACK.take(),
        transient: [0.0; 32],
        x: origin.0,
        y: origin.1,
        stems: 0,
        width_done: ctx.cff2,
        res: Result::default(),
        ops: 0,
        vsindex: ctx.vsindex,
        done: false,
        trace,
    };
    m.stack.clear();
    let ok = m.exec(cs, 0);
    m.pen.finish();
    STACK.set(std::mem::take(&mut m.stack));
    ok?;
    Some(m.res)
}

impl Machine<'_, '_> {
    fn max_stack(&self) -> usize {
        if self.ctx.cff2 { 513 } else { 48 }
    }

    fn push(&mut self, v: f32) -> Option<()> {
        if self.stack.len() >= self.max_stack() {
            return None;
        }
        self.stack.push(v);
        Some(())
    }

    fn pop(&mut self) -> Option<f32> {
        self.stack.pop()
    }

    /// Takes the width operand of the first stack-clearing operator if the count says so.
    fn width(&mut self, odd_if_width: bool) {
        if !self.width_done {
            self.width_done = true;
            let has = if odd_if_width {
                self.stack.len() % 2 == 1
            } else {
                self.stack.len() % 2 == 0 && !self.stack.is_empty()
            };
            if has {
                self.res.width = Some(self.stack.remove(0));
            }
        }
    }

    fn stem(&mut self) {
        self.width(true);
        self.stems += self.stack.len() / 2;
        self.stack.clear();
    }

    fn line(&mut self, dx: f32, dy: f32) {
        self.x += dx;
        self.y += dy;
        self.pen.line_to(self.x, self.y);
    }

    fn curve(&mut self, d: [f32; 6]) {
        let (x1, y1) = (self.x + d[0], self.y + d[1]);
        let (x2, y2) = (x1 + d[2], y1 + d[3]);
        self.x = x2 + d[4];
        self.y = y2 + d[5];
        self.pen.curve_to(x1, y1, x2, y2, self.x, self.y);
    }

    fn exec(&mut self, cs: &[u8], depth: u32) -> Option<()> {
        if depth > MAX_DEPTH {
            return None;
        }
        let mut i = 0;
        while i < cs.len() && !self.done {
            self.ops += 1;
            if self.ops > MAX_OPS {
                return None;
            }
            let b = cs[i];
            i += 1;
            match b {
                32..=246 => self.push(f32::from(b) - 139.0)?,
                247..=250 => {
                    let w = *cs.get(i)?;
                    i += 1;
                    self.push((f32::from(b - 247)) * 256.0 + f32::from(w) + 108.0)?;
                }
                251..=254 => {
                    let w = *cs.get(i)?;
                    i += 1;
                    self.push(-(f32::from(b - 251)) * 256.0 - f32::from(w) - 108.0)?;
                }
                28 => {
                    let v = i16_at(cs, i)?;
                    i += 2;
                    self.push(f32::from(v))?;
                }
                255 => {
                    let v = u32_at(cs, i)? as i32;
                    i += 4;
                    self.push(v as f32 / 65536.0)?;
                }
                1 | 3 | 18 | 23 => self.stem(),
                19 | 20 => {
                    self.stem();
                    i += self.stems.div_ceil(8);
                }
                21 => {
                    self.width(true);
                    let s = &self.stack;
                    let (dx, dy) = (
                        s.len().checked_sub(2).map_or(0.0, |k| s[k]),
                        s.last().copied().unwrap_or(0.0),
                    );
                    self.x += dx;
                    self.y += dy;
                    self.pen.move_to(self.x, self.y);
                    self.stack.clear();
                }
                22 | 4 => {
                    self.width(false);
                    let d = self.stack.last().copied().unwrap_or(0.0);
                    if b == 22 {
                        self.x += d;
                    } else {
                        self.y += d;
                    }
                    self.pen.move_to(self.x, self.y);
                    self.stack.clear();
                }
                5 => {
                    let s = std::mem::take(&mut self.stack);
                    for p in s.as_chunks::<2>().0 {
                        self.line(p[0], p[1]);
                    }
                    self.stack = s;
                    self.stack.clear();
                }
                6 | 7 => {
                    let s = std::mem::take(&mut self.stack);
                    let mut horiz = b == 6;
                    for &d in &s {
                        if horiz {
                            self.line(d, 0.0);
                        } else {
                            self.line(0.0, d);
                        }
                        horiz = !horiz;
                    }
                    self.stack = s;
                    self.stack.clear();
                }
                8 => {
                    let s = std::mem::take(&mut self.stack);
                    for c in s.as_chunks::<6>().0 {
                        self.curve([c[0], c[1], c[2], c[3], c[4], c[5]]);
                    }
                    self.stack = s;
                    self.stack.clear();
                }
                24 => {
                    // rcurveline
                    let s = std::mem::take(&mut self.stack);
                    let n = s.len().saturating_sub(2) / 6;
                    for c in s.as_chunks::<6>().0.iter().take(n) {
                        self.curve([c[0], c[1], c[2], c[3], c[4], c[5]]);
                    }
                    if let [.., dx, dy] = s[n * 6..] {
                        self.line(dx, dy);
                    }
                    self.stack = s;
                    self.stack.clear();
                }
                25 => {
                    // rlinecurve
                    let s = std::mem::take(&mut self.stack);
                    let lines = s.len().saturating_sub(6) / 2;
                    for p in s.as_chunks::<2>().0.iter().take(lines) {
                        self.line(p[0], p[1]);
                    }
                    if let Some(c) = s.get(lines * 2..lines * 2 + 6) {
                        self.curve([c[0], c[1], c[2], c[3], c[4], c[5]]);
                    }
                    self.stack = s;
                    self.stack.clear();
                }
                26 | 27 => {
                    // vvcurveto / hhcurveto
                    let s = std::mem::take(&mut self.stack);
                    let mut k = 0;
                    let mut first = 0.0;
                    if s.len() % 4 == 1 {
                        first = s[0];
                        k = 1;
                    }
                    while k + 4 <= s.len() {
                        let c = &s[k..k + 4];
                        if b == 26 {
                            self.curve([first, c[0], c[1], c[2], 0.0, c[3]]);
                        } else {
                            self.curve([c[0], first, c[1], c[2], c[3], 0.0]);
                        }
                        first = 0.0;
                        k += 4;
                    }
                    self.stack = s;
                    self.stack.clear();
                }
                30 | 31 => {
                    // vhcurveto / hvcurveto
                    let s = std::mem::take(&mut self.stack);
                    let mut horiz = b == 31;
                    let mut k = 0;
                    while k + 4 <= s.len() {
                        let c = &s[k..k + 4];
                        let last = if s.len() - (k + 4) == 1 {
                            s[k + 4]
                        } else {
                            0.0
                        };
                        if horiz {
                            self.curve([c[0], 0.0, c[1], c[2], last, c[3]]);
                        } else {
                            self.curve([0.0, c[0], c[1], c[2], c[3], last]);
                        }
                        horiz = !horiz;
                        k += 4;
                    }
                    self.stack = s;
                    self.stack.clear();
                }
                10 | 29 => {
                    let subrs = if b == 10 {
                        self.ctx.lsubrs
                    } else {
                        self.ctx.gsubrs
                    };
                    let n = self.pop()? as i32 + bias(subrs.count as usize);
                    let n = u32::try_from(n).ok()?;
                    let sub = subrs.get(self.ctx.data, n)?;
                    if let Some(t) = self.trace.as_deref_mut() {
                        let v = if b == 10 { &mut t.local } else { &mut t.global };
                        if let Some(slot) = v.get_mut(n as usize) {
                            *slot = true;
                        }
                    }
                    self.exec(sub, depth + 1)?;
                }
                11 => return Some(()),
                14 => {
                    if self.ctx.cff2 {
                        return None;
                    }
                    if self.stack.len() >= 4 {
                        if !self.width_done && self.stack.len() == 5 {
                            self.width_done = true;
                            self.res.width = Some(self.stack.remove(0));
                        }
                        let s = &self.stack;
                        let n = s.len();
                        self.res.seac = Some(Seac {
                            adx: s[n - 4],
                            ady: s[n - 3],
                            base: s[n - 2] as u8,
                            accent: s[n - 1] as u8,
                        });
                    } else {
                        self.width(true);
                    }
                    self.stack.clear();
                    self.done = true;
                }
                15 if self.ctx.cff2 => {
                    self.vsindex = self.pop()? as u16;
                    self.stack.clear();
                }
                16 if self.ctx.cff2 => {
                    // blend: keep the default values.
                    let n = self.pop()? as usize;
                    let k = usize::from(*self.ctx.regions.get(usize::from(self.vsindex))?);
                    let total = n.checked_mul(k + 1)?;
                    let base = self.stack.len().checked_sub(total)?;
                    self.stack.truncate(base + n);
                }
                12 => {
                    let op = *cs.get(i)?;
                    i += 1;
                    self.escape(op)?;
                }
                _ => self.stack.clear(),
            }
        }
        Some(())
    }

    fn escape(&mut self, op: u8) -> Option<()> {
        let s = &mut self.stack;
        match op {
            34 => {
                // hflex
                let a: [f32; 7] = s.get(..7)?.try_into().ok()?;
                let y0 = self.y;
                self.curve([a[0], 0.0, a[1], a[2], a[3], 0.0]);
                self.curve([a[4], 0.0, a[5], y0 - self.y, a[6], 0.0]);
                self.stack.clear();
            }
            35 => {
                // flex
                let a: [f32; 12] = s.get(..12)?.try_into().ok()?;
                self.curve([a[0], a[1], a[2], a[3], a[4], a[5]]);
                self.curve([a[6], a[7], a[8], a[9], a[10], a[11]]);
                self.stack.clear();
            }
            36 => {
                // hflex1
                let a: [f32; 9] = s.get(..9)?.try_into().ok()?;
                let y0 = self.y;
                self.curve([a[0], a[1], a[2], a[3], a[4], 0.0]);
                let dy = y0 - (self.y + a[7]);
                self.curve([a[5], 0.0, a[6], a[7], a[8], dy]);
                self.stack.clear();
            }
            37 => {
                // flex1
                let a: [f32; 11] = s.get(..11)?.try_into().ok()?;
                let (x0, y0) = (self.x, self.y);
                let dx: f32 = a[0] + a[2] + a[4] + a[6] + a[8];
                let dy: f32 = a[1] + a[3] + a[5] + a[7] + a[9];
                self.curve([a[0], a[1], a[2], a[3], a[4], a[5]]);
                if dx.abs() > dy.abs() {
                    self.curve([a[6], a[7], a[8], a[9], a[10], y0 - (y0 + dy)]);
                } else {
                    self.curve([a[6], a[7], a[8], a[9], x0 - (x0 + dx), a[10]]);
                }
                self.stack.clear();
            }
            0 => s.clear(),
            3 | 4 => {
                let b = s.pop()?;
                let a = s.pop()?;
                let r = if op == 3 {
                    a != 0.0 && b != 0.0
                } else {
                    a != 0.0 || b != 0.0
                };
                s.push(f32::from(u8::from(r)));
            }
            5 => {
                let a = s.pop()?;
                s.push(f32::from(u8::from(a == 0.0)));
            }
            9 => {
                let a = s.pop()?;
                s.push(a.abs());
            }
            10 | 11 | 12 | 24 => {
                let b = s.pop()?;
                let a = s.pop()?;
                s.push(match op {
                    10 => a + b,
                    11 => a - b,
                    12 if b != 0.0 => a / b,
                    12 => 0.0,
                    _ => a * b,
                });
            }
            14 => {
                let a = s.pop()?;
                s.push(-a);
            }
            15 => {
                let b = s.pop()?;
                let a = s.pop()?;
                s.push(f32::from(u8::from(a == b)));
            }
            18 => {
                s.pop()?;
            }
            20 => {
                let i = s.pop()?;
                let v = s.pop()?;
                *self.transient.get_mut(i as usize)? = v;
            }
            21 => {
                let i = s.pop()?;
                let v = *self.transient.get(i as usize)?;
                s.push(v);
            }
            22 => {
                let v2 = s.pop()?;
                let v1 = s.pop()?;
                let s2 = s.pop()?;
                let s1 = s.pop()?;
                s.push(if v1 <= v2 { s1 } else { s2 });
            }
            23 => s.push(0.5),
            26 => {
                let a = s.pop()?;
                s.push(a.max(0.0).sqrt());
            }
            27 => {
                let a = *s.last()?;
                self.push(a)?;
            }
            28 => {
                let n = s.len();
                if n >= 2 {
                    s.swap(n - 1, n - 2);
                }
            }
            29 => {
                let i = s.pop()?;
                let n = s.len();
                let i = if i < 0.0 { 0 } else { i as usize };
                let v = *s.get(n.checked_sub(i + 1)?)?;
                s.push(v);
            }
            30 => {
                let j = s.pop()? as i32;
                let n = s.pop()?;
                let n = usize::try_from(n as i32).ok()?;
                let len = s.len();
                if n > 0 && n <= len {
                    let part = &mut s[len - n..];
                    let j = j.rem_euclid(n as i32) as usize;
                    part.rotate_right(j);
                }
            }
            _ => s.clear(),
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outline::{Path, Verb};

    fn draw(cs: &[u8], cff2: bool) -> (Path, Option<Result>) {
        let empty = Index::default();
        let ctx = Ctx {
            data: &[],
            gsubrs: &empty,
            lsubrs: &empty,
            cff2,
            regions: &[1, 2],
            vsindex: 0,
        };
        let mut p = Path::new();
        let mut pen = Pen::new(&mut p, None);
        let r = run(&ctx, cs, &mut pen, (0.0, 0.0));
        (p, r)
    }

    fn n(v: i32) -> u8 {
        (v + 139) as u8
    }

    #[test]
    fn blend_keeps_defaults() {
        // 10 20 (deltas 1 2) 2 blend rmoveto; 5 0 rlineto; then vsindex 1 with two regions.
        let cs = [
            n(10),
            n(20),
            n(1),
            n(2),
            n(2),
            16,
            21,
            n(5),
            n(0),
            5,
            n(1),
            15,
            n(7),
            n(1),
            n(1),
            n(1),
            16,
            n(0),
            5,
        ];
        let (p, r) = draw(&cs, true);
        assert!(r.is_some());
        assert_eq!(p.verbs, [Verb::Move, Verb::Line, Verb::Line, Verb::Close]);
        assert_eq!(p.points, [(10.0, 20.0), (15.0, 20.0), (22.0, 20.0)]);
        // endchar is not a CFF2 operator.
        assert!(draw(&[14], true).1.is_none());
    }

    #[test]
    fn widths_and_seac() {
        // Width before hmoveto, then endchar with a seac request.
        let (_, r) = draw(
            &[n(50), n(5), 22, n(0), n(1), n(2), n(65), n(66), 14],
            false,
        );
        let r = r.unwrap();
        assert_eq!(r.width, Some(50.0));
        assert_eq!(
            r.seac,
            Some(Seac {
                adx: 1.0,
                ady: 2.0,
                base: 65,
                accent: 66
            })
        );
        // No width when the argument count is exact.
        let (_, r) = draw(&[n(5), n(5), 21, 14], false);
        assert_eq!(r.unwrap().width, None);
        let (_, r) = draw(&[n(9), n(5), n(5), 21, 14], false);
        assert_eq!(r.unwrap().width, Some(9.0));
    }

    #[test]
    fn flex_variants_end_where_they_should() {
        // hflex: the pen returns to the starting y.
        let cs = [
            n(0),
            n(0),
            21,
            n(10),
            n(10),
            n(5),
            n(10),
            n(10),
            n(10),
            n(10),
            12,
            34,
            14,
        ];
        let (p, _) = draw(&cs, false);
        assert_eq!(p.points.last(), Some(&(60.0, 0.0)));
        // flex1 with a mostly horizontal span: the last y returns to the start.
        let cs = [
            n(0),
            n(0),
            21,
            n(10),
            n(3),
            n(10),
            n(3),
            n(10),
            n(3),
            n(10),
            n(-3),
            n(10),
            n(-1),
            n(10),
            12,
            37,
            14,
        ];
        let (p, _) = draw(&cs, false);
        assert_eq!(p.points.last(), Some(&(60.0, 0.0)));
    }
}
