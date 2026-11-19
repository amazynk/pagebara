//! The display list: a flat vector of small commands referring to shared tables of paths,
//! transforms, paints and glyph runs. Coordinates are user space; each command names the
//! transform to page space, so a list replays at any zoom or tile without reinterpreting.

use std::sync::Arc;

use pagebara_syntax::{Dict, Name, Object, Stream};

use crate::font::Font;
use crate::geom::{Matrix, Point, Rect};

/// Index into one of the list's tables.
pub type Id = u32;
/// "No entry" for optional [`Id`]s.
pub const NONE: Id = Id::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cmd {
    Fill {
        path: Id,
        ctm: Id,
        paint: Id,
        rule: FillRule,
    },
    Stroke {
        path: Id,
        ctm: Id,
        paint: Id,
        style: Id,
    },
    /// Intersects the clip with a path until the matching [`Cmd::PopClip`].
    Clip {
        path: Id,
        ctm: Id,
        rule: FillRule,
    },
    /// Intersects the clip with the glyph outlines of runs `start..end` (text modes 4–7).
    ClipText {
        start: Id,
        end: Id,
    },
    /// Ends the innermost `count` clips.
    PopClip {
        count: u32,
    },
    Text {
        run: Id,
    },
    /// An image XObject or inline image drawn into the unit square of `ctm`. `paint` is the
    /// fill used by image masks and the alpha, blend mode and soft mask of the image.
    Image {
        image: Id,
        ctm: Id,
        paint: Id,
    },
    /// `sh`: a shading filling the current clip.
    Shading {
        shading: Id,
        ctm: Id,
        paint: Id,
    },
    /// A transparency group (a form XObject with `/Group`) begins.
    BeginGroup {
        group: Id,
    },
    EndGroup,
    BeginMarked {
        marked: Id,
    },
    EndMarked,
}

impl Cmd {
    /// A short name for statistics.
    pub fn kind(&self) -> &'static str {
        match self {
            Cmd::Fill { .. } => "fill",
            Cmd::Stroke { .. } => "stroke",
            Cmd::Clip { .. } => "clip",
            Cmd::ClipText { .. } => "clip-text",
            Cmd::PopClip { .. } => "pop-clip",
            Cmd::Text { .. } => "text",
            Cmd::Image { .. } => "image",
            Cmd::Shading { .. } => "shading",
            Cmd::BeginGroup { .. } => "begin-group",
            Cmd::EndGroup => "end-group",
            Cmd::BeginMarked { .. } => "begin-marked",
            Cmd::EndMarked => "end-marked",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Verb {
    Move,
    Line,
    Cubic,
    Close,
}

impl Verb {
    /// Points consumed by the verb.
    pub fn points(self) -> usize {
        match self {
            Verb::Move | Verb::Line => 1,
            Verb::Cubic => 3,
            Verb::Close => 0,
        }
    }
}

/// A path as ranges of the verb and point tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathSpan {
    pub verbs: (u32, u32),
    pub points: (u32, u32),
}

/// A borrowed path.
#[derive(Clone, Copy, Debug)]
pub struct Path<'a> {
    pub verbs: &'a [Verb],
    pub points: &'a [Point],
}

impl Path<'_> {
    /// Control-point bounds (`None` for an empty path).
    pub fn bounds(&self) -> Option<Rect> {
        (!self.points.is_empty()).then(|| Rect::from_points(self.points))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorSpace {
    DeviceGray,
    DeviceRgb,
    DeviceCmyk,
    /// Entry of [`DisplayList::color_spaces`].
    Other(Id),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorSpaceKind {
    CalGray,
    CalRgb,
    Lab,
    IccBased,
    Indexed,
    Separation,
    DeviceN,
    Pattern,
    /// Unrecognised: treated as having `components` gray-like components.
    Unknown,
}

/// A colour space other than the device ones, kept as its PDF object for conversion.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorSpaceEntry {
    pub kind: ColorSpaceKind,
    pub components: u8,
    /// The base of `Indexed`, the underlying space of `Pattern`, or the alternate space of
    /// `ICCBased`, `Separation` and `DeviceN`.
    pub base: Option<ColorSpace>,
    /// The colour space object, references unresolved.
    pub object: Object,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl BlendMode {
    pub fn from_name(n: &[u8]) -> Option<Self> {
        use BlendMode::*;
        Some(match n {
            b"Normal" | b"Compatible" => Normal,
            b"Multiply" => Multiply,
            b"Screen" => Screen,
            b"Overlay" => Overlay,
            b"Darken" => Darken,
            b"Lighten" => Lighten,
            b"ColorDodge" => ColorDodge,
            b"ColorBurn" => ColorBurn,
            b"HardLight" => HardLight,
            b"SoftLight" => SoftLight,
            b"Difference" => Difference,
            b"Exclusion" => Exclusion,
            b"Hue" => Hue,
            b"Saturation" => Saturation,
            b"Color" => Color,
            b"Luminosity" => Luminosity,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum RenderingIntent {
    AbsoluteColorimetric,
    #[default]
    RelativeColorimetric,
    Saturation,
    Perceptual,
}

impl RenderingIntent {
    pub fn from_name(n: &[u8]) -> Self {
        match n {
            b"AbsoluteColorimetric" => RenderingIntent::AbsoluteColorimetric,
            b"Saturation" => RenderingIntent::Saturation,
            b"Perceptual" => RenderingIntent::Perceptual,
            _ => RenderingIntent::RelativeColorimetric,
        }
    }
}

/// How something is painted: colour, pattern and compositing state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Paint {
    pub space: ColorSpace,
    /// Components `start..end` of [`DisplayList::components`].
    pub components: (u32, u32),
    /// Entry of [`DisplayList::patterns`] when painting with a pattern.
    pub pattern: Id,
    pub alpha: f32,
    pub blend: BlendMode,
    /// Entry of [`DisplayList::soft_masks`].
    pub soft_mask: Id,
    pub overprint: bool,
    pub overprint_mode: u8,
    pub intent: RenderingIntent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LineCap {
    Butt,
    Round,
    Square,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LineJoin {
    Miter,
    Round,
    Bevel,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StrokeStyle {
    /// In user space; 0 is the thinnest line the device can draw.
    pub width: f32,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter_limit: f32,
    /// Dash lengths `start..end` of [`DisplayList::dashes`]; empty for solid lines.
    pub dash: (u32, u32),
    pub dash_phase: f32,
    pub stroke_adjust: bool,
}

/// A pattern colour: the pattern object and the matrix from pattern space to page space
/// (the pattern's `/Matrix` followed by the default space of its content stream).
#[derive(Clone, Debug, PartialEq)]
pub struct PatternRef {
    pub object: Object,
    pub matrix: Matrix,
}

/// A soft mask from `/SMask` of an ExtGState, with the CTM in effect when it was set.
#[derive(Clone, Debug, PartialEq)]
pub struct SoftMask {
    /// The soft mask dictionary (`/S`, `/G`, `/BC`, `/TR`).
    pub object: Object,
    pub ctm: Matrix,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ImageSource {
    /// An image XObject, usually a reference.
    XObject(Object),
    /// An inline image with its dictionary in full key and value names, and the colour
    /// space resolved from resources. Decode with [`pagebara_syntax::Document::decode`].
    Inline(Arc<Stream>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub source: ImageSource,
    pub width: u32,
    pub height: u32,
    /// A stencil mask painted with the fill paint.
    pub is_mask: bool,
}

/// A transparency group: a form XObject with `/Group`. Contents follow until
/// [`Cmd::EndGroup`] and composite as one with `paint`'s alpha, blend mode and soft mask.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    /// The form's bounding box in form space, mapped to page space by `ctm`.
    pub bbox: Rect,
    pub ctm: Id,
    pub isolated: bool,
    pub knockout: bool,
    pub color_space: Option<ColorSpace>,
    pub paint: Id,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Marked {
    pub tag: Name,
    /// The property list, inline or from `/Properties` (references unresolved).
    pub properties: Option<Dict>,
}

impl Marked {
    pub fn mcid(&self) -> Option<i64> {
        self.properties.as_ref()?.int(b"MCID")
    }
}

/// A font used by the list.
#[derive(Clone, Debug)]
pub struct FontRef {
    /// The resource name it was selected by.
    pub name: Name,
    /// The font object, usually a reference.
    pub object: Object,
    pub font: Arc<Font>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TextMode {
    Fill,
    Stroke,
    FillStroke,
    Invisible,
    FillClip,
    StrokeClip,
    FillStrokeClip,
    Clip,
}

impl TextMode {
    pub fn from_int(v: i64) -> Self {
        use TextMode::*;
        [
            Fill,
            Stroke,
            FillStroke,
            Invisible,
            FillClip,
            StrokeClip,
            FillStrokeClip,
            Clip,
        ][v.clamp(0, 7) as usize]
    }

    pub fn fills(self) -> bool {
        matches!(
            self,
            TextMode::Fill | TextMode::FillStroke | TextMode::FillClip | TextMode::FillStrokeClip
        )
    }

    pub fn strokes(self) -> bool {
        matches!(
            self,
            TextMode::Stroke
                | TextMode::FillStroke
                | TextMode::StrokeClip
                | TextMode::FillStrokeClip
        )
    }

    pub fn clips(self) -> bool {
        self as u8 >= 4
    }
}

/// One shown glyph. Its origin is at `(x, y)` in the run's text space, relative to the
/// run's text matrix.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glyph {
    pub code: u32,
    pub x: f32,
    pub y: f32,
    /// Horizontal glyph width in text space (font width × size × scaling), for bounds.
    pub width: f32,
}

/// The glyphs of one text-showing operator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextRun {
    pub font: Id,
    pub size: f32,
    /// `Tz / 100`.
    pub hscale: f32,
    pub rise: f32,
    /// Text matrix at the start of the run, then the CTM.
    pub matrix: Matrix,
    pub ctm: Id,
    pub mode: TextMode,
    pub fill: Id,
    pub stroke: Id,
    pub style: Id,
    /// Glyphs `start..end` of [`DisplayList::glyphs`].
    pub glyphs: (u32, u32),
    pub vertical: bool,
}

impl TextRun {
    /// Maps text space units of a glyph scaled by font size (a glyph's outline after the
    /// font matrix) to page space.
    pub fn glyph_matrix(&self, g: &Glyph) -> Matrix {
        Matrix::new(
            self.size * self.hscale,
            0.0,
            0.0,
            self.size,
            g.x,
            g.y + self.rise,
        )
        .then(&self.matrix)
    }
}

/// `d0` or `d1` of a Type 3 glyph description.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphMetrics {
    pub width: Point,
    /// Set by `d1`: the glyph is a mask painted with the text's colour.
    pub bbox: Option<Rect>,
}

#[derive(Clone, Debug, Default)]
pub struct DisplayList {
    pub cmds: Vec<Cmd>,
    pub verbs: Vec<Verb>,
    pub points: Vec<Point>,
    pub paths: Vec<PathSpan>,
    pub matrices: Vec<Matrix>,
    pub paints: Vec<Paint>,
    pub components: Vec<f32>,
    pub strokes: Vec<StrokeStyle>,
    pub dashes: Vec<f32>,
    pub color_spaces: Vec<ColorSpaceEntry>,
    pub patterns: Vec<PatternRef>,
    pub soft_masks: Vec<SoftMask>,
    pub images: Vec<Image>,
    pub shadings: Vec<Object>,
    pub groups: Vec<Group>,
    pub marked: Vec<Marked>,
    pub fonts: Vec<FontRef>,
    pub runs: Vec<TextRun>,
    pub glyphs: Vec<Glyph>,
    /// Set for Type 3 glyph descriptions.
    pub glyph_metrics: Option<GlyphMetrics>,
}

impl DisplayList {
    pub fn path(&self, id: Id) -> Path<'_> {
        let s = self.paths[id as usize];
        Path {
            verbs: &self.verbs[s.verbs.0 as usize..s.verbs.1 as usize],
            points: &self.points[s.points.0 as usize..s.points.1 as usize],
        }
    }

    pub fn matrix(&self, id: Id) -> Matrix {
        self.matrices[id as usize]
    }

    pub fn paint(&self, id: Id) -> &Paint {
        &self.paints[id as usize]
    }

    pub fn paint_components(&self, p: &Paint) -> &[f32] {
        &self.components[p.components.0 as usize..p.components.1 as usize]
    }

    pub fn dash(&self, s: &StrokeStyle) -> &[f32] {
        &self.dashes[s.dash.0 as usize..s.dash.1 as usize]
    }

    pub fn run_glyphs(&self, r: &TextRun) -> &[Glyph] {
        &self.glyphs[r.glyphs.0 as usize..r.glyphs.1 as usize]
    }

    /// Approximate heap size in bytes.
    pub fn heap_size(&self) -> usize {
        use std::mem::size_of_val;
        size_of_val(&self.cmds[..])
            + size_of_val(&self.verbs[..])
            + size_of_val(&self.points[..])
            + size_of_val(&self.paths[..])
            + size_of_val(&self.matrices[..])
            + size_of_val(&self.paints[..])
            + size_of_val(&self.components[..])
            + size_of_val(&self.strokes[..])
            + size_of_val(&self.glyphs[..])
            + size_of_val(&self.runs[..])
    }

    /// Releases spare capacity after building.
    pub fn shrink_to_fit(&mut self) {
        self.cmds.shrink_to_fit();
        self.verbs.shrink_to_fit();
        self.points.shrink_to_fit();
        self.paths.shrink_to_fit();
        self.matrices.shrink_to_fit();
        self.paints.shrink_to_fit();
        self.components.shrink_to_fit();
        self.glyphs.shrink_to_fit();
        self.runs.shrink_to_fit();
    }
}
