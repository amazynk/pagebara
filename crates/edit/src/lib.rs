//! Editing what pages draw: their text, images, vector graphics, forms and shadings.
//!
//! [`parse`] reads a page into [`PageObjects`]: text blocks (lines of runs, grouped the
//! way text selection and copying group them), images (XObjects and inline), paths
//! (consecutive painting in one state is one object), form XObjects (with the objects
//! they draw as children) and shadings. Each object knows the content-stream bytes it
//! came from, its bounds in page space, the clip and marked content around it (tagged
//! PDF MCIDs, optional content). Ids are valid for that revision of the page; parsing it
//! again gives the same ids.
//!
//! Operations are pure functions on a [`Document`] (so they fit
//! `Session::edit`), or methods of an [`Editor`] to apply several in one step: delete,
//! transform (a `cm` or text matrix is prepended; nothing is re-encoded), colours,
//! opacity, painting order, duplicate, crop, replace, add and extract images, text size
//! and colours of whole runs, replace the text of a run or line and add new text.
//!
//! The writer changes only what an edit needs: untouched operators stay byte for byte as
//! they were (unchanged streams are not even rewritten), edited objects are wrapped in
//! `q … Q` with their changes or, inside text objects, given their own text state and
//! followed by a text position adjustment so the text after them stays put. Objects in a
//! form drawn elsewhere too are edited in a copy of the form for this page
//! ([`FormEdit`]). Marked content stays with its content; `/ActualText` of changed text
//! is updated or removed. Resources gain entries as needed and lose the ones an edit made
//! unused.
//!
//! # Text encoding
//!
//! New text goes through a [`TextEncoder`]: given the run's font (or none, for new text)
//! and a string, it returns the bytes to show as `TJ` items, each glyph's width, and
//! optionally a different font for the writer to add to the page's resources:
//!
//! ```text
//! pub trait TextEncoder {
//!     fn encode(&self, doc: &mut Document, req: &EncodeRequest<'_>) -> Result<Encoded, EncodeError>;
//! }
//! pub struct EncodeRequest<'a> { pub text: &'a str, pub font: Option<FontSlot<'a>>, pub size: f32 }
//! pub struct FontSlot<'a> { pub name: Option<&'a [u8]>, pub object: &'a Object, pub font: &'a Font }
//! pub struct Encoded { pub font: FontChoice, pub show: Vec<ShowItem>, pub advances: Vec<f32> }
//! pub enum FontChoice { Same, New(Object) }
//! pub enum ShowItem { Text(Vec<u8>), Adjust(f32), Font(FontChoice) }
//! ```
//!
//! [`SimpleEncoder`] maps characters to codes the font's encoding already has (failing
//! with [`EncodeError::Missing`] otherwise) and sets new text in standard Helvetica.
//! [`TypesetEncoder`] (built on `pagebara-typeset`, through [`Fonts`]) keeps the run's font,
//! adds codes for glyphs its program has, switches to an embedded fallback subset for the
//! rest (`ShowItem::Font`) and kerns with `TJ` adjustments. Fonts other objects share are
//! copied before they change.
//!
//! # Paragraphs
//!
//! [`block_text`] reads a text block as styled paragraphs in an inferred box (alignment,
//! width, first-line indent, leading, paragraph breaks, joined hyphenations), merging
//! blocks that continue the paragraph. [`layout_block`] lays styled text out in that box
//! (for previews: lines, glyph quads, carets, outlines), and [`Editor::edit_block`] writes it
//! in place of the block's runs: at the first run's place inside its marked content, the
//! text state restored after it, everything else byte for byte as it was; `/ActualText`
//! and structure elements' alternate text follow. [`Editor::add_text_box`] adds new
//! paragraphs. [`restyle_range`] and [`StyleChange`] make a range bold, italic, another
//! size, colour or family (a real face of the family when there is one, else emulated).
//!
//! ```standalone_crate,no_run
//! # fn f(doc: &mut pagebara_syntax::Document) -> Result<(), pagebara_edit::Error> {
//! use pagebara_edit::{Target, SimpleEncoder};
//! let objs = pagebara_edit::parse(doc, 0)?;
//! let run = objs.runs.iter().find(|r| r.text.contains("Draft")).unwrap().id;
//! pagebara_edit::replace_text(doc, 0, Target::Run(run), "Final", &SimpleEncoder)?;
//! # Ok(()) }
//! ```

#![forbid(unsafe_code)]

mod assemble;
mod edit;
mod emit;
mod encode;
mod fonts;
pub mod geom;
pub mod hit;
mod image;
mod inline;
mod model;
mod paragraph;
mod rich;
mod scan;
mod tagged;
mod write;

pub use pagebara_content::{Matrix, Point, Rect};
use pagebara_syntax::Document;

pub use edit::{Editor, NewText, TextStyle, ZOrder};
pub use encode::{
    EncodeError, EncodeRequest, Encoded, FontChoice, FontSlot, ShowItem, SimpleEncoder,
    TextEncoder, helvetica,
};
pub use fonts::{Fonts, GlyphPath, PathVerb, SpanFont, TypesetEncoder};
pub use geom::PageFrame;
pub use image::{ImageFit, NewImage, embed as embed_image, image_size, placement_in};
pub use model::{
    Color, FontInfo, FormInfo, Glyph, ImageInfo, Kind, MarkedContent, ObjectData, ObjectId,
    PageObject, PageObjects, Paint, PathInfo, RunId, ShadingInfo, Span, StreamId, Target,
    TextBlock, TextLine, TextRun,
};
pub use pagebara_typeset::{Align, Synthetic};
pub use paragraph::{
    BlockLayout, BlockReport, BlockText, Caret, LayoutGlyph, LayoutLine, PreviewGlyph, SpanStyle,
    StyledText, TextFrame, block_text, layout_block, merge, paint_of, run_style,
    set_default_language,
};
pub use rich::{
    StyleChange, font_families, restyle, restyle_plain, restyle_range, slice, style_at,
};
pub use write::FormEdit;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("there is no page {0}")]
    NoSuchPage(usize),
    #[error("not a page")]
    NotAPage,
    #[error("no object {0:?} on the page")]
    NoSuchObject(Target),
    #[error("the document does not allow changes")]
    NotPermitted,
    #[error("not supported: {0}")]
    Unsupported(&'static str),
    #[error("edits overlap")]
    Conflict,
    #[error(transparent)]
    Encode(#[from] EncodeError),
    #[error("image: {0}")]
    Image(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Text in Unicode normalization form C, as text is laid out and encoded.
pub fn nfc_string(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    s.nfc().collect()
}

/// The objects of page `page`.
pub fn parse(doc: &Document, page: usize) -> Result<PageObjects> {
    Ok(edit::parse_page(doc, page, false)?.model)
}

fn with(
    doc: &mut Document,
    page: usize,
    f: impl FnOnce(&mut Editor<'_>) -> Result<()>,
) -> Result<()> {
    let mut e = Editor::open(doc, page)?;
    f(&mut e)?;
    e.commit()
}

pub fn delete(doc: &mut Document, page: usize, targets: &[Target]) -> Result<()> {
    with(doc, page, |e| e.delete(targets))
}

/// Moves, scales or rotates by a page-space matrix.
pub fn transform(doc: &mut Document, page: usize, targets: &[Target], m: Matrix) -> Result<()> {
    with(doc, page, |e| e.transform(targets, m))
}

pub fn set_fill(doc: &mut Document, page: usize, targets: &[Target], c: Color) -> Result<()> {
    with(doc, page, |e| e.set_fill(targets, c))
}

pub fn set_stroke(doc: &mut Document, page: usize, targets: &[Target], c: Color) -> Result<()> {
    with(doc, page, |e| e.set_stroke(targets, c))
}

pub fn set_opacity(
    doc: &mut Document,
    page: usize,
    targets: &[Target],
    fill: Option<f32>,
    stroke: Option<f32>,
) -> Result<()> {
    with(doc, page, |e| e.set_opacity(targets, fill, stroke))
}

pub fn set_line(
    doc: &mut Document,
    page: usize,
    targets: &[Target],
    width: Option<f32>,
    dash: Option<&[f32]>,
) -> Result<()> {
    with(doc, page, |e| e.set_line(targets, width, dash))
}

pub fn reorder(doc: &mut Document, page: usize, targets: &[Target], z: ZOrder) -> Result<()> {
    with(doc, page, |e| e.reorder(targets, z))
}

pub fn duplicate(
    doc: &mut Document,
    page: usize,
    targets: &[Target],
    offset: Matrix,
) -> Result<()> {
    with(doc, page, |e| e.duplicate(targets, offset))
}

/// Clips objects (usually images) to a page-space rectangle.
pub fn crop(doc: &mut Document, page: usize, targets: &[Target], rect: Rect) -> Result<()> {
    with(doc, page, |e| e.crop(targets, rect))
}

pub fn replace_image(
    doc: &mut Document,
    page: usize,
    target: Target,
    img: &NewImage<'_>,
    fit: ImageFit,
) -> Result<()> {
    with(doc, page, |e| e.replace_image(target, img, fit))
}

/// Adds an image drawn by `placement` (its unit square to page space).
pub fn add_image(
    doc: &mut Document,
    page: usize,
    img: &NewImage<'_>,
    placement: Matrix,
) -> Result<()> {
    with(doc, page, |e| e.add_image(img, placement))
}

pub fn set_text_style(
    doc: &mut Document,
    page: usize,
    targets: &[Target],
    s: TextStyle,
) -> Result<()> {
    with(doc, page, |e| e.set_text_style(targets, s))
}

pub fn replace_text(
    doc: &mut Document,
    page: usize,
    run: Target,
    text: &str,
    enc: &dyn TextEncoder,
) -> Result<()> {
    with(doc, page, |e| e.replace_text(run, text, enc))
}

pub fn replace_line(
    doc: &mut Document,
    page: usize,
    block: ObjectId,
    line: usize,
    text: &str,
    enc: &dyn TextEncoder,
) -> Result<()> {
    with(doc, page, |e| e.replace_line(block, line, text, enc))
}

pub fn add_text(
    doc: &mut Document,
    page: usize,
    t: &NewText<'_>,
    enc: &dyn TextEncoder,
) -> Result<()> {
    with(doc, page, |e| e.add_text(t, enc))
}

/// Writes every content stream of a page anew without changes (for testing the writer).
#[doc(hidden)]
pub fn rewrite_all(doc: &mut Document, page: usize) -> Result<()> {
    let p = edit::parse_page(doc, page, false)?;
    let mut ch = write::Changes {
        rewrite: true,
        ..Default::default()
    };
    for i in 0..p.scan.streams.len() {
        ch.patches.insert(StreamId(i as u32), Vec::new());
    }
    write::commit(doc, &p, ch, FormEdit::AllUses)
}

/// An image object as a PNG file at its own resolution.
pub fn extract_image(doc: &Document, page: usize, id: ObjectId) -> Result<Vec<u8>> {
    let p = edit::parse_page(doc, page, false)?;
    let ob = p
        .model
        .get(id)
        .ok_or(Error::NoSuchObject(Target::Object(id)))?;
    let ObjectData::Image(info) = &ob.data else {
        return Err(Error::Unsupported("not an image"));
    };
    let k = p.obj_items[id.0 as usize][0];
    let it = &p.scan.items[k];
    let rec = &p.scan.streams[it.stream.0 as usize];
    let stream = match info.object {
        Some(r) => doc
            .get(r)
            .and_then(|o| o.as_stream().cloned())
            .ok_or(Error::Unsupported("missing image"))?,
        None => {
            let span = it.span();
            let data = &rec.data[span.start as usize..span.end as usize];
            let mut cp = pagebara_content::ContentParser::new(data);
            let i = cp
                .next()
                .ok_or(Error::Unsupported("unreadable inline image"))?;
            inline::to_xobject(doc, &rec.res, &i)
        }
    };
    image::to_png(doc, &stream)
}
