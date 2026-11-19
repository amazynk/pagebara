//! Default appearance strings (`/DA`, ISO 32000-2 §12.7.4.3).

use pagebara_annot::Color;
use pagebara_syntax::lexer::{Lexer, Token};

/// What a `/DA` string sets: font resource, size (0 = auto) and text colour.
#[derive(Clone, Debug, PartialEq)]
pub struct DefaultAppearance {
    pub font: Option<String>,
    pub size: f64,
    pub color: Color,
}

impl Default for DefaultAppearance {
    fn default() -> Self {
        DefaultAppearance {
            font: None,
            size: 0.0,
            color: Color::BLACK,
        }
    }
}

impl DefaultAppearance {
    /// Parses the operators of `da`; the last `Tf` and colour operator win.
    pub fn parse(da: &[u8]) -> DefaultAppearance {
        let mut out = DefaultAppearance::default();
        let mut lex = Lexer::new(da, 0);
        let mut nums: Vec<f64> = Vec::new();
        let mut name: Option<String> = None;
        loop {
            match lex.next_token() {
                Token::Eof => break,
                Token::Int(i) => nums.push(i as f64),
                Token::Real(r) => nums.push(r),
                Token::Name(n) => {
                    name = Some(crate::text::name_text(n.as_bytes()));
                    nums.clear();
                }
                Token::Keyword(op) => {
                    let c = |v: f64| v.clamp(0.0, 1.0) as f32;
                    match (op, nums.as_slice()) {
                        (b"Tf", [.., size]) => {
                            out.font = name.take();
                            out.size = size.abs();
                        }
                        (b"g", [.., g]) => out.color = Color::Gray(c(*g)),
                        (b"rg", [.., r, g, b]) => out.color = Color::Rgb(c(*r), c(*g), c(*b)),
                        (b"k", [.., cc, m, y, k]) => {
                            out.color = Color::Cmyk(c(*cc), c(*m), c(*y), c(*k))
                        }
                        _ => {}
                    }
                    nums.clear();
                }
                _ => nums.clear(),
            }
        }
        out
    }

    /// Serialises as a `/DA` string.
    pub fn to_string_with(&self, font: &str) -> String {
        let comps: Vec<String> = self
            .color
            .components()
            .iter()
            .map(|v| crate::content::fmt_num(f64::from(*v)))
            .collect();
        let op = match self.color {
            Color::Gray(_) => "g",
            Color::Rgb(..) => "rg",
            Color::Cmyk(..) => "k",
        };
        format!(
            "/{font} {} Tf {} {op}",
            crate::content::fmt_num(self.size),
            comps.join(" ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        let d = DefaultAppearance::parse(b"/Helv 0 Tf 0 g");
        assert_eq!(d.font.as_deref(), Some("Helv"));
        assert_eq!(d.size, 0.0);
        assert_eq!(d.color, Color::Gray(0.0));
        let d = DefaultAppearance::parse(b"0.2 0.4 1 rg /F1#20x 11.5 Tf");
        assert_eq!(d.font.as_deref(), Some("F1 x"));
        assert_eq!(d.size, 11.5);
        assert_eq!(d.color, Color::Rgb(0.2, 0.4, 1.0));
        let d = DefaultAppearance::parse(b"garbage (x) Tj");
        assert_eq!(d, DefaultAppearance::default());
        assert_eq!(
            DefaultAppearance::parse(b"/ZaDb 12 Tf 1 0 0 0 k").to_string_with("ZaDb"),
            "/ZaDb 12 Tf 1 0 0 0 k"
        );
    }
}
