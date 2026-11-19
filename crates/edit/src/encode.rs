//! Turning Unicode text into the bytes a font shows: the [`TextEncoder`] trait and
//! [`SimpleEncoder`], which uses what a font's encoding already covers.

use std::collections::HashMap;

use pagebara_content::font::Font;
use pagebara_syntax::{Dict, Document, Object};

/// The font a run uses, as the encoder sees it.
#[derive(Clone, Copy, Debug)]
pub struct FontSlot<'a> {
    /// The resource name, when the run selects the font with `Tf`.
    pub name: Option<&'a [u8]>,
    /// The font object, usually a reference.
    pub object: &'a Object,
    pub font: &'a Font,
}

/// What to encode.
#[derive(Clone, Copy, Debug)]
pub struct EncodeRequest<'a> {
    pub text: &'a str,
    /// The font the text is to be set in: the edited run's, or `None` for new text where
    /// the encoder picks one.
    pub font: Option<FontSlot<'a>>,
    /// Font size in text space units, for encoders that adjust spacing.
    pub size: f32,
}

/// The font the encoded text is shown with.
#[derive(Clone, Debug, PartialEq)]
pub enum FontChoice {
    /// The requested font, unchanged.
    Same,
    /// Another font (a font dictionary or a reference to one the encoder added to the
    /// document); the writer adds it to the resources and selects it for the text.
    New(Object),
}

/// An element of a `TJ` array.
#[derive(Clone, Debug, PartialEq)]
pub enum ShowItem {
    /// Encoded codes, shown as a string.
    Text(Vec<u8>),
    /// A position adjustment in thousandths of text space (positive moves back).
    Adjust(f32),
    /// The text after it is in another font (at the same size): `Same` returns to the
    /// font of [`Encoded::font`].
    Font(FontChoice),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Encoded {
    pub font: FontChoice,
    pub show: Vec<ShowItem>,
    /// Each glyph's width in thousandths of text space (glyph space for 1000-unit fonts).
    pub advances: Vec<f32>,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum EncodeError {
    /// Characters the font has no glyph or code for.
    #[error("the font cannot show {0:?}")]
    Missing(String),
    #[error("{0}")]
    Other(String),
}

/// Encodes text for a font. The writer calls it for replaced and new text; it may add
/// objects to the document (an embedded fallback font, glyphs added to a subset) and
/// return a new font for the writer to use instead.
///
/// [`SimpleEncoder`] needs no font changes: it only maps characters to codes the font
/// already has.
pub trait TextEncoder {
    fn encode(&self, doc: &mut Document, req: &EncodeRequest<'_>) -> Result<Encoded, EncodeError>;
}

/// Encodes with the codes a font's encoding already maps to Unicode (simple fonts, and
/// composite fonts with two-byte codes), checking that embedded subsets have the glyphs.
/// New text without a font is set in standard Helvetica (WinAnsi).
#[derive(Clone, Copy, Debug, Default)]
pub struct SimpleEncoder;

/// The standard Helvetica font dictionary with WinAnsi encoding.
pub fn helvetica() -> Dict {
    let mut d = Dict::new();
    d.set(b"Type", Object::name(b"Font"));
    d.set(b"Subtype", Object::name(b"Type1"));
    d.set(b"BaseFont", Object::name(b"Helvetica"));
    d.set(b"Encoding", Object::name(b"WinAnsiEncoding"));
    d
}

impl TextEncoder for SimpleEncoder {
    fn encode(&self, doc: &mut Document, req: &EncodeRequest<'_>) -> Result<Encoded, EncodeError> {
        let Some(slot) = req.font else {
            let mut bytes = Vec::new();
            let mut missing = String::new();
            for c in req.text.chars() {
                match pagebara_annot::Helvetica::code(c) {
                    Some(b) => bytes.push(b),
                    None => missing.push(c),
                }
            }
            if !missing.is_empty() {
                return Err(EncodeError::Missing(missing));
            }
            let dict = helvetica();
            let font = Font::load(doc, &dict);
            let advances = bytes
                .iter()
                .map(|&b| font.width(b.into()) * 1000.0)
                .collect();
            return Ok(Encoded {
                font: FontChoice::New(Object::Dict(dict)),
                show: vec![ShowItem::Text(bytes)],
                advances,
            });
        };
        let map = reverse_map(doc, slot.font);
        let mut bytes = Vec::new();
        let mut advances = Vec::new();
        let mut missing = String::new();
        let mut buf = [0u8; 4];
        for c in req.text.chars() {
            let s: &str = c.encode_utf8(&mut buf);
            let code = map.get(s).or_else(|| {
                // A no-break space shows as a space.
                (c == '\u{a0}').then(|| map.get(" ")).flatten()
            });
            match code {
                Some(b) => {
                    let (code, _) = slot.font.next_code(b);
                    advances.push(slot.font.width(code) * 1000.0);
                    bytes.extend_from_slice(b);
                }
                None => missing.push(c),
            }
        }
        if !missing.is_empty() {
            return Err(EncodeError::Missing(missing));
        }
        Ok(Encoded {
            font: FontChoice::Same,
            show: vec![ShowItem::Text(bytes)],
            advances,
        })
    }
}

/// Text of each code the font can show, to the code's bytes (the lowest code wins).
fn reverse_map(doc: &Document, font: &Font) -> HashMap<String, Vec<u8>> {
    let mut m: HashMap<String, Vec<u8>> = HashMap::new();
    let has_glyph = |code: u32| match &font.program {
        Some(_) => font.glyph_id(code).is_some_and(|g| g != 0),
        None => true,
    };
    let mut s = String::new();
    if !font.is_composite() {
        for code in 0..=255u32 {
            s.clear();
            if font.unicode(doc, code, &mut s) && !s.is_empty() && has_glyph(code) {
                m.entry(s.clone()).or_insert_with(|| vec![code as u8]);
            }
        }
        return m;
    }
    for code in 0..=0xFFFFu32 {
        let b = [(code >> 8) as u8, code as u8];
        if font.next_code(&b).1 != 2 {
            continue;
        }
        s.clear();
        if font.unicode(doc, code, &mut s)
            && !s.is_empty()
            && font.cid(code) != 0
            && has_glyph(code)
        {
            m.entry(s.clone()).or_insert_with(|| b.to_vec());
        }
    }
    m
}
