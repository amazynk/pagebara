//! Text encoded for a content stream: codes per font, advances and kerning.

use std::fmt::Write as _;
use std::ops::Range;

/// A font opened in a [`FontSession`](crate::FontSession): an existing PDF font resource or
/// a font face added to the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FontId(pub usize);

/// A font resource the session creates (a subset of a system font, embedded as a Type 0
/// font with `Identity-H` encoding) when the fonts asked for lack characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FallbackId(pub usize);

/// The font resource a run of glyphs is shown with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RunFont {
    /// The PDF font that was opened (its dictionary may gain codes, see
    /// [`FontSession::finish`](crate::FontSession::finish)).
    Font(FontId),
    /// A new font resource; its object is known after `finish`.
    Fallback(FallbackId),
}

/// One glyph of encoded text.
#[derive(Clone, Debug, PartialEq)]
pub struct EncodedGlyph {
    /// The character code as shown in the string operand (big-endian, `code_len` bytes).
    pub code: u32,
    pub code_len: u8,
    /// Byte range of the characters this glyph stands for in the encoded text.
    pub cluster: Range<usize>,
    /// Horizontal advance in text space for a font size of 1 (the PDF width / 1000).
    pub advance: f32,
    /// Kerning between this glyph and the next one in the run, in the same units
    /// (negative moves the next glyph closer). Written as a `TJ` adjustment of
    /// `-kern * 1000`.
    pub kern: f32,
    /// A single-byte code 32, so word spacing (`Tw`) applies to it.
    pub word_space: bool,
}

impl EncodedGlyph {
    /// The code's bytes.
    pub fn bytes(&self) -> impl Iterator<Item = u8> + '_ {
        let n = u32::from(self.code_len);
        (0..n).map(move |i| (self.code >> (8 * (n - 1 - i))) as u8)
    }
}

/// Consecutive glyphs in one font.
#[derive(Clone, Debug, PartialEq)]
pub struct EncodedRun {
    pub font: RunFont,
    pub glyphs: Vec<EncodedGlyph>,
}

impl EncodedRun {
    /// The string operand's bytes (all codes, no kerning).
    pub fn bytes(&self) -> Vec<u8> {
        self.glyphs.iter().flat_map(|g| g.bytes()).collect()
    }

    /// Width for a font size of 1, kerning included.
    pub fn width(&self) -> f32 {
        let n = self.glyphs.len();
        self.glyphs
            .iter()
            .enumerate()
            .map(|(i, g)| g.advance + if i + 1 < n { g.kern } else { 0.0 })
            .sum()
    }

    /// The `TJ` array operand (`[<...> -12 <...>]`) with kerning, without the operator.
    pub fn tj_operand(&self) -> String {
        tj_array(&self.glyphs, |_| 0.0)
    }
}

/// Writes a `TJ` array for `glyphs`: hex strings, with `-(kern + extra(i)) * 1000` after
/// glyph `i` where non-zero. `extra` is in the same units as [`EncodedGlyph::kern`].
pub fn tj_array(glyphs: &[EncodedGlyph], mut extra: impl FnMut(usize) -> f32) -> String {
    let mut s = String::from("[<");
    let n = glyphs.len();
    for (i, g) in glyphs.iter().enumerate() {
        for b in g.bytes() {
            let _ = write!(s, "{b:02X}");
        }
        let adj = if i + 1 < n { g.kern } else { 0.0 } + extra(i);
        let tj = -adj * 1000.0;
        if tj.abs() >= 0.0005 && i + 1 < n {
            s.push('>');
            s.push_str(&fmt_num(tj));
            s.push('<');
        }
    }
    s.push_str(">]");
    s
}

/// A number for a content stream: at most 3 decimals, no trailing zeros.
pub fn fmt_num(v: f32) -> String {
    let r = (f64::from(v) * 1000.0).round() / 1000.0;
    if r == r.trunc() {
        format!("{}", r as i64)
    } else {
        let s = format!("{r:.3}");
        s.trim_end_matches('0').to_string()
    }
}

/// How one character of the input was (or would be) encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CharStatus {
    /// The font already has a code for it, with a glyph and a width.
    Existing,
    /// The font's program has the glyph; a code (and width, and `/ToUnicode` entry) is added
    /// to the font.
    Added,
    /// Shown with a fallback font resource.
    Fallback,
    /// No font available has it (or it is a control character); it is left out.
    Unsupported,
}

/// Coverage of one character.
#[derive(Clone, Debug, PartialEq)]
pub struct CharCoverage {
    /// Byte offset in the (NFC-normalized) text.
    pub offset: usize,
    pub ch: char,
    pub status: CharStatus,
}

/// Text encoded in one or more fonts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Encoded {
    /// The text the clusters index (the input, normalized to NFC).
    pub text: String,
    pub runs: Vec<EncodedRun>,
    /// Characters left out (byte offset in `text`, character).
    pub missing: Vec<(usize, char)>,
}

impl Encoded {
    /// Total width for a font size of 1 (kerning inside runs included).
    pub fn width(&self) -> f32 {
        self.runs.iter().map(EncodedRun::width).sum()
    }

    /// Whether every character was encoded.
    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }

    /// All glyphs in order with their run's font.
    pub fn glyphs(&self) -> impl Iterator<Item = (RunFont, &EncodedGlyph)> {
        self.runs
            .iter()
            .flat_map(|r| r.glyphs.iter().map(move |g| (r.font, g)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(code: u32, len: u8, adv: f32, kern: f32) -> EncodedGlyph {
        EncodedGlyph {
            code,
            code_len: len,
            cluster: 0..1,
            advance: adv,
            kern,
            word_space: false,
        }
    }

    #[test]
    fn tj_with_kerning() {
        let r = EncodedRun {
            font: RunFont::Font(FontId(0)),
            glyphs: vec![
                g(0x41, 1, 0.6, -0.05),
                g(0x56, 1, 0.6, 0.0),
                g(0x0102, 2, 0.5, 0.1),
            ],
        };
        assert_eq!(r.bytes(), [0x41, 0x56, 0x01, 0x02]);
        assert_eq!(r.tj_operand(), "[<41>50<560102>]");
        assert!((r.width() - 1.65).abs() < 1e-6);
    }

    #[test]
    fn numbers() {
        assert_eq!(fmt_num(12.0), "12");
        assert_eq!(fmt_num(-0.5), "-0.5");
        assert_eq!(fmt_num(1.23456), "1.235");
    }
}
