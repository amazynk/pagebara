//! True redaction: what lies under a marked area is removed from the file, not covered.
//!
//! 1. **Mark** areas as `/Redact` annotations ([`add_mark`]), by hand or from search hits
//!    ([`find`]: text, regular expressions, e-mail addresses, phone numbers, IBANs, card
//!    numbers). Marks are ordinary annotations until applied.
//! 2. **Apply** them ([`apply`]): page content is rewritten so glyphs under a mark are gone
//!    (the remaining ones keep their exact positions), path geometry inside it is cut away,
//!    image pixels under it are overwritten in the image data (masks included), form
//!    XObjects meeting it are rewritten as a copy for that page only, and annotations and
//!    form fields over it are removed (field values cleared). The mark's fill and overlay
//!    text become page content.
//! 3. **Sanitise** ([`sanitize`]): metadata, scripts, attachments, hidden layers,
//!    thumbnails, XFA data and unused resources.
//! 4. **Save** with [`save`]: always a full rewrite without unreachable objects. An
//!    incremental save would keep the original content in the file.
//! 5. **Verify** the saved bytes ([`verify`]).
//!
//! ```standalone_crate,no_run
//! # fn f(bytes: Vec<u8>) -> Result<(), Box<dyn std::error::Error>> {
//! use pagebara_redact::{ApplyOptions, Pattern, Sanitize, add_mark, apply, find, save, sanitize, verify};
//! let mut doc = pagebara_syntax::Document::open(bytes)?;
//! let pages = pagebara_pages::PageTree::load(&doc);
//! for hit in find(&doc, &Pattern::text("Jane Doe"), None) {
//!     add_mark(&mut doc, pages.pages()[hit.page].r, &hit.mark())?;
//! }
//! let report = apply(&mut doc, &ApplyOptions::default())?;
//! sanitize(&mut doc, &Sanitize::default());
//! let out = save(&doc)?;
//! assert!(verify(&out, None, &report.regions(), &["Jane Doe"]).is_clean());
//! # Ok(()) }
//! ```
//!
//! Glyphs removed everywhere also lose their outlines and names in embedded TrueType, CFF,
//! Type 1 and Type 3 fonts (with their `/ToUnicode`, `/Differences`, `cmap` and `post`
//! entries), whether redaction or sanitising hidden layers removed them. Mesh shadings
//! lose the triangles and patches inside regions (cut at the region's boundary), sampled
//! functions of function-based shadings and of meshes the samples shown only under
//! regions, tiling patterns whose cells hold content shown only under a region are replaced
//! by copies without it, and [`scrub`] replaces the redacted words in bookmarks,
//! destination names, metadata and other strings.
//!
//! Other smooth shadings are clipped: axial and radial ones, and function-based ones with
//! a PostScript function, hold no samples, only colours computed from the position. (A
//! sampled function of an axial or radial shading keeps its samples: each colour spans a
//! band across the shading.)
//!
//! Limits: annotation appearances outside regions are kept, and text in a font that cannot
//! be read is removed by an estimate of its extent.

mod apply;
mod emit;
mod fonts;
mod geom;
mod image;
mod mark;
mod overlay;
mod path;
mod prune;
mod rewrite;
mod sanitize;
mod scrub;
mod search;
mod shading;
mod tiling;
mod tounicode;
mod verify;

pub use apply::{ApplyOptions, GLYPH_OVERLAP, PageReport, Report, apply, apply_marks};
pub use geom::{Matrix, Point, Rect, Region};
pub use mark::{Align, Mark, add_mark, marks, read as read_mark, remove_mark};
pub use overlay::OVERLAY_TAG;
pub use rewrite::Stats;
pub use sanitize::{Sanitize, SanitizeReport, sanitize};
pub use scrub::{REPLACEMENT, scrub};
pub use search::{Hit, Kind, Pattern, find, iban_valid, luhn_valid, matches};
pub use verify::{AnnotationLeak, ImageLeak, Location, SecretHit, TextLeak, VerifyReport, verify};

use pagebara_syntax::{Document, SaveOptions};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a page object")]
    NotAPage,
    #[error("page {0} does not exist")]
    NoSuchPage(usize),
    #[error("annotation not found")]
    NotFound,
    #[error("a mark needs at least one area")]
    EmptyMark,
    #[error(
        "the document's security settings do not allow changing it; the owner password grants it"
    )]
    NotPermitted,
    #[error(transparent)]
    Syntax(#[from] pagebara_syntax::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Saves a redacted document: a full rewrite holding only objects still in use, so neither
/// earlier revisions nor the replaced content remain. `doc` itself is unchanged.
pub fn save(doc: &Document) -> Result<Vec<u8>> {
    let mut d = doc.clone();
    pagebara_pages::compact(&mut d);
    Ok(d.save_full_with(&SaveOptions::compact())?)
}

/// Applies all marks, sanitises and saves in one go.
pub fn redact(
    doc: &Document,
    opts: &ApplyOptions,
    clean: &Sanitize,
) -> Result<(Vec<u8>, Report, SanitizeReport)> {
    let mut d = doc.clone();
    let report = apply(&mut d, opts)?;
    let s = sanitize(&mut d, clean);
    Ok((save(&d)?, report, s))
}
