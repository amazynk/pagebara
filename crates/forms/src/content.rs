//! A small content-stream writer.

use std::io::Write as _;

use pagebara_annot::{Color, Rect};

/// A number as content-stream text: at most three decimals, no exponent, no `-0`.
pub(crate) fn fmt_num(v: f64) -> String {
    let r = (v * 1000.0).round() / 1000.0;
    let r = if r == 0.0 || !r.is_finite() { 0.0 } else { r };
    format!("{r}")
}

#[derive(Debug, Default)]
pub(crate) struct Content(pub Vec<u8>);

impl Content {
    pub fn n(&mut self, v: f64) -> &mut Self {
        let s = fmt_num(v);
        self.0.extend_from_slice(s.as_bytes());
        self.0.push(b' ');
        self
    }

    pub fn op(&mut self, op: &str) -> &mut Self {
        self.0.extend_from_slice(op.as_bytes());
        self.0.push(b'\n');
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

    pub fn line_width(&mut self, w: f64) -> &mut Self {
        self.n(w).op("w")
    }

    pub fn dash(&mut self, d: &[f64]) -> &mut Self {
        self.0.push(b'[');
        for &v in d {
            self.n(v);
        }
        self.op("] 0 d")
    }

    pub fn color(&mut self, c: Color, stroke: bool) -> &mut Self {
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

    pub fn fill_color(&mut self, c: Color) -> &mut Self {
        self.color(c, false)
    }

    pub fn stroke_color(&mut self, c: Color) -> &mut Self {
        self.color(c, true)
    }

    pub fn move_to(&mut self, x: f64, y: f64) -> &mut Self {
        self.n(x).n(y).op("m")
    }

    pub fn line_to(&mut self, x: f64, y: f64) -> &mut Self {
        self.n(x).n(y).op("l")
    }

    pub fn curve_to(&mut self, p: [f64; 6]) -> &mut Self {
        for v in p {
            self.n(v);
        }
        self.op("c")
    }

    pub fn rect(&mut self, r: Rect) -> &mut Self {
        self.n(r.x0).n(r.y0).n(r.width()).n(r.height()).op("re")
    }

    /// An ellipse inscribed in `r`.
    pub fn ellipse(&mut self, r: Rect) -> &mut Self {
        const K: f64 = 0.552_284_75;
        let (cx, cy) = ((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
        let (rx, ry) = (r.width() / 2.0, r.height() / 2.0);
        let (kx, ky) = (rx * K, ry * K);
        self.move_to(cx + rx, cy)
            .curve_to([cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry])
            .curve_to([cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy])
            .curve_to([cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry])
            .curve_to([cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy])
            .op("h")
    }

    pub fn fill(&mut self) -> &mut Self {
        self.op("f")
    }

    pub fn stroke(&mut self) -> &mut Self {
        self.op("S")
    }

    pub fn clip(&mut self) -> &mut Self {
        self.op("W n")
    }

    pub fn begin_text(&mut self, font: &str, size: f64) -> &mut Self {
        self.op("BT");
        self.name(font);
        self.n(size).op("Tf")
    }

    pub fn end_text(&mut self) -> &mut Self {
        self.op("ET")
    }

    pub fn td(&mut self, x: f64, y: f64) -> &mut Self {
        self.n(x).n(y).op("Td")
    }

    pub fn show(&mut self, encoded: &[u8]) -> &mut Self {
        self.string(encoded);
        self.op(" Tj")
    }

    pub fn name(&mut self, n: &str) -> &mut Self {
        let mut v = Vec::new();
        pagebara_syntax::write_object(&mut v, &pagebara_syntax::Object::name(n.as_bytes()));
        self.0.extend_from_slice(&v);
        self.0.push(b' ');
        self
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

    pub fn do_xobject(&mut self, name: &str) -> &mut Self {
        self.name(name);
        let _ = writeln!(self.0, "Do");
        self
    }
}
