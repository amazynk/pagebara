//! [`FontSession`]: encoding new text in a document's fonts.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use pagebara_font::Font;
use pagebara_font::fallback::{self, FaceInfo, FontDb, Script, normalize};
use pagebara_font::outline::{OutlineSink, Path};
use pagebara_font::pdf::PdfFont;
use pagebara_font::subset::Embedder;
use pagebara_syntax::{Dict, Document, ObjRef, Object};
use unicode_normalization::UnicodeNormalization;

use crate::Error;
use crate::encoded::{
    CharCoverage, CharStatus, Encoded, EncodedGlyph, EncodedRun, FallbackId, FontId, RunFont,
};
use crate::kern::Kerner;
use crate::pdffont::{Choice, PdfFontState, Transformed};
use crate::style::{FontClass, FontStyle, installed_family, request};

/// How [`FontSession::finish`] writes a modified font.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WriteMode {
    /// Overwrite the font object: every page using it sees the new codes (old codes keep
    /// their meaning, so existing text is unaffected).
    #[default]
    InPlace,
    /// Always write a new font object; the caller points its resource at it.
    Copy,
    /// Write a new object only when other objects refer to the font too.
    CopyIfShared,
}

/// Options for encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodeOptions {
    /// Add codes for glyphs the font's program has but its encoding lacks.
    pub add_codes: bool,
    /// Use fallback fonts for characters the font cannot show.
    pub fallback: bool,
    /// Apply pair kerning (`GPOS`/`kern`) as `TJ` adjustments.
    pub kerning: bool,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        EncodeOptions {
            add_codes: true,
            fallback: true,
            kerning: true,
        }
    }
}

/// A font changed by [`FontSession::finish`].
#[derive(Clone, Debug, PartialEq)]
pub struct FontChange {
    pub font: FontId,
    /// The font's object: the original one (updated in place) or a new copy.
    pub object: ObjRef,
    /// A new object: the caller must point the page's font resource at it.
    pub copied: bool,
}

/// What [`FontSession::finish`] wrote.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changes {
    pub fonts: Vec<FontChange>,
    /// New font resources (Type 0 fonts) to add to the page resources.
    pub fallbacks: Vec<(FallbackId, ObjRef)>,
}

impl Changes {
    /// The object to use for a run's font (`None` for an unchanged font: keep its resource).
    pub fn object(&self, font: RunFont) -> Option<ObjRef> {
        match font {
            RunFont::Font(id) => self.fonts.iter().find(|c| c.font == id).map(|c| c.object),
            RunFont::Fallback(id) => self.fallbacks.iter().find(|f| f.0 == id).map(|f| f.1),
        }
    }
}

/// A subset of a system font being collected.
#[derive(Debug)]
struct Fallback {
    face: FaceInfo,
    embedder: Embedder,
    kerner: Kerner,
    /// CID → text, to keep `/ToUnicode` right.
    used: HashMap<u16, String>,
    /// Glyphs added since the last write.
    pending: bool,
    /// The object written for it, rewritten in place by later finishes.
    written: Option<ObjRef>,
}

#[derive(Debug)]
enum Entry {
    Pdf(Box<PdfFontState>),
    /// A face opened with [`FontSession::open_face`]: shown with its fallback resource.
    Face(FallbackId, FontStyle),
}

/// A session editing text in a document's fonts.
///
/// Open the fonts text will use ([`FontSession::open_font`], or an installed face with
/// [`FontSession::open_face`]), encode strings ([`FontSession::encode`]), write the content
/// stream with the returned codes, then call [`FontSession::finish`] once to write the font
/// changes into the document.
///
/// For each character, in order: an existing code of the font that extracts to it and has
/// a glyph ([`CharStatus::Existing`]); else a new code for a glyph the font program has
/// ([`CharStatus::Added`]: `/Differences` in a free code for simple fonts, a CID with
/// `/W` and `CIDToGIDMap` entries for `Identity-H` fonts, a `/ToUnicode` entry always);
/// else a fallback font ([`CharStatus::Fallback`]): the font's family if installed, else a
/// metric-compatible or similar face (class, weight, slant, x-height), else any face with
/// the character, embedded as a subset in a new Type 0 font. Fallback resources are shared
/// by all fonts of the session.
#[derive(Debug)]
pub struct FontSession {
    db: Option<Arc<FontDb>>,
    fonts: Vec<Entry>,
    fallbacks: Vec<Fallback>,
    /// Parsed system faces (`None`: unusable).
    faces: HashMap<(PathBuf, u32), Option<Arc<Font>>>,
    /// Candidate faces per font (indices into the database).
    candidates: HashMap<usize, Vec<usize>>,
    /// Programs standing in for fonts without one, for outlines.
    substitutes: HashMap<usize, Option<PdfFont>>,
    pub options: EncodeOptions,
}

fn is_blank(c: char) -> bool {
    c.is_whitespace()
}

impl FontSession {
    /// A session; `db` supplies fallback faces (`None`: no fallbacks).
    pub fn new(db: Option<Arc<FontDb>>) -> FontSession {
        FontSession {
            db,
            fonts: Vec::new(),
            fallbacks: Vec::new(),
            faces: HashMap::new(),
            candidates: HashMap::new(),
            substitutes: HashMap::new(),
            options: EncodeOptions::default(),
        }
    }

    /// Opens a font resource by reference.
    pub fn open_font(&mut self, doc: &Document, font: ObjRef) -> Result<FontId, Error> {
        let o = doc.get(font).ok_or(Error::NotAFont)?;
        let d = o.as_dict().ok_or(Error::NotAFont)?.clone();
        self.open(doc, Some(font), d)
    }

    /// Opens a font given as a direct dictionary (finish always writes it as a new object).
    pub fn open_font_dict(&mut self, doc: &Document, font: Dict) -> Result<FontId, Error> {
        self.open(doc, None, font)
    }

    fn open(&mut self, doc: &Document, r: Option<ObjRef>, d: Dict) -> Result<FontId, Error> {
        if d.name(b"Type").is_some_and(|t| t != b"Font") || !d.contains(b"Subtype") {
            return Err(Error::NotAFont);
        }
        let st = PdfFontState::open(doc, r, d)?;
        self.fonts.push(Entry::Pdf(Box::new(st)));
        Ok(FontId(self.fonts.len() - 1))
    }

    /// Opens an installed face (TrueType or OpenType, embeddable); text in it is shown with
    /// a new Type 0 font resource ([`RunFont::Fallback`]).
    pub fn open_face(&mut self, face: &FaceInfo) -> Result<FontId, Error> {
        let id = self.fallback_for_face(face).ok_or(Error::Unembeddable)?;
        let style = FontStyle {
            base_font: face.postscript_name.clone(),
            family: face.family.clone(),
            weight: face.weight,
            italic: face.italic,
            class: if face.monospace {
                FontClass::Mono
            } else if face.serif == Some(true) {
                FontClass::Serif
            } else {
                FontClass::Sans
            },
            italic_angle: 0.0,
            x_height: None,
            cap_height: None,
            embedded: true,
            subset: false,
        };
        self.fonts.push(Entry::Face(id, style));
        Ok(FontId(self.fonts.len() - 1))
    }

    /// The style of an opened font.
    pub fn style(&self, font: FontId) -> Option<&FontStyle> {
        match self.fonts.get(font.0)? {
            Entry::Pdf(p) => Some(&p.style),
            Entry::Face(_, s) => Some(s),
        }
    }

    /// The face a fallback resource embeds.
    pub fn fallback_face(&self, id: FallbackId) -> Option<&FaceInfo> {
        self.fallbacks.get(id.0).map(|f| &f.face)
    }

    /// Ascent and descent of a font in text space per unit size (for line placement).
    pub fn metrics(&self, font: FontId) -> (f32, f32) {
        match self.fonts.get(font.0) {
            Some(Entry::Pdf(p)) => p.metrics(),
            Some(Entry::Face(f, _)) => {
                let (a, d) = self.fallbacks[f.0].embedder.ascent_descent();
                (a / 1000.0, d / 1000.0)
            }
            None => (0.8, -0.2),
        }
    }

    /// What [`FontSession::encode`] would do with each character, without changing
    /// anything.
    pub fn coverage(&mut self, font: FontId, text: &str) -> Vec<CharCoverage> {
        let text: String = text.nfc().collect();
        let mut out = Vec::new();
        for (i, c) in text.char_indices() {
            let status = self.status(font, c);
            out.push(CharCoverage {
                offset: i,
                ch: c,
                status,
            });
        }
        out
    }

    fn status(&mut self, font: FontId, c: char) -> CharStatus {
        if c.is_control() {
            return CharStatus::Unsupported;
        }
        let s = c.to_string();
        let opts = self.options;
        if let Some(Entry::Pdf(p)) = self.fonts.get_mut(font.0) {
            if let Some(ch) = p.existing(&s) {
                return if ch.added {
                    CharStatus::Added
                } else {
                    CharStatus::Existing
                };
            }
            if opts.add_codes && p.can_add(c) {
                return CharStatus::Added;
            }
        }
        if let Some(Entry::Face(f, _)) = self.fonts.get(font.0)
            && self.fallback_has(*f, c)
        {
            return CharStatus::Existing;
        }
        if opts.fallback && self.find_fallback(font, c, false).is_some() {
            CharStatus::Fallback
        } else {
            CharStatus::Unsupported
        }
    }

    /// Encodes `text` (normalized to NFC) for font `font`: runs in the font and in fallback
    /// fonts, with codes, advances and kerning. Characters no font can show (and control
    /// characters) are left out and listed in [`Encoded::missing`].
    pub fn encode(&mut self, font: FontId, text: &str) -> Encoded {
        let text: String = text.nfc().collect();
        let mut enc = Encoded {
            text: text.clone(),
            ..Encoded::default()
        };
        // (run font, glyph, gid for kerning)
        let mut items: Vec<(RunFont, EncodedGlyph, Option<u16>)> = Vec::new();
        for (i, c) in text.char_indices() {
            let cluster = i..i + c.len_utf8();
            match self.encode_char(font, c) {
                Some((rf, choice)) => {
                    let word_space = match (rf, self.fonts.get(font.0)) {
                        (RunFont::Font(_), Some(Entry::Pdf(p))) => {
                            p.is_word_space(choice.code, choice.code_len)
                        }
                        _ => false,
                    };
                    items.push((
                        rf,
                        EncodedGlyph {
                            code: choice.code,
                            code_len: choice.code_len,
                            cluster,
                            advance: choice.advance,
                            kern: 0.0,
                            word_space,
                        },
                        choice.gid,
                    ));
                }
                None => enc.missing.push((i, c)),
            }
        }
        if self.options.kerning {
            for i in 1..items.len() {
                let (a, b) = (&items[i - 1], &items[i]);
                if a.0 != b.0 {
                    continue;
                }
                let (Some(l), Some(r)) = (a.2, b.2) else {
                    continue;
                };
                let k = self.kern(a.0, l, r);
                items[i - 1].1.kern = k;
            }
        }
        for (rf, g, _) in items {
            match enc.runs.last_mut() {
                Some(run) if run.font == rf => run.glyphs.push(g),
                _ => enc.runs.push(EncodedRun {
                    font: rf,
                    glyphs: vec![g],
                }),
            }
        }
        enc
    }

    fn kern(&self, rf: RunFont, l: u16, r: u16) -> f32 {
        match rf {
            RunFont::Font(id) => match self.fonts.get(id.0) {
                Some(Entry::Pdf(p)) => p.kern(l, r),
                _ => 0.0,
            },
            RunFont::Fallback(id) => {
                let f = &self.fallbacks[id.0];
                let s = f.embedder.font();
                f.kerner.kern(s, l, r) as f32 / f32::from(s.units_per_em().max(1))
            }
        }
    }

    fn encode_char(&mut self, font: FontId, c: char) -> Option<(RunFont, Choice)> {
        if c.is_control() {
            return None;
        }
        let opts = self.options;
        let s = c.to_string();
        match self.fonts.get_mut(font.0)? {
            Entry::Pdf(p) => {
                if let Some(ch) = p.existing(&s) {
                    return Some((RunFont::Font(font), ch));
                }
                if opts.add_codes
                    && let Some(ch) = p.add(c)
                {
                    return Some((RunFont::Font(font), ch));
                }
            }
            Entry::Face(f, _) => {
                let f = *f;
                if let Some(ch) = self.use_fallback(f, c) {
                    return Some((RunFont::Fallback(f), ch));
                }
            }
        }
        if !opts.fallback {
            return None;
        }
        let f = self.find_fallback(font, c, true)?;
        let ch = self.use_fallback(f, c)?;
        Some((RunFont::Fallback(f), ch))
    }

    /// Records `c` in fallback `f` and returns its code.
    fn use_fallback(&mut self, f: FallbackId, c: char) -> Option<Choice> {
        let fb = &mut self.fallbacks[f.0];
        let gid = fb.embedder.glyph(c);
        if gid == 0 {
            return None;
        }
        let s = c.to_string();
        let cid = fb.embedder.cid(gid);
        match fb.used.get(&cid) {
            Some(t) if *t != s => return None,
            _ => {}
        }
        if fb.used.insert(cid, s.clone()).is_none() {
            fb.pending = true;
        }
        fb.embedder.use_glyph(gid, &s);
        Some(Choice {
            code: u32::from(cid),
            code_len: 2,
            advance: fb.embedder.advance(gid) / 1000.0,
            gid: Some(gid),
            added: false,
        })
    }

    fn fallback_has(&self, f: FallbackId, c: char) -> bool {
        let fb = &self.fallbacks[f.0];
        let gid = fb.embedder.glyph(c);
        gid != 0
            && fb
                .used
                .get(&fb.embedder.cid(gid))
                .is_none_or(|t| *t == c.to_string())
            && (is_blank(c) || sfnt_has_outline(fb.embedder.font(), gid))
    }

    /// The fallback resource for a face, created on first use.
    fn fallback_for_face(&mut self, face: &FaceInfo) -> Option<FallbackId> {
        if let Some(i) = self
            .fallbacks
            .iter()
            .position(|f| f.face.path == face.path && f.face.index == face.index)
        {
            return Some(FallbackId(i));
        }
        if !face.sfnt || !face.embeddable() {
            return None;
        }
        let font = match &self.db {
            Some(db) => db.load(face)?,
            None => Font::parse_index(std::fs::read(&face.path).ok()?, face.index)?,
        };
        let embedder = Embedder::from_font(font)?;
        let kerner = Kerner::new(embedder.font());
        self.fallbacks.push(Fallback {
            face: face.clone(),
            embedder,
            kerner,
            used: HashMap::new(),
            pending: false,
            written: None,
        });
        Some(FallbackId(self.fallbacks.len() - 1))
    }

    fn load_face(&mut self, face: &FaceInfo) -> Option<Arc<Font>> {
        let key = (face.path.clone(), face.index);
        if let Some(f) = self.faces.get(&key) {
            return f.clone();
        }
        let f = self
            .db
            .as_ref()
            .and_then(|db| db.load(face))
            .filter(|f| match f.as_sfnt() {
                Some(s) => s.has_glyf() || s.cff().is_some_and(|c| !c.is_cff2()),
                None => false,
            })
            .map(Arc::new);
        self.faces.insert(key, f.clone());
        f
    }

    fn face_has(&mut self, face: &FaceInfo, c: char) -> bool {
        let Some(f) = self.load_face(face) else {
            return false;
        };
        let Some(s) = f.as_sfnt() else { return false };
        match s.glyph_for_char(c) {
            Some(g) if g != 0 => is_blank(c) || sfnt_has_outline(s, g),
            _ => false,
        }
    }

    /// A fallback for `c` when font `font` lacks it. With `create`, the fallback resource is
    /// made if needed; otherwise only existence is checked.
    fn find_fallback(&mut self, font: FontId, c: char, create: bool) -> Option<FallbackId> {
        let db = self.db.clone()?;
        let cands = self.candidate_faces(font, &db);
        for i in cands {
            let face = &db.faces()[i];
            if !self.face_has(face, c) {
                continue;
            }
            if let Some(fid) = self
                .fallbacks
                .iter()
                .position(|f| f.face.path == face.path && f.face.index == face.index)
            {
                if self.fallback_has(FallbackId(fid), c) {
                    return Some(FallbackId(fid));
                }
                continue;
            }
            if !create {
                return Some(FallbackId(usize::MAX));
            }
            if let Some(id) = self.fallback_for_face(face) {
                return Some(id);
            }
        }
        // Any face covering the character's script, closest style first.
        let style = self.style(font).cloned();
        let script = Script::of(c);
        let mut by_script: Vec<usize> = db
            .faces()
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                f.sfnt
                    && f.embeddable()
                    && (script.range_bits().iter().any(|&b| f.claims_range(b))
                        || script
                            .families()
                            .iter()
                            .any(|n| normalize(n) == normalize(&f.family)))
            })
            .map(|(i, _)| i)
            .collect();
        by_script.sort_by_key(|&i| style.as_ref().map_or(0, |s| style_score(s, &db.faces()[i])));
        for i in by_script.into_iter().take(64) {
            let face = db.faces()[i].clone();
            if self.face_has(&face, c) {
                if !create {
                    return Some(FallbackId(usize::MAX));
                }
                if let Some(id) = self.fallback_for_face(&face)
                    && self.fallback_has(id, c)
                {
                    return Some(id);
                }
            }
        }
        None
    }

    /// Faces to try for a font's missing characters, best first: its family, the
    /// replacements and class families of the substitution model, then faces of the same
    /// class by style and x-height.
    fn candidate_faces(&mut self, font: FontId, db: &FontDb) -> Vec<usize> {
        if let Some(c) = self.candidates.get(&font.0) {
            return c.clone();
        }
        let Some(style) = self.style(font).cloned() else {
            return Vec::new();
        };
        let faces = db.faces();
        let usable = |f: &FaceInfo| f.sfnt && f.embeddable();
        let mut out: Vec<usize> = Vec::new();
        let push_group = |out: &mut Vec<usize>, mut group: Vec<usize>| {
            group.sort_by_key(|&i| style_score(&style, &faces[i]));
            for i in group {
                if !out.contains(&i) {
                    out.push(i);
                }
            }
        };
        let fam: Vec<usize> = installed_family(db, &style)
            .into_iter()
            .filter_map(|f| faces.iter().position(|x| std::ptr::eq(x, f)))
            .filter(|&i| usable(&faces[i]))
            .collect();
        push_group(&mut out, fam);
        for name in fallback::candidates(&request(&style)) {
            let n = normalize(name);
            let g: Vec<usize> = faces
                .iter()
                .enumerate()
                .filter(|(_, f)| usable(f) && normalize(&f.family) == n)
                .map(|(i, _)| i)
                .collect();
            push_group(&mut out, g);
        }
        // Same class, ranked by style, then refined by x-height among the closest.
        let mut class: Vec<usize> = faces
            .iter()
            .enumerate()
            .filter(|(_, f)| usable(f) && face_class(f) == style.class && f.claims_range(0))
            .map(|(i, _)| i)
            .collect();
        class.sort_by_key(|&i| style_score(&style, &faces[i]));
        class.truncate(24);
        if let Some(xh) = style.x_height {
            let mut scored: Vec<(u32, usize)> = Vec::new();
            for i in class {
                let face = faces[i].clone();
                let xs = self
                    .load_face(&face)
                    .and_then(|f| {
                        let s = f.as_sfnt()?;
                        let o = s.os2()?;
                        Some(f32::from(o.x_height) * 1000.0 / f32::from(s.units_per_em().max(1)))
                    })
                    .filter(|v| *v > 0.0)
                    .map_or(200, |v| ((v - xh).abs() * 2.0) as u32);
                scored.push((style_score(&style, &face) + xs, i));
            }
            scored.sort_unstable();
            class = scored.into_iter().map(|s| s.1).collect();
        }
        for i in class {
            if !out.contains(&i) {
                out.push(i);
            }
        }
        self.candidates.insert(font.0, out.clone());
        out
    }

    /// Writes the font changes into `doc`: modified fonts (per `mode`) and the new fallback
    /// fonts. It may be called again after more encoding: it then writes only what changed
    /// since, to the objects written before (a font copied once is updated in place).
    pub fn finish(&mut self, doc: &mut Document, mode: WriteMode) -> Changes {
        let mut changes = Changes::default();
        for (i, e) in self.fonts.iter_mut().enumerate() {
            let Entry::Pdf(p) = e else { continue };
            if !p.pending {
                continue;
            }
            let d = p.write(doc);
            let copy = match (p.r, mode) {
                (None, _) | (_, WriteMode::Copy) => true,
                (Some(r), WriteMode::CopyIfShared) => references(doc, r) > 1,
                (Some(_), WriteMode::InPlace) => false,
            };
            let (object, copied) = match p.r {
                Some(r) if !copy => {
                    doc.set(r, Object::Dict(d));
                    (r, false)
                }
                _ => (doc.add(Object::Dict(d)), true),
            };
            p.r = Some(object);
            p.pending = false;
            changes.fonts.push(FontChange {
                font: FontId(i),
                object,
                copied,
            });
        }
        for (i, f) in self.fallbacks.iter_mut().enumerate() {
            if f.used.is_empty() || !f.pending {
                continue;
            }
            if let Some(r) = f.embedder.write(doc) {
                let r = match f.written {
                    Some(old) => {
                        if let Some(o) = doc.get(r).map(|o| (*o).clone()) {
                            doc.set(old, o);
                        }
                        old
                    }
                    None => r,
                };
                f.written = Some(r);
                f.pending = false;
                changes.fallbacks.push((FallbackId(i), r));
            }
        }
        changes
    }

    /// Draws the glyph of an encoded code ([`EncodedGlyph::code`]) of a run font in text
    /// space per unit size (y up). Fonts without a program draw with the substitute the
    /// renderer would use. False when there is no glyph.
    pub fn glyph_outline(&mut self, font: RunFont, code: u32, sink: &mut dyn OutlineSink) -> bool {
        match font {
            RunFont::Font(id) => {
                let Some(Entry::Pdf(p)) = self.fonts.get(id.0) else {
                    return false;
                };
                if !p.has_program() && !self.substitutes.contains_key(&id.0) {
                    let sub = p.spec().and_then(|spec| {
                        let db = self.db.as_ref()?;
                        let req = fallback::FontRequest::from_spec(spec);
                        let face = fallback::find(db, &req)?;
                        Some(PdfFont::substitute(db.load(face)?, spec.clone()))
                    });
                    self.substitutes.insert(id.0, sub);
                }
                let Some(Entry::Pdf(p)) = self.fonts.get(id.0) else {
                    return false;
                };
                let sub = self.substitutes.get(&id.0).and_then(Option::as_ref);
                p.outline(code, sub, sink)
            }
            RunFont::Fallback(id) => {
                let Some(f) = self.fallbacks.get(id.0) else {
                    return false;
                };
                let s = f.embedder.font();
                let gid = f.embedder.glyph_of_cid(code as u16);
                let k = 1.0 / f64::from(s.units_per_em().max(1));
                let mut t = Transformed {
                    m: pagebara_font::Matrix([k, 0.0, 0.0, k, 0.0, 0.0]),
                    sink,
                };
                gid != 0 && s.outline(gid, &mut t)
            }
        }
    }

    /// The object a font is written to now: its own (or the copy [`FontSession::finish`]
    /// made); `None` for a direct dictionary not written yet and for faces.
    pub fn font_object(&self, font: FontId) -> Option<ObjRef> {
        match self.fonts.get(font.0)? {
            Entry::Pdf(p) => p.r,
            Entry::Face(..) => None,
        }
    }

    /// The object of a fallback font, once written.
    pub fn fallback_object(&self, id: FallbackId) -> Option<ObjRef> {
        self.fallbacks.get(id.0)?.written
    }

    /// Whether encoding added codes or glyphs not written yet.
    pub fn has_pending(&self) -> bool {
        self.fonts
            .iter()
            .any(|e| matches!(e, Entry::Pdf(p) if p.pending))
            || self
                .fallbacks
                .iter()
                .any(|f| f.pending && !f.used.is_empty())
    }
}

fn sfnt_has_outline(s: &pagebara_font::sfnt::Sfnt, gid: u16) -> bool {
    let mut p = Path::new();
    s.outline(gid, &mut p) && !p.is_empty()
}

fn face_class(f: &FaceInfo) -> FontClass {
    if f.monospace {
        FontClass::Mono
    } else if f.serif == Some(true) {
        FontClass::Serif
    } else {
        FontClass::Sans
    }
}

/// Lower is closer: class, slant, weight.
fn style_score(s: &FontStyle, f: &FaceInfo) -> u32 {
    let class = if face_class(f) == s.class { 0 } else { 1000 };
    let italic = if f.italic == s.italic { 0 } else { 400 };
    let weight = u32::from(f.weight.abs_diff(s.weight));
    class + italic + weight
}

/// How many objects refer to `r` (direct references from dictionaries, arrays and stream
/// dictionaries of every object).
pub fn references(doc: &Document, r: ObjRef) -> usize {
    fn count(o: &Object, r: ObjRef) -> usize {
        match o {
            Object::Ref(x) => usize::from(*x == r),
            Object::Array(a) => a.iter().map(|v| count(v, r)).sum(),
            Object::Dict(d) => d.iter().map(|(_, v)| count(v, r)).sum(),
            Object::Stream(s) => s.dict.iter().map(|(_, v)| count(v, r)).sum(),
            _ => 0,
        }
    }
    let mut n = 0;
    for num in doc.object_numbers() {
        let Some(x) = doc.reference(num) else {
            continue;
        };
        if x == r {
            continue;
        }
        if let Some(o) = doc.get(x) {
            n += count(&o, r);
        }
    }
    n
}

/// A font resource name not used in `fonts` (a page's `/Font` dictionary), `prefix`
/// followed by a number.
pub fn unique_resource_name(fonts: &Dict, prefix: &str) -> String {
    (1..)
        .map(|i| format!("{prefix}{i}"))
        .find(|n| !fonts.contains(n.as_bytes()))
        .unwrap_or_default()
}
