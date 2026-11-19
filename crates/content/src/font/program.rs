//! A hook for what only the font program knows: its built-in encoding, glyph indices, glyph
//! widths and Unicode from its own tables. A [`ProgramLoader`] supplies it; the
//! [`Interpreter`](crate::Interpreter) uses [`embedded_programs`] (backed by `pagebara-font`)
//! unless given another one with
//! [`Interpreter::with_font_programs`](crate::Interpreter::with_font_programs).

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::{Arc, Mutex, OnceLock};

use pagebara_font::PdfFont;
use pagebara_font::pdf::{embedded_program, font_descriptor, font_file_ref, spec_from_dict};
use pagebara_syntax::{Dict, Document, ObjRef};

/// Facts of an embedded font program. Every method may decline.
pub trait FontProgram: Send + Sync + Debug {
    /// The glyph name the program's built-in encoding gives a code of a simple font (a Type 1
    /// `/Encoding`, a CFF encoding and charset, a TrueType `post` name).
    fn glyph_name(&self, _code: u32) -> Option<&[u8]> {
        None
    }

    /// The glyph index the program draws for a code of a simple font or a CID of a composite
    /// font.
    fn glyph_id(&self, _code: u32) -> Option<u32> {
        None
    }

    /// Appends the Unicode text the program's own tables give a code of a simple font or a
    /// CID of a composite font (a TrueType `cmap`, glyph names); false if unknown.
    fn unicode(&self, _code: u32, _out: &mut String) -> bool {
        false
    }

    /// The advance width of a code of a simple font in thousandths of text space, for fonts
    /// without `/Widths`.
    fn width(&self, _code: u32) -> Option<f32> {
        None
    }
}

/// Reads the program of a font dictionary, called once per font when it is loaded.
pub type ProgramLoader = dyn Fn(&Document, &Dict) -> Option<Arc<dyn FontProgram>> + Send + Sync;

/// A loader reading embedded programs (`/FontFile`, `/FontFile2`, `/FontFile3`) with
/// `pagebara-font`. A program is parsed when first asked for something the font dictionaries
/// leave out, and once per program object, so use one loader per document.
pub fn embedded_programs() -> Arc<ProgramLoader> {
    let cache: Arc<Mutex<Cache>> = Arc::default();
    Arc::new(move |doc, dict| {
        let fd = font_descriptor(doc, dict)?;
        if ![&b"FontFile"[..], b"FontFile2", b"FontFile3"]
            .iter()
            .any(|k| fd.contains(k))
        {
            return None;
        }
        Some(Arc::new(Embedded {
            doc: doc.clone(),
            dict: dict.clone(),
            cache: Arc::clone(&cache),
            font: OnceLock::new(),
        }) as Arc<dyn FontProgram>)
    })
}

/// Parsed programs by the object holding them.
type Cache = HashMap<ObjRef, Option<Arc<pagebara_font::Font>>>;

/// An embedded program with the PDF glyph selection rules applied, parsed on first use.
#[derive(Debug)]
struct Embedded {
    doc: Document,
    dict: Dict,
    cache: Arc<Mutex<Cache>>,
    font: OnceLock<Option<PdfFont>>,
}

impl Embedded {
    fn get(&self) -> Option<&PdfFont> {
        self.font
            .get_or_init(|| {
                let (doc, dict) = (&self.doc, &self.dict);
                let spec = spec_from_dict(doc, dict)?;
                let parse = || embedded_program(doc, dict).map(Arc::new);
                let font = match font_file_ref(doc, dict) {
                    Some(r) => self
                        .cache
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .entry(r)
                        .or_insert_with(parse)
                        .clone(),
                    None => parse(),
                }?;
                Some(PdfFont::new(font, spec))
            })
            .as_ref()
    }
}

impl FontProgram for Embedded {
    fn glyph_name(&self, code: u32) -> Option<&[u8]> {
        let f = self.get()?.font();
        let name = f.glyph_name(f.builtin_glyph(u8::try_from(code).ok()?)?)?;
        (name != ".notdef").then_some(name.as_bytes())
    }

    fn glyph_id(&self, code: u32) -> Option<u32> {
        Some(self.get()?.glyph(code))
            .filter(|&g| g != 0)
            .map(u32::from)
    }

    fn unicode(&self, code: u32, out: &mut String) -> bool {
        let Some(f) = self.get() else { return false };
        match f.unicode(code) {
            Some(u) if !u.is_empty() && u != "\u{FFFD}" => out.push_str(&u),
            // An unnamed, unmapped glyph that draws nothing but advances is a space (as in
            // subsets that renumber codes and drop the cmap).
            _ if is_blank(f, code) => out.push(' '),
            _ => return false,
        }
        true
    }

    fn width(&self, code: u32) -> Option<f32> {
        let f = self.get()?;
        if f.glyph(code) == 0 {
            return None;
        }
        f.advance(code).map(|w| w * 1000.0)
    }
}

fn is_blank(f: &PdfFont, code: u32) -> bool {
    let mut path = pagebara_font::Path::new();
    f.glyph(code) != 0
        && f.outline(code, &mut path)
        && path.is_empty()
        && f.advance(code).is_some_and(|w| w > 0.0)
}
