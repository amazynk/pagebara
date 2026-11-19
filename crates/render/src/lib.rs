//! Page rendering (ISO 32000-2 §8–11): an anti-aliased scan converter with exact area
//! coverage, colour management to sRGB, the transparency model, images, shadings and text.
//!
//! A [`Renderer`] is shared by all threads rendering one document. It interprets each page
//! once into a display list and caches lists, decoded images, glyphs and colour transforms,
//! so any device rectangle of a page renders at any scale without drawing the rest:
//!
//! ```no_run,standalone_crate
//! # fn f(doc: pagebara_syntax::Document) -> Result<(), pagebara_render::RenderError> {
//! use pagebara_render::{Renderer, RenderRequest, PixelRect};
//! let r = Renderer::new(doc);
//! let req = RenderRequest::new(2.0, PixelRect { x: 0, y: 0, w: 256, h: 256 });
//! let mut tile = vec![0u8; 256 * 256 * 4];
//! r.render_into(0, &req, &mut tile)?;
//! # Ok(()) }
//! ```
//!
//! Pixels are RGBA8, premultiplied unless [`RenderRequest::straight_alpha`] is set (with an
//! opaque background both are the same).

#![feature(portable_simd)]

mod annots;
mod cache;
mod canvas;
pub mod color;
mod draw;
pub mod function;
mod image;
mod list;
mod pattern;
pub mod raster;
mod shading;
mod smask;
mod text;

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};

use pagebara_content::display::FontRef;
use pagebara_content::oc::OptionalContent;
use pagebara_content::{Interpreter, Matrix, Rect};
use pagebara_pages::PageTree;
use pagebara_syntax::{Dict, Document, ObjRef, Object};
use parking_lot::Mutex;

use cache::Lru;
use canvas::Pixmap;
use color::Cs;
use color::icc::{Profile, Transform};
use list::Prepared;
use raster::IRect;
use text::{FontCache, FontKey, GlyphCache};

pub use text::{DefaultGlyphs, GlyphOutlines, GlyphSource, Outline};

/// A rectangle of device pixels of the displayed (rotated) page at some scale, from its
/// top-left corner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PixelRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// Which annotations to draw over the page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Annotations {
    /// Only page content.
    None,
    /// Annotations visible on screen (not `Hidden` or `NoView`).
    #[default]
    View,
    /// Annotations that print (`Print` set, not `Hidden`).
    Print,
    /// As [`Annotations::View`], except these (the app draws annotations it edits).
    ViewExcept(Arc<Vec<ObjRef>>),
}

/// Optional content group overrides on top of the document's default configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct OptionalContentConfig {
    pub on: Vec<ObjRef>,
    pub off: Vec<ObjRef>,
}

impl OptionalContentConfig {
    /// The layers as printing shows them (§8.11.4.4): groups that the default
    /// configuration's `/AS` lists for the `Print` event take their `/Usage /Print
    /// /PrintState`, so print-only stamps print and view-only ones do not.
    pub fn for_print(doc: &Document) -> OptionalContentConfig {
        let mut cfg = OptionalContentConfig::default();
        let Some(cat) = doc.catalog() else { return cfg };
        let Some(cat) = cat.as_dict() else { return cfg };
        let props = doc.get_in(cat, b"OCProperties");
        let Some(props) = props.as_dict() else {
            return cfg;
        };
        let d = doc.get_in(props, b"D");
        let Some(d) = d.as_dict() else { return cfg };
        let auto = doc.get_in(d, b"AS");
        for a in auto.as_array().unwrap_or_default() {
            let a = doc.resolve(a);
            let Some(a) = a.as_dict() else { continue };
            if doc.get_in(a, b"Event").as_name() != Some(b"Print") {
                continue;
            }
            let cats = doc.get_in(a, b"Category");
            let print = match &*cats {
                Object::Name(n) => n.as_bytes() == b"Print",
                Object::Array(c) => c.iter().any(|c| c.as_name() == Some(b"Print")),
                _ => false,
            };
            if !print {
                continue;
            }
            for g in doc.get_in(a, b"OCGs").as_array().unwrap_or_default() {
                let Some(r) = g.as_objref() else { continue };
                let state = doc.get(r).and_then(|o| {
                    let d = o.as_dict()?;
                    let u = doc.get_in(d, b"Usage");
                    let p = doc.get_in(u.as_dict()?, b"Print");
                    doc.get_in(p.as_dict()?, b"PrintState")
                        .as_name()
                        .map(<[u8]>::to_vec)
                });
                match state.as_deref() {
                    Some(b"ON") => cfg.on.push(r),
                    Some(b"OFF") => cfg.off.push(r),
                    _ => {}
                }
            }
        }
        cfg
    }
}

#[derive(Clone, Debug)]
pub struct RenderRequest<'a> {
    /// Device pixels per point (the page's `/UserUnit` is applied on top).
    pub scale: f32,
    /// The pixels to render in the displayed page's device space.
    pub rect: PixelRect,
    /// Clockwise rotation added to the page's `/Rotate` (a multiple of 90).
    pub rotation: u16,
    /// Straight RGBA painted under the page; `None` leaves it transparent.
    pub background: Option<[u8; 4]>,
    pub annotations: Annotations,
    pub optional_content: Option<&'a OptionalContentConfig>,
    /// Checked periodically; when set the render stops with [`RenderError::Cancelled`].
    pub cancel: Option<&'a AtomicBool>,
    /// Output straight instead of premultiplied alpha.
    pub straight_alpha: bool,
}

impl RenderRequest<'_> {
    /// White paper, annotations as on screen.
    pub fn new(scale: f32, rect: PixelRect) -> Self {
        RenderRequest {
            scale,
            rect,
            rotation: 0,
            background: Some([255; 4]),
            annotations: Annotations::View,
            optional_content: None,
            cancel: None,
            straight_alpha: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderError {
    NoSuchPage(usize),
    /// The output buffer is not `w * h * 4` bytes.
    BufferSize {
        expected: usize,
        got: usize,
    },
    Cancelled,
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::NoSuchPage(i) => write!(f, "no page {i}"),
            RenderError::BufferSize { expected, got } => {
                write!(f, "buffer is {got} bytes, expected {expected}")
            }
            RenderError::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for RenderError {}

/// An RGBA8 image.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub premultiplied: bool,
}

/// Cache budgets in bytes.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub display_lists: usize,
    pub images: usize,
    pub glyphs: usize,
    /// Pattern cells and shading meshes.
    pub misc: usize,
}

impl Limits {
    /// The default budgets scaled to a machine with `total` bytes of memory: a sixteenth of
    /// it, between 192 MiB (phones) and the defaults' 960 MiB.
    pub fn for_memory(total: u64) -> Limits {
        let d = Limits::default();
        let all = d.display_lists + d.images + d.glyphs + d.misc;
        let budget = usize::try_from(total / 16)
            .unwrap_or(usize::MAX)
            .clamp(192 << 20, all);
        let part = |b: usize| (b as u128 * budget as u128 / all as u128) as usize;
        Limits {
            display_lists: part(d.display_lists),
            images: part(d.images),
            glyphs: part(d.glyphs),
            misc: part(d.misc),
        }
    }

    /// [`Limits::for_memory`] of this machine (the defaults when its memory is unknown).
    pub fn for_this_machine() -> Limits {
        pagebara_budget::total_memory().map_or_else(Limits::default, Limits::for_memory)
    }
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            display_lists: 256 << 20,
            images: 512 << 20,
            glyphs: 64 << 20,
            misc: 128 << 20,
        }
    }
}

/// Renders the pages of one document; cheap to clone and share across threads.
#[derive(Clone)]
pub struct Renderer {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer")
            .field("pages", &self.inner.pages.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ListKey {
    Page(usize, u64),
    Form(ObjRef, [u32; 6]),
    Type3(FontKey, u32),
    Pattern(ObjRef),
}

type Slot = Arc<OnceLock<Option<Arc<Prepared>>>>;

pub(crate) struct Inner {
    pub doc: Document,
    pages: PageTree,
    pub gray: Arc<Cs>,
    pub rgb: Arc<Cs>,
    pub cmyk: Arc<Cs>,
    pub fonts: FontCache,
    pub glyphs: GlyphCache,
    pub images: image::ImageCache,
    pub misc: Lru<MiscKey, Arc<dyn std::any::Any + Send + Sync>>,
    lists: Lru<ListKey, Slot>,
    spaces: Mutex<HashMap<ObjRef, Option<Arc<Cs>>>>,
    icc: Mutex<HashMap<u64, Option<Arc<Transform>>>>,
}

/// Keys of the shared miscellaneous cache.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum MiscKey {
    Shading(ObjRef),
    Cell(ObjRef, [i32; 4], [u8; 4]),
}

impl Renderer {
    pub fn new(doc: Document) -> Self {
        Renderer::with_options(doc, Limits::default(), Arc::new(DefaultGlyphs))
    }

    /// A renderer with explicit cache budgets and glyph source.
    pub fn with_options(doc: Document, limits: Limits, glyphs: Arc<dyn GlyphSource>) -> Self {
        let pages = PageTree::open(&doc);
        Renderer {
            inner: Arc::new(Inner {
                doc,
                pages,
                gray: Arc::new(Cs::Gray),
                rgb: Arc::new(Cs::Rgb),
                cmyk: Arc::new(Cs::Cmyk),
                fonts: FontCache::new(glyphs),
                glyphs: GlyphCache::new(limits.glyphs),
                images: image::ImageCache::new(limits.images),
                misc: Lru::new(limits.misc),
                lists: Lru::new(limits.display_lists),
                spaces: Mutex::new(HashMap::new()),
                icc: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn document(&self) -> &Document {
        &self.inner.doc
    }

    pub fn page_count(&self) -> usize {
        self.inner.pages.len()
    }

    /// The displayed size of a page in points (crop box, user unit and rotation applied).
    pub fn page_size(&self, page: usize) -> Option<(f32, f32)> {
        let p = self.inner.pages.page(page)?;
        let (w, h) = p.size();
        Some((w as f32, h as f32))
    }

    /// Renders `req.rect` of a page into `out` (`w * h * 4` bytes of RGBA8).
    pub fn render_into(
        &self,
        page: usize,
        req: &RenderRequest<'_>,
        out: &mut [u8],
    ) -> Result<(), RenderError> {
        let expected = req.rect.w as usize * req.rect.h as usize * 4;
        if out.len() != expected {
            return Err(RenderError::BufferSize {
                expected,
                got: out.len(),
            });
        }
        let pix = self.inner.render(page, req)?;
        out.copy_from_slice(&pix.data);
        if req.straight_alpha {
            unpremultiply(out);
        }
        Ok(())
    }

    pub fn render(&self, page: usize, req: &RenderRequest<'_>) -> Result<RgbaImage, RenderError> {
        let pix = self.inner.render(page, req)?;
        let mut data = pix.data;
        if req.straight_alpha {
            unpremultiply(&mut data);
        }
        Ok(RgbaImage {
            width: req.rect.w,
            height: req.rect.h,
            data,
            premultiplied: !req.straight_alpha,
        })
    }

    /// The whole page fitted into `max` × `max` pixels on white.
    pub fn thumbnail(&self, page: usize, max: u32) -> Result<RgbaImage, RenderError> {
        let (w, h) = self.page_size(page).ok_or(RenderError::NoSuchPage(page))?;
        let scale = max as f32 / w.max(h).max(1.0);
        let rect = PixelRect {
            x: 0,
            y: 0,
            w: ((w * scale).round() as u32).max(1),
            h: ((h * scale).round() as u32).max(1),
        };
        self.render(page, &RenderRequest::new(scale, rect))
    }

    /// Drops cached display lists, images, glyphs and pattern cells.
    pub fn clear_caches(&self) {
        self.inner.lists.clear();
        self.inner.images.clear();
        self.inner.misc.clear();
    }
}

impl Inner {
    fn render(&self, page: usize, req: &RenderRequest<'_>) -> Result<Pixmap, RenderError> {
        let p = self.pages.page(page).ok_or(RenderError::NoSuchPage(page))?;
        let crop = Rect::new(
            p.crop_box.x0 as f32,
            p.crop_box.y0 as f32,
            p.crop_box.x1 as f32,
            p.crop_box.y1 as f32,
        );
        let rotate = (p.rotate + req.rotation) % 360;
        let scale = req.scale * p.user_unit as f32;
        let base = page_matrix(crop, rotate, scale, req.rect.x, req.rect.y);
        let rect = IRect::new(0, 0, req.rect.w as i32, req.rect.h as i32);
        let bg = req.background.map_or([0; 4], premultiply);
        let target = Pixmap::filled(rect, bg);
        let oc_hash = req.optional_content.map_or(0, hash_of);
        let list = self.page_list(page, p.r, req.optional_content, oc_hash);
        let mut ctx = draw::Ctx::new(self, target, req.cancel);
        ctx.push_clip_rect(crop, &base);
        if let Some(l) = &list {
            ctx.run(l, base, None);
        }
        if req.annotations != Annotations::None {
            let oc = self.optional_content(req.optional_content);
            for (alist, m) in annots::page_annotations(self, page, p.r, &req.annotations, &oc) {
                ctx.run(&alist, m.then(&base), None);
            }
        }
        let cancelled = ctx.cancelled;
        let pix = ctx.finish();
        if cancelled {
            return Err(RenderError::Cancelled);
        }
        Ok(pix)
    }

    /// The document's default optional content configuration with `oc` applied.
    fn optional_content(&self, oc: Option<&OptionalContentConfig>) -> OptionalContent {
        let mut v = OptionalContent::new(&self.doc);
        for (groups, on) in oc.iter().flat_map(|c| [(&c.on, true), (&c.off, false)]) {
            for &g in groups {
                v.set_group(g, on);
            }
        }
        v
    }

    fn page_list(
        &self,
        page: usize,
        r: ObjRef,
        oc: Option<&OptionalContentConfig>,
        oc_hash: u64,
    ) -> Option<Arc<Prepared>> {
        let key = ListKey::Page(page, oc_hash);
        self.list(key, || {
            let doc = &self.doc;
            let o = doc.get(r)?;
            let d = o.as_dict()?;
            let res = inherited(doc, d, b"Resources")
                .and_then(|o| doc.resolve(&o).as_dict().cloned())
                .unwrap_or_default();
            let mut it = Interpreter::new(doc).with_optional_content(self.optional_content(oc));
            Some(it.page(d, &res))
        })
    }

    fn list(
        &self,
        key: ListKey,
        build: impl FnOnce() -> Option<pagebara_content::DisplayList>,
    ) -> Option<Arc<Prepared>> {
        let slot = match self.lists.get(&key) {
            Some(s) => s,
            None => {
                let s: Slot = Arc::default();
                self.lists.insert(key.clone(), s.clone(), 256);
                s
            }
        };
        let mut fresh = false;
        let v = slot
            .get_or_init(|| {
                fresh = true;
                build().map(Prepared::new)
            })
            .clone();
        if fresh {
            let size = v.as_ref().map_or(64, |p| p.heap);
            self.lists.resize(&key, size);
        }
        v
    }

    /// A form XObject as its own list (soft mask groups, annotation appearances).
    pub fn form_list(&self, o: &Object, ctm: Matrix) -> Option<Arc<Prepared>> {
        let build = || {
            let s = self.doc.resolve(o);
            let s = s.as_stream()?;
            let mut it = Interpreter::new(&self.doc);
            Some(it.form(s, &Dict::new(), ctm))
        };
        match o.as_objref() {
            Some(r) => self.list(ListKey::Form(r, draw::base_bits(&ctm)), build),
            None => build().map(Prepared::new),
        }
    }

    /// A tiling pattern's cell content in pattern space.
    pub fn pattern_list(&self, o: &Object) -> Option<Arc<Prepared>> {
        let build = || {
            let s = self.doc.resolve(o);
            let s = s.as_stream()?;
            let data = self.doc.decode(s).ok()?.data;
            let res = self.doc.get_in(&s.dict, b"Resources");
            let res = res.as_dict().cloned().unwrap_or_default();
            let mut it = Interpreter::new(&self.doc);
            Some(it.content(&data, &res, Matrix::IDENTITY))
        };
        match o.as_objref() {
            Some(r) => self.list(ListKey::Pattern(r), build),
            None => build().map(Prepared::new),
        }
    }

    /// The glyph description of a Type 3 code.
    pub fn type3(&self, f: &FontRef, code: u32) -> Option<Arc<Prepared>> {
        self.list(ListKey::Type3(FontKey::of(f), code), || {
            let mut it = Interpreter::new(&self.doc);
            it.type3_glyph(&f.font, code, &Dict::new())
        })
    }

    pub fn resolve_space(&self, o: &Object) -> Option<Arc<Cs>> {
        if let Some(r) = o.as_objref()
            && let Some(c) = self.spaces.lock().get(&r)
        {
            return c.clone();
        }
        let icc = |data: &[u8]| self.icc_transform(data);
        let res = color::Resolver {
            doc: &self.doc,
            icc: &icc,
        };
        let cs = res.resolve(o).map(Arc::new);
        if let Some(r) = o.as_objref() {
            self.spaces.lock().insert(r, cs.clone());
        }
        cs
    }

    pub fn icc_transform(&self, data: &[u8]) -> Option<Arc<Transform>> {
        let key = {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            data.hash(&mut h);
            h.finish()
        };
        if let Some(t) = self.icc.lock().get(&key) {
            return t.clone();
        }
        let t = Profile::parse(data).map(|p| Transform::new(&p));
        self.icc.lock().insert(key, t.clone());
        t
    }
}

/// Default user space of a page to device pixels of `rect` at `scale`.
pub fn page_matrix(crop: Rect, rotate: u16, scale: f32, x: i32, y: i32) -> Matrix {
    let (w, h) = (crop.width(), crop.height());
    let flip = Matrix::new(1.0, 0.0, 0.0, -1.0, -crop.x0, crop.y1);
    let rot = match rotate {
        90 => Matrix::new(0.0, 1.0, -1.0, 0.0, h, 0.0),
        180 => Matrix::new(-1.0, 0.0, 0.0, -1.0, w, h),
        270 => Matrix::new(0.0, -1.0, 1.0, 0.0, 0.0, w),
        _ => Matrix::IDENTITY,
    };
    flip.then(&rot)
        .then(&Matrix::scale(scale, scale))
        .then(&Matrix::translate(-x as f32, -y as f32))
}

/// An inheritable page attribute.
fn inherited(doc: &Document, page: &Dict, key: &[u8]) -> Option<Object> {
    let mut d = page.clone();
    for _ in 0..64 {
        if let Some(v) = d.get(key) {
            return Some(v.clone());
        }
        let parent = doc.get_in(&d, b"Parent");
        d = parent.as_dict()?.clone();
    }
    None
}

fn premultiply(c: [u8; 4]) -> [u8; 4] {
    let a = u16::from(c[3]);
    [
        ((u16::from(c[0]) * a + 127) / 255) as u8,
        ((u16::from(c[1]) * a + 127) / 255) as u8,
        ((u16::from(c[2]) * a + 127) / 255) as u8,
        c[3],
    ]
}

fn unpremultiply(data: &mut [u8]) {
    for px in data.as_chunks_mut::<4>().0 {
        let a = px[3];
        if a != 0 && a != 255 {
            for c in &mut px[..3] {
                *c = ((u16::from(*c) * 255 + u16::from(a) / 2) / u16::from(a)).min(255) as u8;
            }
        }
    }
}

fn hash_of<T: Hash>(v: T) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish() | 1
}
