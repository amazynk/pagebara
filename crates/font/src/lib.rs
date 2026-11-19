//! Font programs (ISO 32000-2 §9.6–9.9): TrueType/OpenType (and collections), CFF and CFF2,
//! and Type 1. Parsers never panic on malformed data; they return `None` or skip the
//! broken part.
//!
//! - [`Font`] parses any program and draws glyph outlines into an [`OutlineSink`] in
//!   glyph space, with [`Font::font_matrix`] mapping glyph space to text space.
//! - [`PdfFont`] applies the PDF glyph selection rules (simple font encodings, symbolic
//!   TrueType cmaps, `CIDToGIDMap`, CFF charsets) so a renderer can go from character
//!   codes (or CIDs) to glyphs, outlines and a Unicode guess.
//! - [`fallback`] picks system fonts for fonts that are not embedded.
//! - [`subset`] subsets TrueType and CFF fonts and writes Type 0 / Identity-H fonts.

pub mod cff;
mod data;
pub mod fallback;
mod font;
pub mod outline;
pub mod pdf;
mod reader;
pub mod sfnt;
pub mod subset;
pub mod type1;

pub use data::{BaseEncoding, name_to_char, name_to_unicode, unicode_to_name};
pub use font::{Font, Format};
pub use outline::{Matrix, OutlineSink, Path, Verb};
pub use pdf::{CidToGid, FontSpec, PdfFont, PdfFontType, SimpleEncoding};
