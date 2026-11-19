//! The editable objects of a page.

use std::collections::HashMap;
use std::sync::Arc;

use pagebara_content::font::Font;
use pagebara_content::{Matrix, Point, Rect};
use pagebara_syntax::{Dict, Name, ObjRef, Object};

use crate::geom::PageFrame;

/// An object of one page revision: its index in [`PageObjects::objects`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId(pub u32);

/// A text run (the glyphs of one text-showing operator): its index in
/// [`PageObjects::runs`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RunId(pub u32);

/// What an operation acts on: a whole object, or one text run of a text block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    Object(ObjectId),
    Run(RunId),
}

impl From<ObjectId> for Target {
    fn from(id: ObjectId) -> Self {
        Target::Object(id)
    }
}

impl From<RunId> for Target {
    fn from(id: RunId) -> Self {
        Target::Run(id)
    }
}

/// A content stream the page draws: the page's contents (all its streams joined) or a
/// form XObject's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StreamId(pub u32);

/// Bytes `start..end` of a decoded content stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub stream: StreamId,
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Image,
    Path,
    Form,
    Shading,
}

/// One editable object.
#[derive(Clone, Debug)]
pub struct PageObject {
    pub id: ObjectId,
    pub kind: Kind,
    pub data: ObjectData,
    /// Bounds in default user space (page space).
    pub bbox: Rect,
    /// The object's own frame mapped to page space (for rotated handles): for images and
    /// forms their unit square or box, otherwise the corners of `bbox`.
    pub quad: [Point; 4],
    /// The form object this object is drawn by, if any.
    pub parent: Option<ObjectId>,
    /// Bounds of the clip in effect, when something clips it.
    pub clip: Option<Rect>,
    /// Enclosing marked content, outermost first.
    pub marked: Vec<MarkedContent>,
    /// Inside optional content that the default configuration hides.
    pub hidden: bool,
    /// Where the object is written, in paint order.
    pub spans: Vec<Span>,
}

impl PageObject {
    pub fn text(&self) -> Option<&TextBlock> {
        match &self.data {
            ObjectData::Text(t) => Some(t),
            _ => None,
        }
    }

    pub fn image(&self) -> Option<&ImageInfo> {
        match &self.data {
            ObjectData::Image(i) => Some(i),
            _ => None,
        }
    }

    pub fn path(&self) -> Option<&PathInfo> {
        match &self.data {
            ObjectData::Path(p) => Some(p),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum ObjectData {
    Text(TextBlock),
    Image(ImageInfo),
    Path(PathInfo),
    Form(FormInfo),
    Shading(ShadingInfo),
}

/// A marked-content sequence around an object.
#[derive(Clone, Debug, PartialEq)]
pub struct MarkedContent {
    pub tag: Name,
    pub mcid: Option<i64>,
    /// Optional content membership (`/OC` sequences): the group or membership object.
    pub optional_content: Option<Object>,
    /// The property list, inline or from `/Properties` (references unresolved).
    pub properties: Option<Dict>,
}

/// A text block: lines of runs, grouped the way text selection and copying group them.
#[derive(Clone, Debug, Default)]
pub struct TextBlock {
    pub lines: Vec<TextLine>,
    /// The block's text, lines joined by `\n`.
    pub text: String,
}

impl TextBlock {
    pub fn runs(&self) -> impl Iterator<Item = RunId> + '_ {
        self.lines.iter().flat_map(|l| l.runs.iter().copied())
    }
}

#[derive(Clone, Debug, Default)]
pub struct TextLine {
    /// Runs in reading order.
    pub runs: Vec<RunId>,
    pub bbox: Rect,
    pub text: String,
}

/// A colour as written: its space and components.
#[derive(Clone, Debug, PartialEq)]
pub struct Paint {
    /// `DeviceGray`, `DeviceRGB`, `DeviceCMYK`, or the resource name of another space.
    pub space: Name,
    pub components: Vec<f32>,
    /// The pattern name, for pattern colours.
    pub pattern: Option<Name>,
}

impl Paint {
    pub fn black() -> Self {
        Paint {
            space: Name::new(b"DeviceGray"),
            components: vec![0.0],
            pattern: None,
        }
    }

    /// The colour as device RGB, for device colour spaces.
    pub fn rgb(&self) -> Option<[f32; 3]> {
        let c = &self.components;
        match (self.space.as_bytes(), c.len()) {
            (b"DeviceGray", 1) => Some([c[0]; 3]),
            (b"DeviceRGB", 3) => Some([c[0], c[1], c[2]]),
            (b"DeviceCMYK", 4) => {
                Some([c[0], c[1], c[2]].map(|v| (1.0 - (v + c[3]).min(1.0)).max(0.0)))
            }
            _ => None,
        }
    }
}

/// A device colour to paint with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Color {
    Gray(f32),
    Rgb(f32, f32, f32),
    Cmyk(f32, f32, f32, f32),
}

/// One shown glyph.
#[derive(Clone, Debug, PartialEq)]
pub struct Glyph {
    pub code: u32,
    /// The bytes of the code in the shown string.
    pub bytes: Vec<u8>,
    /// Its text (empty when an `/ActualText` gave the text to another glyph).
    pub text: String,
    /// The glyph's box in page space: origin, along the advance, up to the ascent.
    pub quad: [Point; 4],
    /// The origin in page space.
    pub origin: Point,
    /// The advance in text space (font width × size plus spacing, scaled).
    pub advance: f32,
}

/// A font a run uses.
#[derive(Clone, Debug)]
pub struct FontInfo {
    /// The `Tf` resource name (none when set by an ExtGState `/Font`).
    pub name: Option<Name>,
    /// The font object, usually a reference.
    pub object: Object,
    pub font: Arc<Font>,
}

/// The glyphs of one text-showing operator.
#[derive(Clone, Debug)]
pub struct TextRun {
    pub id: RunId,
    /// The text block it belongs to.
    pub block: ObjectId,
    pub font: Option<FontInfo>,
    pub size: f32,
    /// The text matrix where the run starts.
    pub text_matrix: Matrix,
    pub ctm: Matrix,
    pub char_spacing: f32,
    pub word_spacing: f32,
    /// `Tz / 100`.
    pub hscale: f32,
    pub rise: f32,
    /// `Tr`, 0 to 7.
    pub render_mode: u8,
    pub fill: Paint,
    pub stroke: Paint,
    pub fill_alpha: f32,
    pub stroke_alpha: f32,
    pub vertical: bool,
    pub glyphs: Vec<Glyph>,
    pub text: String,
    pub bbox: Rect,
    pub span: Span,
    pub parent: Option<ObjectId>,
    /// Enclosing marked content, outermost first.
    pub marked: Vec<MarkedContent>,
    pub hidden: bool,
}

impl TextRun {
    /// Text space at the run's start to page space.
    pub fn matrix(&self) -> Matrix {
        self.text_matrix.then(&self.ctm)
    }
}

#[derive(Clone, Debug)]
pub struct ImageInfo {
    /// The XObject resource name; `None` for an inline image.
    pub name: Option<Name>,
    pub object: Option<ObjRef>,
    pub width: u32,
    pub height: u32,
    pub bits_per_component: u32,
    /// The colour space as written (`None` for stencil masks).
    pub color_space: Option<Object>,
    /// A stencil mask painted with the fill colour.
    pub is_mask: bool,
    /// `/SMask` or `/Mask` present.
    pub has_soft_mask: bool,
    pub has_mask: bool,
    pub filter: Option<Name>,
    /// The unit square to page space.
    pub matrix: Matrix,
    pub fill: Paint,
    pub alpha: f32,
}

#[derive(Clone, Debug)]
pub struct PathInfo {
    /// Painting operators drawn (consecutive ones with the same state form one object).
    pub paints: u32,
    pub fill: Option<Paint>,
    pub stroke: Option<Paint>,
    pub line_width: f32,
    pub dashed: bool,
    pub ctm: Matrix,
    pub fill_alpha: f32,
    pub stroke_alpha: f32,
    /// The path also clips what follows.
    pub clips: bool,
    /// Subpaths in page space (curves as their control points), for hit testing.
    pub outline: Vec<Vec<Point>>,
}

#[derive(Clone, Debug)]
pub struct FormInfo {
    pub name: Name,
    pub object: Option<ObjRef>,
    /// The form's `/Matrix` then the CTM.
    pub matrix: Matrix,
    pub bbox: Option<Rect>,
    /// The stream with the form's content.
    pub stream: StreamId,
    /// Objects drawn by the form.
    pub children: Vec<ObjectId>,
    /// Times the page draws the form.
    pub uses_on_page: u32,
}

#[derive(Clone, Debug)]
pub struct ShadingInfo {
    pub name: Name,
    pub object: Option<Object>,
    pub ctm: Matrix,
}

/// The objects of one page revision. Ids are valid for this revision only: parsing the
/// same content again gives the same ids.
#[derive(Clone, Debug)]
pub struct PageObjects {
    pub page: usize,
    pub page_ref: ObjRef,
    pub frame: PageFrame,
    pub objects: Vec<PageObject>,
    pub runs: Vec<TextRun>,
    /// Content stream objects and forms the page was parsed from; changes when the page's
    /// content changes.
    pub fingerprint: u64,
    /// The document's language (the catalog's `/Lang`, a BCP 47 tag).
    pub lang: Option<String>,
    /// Languages of the page's marked content by MCID, from the structure tree.
    pub mcid_lang: HashMap<i64, String>,
}

impl PageObjects {
    pub fn get(&self, id: ObjectId) -> Option<&PageObject> {
        self.objects.get(id.0 as usize)
    }

    pub fn run(&self, id: RunId) -> Option<&TextRun> {
        self.runs.get(id.0 as usize)
    }

    /// Objects drawn by the page itself, not by forms.
    pub fn top_level(&self) -> impl Iterator<Item = &PageObject> {
        self.objects.iter().filter(|o| o.parent.is_none())
    }

    /// The bounds of a target.
    pub fn bbox(&self, t: Target) -> Option<Rect> {
        match t {
            Target::Object(o) => self.get(o).map(|o| o.bbox),
            Target::Run(r) => self.run(r).map(|r| r.bbox),
        }
    }
}
