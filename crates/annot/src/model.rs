//! The typed annotation model.

use pagebara_syntax::ObjRef;

use crate::geom::{Color, Point, Quad, Rect};
use crate::link::LinkAction;

/// Annotation flags (`/F`, ISO 32000-2 Table 167).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Flags(pub u32);

impl Flags {
    pub const INVISIBLE: Flags = Flags(1);
    pub const HIDDEN: Flags = Flags(1 << 1);
    pub const PRINT: Flags = Flags(1 << 2);
    pub const NO_ZOOM: Flags = Flags(1 << 3);
    pub const NO_ROTATE: Flags = Flags(1 << 4);
    pub const NO_VIEW: Flags = Flags(1 << 5);
    pub const READ_ONLY: Flags = Flags(1 << 6);
    pub const LOCKED: Flags = Flags(1 << 7);
    pub const TOGGLE_NO_VIEW: Flags = Flags(1 << 8);
    pub const LOCKED_CONTENTS: Flags = Flags(1 << 9);

    pub fn contains(self, f: Flags) -> bool {
        self.0 & f.0 == f.0
    }
}

impl std::ops::BitOr for Flags {
    type Output = Flags;
    fn bitor(self, o: Flags) -> Flags {
        Flags(self.0 | o.0)
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub enum BorderStyle {
    #[default]
    Solid,
    /// Dash and gap lengths.
    Dashed(Vec<f64>),
    Beveled,
    Inset,
    Underline,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Border {
    pub width: f64,
    pub style: BorderStyle,
}

impl Default for Border {
    fn default() -> Self {
        Border {
            width: 1.0,
            style: BorderStyle::Solid,
        }
    }
}

/// How a reply relates to the annotation in `/IRT`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ReplyType {
    #[default]
    Reply,
    /// Grouped with the parent rather than a comment on it.
    Group,
}

/// `/LE` line ending styles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LineEnding {
    #[default]
    None,
    Square,
    Circle,
    Diamond,
    OpenArrow,
    ClosedArrow,
    Butt,
    ROpenArrow,
    RClosedArrow,
    Slash,
}

impl LineEnding {
    const NAMES: [(LineEnding, &'static [u8]); 10] = [
        (LineEnding::None, b"None"),
        (LineEnding::Square, b"Square"),
        (LineEnding::Circle, b"Circle"),
        (LineEnding::Diamond, b"Diamond"),
        (LineEnding::OpenArrow, b"OpenArrow"),
        (LineEnding::ClosedArrow, b"ClosedArrow"),
        (LineEnding::Butt, b"Butt"),
        (LineEnding::ROpenArrow, b"ROpenArrow"),
        (LineEnding::RClosedArrow, b"RClosedArrow"),
        (LineEnding::Slash, b"Slash"),
    ];

    pub fn name(self) -> &'static [u8] {
        Self::NAMES
            .iter()
            .find(|(e, _)| *e == self)
            .map_or(b"None", |(_, n)| n)
    }

    pub fn from_name(n: &[u8]) -> LineEnding {
        Self::NAMES
            .iter()
            .find(|(_, name)| *name == n)
            .map_or(LineEnding::None, |(e, _)| *e)
    }

    /// Endings with an inside, which the interior colour (`/IC`) fills.
    pub fn is_closed(self) -> bool {
        matches!(
            self,
            LineEnding::Square
                | LineEnding::Circle
                | LineEnding::Diamond
                | LineEnding::ClosedArrow
                | LineEnding::RClosedArrow
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// The four text markup annotations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkupKind {
    Highlight,
    Underline,
    StrikeOut,
    Squiggly,
}

/// What an annotation is, with its type-specific data.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// A sticky note; `icon` is the `/Name` (Note, Comment, Key, Help, …).
    Text {
        icon: String,
        open: bool,
    },
    /// A link. URI and GoTo targets are written; others are read and kept as they are.
    /// `quads` optionally narrows the active area to text runs inside the rectangle.
    Link {
        action: Option<LinkAction>,
        quads: Vec<Quad>,
    },
    FreeText {
        font_size: f64,
        text_color: Color,
        align: Align,
    },
    Line {
        start: Point,
        end: Point,
        endings: [LineEnding; 2],
        interior: Option<Color>,
    },
    Square {
        interior: Option<Color>,
    },
    Circle {
        interior: Option<Color>,
    },
    Polygon {
        vertices: Vec<Point>,
        interior: Option<Color>,
    },
    PolyLine {
        vertices: Vec<Point>,
        endings: [LineEnding; 2],
        /// Fill of closed endings.
        interior: Option<Color>,
    },
    Markup {
        kind: MarkupKind,
        quads: Vec<Quad>,
    },
    /// Freehand strokes.
    Ink {
        strokes: Vec<Vec<Point>>,
    },
    /// A rubber stamp; `icon` is the `/Name`. Image stamps (placed signatures) keep the
    /// appearance they were created with.
    Stamp {
        icon: Option<String>,
    },
    Popup {
        parent: Option<ObjRef>,
        open: bool,
    },
    /// A form field's widget: read only here.
    Widget {
        field_type: Option<String>,
        field_name: Option<String>,
    },
    /// Any other subtype, read only.
    Other(String),
}

impl Kind {
    pub fn subtype(&self) -> &str {
        match self {
            Kind::Text { .. } => "Text",
            Kind::Link { .. } => "Link",
            Kind::FreeText { .. } => "FreeText",
            Kind::Line { .. } => "Line",
            Kind::Square { .. } => "Square",
            Kind::Circle { .. } => "Circle",
            Kind::Polygon { .. } => "Polygon",
            Kind::PolyLine { .. } => "PolyLine",
            Kind::Markup { kind, .. } => match kind {
                MarkupKind::Highlight => "Highlight",
                MarkupKind::Underline => "Underline",
                MarkupKind::StrikeOut => "StrikeOut",
                MarkupKind::Squiggly => "Squiggly",
            },
            Kind::Ink { .. } => "Ink",
            Kind::Stamp { .. } => "Stamp",
            Kind::Popup { .. } => "Popup",
            Kind::Widget { .. } => "Widget",
            Kind::Other(s) => s,
        }
    }

    /// Kinds this crate can write.
    pub fn is_editable(&self) -> bool {
        !matches!(self, Kind::Widget { .. } | Kind::Other(_))
    }
}

/// An annotation with the fields common to all kinds.
#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    /// Where it is stored; `None` for one not yet added (or held directly in `/Annots`).
    pub id: Option<ObjRef>,
    pub kind: Kind,
    pub rect: Rect,
    pub contents: Option<String>,
    /// `/T`.
    pub author: Option<String>,
    pub subject: Option<String>,
    /// `/NM`, unique on the page.
    pub name: Option<String>,
    /// `/M` and `/CreationDate` as PDF date strings.
    pub modified: Option<String>,
    pub created: Option<String>,
    /// `/C`: stroke colour for most kinds, the background of free text.
    pub color: Option<Color>,
    /// `/CA`.
    pub opacity: f64,
    pub border: Border,
    pub flags: Flags,
    pub in_reply_to: Option<ObjRef>,
    pub reply_type: ReplyType,
    pub popup: Option<ObjRef>,
    /// `/Rotate`: the page rotation (degrees clockwise) that free text and stamps are drawn
    /// upright for. `None` follows the page's `/Rotate` when written.
    pub rotate: Option<u16>,
}

impl Annotation {
    /// A printable annotation of `kind` with defaults suited to it. The rectangle of kinds
    /// with geometry (lines, ink, markup, polygons) is computed from it when added.
    pub fn new(kind: Kind, rect: Rect) -> Self {
        let (color, width) = match &kind {
            Kind::Markup {
                kind: MarkupKind::Highlight,
                ..
            } => (Some(Color::YELLOW), 1.0),
            Kind::Markup { .. } => (Some(Color::RED), 1.0),
            Kind::Text { .. } => (Some(Color::YELLOW), 1.0),
            Kind::FreeText { .. } | Kind::Stamp { .. } | Kind::Link { .. } => (None, 0.0),
            Kind::Ink { .. } => (Some(Color::BLUE), 2.0),
            _ => (Some(Color::RED), 1.0),
        };
        let flags = match kind {
            Kind::Text { .. } => Flags::PRINT | Flags::NO_ZOOM | Flags::NO_ROTATE,
            _ => Flags::PRINT,
        };
        Annotation {
            id: None,
            kind,
            rect,
            contents: None,
            author: None,
            subject: None,
            name: None,
            modified: None,
            created: None,
            color,
            opacity: 1.0,
            border: Border {
                width,
                style: BorderStyle::Solid,
            },
            flags,
            in_reply_to: None,
            reply_type: ReplyType::Reply,
            popup: None,
            rotate: None,
        }
    }

    /// A line from `start` to `end` with a filled arrow head at `end`, all in `color`.
    pub fn arrow(start: Point, end: Point, color: Color, width: f64) -> Self {
        let mut a = Annotation::new(
            Kind::Line {
                start,
                end,
                endings: [LineEnding::None, LineEnding::ClosedArrow],
                interior: Some(color),
            },
            Rect::default(),
        );
        a.color = Some(color);
        a.border.width = width;
        a
    }

    /// Sets the stroke colour; a line's interior follows when it matched the old one, so
    /// arrow heads keep the line's colour.
    pub fn set_color(&mut self, color: Option<Color>) {
        let old = self.color;
        if let Kind::Line { interior, .. } | Kind::PolyLine { interior, .. } = &mut self.kind
            && *interior == old
        {
            *interior = color;
        }
        self.color = color;
    }

    /// Closed line endings without an interior colour are filled with the stroke colour,
    /// written as `/IC` so every reader fills them alike.
    pub(crate) fn fill_closed_endings(&mut self) {
        let stroke = self.color;
        if let Kind::Line {
            endings, interior, ..
        }
        | Kind::PolyLine {
            endings, interior, ..
        } = &mut self.kind
            && interior.is_none()
            && endings.iter().any(|e| e.is_closed())
        {
            *interior = stroke;
        }
    }

    pub fn subtype(&self) -> &str {
        self.kind.subtype()
    }

    /// The rectangle implied by the geometry, padded for line width and endings.
    pub fn geometry_rect(&self) -> Option<Rect> {
        let w = self.border.width;
        let pad = |r: Rect, d: f64| Rect::new(r.x0 - d, r.y0 - d, r.x1 + d, r.y1 + d);
        let ending_pad = |e: &[LineEnding; 2]| {
            if e.iter().all(|&e| e == LineEnding::None) {
                w
            } else {
                crate::appearance::ending_size(w) + w
            }
        };
        match &self.kind {
            Kind::Line {
                start,
                end,
                endings,
                ..
            } => Rect::bounding([*start, *end]).map(|r| pad(r, ending_pad(endings))),
            Kind::PolyLine {
                vertices, endings, ..
            } => Rect::bounding(vertices.iter().copied()).map(|r| pad(r, ending_pad(endings))),
            Kind::Link { quads, .. } if !quads.is_empty() => {
                Rect::bounding(quads.iter().flat_map(|q| q.0))
            }
            Kind::Polygon { vertices, .. } => {
                Rect::bounding(vertices.iter().copied()).map(|r| pad(r, w))
            }
            Kind::Ink { strokes } => {
                Rect::bounding(strokes.iter().flatten().copied()).map(|r| pad(r, w))
            }
            Kind::Markup { quads, kind } => {
                let r = Rect::bounding(quads.iter().flat_map(|q| q.0))?;
                // Squiggles dip below the baseline.
                let h = quads.iter().map(Quad::height).fold(0.0, f64::max);
                Some(match kind {
                    MarkupKind::Squiggly => pad(r, h / 6.0 + 1.0),
                    _ => pad(r, 1.0),
                })
            }
            _ => None,
        }
    }
}
