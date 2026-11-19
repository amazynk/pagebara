//! Glyph outlines from font programs (through [`GlyphSource`]), cached per font and code,
//! and rasterised glyph masks cached per font, code, transform and subpixel offset.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use pagebara_content::display::{FontRef, Verb};
use pagebara_content::font::FontKind;
use pagebara_content::{Font, Matrix, Point};
use pagebara_font::fallback::FontDb;
use pagebara_font::{OutlineSink, PdfFont};
use pagebara_syntax::{Document, Object};
use parking_lot::Mutex;

use crate::raster::Mask;

/// A glyph outline in text space per unit font size (the font matrix applied), filled
/// nonzero.
#[derive(Debug, Default)]
pub struct Outline {
    pub verbs: Vec<Verb>,
    pub points: Vec<Point>,
}

impl Outline {
    pub fn path(&self) -> pagebara_content::display::Path<'_> {
        pagebara_content::display::Path {
            verbs: &self.verbs,
            points: &self.points,
        }
    }

    pub fn bounds(&self) -> Option<pagebara_content::Rect> {
        self.path().bounds()
    }
}

/// Supplies glyph outlines for a document's fonts. The default uses `pagebara-font` with
/// system font substitution.
pub trait GlyphSource: Send + Sync {
    /// Outlines for one font, or `None` to draw its text as nothing.
    fn load(&self, doc: &Document, font: &Font) -> Option<Arc<dyn GlyphOutlines>>;
}

/// The glyphs of one font.
pub trait GlyphOutlines: Send + Sync {
    /// The outline of a shown code (a character code; for composite fonts the code before
    /// the CMap, `font` gives the CID) in text space per unit font size.
    fn outline(&self, doc: &Document, font: &Font, code: u32) -> Option<Outline>;
}

/// The default [`GlyphSource`]: embedded programs, else a substitute from the system's
/// fonts (scanned once per process, on first need).
#[derive(Debug, Default)]
pub struct DefaultGlyphs;

impl DefaultGlyphs {
    fn db(&self) -> &'static FontDb {
        static SYSTEM: OnceLock<FontDb> = OnceLock::new();
        SYSTEM.get_or_init(FontDb::system)
    }

    /// Scans the system's fonts now (call on a background thread), so that the first page
    /// with a font that is not embedded does not wait for the scan.
    pub fn warm_up() {
        DefaultGlyphs.db();
    }
}

impl GlyphSource for DefaultGlyphs {
    fn load(&self, doc: &Document, font: &Font) -> Option<Arc<dyn GlyphOutlines>> {
        if font.kind == FontKind::Type3 {
            return None;
        }
        let spec = pagebara_font::pdf::spec_from_dict(doc, &font.dict)?;
        let pdf = match pagebara_font::pdf::embedded_program(doc, &font.dict) {
            Some(program) => PdfFont::new(program, spec),
            // Not embedded, or broken: a substitute.
            None => {
                let req = pagebara_font::fallback::FontRequest::from_spec(&spec);
                let face = pagebara_font::fallback::find(self.db(), &req)?;
                PdfFont::substitute(self.db().load(face)?, spec)
            }
        };
        Some(Arc::new(FontGlyphs { pdf }))
    }
}

struct FontGlyphs {
    pdf: PdfFont,
}

struct Sink {
    m: pagebara_font::Matrix,
    out: Outline,
    cur: Point,
}

impl Sink {
    fn pt(&self, x: f32, y: f32) -> Point {
        let (x, y) = self.m.apply(x, y);
        Point::new(x, y)
    }
}

impl OutlineSink for Sink {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.pt(x, y);
        self.out.verbs.push(Verb::Move);
        self.out.points.push(p);
        self.cur = p;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.pt(x, y);
        self.out.verbs.push(Verb::Line);
        self.out.points.push(p);
        self.cur = p;
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let c = self.pt(x1, y1);
        let p = self.pt(x, y);
        let p0 = self.cur;
        let a = Point::new(
            p0.x + 2.0 / 3.0 * (c.x - p0.x),
            p0.y + 2.0 / 3.0 * (c.y - p0.y),
        );
        let b = Point::new(p.x + 2.0 / 3.0 * (c.x - p.x), p.y + 2.0 / 3.0 * (c.y - p.y));
        self.out.verbs.push(Verb::Cubic);
        self.out.points.extend_from_slice(&[a, b, p]);
        self.cur = p;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (a, b, p) = (self.pt(x1, y1), self.pt(x2, y2), self.pt(x, y));
        self.out.verbs.push(Verb::Cubic);
        self.out.points.extend_from_slice(&[a, b, p]);
        self.cur = p;
    }

    fn close(&mut self) {
        self.out.verbs.push(Verb::Close);
    }
}

impl GlyphOutlines for FontGlyphs {
    fn outline(&self, doc: &Document, font: &Font, code: u32) -> Option<Outline> {
        let pdf = &self.pdf;
        let mut m = pdf.font_matrix();
        let composite = font.is_composite();
        let key = if composite { font.cid(code) } else { code };
        let mut gid = pdf.glyph(key);
        if gid == 0 && pdf.is_substitute() {
            let mut s = String::new();
            if font.unicode(doc, code, &mut s)
                && let Some(c) = s.chars().next()
            {
                gid = pdf.glyph_for_char(c).unwrap_or(0);
            }
        }
        if gid == 0 {
            return None;
        }
        if pdf.is_substitute() {
            // Fit the substitute's advance to the PDF width.
            let w = font.width(code);
            if let Some(adv) = pdf.font().advance(gid) {
                let (ax, _) = m.apply(adv, 0.0);
                let (ox, _) = m.apply(0.0, 0.0);
                let adv = ax - ox;
                if w > 0.0 && adv > 0.0 {
                    let r = (w / adv).clamp(0.5, 2.0) as f64;
                    m = m.then(&pagebara_font::Matrix([r, 0.0, 0.0, 1.0, 0.0, 0.0]));
                }
            }
        }
        let mut sink = Sink {
            m,
            out: Outline::default(),
            cur: Point::default(),
        };
        pdf.font().outline(gid, &mut sink).then_some(sink.out)
    }
}

/// Identifies a font across display lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontKey(pub u64);

impl FontKey {
    pub fn of(f: &FontRef) -> FontKey {
        match &f.object {
            Object::Ref(r) => FontKey(u64::from(r.num) << 16 | u64::from(r.generation)),
            _ => FontKey(1 << 63 | Arc::as_ptr(&f.font) as usize as u64),
        }
    }
}

type Outlines = Mutex<HashMap<u32, Option<Arc<Outline>>>>;
type Loaded = Option<(Arc<dyn GlyphOutlines>, Arc<Outlines>)>;

/// Loaded fonts and their outlines.
pub struct FontCache {
    source: Arc<dyn GlyphSource>,
    fonts: Mutex<HashMap<FontKey, Arc<OnceLock<Loaded>>>>,
}

impl std::fmt::Debug for FontCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontCache")
            .field("fonts", &self.fonts.lock().len())
            .finish_non_exhaustive()
    }
}

impl FontCache {
    pub fn new(source: Arc<dyn GlyphSource>) -> Self {
        FontCache {
            source,
            fonts: Mutex::new(HashMap::new()),
        }
    }

    pub fn outline(&self, doc: &Document, f: &FontRef, code: u32) -> Option<Arc<Outline>> {
        let key = FontKey::of(f);
        // One thread loads a font; others needing it wait for that.
        let slot = self.fonts.lock().entry(key).or_default().clone();
        let entry = slot.get_or_init(|| {
            self.source
                .load(doc, &f.font)
                .map(|g| (g, Arc::new(Mutex::new(HashMap::new()))))
        });
        let (glyphs, outlines) = entry.as_ref()?;
        if let Some(o) = outlines.lock().get(&code) {
            return o.clone();
        }
        let o = glyphs.outline(doc, &f.font, code).map(Arc::new);
        outlines.lock().insert(code, o.clone());
        o
    }
}

/// Glyph mask key: font, code, the linear part of the device transform and the subpixel
/// phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub font: FontKey,
    pub code: u32,
    pub m: [i32; 4],
    pub sub: (u8, u8),
}

/// Splits a glyph origin coordinate into whole pixels and a quantised subpixel offset
/// (in 1/256 px). Along the baseline: quarter pixels below 24 px glyphs, halves below 48,
/// whole pixels above; across it: whole pixels from 8 px.
pub fn subpixel(v: f32, size: f32, along: bool) -> (i32, u8) {
    let (q, r) = if along {
        match size {
            s if s >= 48.0 => (0u32, 0.5),
            s if s >= 24.0 => (128, 0.25),
            _ => (192, 0.125),
        }
    } else {
        match size {
            s if s >= 8.0 => (0u32, 0.5),
            s if s >= 4.0 => (128, 0.25),
            _ => (192, 0.125),
        }
    };
    let v = v + r;
    let p = v.floor();
    let frac = ((v - p) * 256.0) as u32 & q;
    (p as i32, frac as u8)
}

const SHARDS: usize = 16;

/// Rasterised glyphs shared by all render threads.
#[derive(Debug)]
pub struct GlyphCache {
    shards: Vec<Mutex<HashMap<GlyphKey, Option<Arc<Mask>>>>>,
    bytes: AtomicUsize,
    budget: usize,
}

impl GlyphCache {
    pub fn new(budget: usize) -> Self {
        GlyphCache {
            shards: (0..SHARDS).map(|_| Mutex::new(HashMap::new())).collect(),
            bytes: AtomicUsize::new(0),
            budget,
        }
    }

    fn shard(&self, k: &GlyphKey) -> &Mutex<HashMap<GlyphKey, Option<Arc<Mask>>>> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        k.hash(&mut h);
        &self.shards[h.finish() as usize % SHARDS]
    }

    pub fn get(&self, k: &GlyphKey) -> Option<Option<Arc<Mask>>> {
        self.shard(k).lock().get(k).cloned()
    }

    pub fn insert(&self, k: GlyphKey, m: Option<Arc<Mask>>) {
        let size = m.as_ref().map_or(16, |m| m.data.len() + 64);
        if self.bytes.fetch_add(size, Ordering::Relaxed) + size > self.budget {
            // Crude eviction: drop one shard.
            let s = self.shard(&k);
            let mut g = s.lock();
            let freed: usize = g
                .values()
                .map(|m| m.as_ref().map_or(16, |m| m.data.len() + 64))
                .sum();
            g.clear();
            self.bytes.fetch_sub(
                freed.min(self.bytes.load(Ordering::Relaxed)),
                Ordering::Relaxed,
            );
            g.insert(k, m);
            return;
        }
        self.shard(&k).lock().insert(k, m);
    }
}

/// Quantises the linear part of a glyph transform for cache keys.
pub fn quantize(m: &Matrix) -> [i32; 4] {
    [m.a, m.b, m.c, m.d].map(|v| (v * 256.0).round() as i32)
}
