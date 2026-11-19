//! [`EmbeddedFont`]: text in any script, drawn with system fonts embedded as Type 0
//! (Identity-H) subsets with `/W` widths and `/ToUnicode`.

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use pagebara_font::fallback::{FaceInfo, FontDb, find_for_char};
use pagebara_font::subset::Embedder;
use pagebara_syntax::{Document, Object};

use crate::font::{Glyph, Helvetica, TextFont, unsupported_chars};

/// Faces at most in one font (codes carry the face in a `u8`; resources stay small).
const MAX_FACES: usize = 32;

/// A font made of faces from a [`FontDb`]: a primary face for Latin text and, per
/// character it lacks, the first face covering the character's script (see
/// [`find_for_char`]). Faces are added as text needs them; each appearance stream embeds a
/// subset of the glyphs it shows, made when its resources are written.
#[derive(Debug)]
pub struct EmbeddedFont {
    db: Arc<FontDb>,
    family: String,
    ascent: f64,
    faces: Mutex<Faces>,
}

#[derive(Debug, Default)]
struct Faces {
    list: Vec<Face>,
    /// Characters no face covers.
    unavailable: Vec<char>,
}

#[derive(Debug)]
struct Face {
    info: FaceInfo,
    /// For lookups only; writing uses a fresh embedder per appearance.
    font: Embedder,
}

impl Face {
    fn load(db: &FontDb, info: &FaceInfo) -> Option<Face> {
        Some(Face {
            font: Embedder::from_font(db.load(info)?)?,
            info: info.clone(),
        })
    }

    fn has(&self, c: char) -> bool {
        self.font.glyph(c) != 0
    }

    fn glyph(&self, gid: u16, face: usize) -> Glyph {
        Glyph {
            code: u32::from(self.font.cid(gid)),
            len: 2,
            width: f64::from(self.font.advance(gid)) / 1000.0,
            face: face as u8,
        }
    }
}

impl Faces {
    /// The index of the face showing `c`, adding one from `db` when needed.
    fn face_for(&mut self, db: &FontDb, c: char) -> Option<usize> {
        if let Some(i) = self.list.iter().position(|f| f.has(c)) {
            return Some(i);
        }
        if self.unavailable.contains(&c) || self.list.len() >= MAX_FACES {
            return None;
        }
        match find_for_char(db, c).and_then(|info| Face::load(db, info)) {
            Some(f) if f.has(c) => {
                self.list.push(f);
                Some(self.list.len() - 1)
            }
            _ => {
                self.unavailable.push(c);
                None
            }
        }
    }
}

impl EmbeddedFont {
    /// A font from `db`'s faces; `None` when none can show Latin text.
    pub fn new(db: Arc<FontDb>) -> Option<EmbeddedFont> {
        let primary = find_for_char(&db, 'a')?.clone();
        EmbeddedFont::with_primary(db, &primary)
    }

    /// A font whose primary face is `face` (a TrueType or OpenType face of `db`).
    pub fn with_primary(db: Arc<FontDb>, face: &FaceInfo) -> Option<EmbeddedFont> {
        let f = Face::load(&db, face)?;
        let ascent = f64::from(f.font.ascent_descent().0) / 1000.0;
        Some(EmbeddedFont {
            family: face.family.clone(),
            ascent: if ascent > 0.0 { ascent } else { 0.8 },
            faces: Mutex::new(Faces {
                list: vec![f],
                unavailable: Vec::new(),
            }),
            db,
        })
    }

    /// One font over the system's fonts, scanned on first use.
    pub fn system() -> Option<&'static EmbeddedFont> {
        static FONT: OnceLock<Option<EmbeddedFont>> = OnceLock::new();
        FONT.get_or_init(|| EmbeddedFont::new(Arc::new(FontDb::system())))
            .as_ref()
    }

    /// The faces used so far, primary first.
    pub fn faces(&self) -> Vec<FaceInfo> {
        self.lock().list.iter().map(|f| f.info.clone()).collect()
    }

    fn lock(&self) -> MutexGuard<'_, Faces> {
        self.faces.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Writes face `face` with the glyphs of `used` shown in it.
    fn write_face(&self, doc: &mut Document, face: usize, used: &[(char, Glyph)]) -> Object {
        let faces = self.lock();
        let Some(f) = faces.list.get(face) else {
            return Helvetica.resource(doc, used);
        };
        let Some(mut e) = Embedder::new(f.font.font().data().clone(), f.info.index) else {
            return Helvetica.resource(doc, used);
        };
        drop(faces);
        let mut buf = [0; 4];
        for (c, g) in used.iter().filter(|u| usize::from(u.1.face) == face) {
            let gid = e.glyph_of_cid(g.code as u16);
            e.use_glyph(gid, c.encode_utf8(&mut buf));
        }
        match e.write(doc) {
            Some(r) => Object::Ref(r),
            None => Helvetica.resource(doc, used),
        }
    }
}

impl TextFont for EmbeddedFont {
    fn resource_name(&self) -> &str {
        "FU"
    }

    fn family(&self) -> &str {
        &self.family
    }

    fn glyph(&self, c: char) -> Option<Glyph> {
        let mut faces = self.lock();
        let i = faces.face_for(&self.db, c)?;
        let f = &faces.list[i];
        Some(f.glyph(f.font.glyph(c), i))
    }

    fn presentation_glyph(&self, c: char, form: char) -> Option<Glyph> {
        let mut faces = self.lock();
        let i = faces.face_for(&self.db, c)?;
        let f = &faces.list[i];
        let gid = f.font.glyph(form);
        (gid != 0).then(|| f.glyph(gid, i))
    }

    fn fallback(&self) -> Glyph {
        let faces = self.lock();
        let f = &faces.list[0];
        f.glyph(f.font.glyph('?'), 0)
    }

    fn ascent(&self) -> f64 {
        self.ascent
    }

    fn resource(&self, doc: &mut Document, used: &[(char, Glyph)]) -> Object {
        self.write_face(doc, 0, used)
    }

    fn resources(&self, doc: &mut Document, used: &[(char, Glyph)]) -> Vec<(String, Object)> {
        let mut faces: Vec<u8> = used.iter().map(|u| u.1.face).collect();
        faces.sort_unstable();
        faces.dedup();
        if faces.is_empty() {
            faces.push(0);
        }
        faces
            .into_iter()
            .map(|f| {
                (
                    self.face_name(f),
                    self.write_face(doc, usize::from(f), used),
                )
            })
            .collect()
    }
}

/// The font the default editing paths draw `text` in: [`Helvetica`] when it shows every
/// character, else the system's [`EmbeddedFont`] when there is one.
pub fn default_font(text: &str) -> &'static dyn TextFont {
    if unsupported_chars(&Helvetica, text).is_empty() {
        return &Helvetica;
    }
    match EmbeddedFont::system() {
        Some(f) => f,
        None => &Helvetica,
    }
}
