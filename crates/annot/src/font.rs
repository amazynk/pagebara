//! Fonts for generated text (free text, stamps, popups): the [`TextFont`] interface, the
//! built-in [`Helvetica`] (a standard 14 font with WinAnsiEncoding, never embedded), and line
//! breaking. [`EmbeddedFont`](crate::EmbeddedFont) covers other scripts.

use std::fmt::Debug;

use pagebara_syntax::{Dict, Document, Object};

/// One character as a font shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Glyph {
    /// The character code, written big-endian in `len` bytes.
    pub code: u32,
    pub len: u8,
    /// Advance width at size 1.
    pub width: f64,
    /// The face showing it, for fonts made of several (see [`TextFont::face_name`]).
    pub face: u8,
}

/// A font appearance streams can show text in. Helvetica covers WinAnsi;
/// [`EmbeddedFont`](crate::EmbeddedFont) embeds Type 0 (Identity-H) faces for any script.
pub trait TextFont: Debug {
    /// The name under `/Font` in resources and in `/DA` (of face 0).
    fn resource_name(&self) -> &str;
    /// The family name for rich-text styles (`/DS`, `/RC`).
    fn family(&self) -> &str;
    /// How the font shows `c`; `None` when it cannot.
    fn glyph(&self, c: char) -> Option<Glyph>;
    /// How the font shows `c` in the contextual presentation form `form` (Arabic joining);
    /// `None` shows the [`glyph`](Self::glyph) of `c`.
    fn presentation_glyph(&self, _c: char, _form: char) -> Option<Glyph> {
        None
    }
    /// What is shown for characters the font lacks.
    fn fallback(&self) -> Glyph;
    /// Ascent above the baseline at size 1.
    fn ascent(&self) -> f64;
    /// The font for resources (a dictionary or a reference to one), made once the text is
    /// laid out: `used` lists every character shown with its glyph, so an embedded font
    /// can subset and build its `/ToUnicode` map.
    fn resource(&self, doc: &mut Document, used: &[(char, Glyph)]) -> Object;
    /// The resource name of face `face` (fonts with several faces).
    fn face_name(&self, face: u8) -> String {
        match face {
            0 => self.resource_name().to_owned(),
            n => format!("{}_{n}", self.resource_name()),
        }
    }
    /// Every face's font for resources, by name: by default the [`resource`](Self::resource)
    /// of face 0.
    fn resources(&self, doc: &mut Document, used: &[(char, Glyph)]) -> Vec<(String, Object)> {
        vec![(self.face_name(0), self.resource(doc, used))]
    }
}

/// A laid-out run of text, in logical order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Line(pub Vec<(char, Glyph)>);

impl Line {
    /// Width at size 1.
    pub fn width(&self) -> f64 {
        self.0.iter().map(|(_, g)| g.width).sum()
    }

    /// The codes as string-operand bytes.
    #[cfg(test)]
    pub fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (_, g) in &self.0 {
            let be = g.code.to_be_bytes();
            out.extend_from_slice(&be[4 - usize::from(g.len.clamp(1, 4))..]);
        }
        out
    }

    /// The codes in visual order (right-to-left runs reversed by the Unicode bidirectional
    /// algorithm), split where the face changes.
    pub fn runs(&self) -> Vec<(u8, Vec<u8>)> {
        let mut out: Vec<(u8, Vec<u8>)> = Vec::new();
        for (_, g) in crate::shape::visual_order(&self.0) {
            let be = g.code.to_be_bytes();
            let bytes = &be[4 - usize::from(g.len.clamp(1, 4))..];
            match out.last_mut() {
                Some((f, b)) if *f == g.face => b.extend_from_slice(bytes),
                _ => out.push((g.face, bytes.to_vec())),
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Text laid out in a font, with the characters it could not show.
#[derive(Debug, Default)]
pub struct Shaper {
    /// Distinct characters shown with the fallback glyph, in order of appearance.
    pub missing: Vec<char>,
}

impl Shaper {
    pub fn line(&mut self, font: &dyn TextFont, s: &str) -> Line {
        let chars: Vec<char> = s.chars().filter(|c| !c.is_control()).collect();
        let forms = crate::shape::arabic_forms(&chars);
        Line(
            chars
                .iter()
                .zip(forms)
                .map(|(&c, form)| {
                    let g = form
                        .and_then(|f| font.presentation_glyph(c, f))
                        .or_else(|| font.glyph(c))
                        .unwrap_or_else(|| {
                            if !self.missing.contains(&c) {
                                self.missing.push(c);
                            }
                            font.fallback()
                        });
                    (c, g)
                })
                .collect(),
        )
    }

    /// Breaks `text` into lines no wider than `max` at `size`: at spaces when possible,
    /// inside a word when it alone is too wide, and at every newline.
    pub fn wrap(&mut self, font: &dyn TextFont, text: &str, size: f64, max: f64) -> Vec<Line> {
        let mut lines = Vec::new();
        let fits = |l: &Line| l.width() * size <= max;
        let space = self.line(font, " ");
        for para in text.split('\n') {
            let mut line = Line::default();
            for word in para.trim_end_matches('\r').split(' ') {
                let word = self.line(font, word);
                let mut candidate = line.clone();
                if !candidate.is_empty() {
                    candidate.0.extend_from_slice(&space.0);
                }
                candidate.0.extend_from_slice(&word.0);
                if fits(&candidate) || line.is_empty() && word.is_empty() {
                    line = candidate;
                    continue;
                }
                if !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                }
                for g in word.0 {
                    line.0.push(g);
                    if !fits(&line) && line.0.len() > 1 {
                        line.0.pop();
                        lines.push(std::mem::replace(&mut line, Line(vec![g])));
                    }
                }
            }
            lines.push(line);
        }
        lines
    }
}

/// The characters of `text` that `font` cannot show, in order of appearance.
pub fn unsupported_chars(font: &dyn TextFont, text: &str) -> Vec<char> {
    let mut s = Shaper::default();
    s.line(font, text);
    s.missing
}

/// Widths of codes 32..=255 in thousandths of the font size; 0 marks an unused code.
#[rustfmt::skip]
const WIDTHS: [u16; 224] = [
    // 32..=126
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278,
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556,
    1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778,
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556,
    333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556,
    556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584, 0,
    // 128..=159
    556, 0, 222, 556, 333, 1000, 556, 556, 333, 1000, 667, 333, 1000, 0, 611, 0,
    0, 222, 222, 333, 333, 350, 556, 1000, 333, 1000, 500, 333, 944, 0, 500, 667,
    // 160..=255
    278, 333, 556, 556, 556, 556, 260, 556, 333, 737, 370, 556, 584, 333, 737, 333,
    400, 584, 333, 333, 333, 556, 537, 278, 333, 333, 365, 556, 834, 834, 834, 611,
    667, 667, 667, 667, 667, 667, 1000, 722, 667, 667, 667, 667, 278, 278, 278, 278,
    722, 722, 778, 778, 778, 778, 778, 584, 778, 722, 722, 722, 722, 667, 667, 611,
    556, 556, 556, 556, 556, 556, 889, 500, 556, 556, 556, 556, 278, 278, 278, 278,
    556, 556, 556, 556, 556, 556, 556, 584, 611, 556, 556, 556, 556, 500, 556, 500,
];

/// Unicode code points of WinAnsi codes 128..=159 (0 where undefined).
const HIGH: [u16; 32] = [
    0x20AC, 0, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0, 0x017D, 0, 0, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC,
    0x2122, 0x0161, 0x203A, 0x0153, 0, 0x017E, 0x0178,
];

/// Helvetica with WinAnsiEncoding: Western European text only.
#[derive(Clone, Copy, Debug, Default)]
pub struct Helvetica;

impl Helvetica {
    /// The WinAnsi code of `c`.
    pub fn code(c: char) -> Option<u8> {
        let u = u32::from(c);
        match u {
            0x20..=0x7E | 0xA0..=0xFF => Some(u as u8),
            _ => HIGH
                .iter()
                .position(|&h| h != 0 && u32::from(h) == u)
                .map(|i| 0x80 + i as u8),
        }
    }

    fn glyph_of(code: u8) -> Glyph {
        let w = code.checked_sub(32).map_or(0, |i| WIDTHS[usize::from(i)]);
        Glyph {
            code: u32::from(code),
            len: 1,
            width: f64::from(w) / 1000.0,
            face: 0,
        }
    }
}

impl TextFont for Helvetica {
    fn resource_name(&self) -> &str {
        "Helv"
    }

    fn family(&self) -> &str {
        "Helvetica"
    }

    fn glyph(&self, c: char) -> Option<Glyph> {
        Helvetica::code(c).map(Helvetica::glyph_of)
    }

    fn fallback(&self) -> Glyph {
        Helvetica::glyph_of(b'?')
    }

    fn ascent(&self) -> f64 {
        0.718
    }

    fn resource(&self, _: &mut Document, _: &[(char, Glyph)]) -> Object {
        let mut d = Dict::new();
        d.set(b"Type", Object::name(b"Font"));
        d.set(b"Subtype", Object::name(b"Type1"));
        d.set(b"BaseFont", Object::name(b"Helvetica"));
        d.set(b"Encoding", Object::name(b"WinAnsiEncoding"));
        Object::Dict(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(s: &str) -> (Line, Vec<char>) {
        let mut sh = Shaper::default();
        let l = sh.line(&Helvetica, s);
        (l, sh.missing)
    }

    #[test]
    fn widths_and_encoding() {
        assert_eq!(
            line("Hello").0.width(),
            (722 + 556 + 222 + 222 + 556) as f64 / 1000.0
        );
        assert_eq!(line("€é—").0.bytes(), vec![0x80, 0xE9, 0x97]);
        let (l, missing) = line("ЖaЖ日");
        assert_eq!(l.bytes(), b"?a??");
        assert_eq!(missing, ['Ж', '日']);
        assert_eq!(line("Ä").0.width(), 0.667);
        assert_eq!(unsupported_chars(&Helvetica, "Grüße"), []);
    }

    #[test]
    fn wrapping() {
        let mut sh = Shaper::default();
        let l = sh.wrap(&Helvetica, "the quick brown fox", 10.0, 50.0);
        assert!(l.len() > 1);
        assert!(l.iter().all(|l| l.width() * 10.0 <= 50.0));
        let l = sh.wrap(&Helvetica, "a\nb", 10.0, 100.0);
        assert_eq!(
            l.iter().map(Line::bytes).collect::<Vec<_>>(),
            [b"a".to_vec(), b"b".to_vec()]
        );
        assert!(sh.wrap(&Helvetica, "WWWWWWWWWW", 10.0, 30.0).len() >= 3);
    }

    #[test]
    fn two_byte_codes() {
        let g = Glyph {
            code: 0x0102,
            len: 2,
            width: 0.5,
            face: 0,
        };
        assert_eq!(Line(vec![('x', g)]).bytes(), [1, 2]);
    }
}
