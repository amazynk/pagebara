//! Fonts for new text, through `pagebara-typeset`: [`Fonts`] keeps the fonts opened for
//! editing between operations, and [`TypesetEncoder`] is the [`TextEncoder`] built on it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use pagebara_font::fallback::{FaceInfo, FontDb};
use pagebara_font::outline::OutlineSink;
use pagebara_syntax::{Dict, Document, ObjRef, Object};
use pagebara_typeset::{FontId, FontSession, RunFont, WriteMode};

use crate::encode::{
    EncodeError, EncodeRequest, Encoded, FontChoice, ShowItem, TextEncoder, helvetica,
};

/// A font for new or edited text.
#[derive(Clone, Debug, PartialEq)]
pub enum SpanFont {
    /// A font of the document, as runs use it (usually a reference).
    Pdf(Object),
    /// An installed face, embedded as a subset when used.
    Face(FaceInfo),
}

impl SpanFont {
    /// Standard Helvetica (not embedded; characters it lacks come from fallback fonts).
    pub fn helvetica() -> SpanFont {
        SpanFont::Pdf(Object::Dict(helvetica()))
    }
}

/// A glyph outline in text space per unit size, y up.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GlyphPath {
    pub verbs: Vec<PathVerb>,
}

/// One element of a [`GlyphPath`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathVerb {
    Move(f32, f32),
    Line(f32, f32),
    Quad(f32, f32, f32, f32),
    Cubic(f32, f32, f32, f32, f32, f32),
    Close,
}

impl OutlineSink for GlyphPath {
    fn move_to(&mut self, x: f32, y: f32) {
        self.verbs.push(PathVerb::Move(x, y));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.verbs.push(PathVerb::Line(x, y));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.verbs.push(PathVerb::Quad(x1, y1, x, y));
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.verbs.push(PathVerb::Cubic(x1, y1, x2, y2, x, y));
    }
    fn close(&mut self) {
        self.verbs.push(PathVerb::Close);
    }
}

/// The fonts text is being set in: a [`FontSession`] with the document's fonts and
/// fallbacks opened so far, reused between edits.
///
/// [`Fonts::finish`] writes added codes and fallback subsets into the document; a font
/// other objects also use is copied first ([`WriteMode::CopyIfShared`]), so other pages
/// keep their font untouched. The fonts written are remembered: when the document no
/// longer has them as written (after an undo, or another change), everything is opened
/// again from the document.
#[derive(Debug)]
pub struct Fonts {
    db: Option<Arc<FontDb>>,
    session: FontSession,
    refs: HashMap<ObjRef, FontId>,
    dicts: Vec<(Dict, FontId)>,
    faces: Vec<(FaceInfo, FontId)>,
    /// Font objects as read or written, to notice the document changing.
    seen: HashMap<ObjRef, Object>,
    outlines: HashMap<(RunFont, u32), Option<Arc<GlyphPath>>>,
    space: HashMap<FontId, f32>,
    pub mode: WriteMode,
}

impl Fonts {
    /// Fonts with `db` supplying fallbacks (`None`: only the document's fonts).
    pub fn new(db: Option<Arc<FontDb>>) -> Fonts {
        Fonts {
            session: FontSession::new(db.clone()),
            db,
            refs: HashMap::new(),
            dicts: Vec::new(),
            faces: Vec::new(),
            seen: HashMap::new(),
            outlines: HashMap::new(),
            space: HashMap::new(),
            mode: WriteMode::CopyIfShared,
        }
    }

    /// Fonts with the installed fonts as fallbacks.
    pub fn system() -> Fonts {
        Fonts::new(Some(pagebara_typeset::system_fonts()))
    }

    pub fn db(&self) -> Option<&Arc<FontDb>> {
        self.db.as_ref()
    }

    /// Opens everything again when `doc` no longer has the fonts as last seen.
    pub fn sync(&mut self, doc: &Document) {
        let stale = self
            .seen
            .iter()
            .any(|(r, o)| doc.get(*r).is_none_or(|cur| *cur != *o));
        if stale {
            let mode = self.mode;
            *self = Fonts::new(self.db.clone());
            self.mode = mode;
        }
    }

    /// The session's id for a font, opening it on first use.
    pub fn open(&mut self, doc: &Document, font: &SpanFont) -> Result<FontId, EncodeError> {
        let err = |e: pagebara_typeset::Error| EncodeError::Other(e.to_string());
        match font {
            SpanFont::Pdf(Object::Ref(r)) => {
                if let Some(&id) = self.refs.get(r) {
                    return Ok(id);
                }
                let id = self.session.open_font(doc, *r).map_err(err)?;
                if let Some(o) = doc.get(*r) {
                    self.seen.insert(*r, (*o).clone());
                }
                self.refs.insert(*r, id);
                Ok(id)
            }
            SpanFont::Pdf(o) => {
                let d = o
                    .as_dict()
                    .ok_or_else(|| EncodeError::Other("not a font".into()))?;
                if let Some((_, id)) = self.dicts.iter().find(|(x, _)| x == d) {
                    return Ok(*id);
                }
                let id = self.session.open_font_dict(doc, d.clone()).map_err(err)?;
                self.dicts.push((d.clone(), id));
                Ok(id)
            }
            SpanFont::Face(f) => {
                if let Some((_, id)) = self.faces.iter().find(|(x, _)| x == f) {
                    return Ok(*id);
                }
                let id = self.session.open_face(f).map_err(err)?;
                self.faces.push((f.clone(), id));
                Ok(id)
            }
        }
    }

    pub fn session(&mut self) -> &mut FontSession {
        &mut self.session
    }

    /// Writes added codes and fallback fonts into `doc`.
    pub fn finish(&mut self, doc: &mut Document) {
        let ch = self.session.finish(doc, self.mode);
        let written = ch
            .fonts
            .iter()
            .map(|c| (c.object, Some(c.font)))
            .chain(ch.fallbacks.iter().map(|f| (f.1, None)));
        for (r, id) in written {
            if let Some(o) = doc.get(r) {
                self.seen.insert(r, (*o).clone());
            }
            if let Some(id) = id {
                self.refs.insert(r, id);
            }
        }
    }

    /// The object to select a run font with: the font as written now (a reference, or the
    /// dictionary of a direct font not written).
    pub fn object(&self, f: RunFont) -> Option<Object> {
        match f {
            RunFont::Font(id) => self.session.font_object(id).map(Object::Ref).or_else(|| {
                self.dicts
                    .iter()
                    .find(|(_, x)| *x == id)
                    .map(|(d, _)| Object::Dict(d.clone()))
            }),
            RunFont::Fallback(id) => self.session.fallback_object(id).map(Object::Ref),
        }
    }

    /// The outline of an encoded glyph (cached).
    pub fn outline(&mut self, f: RunFont, code: u32) -> Option<Arc<GlyphPath>> {
        if let Some(p) = self.outlines.get(&(f, code)) {
            return p.clone();
        }
        let mut p = GlyphPath::default();
        let p = self
            .session
            .glyph_outline(f, code, &mut p)
            .then(|| Arc::new(p));
        self.outlines.insert((f, code), p.clone());
        p
    }

    /// The advance of a space in a font, per unit size.
    pub fn space_width(&mut self, id: FontId) -> f32 {
        if let Some(&w) = self.space.get(&id) {
            return w;
        }
        let w = self.session.encode(id, " ").width();
        let w = if w > 0.0 { w } else { 0.25 };
        self.space.insert(id, w);
        w
    }
}

/// The [`TextEncoder`] for real editing: keeps a run's own font, adds codes for glyphs its
/// program has, shows the rest in a matching installed font embedded as a subset, and
/// kerns with `TJ` adjustments. New text is set in standard Helvetica (with fallbacks).
///
/// Font changes are written as each text is encoded (see [`Fonts::finish`]).
#[derive(Debug)]
pub struct TypesetEncoder {
    fonts: Mutex<Fonts>,
}

impl Default for TypesetEncoder {
    fn default() -> Self {
        TypesetEncoder::new(Fonts::system())
    }
}

impl TypesetEncoder {
    pub fn new(fonts: Fonts) -> TypesetEncoder {
        TypesetEncoder {
            fonts: Mutex::new(fonts),
        }
    }

    /// The fonts, for paragraph editing and previews.
    pub fn fonts(&self) -> MutexGuard<'_, Fonts> {
        self.fonts.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl TextEncoder for TypesetEncoder {
    fn encode(&self, doc: &mut Document, req: &EncodeRequest<'_>) -> Result<Encoded, EncodeError> {
        let mut f = self.fonts();
        f.sync(doc);
        let font = match req.font {
            Some(slot) => SpanFont::Pdf(slot.object.clone()),
            None => SpanFont::helvetica(),
        };
        let id = f.open(doc, &font)?;
        let enc = f.session().encode(id, req.text);
        if !enc.missing.is_empty() {
            return Err(EncodeError::Missing(
                enc.missing.iter().map(|m| m.1).collect(),
            ));
        }
        f.finish(doc);
        let base = f
            .object(RunFont::Font(id))
            .ok_or_else(|| EncodeError::Other("unwritten font".into()))?;
        let font_choice = match req.font {
            Some(slot) if *slot.object == base => FontChoice::Same,
            _ => FontChoice::New(base),
        };
        let mut show = Vec::new();
        let mut advances = Vec::new();
        let mut current = RunFont::Font(id);
        for run in &enc.runs {
            if run.font != current {
                show.push(ShowItem::Font(match run.font {
                    RunFont::Font(_) => FontChoice::Same,
                    f2 => FontChoice::New(
                        f.object(f2)
                            .ok_or_else(|| EncodeError::Other("unwritten font".into()))?,
                    ),
                }));
                current = run.font;
            }
            let mut bytes = Vec::new();
            let n = run.glyphs.len();
            for (i, g) in run.glyphs.iter().enumerate() {
                bytes.extend(g.bytes());
                advances.push(g.advance * 1000.0);
                if i + 1 < n && g.kern.abs() * 1000.0 >= 0.5 {
                    show.push(ShowItem::Text(std::mem::take(&mut bytes)));
                    show.push(ShowItem::Adjust((-g.kern * 1000.0).round()));
                }
            }
            if !bytes.is_empty() {
                show.push(ShowItem::Text(bytes));
            }
        }
        Ok(Encoded {
            font: font_choice,
            show,
            advances,
        })
    }
}
