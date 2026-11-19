//! Text engine for editing PDFs.
//!
//! - **Fonts** ([`FontSession`]): encode new text in a document's existing font resources.
//!   Characters the font shows already keep their codes; glyphs its program has but its
//!   encoding lacks get new codes (`/Differences`, CIDs with `/W` and `CIDToGIDMap`);
//!   anything else goes to a fallback font (the family if installed, else a similar or
//!   metric-compatible face) embedded as a subset. Every new code gets a `/ToUnicode` entry,
//!   so text never renders one way and extracts another. The result ([`Encoded`]) gives
//!   the string bytes, advances in text space and kerning as `TJ` adjustments.
//! - **Layout** ([`layout`]): paragraphs of styled spans broken into lines (UAX #14
//!   essentials, CJK rules, optional hyphenation in several languages), aligned or justified (word
//!   spacing via `Tw` or `TJ`, optional character spacing), reordered per UAX #9 for
//!   right-to-left text; and [`infer_paragraph`] to recover a paragraph's box, alignment,
//!   indent and leading from its existing lines so an edit reflows like the original.
//! - **Style** ([`font_style`], [`find_variant`]): family, weight, slant and class of a PDF
//!   font, and bold/italic variants (a real face, or [`Synthetic`] styling).
//!
//! Units: advances and kerning are in text space for a font size of 1 (a PDF glyph width
//! divided by 1000); layout positions are in points (text space units at the span sizes).
//!
//! A typical edit:
//!
//! ```standalone_crate,no_run
//! use std::sync::Arc;
//! use pagebara_typeset::{FontSession, RunFont, WriteMode};
//! # fn edit(doc: &mut pagebara_syntax::Document, font: pagebara_syntax::ObjRef) {
//! let db = Arc::new(pagebara_font::fallback::FontDb::system());
//! let mut session = FontSession::new(Some(db));
//! let id = session.open_font(doc, font).unwrap();
//! let enc = session.encode(id, "Grüße, Ωμέγα");
//! let mut ops = String::new();
//! for run in &enc.runs {
//!     // The page's name for the font; fallbacks get new names when added below.
//!     let name = match run.font {
//!         RunFont::Font(_) => "F1".to_string(),
//!         RunFont::Fallback(f) => format!("FB{}", f.0),
//!     };
//!     ops += &format!("/{name} 12 Tf {} TJ\n", run.tj_operand());
//! }
//! let changes = session.finish(doc, WriteMode::CopyIfShared);
//! // Add `changes.fallbacks` to the page's /Font resources (and repoint F1 if
//! // `changes.fonts` says the font was copied), then write `ops` inside BT/ET.
//! # }
//! ```

#![forbid(unsafe_code)]

mod cmap;
mod encoded;
pub mod hyphen;
mod infer;
mod kern;
mod layout;
pub mod linebreak;
mod pdffont;
mod session;
mod style;

pub use encoded::{
    CharCoverage, CharStatus, Encoded, EncodedGlyph, EncodedRun, FallbackId, FontId, RunFont,
    fmt_num, tj_array,
};
pub use infer::{InferredParagraph, LineSample, infer_paragraph};
pub use layout::{
    Align, Color, Fragment, Layout, LayoutWarning, Line, LineHeight, ParagraphStyle, PlacedGlyph,
    Shaper, Span, layout, write_content,
};
pub use session::{
    Changes, EncodeOptions, FontChange, FontSession, WriteMode, references, unique_resource_name,
};
pub use style::{
    FontClass, FontStyle, Synthetic, Variant, VariantSource, find_variant, font_style,
    installed_family, same_family, standard_font_dict, standard_name,
};

use std::sync::{Arc, OnceLock};

use pagebara_font::fallback::FontDb;
use pagebara_syntax::{Dict, Object, Stream};

/// The installed fonts, scanned once per process (on first use) and shared.
pub fn system_fonts() -> Arc<FontDb> {
    static DB: OnceLock<Arc<FontDb>> = OnceLock::new();
    DB.get_or_init(|| Arc::new(FontDb::system())).clone()
}

/// Why a font cannot be opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The object is not a font dictionary.
    NotAFont,
    /// The face is not a TrueType/OpenType font that may be embedded.
    Unembeddable,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Error::NotAFont => "not a font dictionary",
            Error::Unembeddable => "the font cannot be embedded",
        })
    }
}

impl std::error::Error for Error {}

pub(crate) fn num(v: f32) -> Object {
    let r = v.round();
    if (v - r).abs() < 1e-3 {
        Object::Int(r as i64)
    } else {
        Object::Real(f64::from((v * 1000.0).round() / 1000.0))
    }
}

pub(crate) fn flate_stream(mut dict: Dict, data: &[u8]) -> Object {
    let z = miniz_oxide::deflate::compress_to_vec_zlib(data, 6);
    dict.set(b"Filter", Object::name(b"FlateDecode"));
    Object::Stream(Stream::new(dict, z))
}
