//! A small content-stream writer.

use std::io::Write as _;

use crate::geom::{Color, Point, Rect};

/// Bézier control-point factor for quarter circles.
pub(crate) const KAPPA: f64 = 0.552_284_75;

#[derive(Debug, Default)]
pub(crate) struct Content(pub Vec<u8>);

impl Content {
    pub fn new() -> Self {
        Content(Vec::new())
    }

    fn n(&mut self, v: f64) -> &mut Self {
        let r = (v * 1000.0).round() / 1000.0;
        let r = if r == 0.0 || !r.is_finite() { 0.0 } else { r };
        let _ = write!(self.0, "{r} ");
        self
    }

    fn op(&mut self, op: &str) -> &mut Self {
        self.0.extend_from_slice(op.as_bytes());
        self.0.push(b'\n');
        self
    }

    pub fn raw(&mut self, s: &[u8]) -> &mut Self {
        self.0.extend_from_slice(s);
        self
    }

    pub fn save(&mut self) -> &mut Self {
        self.op("q")
    }

    pub fn restore(&mut self) -> &mut Self {
        self.op("Q")
    }

    pub fn cm(&mut self, m: [f64; 6]) -> &mut Self {
        for v in m {
            self.n(v);
        }
        self.op("cm")
    }

    pub fn gs(&mut self, name: &str) -> &mut Self {
        self.op(&format!("/{name} gs"))
    }

    pub fn line_width(&mut self, w: f64) -> &mut Self {
        self.n(w).op("w")
    }

    /// Round caps and joins, as for pen strokes.
    pub fn round(&mut self) -> &mut Self {
        self.op("1 J 1 j")
    }

    pub fn dash(&mut self, d: &[f64]) -> &mut Self {
        self.0.push(b'[');
        for &v in d {
            self.n(v);
        }
        self.op("] 0 d")
    }

    pub fn fill_color(&mut self, c: Color) -> &mut Self {
        self.color(c, false)
    }

    pub fn stroke_color(&mut self, c: Color) -> &mut Self {
        self.color(c, true)
    }

    fn color(&mut self, c: Color, stroke: bool) -> &mut Self {
        for v in c.components() {
            self.n(f64::from(v));
        }
        let op = match (c, stroke) {
            (Color::Gray(_), false) => "g",
            (Color::Gray(_), true) => "G",
            (Color::Rgb(..), false) => "rg",
            (Color::Rgb(..), true) => "RG",
            (Color::Cmyk(..), false) => "k",
            (Color::Cmyk(..), true) => "K",
        };
        self.op(op)
    }

    pub fn move_to(&mut self, p: Point) -> &mut Self {
        self.n(p.x).n(p.y).op("m")
    }

    pub fn line_to(&mut self, p: Point) -> &mut Self {
        self.n(p.x).n(p.y).op("l")
    }

    pub fn curve_to(&mut self, a: Point, b: Point, p: Point) -> &mut Self {
        self.n(a.x).n(a.y).n(b.x).n(b.y).n(p.x).n(p.y).op("c")
    }

    pub fn rect(&mut self, r: Rect) -> &mut Self {
        self.n(r.x0).n(r.y0).n(r.width()).n(r.height()).op("re")
    }

    pub fn close(&mut self) -> &mut Self {
        self.op("h")
    }

    pub fn polygon(&mut self, pts: &[Point]) -> &mut Self {
        if let Some((&first, rest)) = pts.split_first() {
            self.move_to(first);
            for &p in rest {
                self.line_to(p);
            }
            self.close();
        }
        self
    }

    /// An ellipse inscribed in `r`.
    pub fn ellipse(&mut self, r: Rect) -> &mut Self {
        let (cx, cy) = ((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
        let (rx, ry) = (r.width() / 2.0, r.height() / 2.0);
        let (kx, ky) = (rx * KAPPA, ry * KAPPA);
        let p = Point::new;
        self.move_to(p(cx + rx, cy))
            .curve_to(p(cx + rx, cy + ky), p(cx + kx, cy + ry), p(cx, cy + ry))
            .curve_to(p(cx - kx, cy + ry), p(cx - rx, cy + ky), p(cx - rx, cy))
            .curve_to(p(cx - rx, cy - ky), p(cx - kx, cy - ry), p(cx, cy - ry))
            .curve_to(p(cx + kx, cy - ry), p(cx + rx, cy - ky), p(cx + rx, cy))
            .close()
    }

    pub fn stroke(&mut self) -> &mut Self {
        self.op("S")
    }

    pub fn fill(&mut self) -> &mut Self {
        self.op("f")
    }

    pub fn fill_stroke(&mut self) -> &mut Self {
        self.op("B")
    }

    pub fn end_path(&mut self) -> &mut Self {
        self.op("n")
    }

    pub fn clip(&mut self) -> &mut Self {
        self.op("W n")
    }

    pub fn do_xobject(&mut self, name: &str) -> &mut Self {
        self.op(&format!("/{name} Do"))
    }

    /// One line of text at `at`: runs of codes, each in a font's encoding.
    pub fn text(&mut self, size: f64, at: Point, runs: &[(String, Vec<u8>)]) -> &mut Self {
        self.op("BT");
        self.n(at.x).n(at.y).op("Td");
        let mut current = None;
        for (font, encoded) in runs {
            if current != Some(font) {
                self.0.extend_from_slice(format!("/{font} ").as_bytes());
                self.n(size).op("Tf");
                current = Some(font);
            }
            self.string(encoded);
            self.op(" Tj");
        }
        self.op("ET")
    }

    fn string(&mut self, s: &[u8]) {
        self.0.push(b'(');
        for &b in s {
            match b {
                b'(' | b')' | b'\\' => self.0.extend_from_slice(&[b'\\', b]),
                b'\r' => self.0.extend_from_slice(b"\\r"),
                b'\n' => self.0.extend_from_slice(b"\\n"),
                _ => self.0.push(b),
            }
        }
        self.0.push(b')');
    }
}
