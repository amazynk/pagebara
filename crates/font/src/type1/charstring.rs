//! Type 1 charstrings (Adobe Type 1 Font Format, chapter 6) including flex and hint
//! replacement through the standard OtherSubrs, and `seac`.

use crate::outline::Pen;
use crate::reader::u32_at;

const MAX_DEPTH: u32 = 10;
const MAX_OPS: u32 = 1 << 16;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Seac {
    pub asb: f32,
    pub adx: f32,
    pub ady: f32,
    pub base: u8,
    pub accent: u8,
}

#[derive(Default, Debug)]
pub(crate) struct Result {
    pub width: f32,
    /// The left side bearing from `hsbw`/`sbw`.
    pub sbx: f32,
    /// The `sbw` operands (side bearing and width vectors), for vertical metrics.
    pub sbw: Option<[f32; 4]>,
    pub seac: Option<Seac>,
}

struct Machine<'s, 'a, 'p> {
    subrs: &'s dyn Fn(usize) -> Option<&'s [u8]>,
    pen: &'a mut Pen<'p>,
    stack: Vec<f32>,
    ps: Vec<f32>,
    x: f32,
    y: f32,
    origin: (f32, f32),
    flex: Option<Vec<(f32, f32)>>,
    res: Result,
    ops: u32,
    done: bool,
}

/// Runs a decrypted charstring with its origin at `origin`.
pub(crate) fn run<'s>(
    cs: &[u8],
    subrs: &'s dyn Fn(usize) -> Option<&'s [u8]>,
    pen: &mut Pen,
    origin: (f32, f32),
) -> Option<Result> {
    let mut m = Machine {
        subrs,
        pen,
        stack: Vec::with_capacity(24),
        ps: Vec::new(),
        x: origin.0,
        y: origin.1,
        origin,
        flex: None,
        res: Result::default(),
        ops: 0,
        done: false,
    };
    m.exec(cs, 0)?;
    m.pen.finish();
    Some(m.res)
}

impl Machine<'_, '_, '_> {
    fn push(&mut self, v: f32) -> Option<()> {
        // Real fonts stay well below this; the limit only bounds memory.
        if self.stack.len() >= 64 {
            return None;
        }
        self.stack.push(v);
        Some(())
    }

    fn arg(&self, i: usize) -> f32 {
        self.stack.get(i).copied().unwrap_or(0.0)
    }

    fn move_by(&mut self, dx: f32, dy: f32) {
        self.x += dx;
        self.y += dy;
        match &mut self.flex {
            Some(pts) => {
                if pts.len() < 16 {
                    pts.push((self.x, self.y));
                }
            }
            None => self.pen.move_to(self.x, self.y),
        }
    }

    fn line_by(&mut self, dx: f32, dy: f32) {
        self.x += dx;
        self.y += dy;
        self.pen.line_to(self.x, self.y);
    }

    fn curve_by(&mut self, d: [f32; 6]) {
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
                    self.push(f32::from(b - 247) * 256.0 + f32::from(w) + 108.0)?;
                }
                251..=254 => {
                    let w = *cs.get(i)?;
                    i += 1;
                    self.push(-f32::from(b - 251) * 256.0 - f32::from(w) - 108.0)?;
                }
                255 => {
                    let v = u32_at(cs, i)? as i32;
                    i += 4;
                    self.push(v as f32)?;
                }
                13 => {
                    // hsbw
                    self.x = self.origin.0 + self.arg(0);
                    self.y = self.origin.1;
                    self.res.width = self.arg(1);
                    self.res.sbx = self.arg(0);
                    self.stack.clear();
                }
                9 => {
                    self.pen.close();
                    self.stack.clear();
                }
                21 => {
                    let (dx, dy) = (self.arg(0), self.arg(1));
                    self.move_by(dx, dy);
                    self.stack.clear();
                }
                22 => {
                    let dx = self.arg(0);
                    self.move_by(dx, 0.0);
                    self.stack.clear();
                }
                4 => {
                    let dy = self.arg(0);
                    self.move_by(0.0, dy);
                    self.stack.clear();
                }
                5 => {
                    let (dx, dy) = (self.arg(0), self.arg(1));
                    self.line_by(dx, dy);
                    self.stack.clear();
                }
                6 => {
                    let dx = self.arg(0);
                    self.line_by(dx, 0.0);
                    self.stack.clear();
                }
                7 => {
                    let dy = self.arg(0);
                    self.line_by(0.0, dy);
                    self.stack.clear();
                }
                8 => {
                    let a = [0, 1, 2, 3, 4, 5].map(|k| self.arg(k));
                    self.curve_by(a);
                    self.stack.clear();
                }
                30 => {
                    let a = [0, 1, 2, 3].map(|k| self.arg(k));
                    self.curve_by([0.0, a[0], a[1], a[2], a[3], 0.0]);
                    self.stack.clear();
                }
                31 => {
                    let a = [0, 1, 2, 3].map(|k| self.arg(k));
                    self.curve_by([a[0], 0.0, a[1], a[2], 0.0, a[3]]);
                    self.stack.clear();
                }
                10 => {
                    let n = self.stack.pop()?;
                    let sub = (self.subrs)(usize::try_from(n as i64).ok()?)?;
                    self.exec(sub, depth + 1)?;
                }
                11 => return Some(()),
                14 => {
                    self.stack.clear();
                    self.done = true;
                }
                1 | 3 => self.stack.clear(),
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
        match op {
            6 => {
                // seac
                if self.stack.len() >= 5 {
                    let n = self.stack.len();
                    let s = &self.stack[n - 5..];
                    self.res.seac = Some(Seac {
                        asb: s[0],
                        adx: s[1],
                        ady: s[2],
                        base: s[3] as u8,
                        accent: s[4] as u8,
                    });
                }
                self.stack.clear();
                self.done = true;
            }
            7 => {
                // sbw
                self.x = self.origin.0 + self.arg(0);
                self.y = self.origin.1 + self.arg(1);
                self.res.width = self.arg(2);
                self.res.sbx = self.arg(0);
                self.res.sbw = Some([0, 1, 2, 3].map(|k| self.arg(k)));
                self.stack.clear();
            }
            12 => {
                let b = self.stack.pop()?;
                let a = self.stack.pop()?;
                self.stack.push(if b != 0.0 { a / b } else { 0.0 });
            }
            16 => {
                let other = self.stack.pop()? as i32;
                let n = (self.stack.pop()? as i32).max(0) as usize;
                let n = n.min(self.stack.len());
                let args = self.stack.split_off(self.stack.len() - n);
                match other {
                    0 => {
                        // End of flex: the reference point and six control/end points.
                        if let Some(p) = self.flex.take()
                            && p.len() >= 7
                        {
                            self.pen
                                .curve_to(p[1].0, p[1].1, p[2].0, p[2].1, p[3].0, p[3].1);
                            self.pen
                                .curve_to(p[4].0, p[4].1, p[5].0, p[5].1, p[6].0, p[6].1);
                            self.x = p[6].0;
                            self.y = p[6].1;
                        }
                        self.ps.clear();
                        self.ps
                            .extend([self.y - self.origin.1, self.x - self.origin.0]);
                    }
                    1 => {
                        self.flex = Some(Vec::with_capacity(7));
                    }
                    2 => {}
                    _ => {
                        self.ps.clear();
                        self.ps.extend(args.iter().rev());
                    }
                }
            }
            17 => {
                let v = self.ps.pop().unwrap_or(0.0);
                self.push(v)?;
            }
            33 => {
                self.x = self.origin.0 + self.arg(0);
                self.y = self.origin.1 + self.arg(1);
                self.stack.clear();
            }
            _ => self.stack.clear(),
        }
        Some(())
    }
}
