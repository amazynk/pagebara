//! PDF/A-1b, 2b, 2u, 3b and 3u: validation and conversion.

mod convert;
mod fonts;
mod validate;

pub use convert::{Conversion, ConvertError, convert};
pub(crate) use convert::{embed_missing_fonts, sync_metadata};
pub use validate::validate;

/// The catalog's XMP packet.
pub(crate) fn metadata_of(doc: &pagebara_syntax::Document) -> Option<Vec<u8>> {
    validate::metadata(doc).map(|m| m.1)
}

pub(crate) fn info_of_doc(doc: &pagebara_syntax::Document) -> crate::xmp::Info {
    validate::info_of(doc)
}
