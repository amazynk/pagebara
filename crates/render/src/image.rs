//! Images (§8.9): decoded once per document (at reduced resolution when only shown small)
//! to premultiplied RGBA or an alpha stencil, then sampled through a box-filtered mip chain.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

use pagebara_content::display::{FillRule, Id, ImageSource};
use pagebara_content::{Matrix, Point};
use pagebara_syntax::{Dict, ObjRef, Object, Stream};

use crate::Inner;
use crate::cache::Lru;
use crate::color::{Cs, rgb8};
use crate::draw::{Ctx, Resolved, Shader, Source};
use crate::function::numbers;
use crate::list::Prepared;
use crate::raster::{Edges, Flattener};

/// Decoded pixels are capped at this many.
const MAX_PIXELS: u64 = 1 << 28;
/// Colour enlarged to the resolution of its mask is capped at this many pixels.
const FIT_PIXELS: u64 = 1 << 24;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Key {
    Ref(ObjRef, u8),
    Inline(u64, u32),
    /// Box-filtered to an exact device size.
    Scaled(Box<Key>, u32, u32),
}

type Slot = Arc<OnceLock<Option<Arc<Decoded>>>>;

#[derive(Debug)]
pub struct ImageCache {
    lru: Lru<Key, Arc<Decoded>>,
    /// Decodes in progress, so concurrent tiles wait for one decode.
    pending: Mutex<HashMap<Key, Slot>>,
}

impl ImageCache {
    pub fn new(budget: usize) -> Self {
        ImageCache {
            lru: Lru::new(budget),
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// The cached value of `k`, or `make`'s, computed once even when asked concurrently.
    /// A `make` that panics leaves the image undrawn.
    ///
    /// When `make` may run rayon jobs (`forks`) and this is a rayon worker, it runs on a
    /// thread of its own: the worker waiting inside the once-cell could otherwise steal
    /// another tile needing the same image and re-enter the cell. Other decodes run here:
    /// a thread per small image costs more than decoding it.
    fn get_or(
        &self,
        k: &Key,
        forks: bool,
        make: impl FnOnce() -> Option<Decoded> + Send,
    ) -> Option<Arc<Decoded>> {
        if let Some(d) = self.lru.get(k) {
            return Some(d);
        }
        let slot = self.pending.lock().entry(k.clone()).or_default().clone();
        let mut fresh = false;
        let d = slot
            .get_or_init(|| {
                fresh = true;
                if forks && rayon::current_thread_index().is_some() {
                    std::thread::scope(|s| {
                        pagebara_thread::spawn_scoped(s, "ren-decode", make)
                            .join()
                            .unwrap_or(None)
                    })
                } else {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(make)).unwrap_or(None)
                }
                .map(Arc::new)
            })
            .clone();
        if fresh {
            if let Some(d) = &d {
                let bytes = d.base.data.len() * 4 / 3 + 256;
                self.lru.insert(k.clone(), d.clone(), bytes);
            }
            self.pending.lock().remove(k);
        }
        d
    }

    pub fn clear(&self) {
        self.lru.clear();
    }
}

#[derive(Debug)]
pub struct Level {
    pub w: u32,
    pub h: u32,
    /// Premultiplied RGBA; one byte per pixel for stencils (alpha) and gray images.
    pub data: Vec<u8>,
}

#[derive(Debug)]
pub struct Decoded {
    /// Level 0 at the decoded resolution.
    pub base: Level,
    /// Stencil mask: one alpha byte per pixel.
    pub stencil: bool,
    /// Opaque gray: one byte per pixel.
    pub gray: bool,
    pub interpolate: bool,
    mips: OnceLock<Vec<Level>>,
}

impl Decoded {
    fn bpp(&self) -> usize {
        if self.stencil || self.gray { 1 } else { 4 }
    }

    /// Level `l` (0 = base); halved by box filtering on first use.
    fn level(&self, l: usize) -> &Level {
        if l == 0 {
            return &self.base;
        }
        let mips = self.mips.get_or_init(|| {
            let mut v: Vec<Level> = Vec::new();
            let bpp = self.bpp();
            loop {
                let prev = v.last().unwrap_or(&self.base);
                if prev.w <= 1 && prev.h <= 1 || v.len() >= 16 {
                    break;
                }
                v.push(halve(prev, bpp));
            }
            v
        });
        mips.get(l - 1).or(mips.last()).unwrap_or(&self.base)
    }
}

fn halve(l: &Level, bpp: usize) -> Level {
    let (w, h) = (l.w.div_ceil(2).max(1), l.h.div_ceil(2).max(1));
    let mut data = vec![0u8; w as usize * h as usize * bpp];
    let sw = l.w as usize;
    for y in 0..h as usize {
        let y0 = (2 * y).min(l.h as usize - 1);
        let y1 = (2 * y + 1).min(l.h as usize - 1);
        for x in 0..w as usize {
            let x0 = (2 * x).min(sw - 1);
            let x1 = (2 * x + 1).min(sw - 1);
            for c in 0..bpp {
                let s = l.data[(y0 * sw + x0) * bpp + c] as u32
                    + l.data[(y0 * sw + x1) * bpp + c] as u32
                    + l.data[(y1 * sw + x0) * bpp + c] as u32
                    + l.data[(y1 * sw + x1) * bpp + c] as u32;
                data[(y * w as usize + x) * bpp + c] = ((s + 2) / 4) as u8;
            }
        }
    }
    Level { w, h, data }
}

/// Draws image `id` of a list into the unit square of `m`.
pub fn draw(ctx: &mut Ctx<'_>, p: &Arc<Prepared>, id: Id, m: Matrix, paint: &Resolved) {
    let Some(img) = p.list.images.get(id as usize) else {
        return;
    };
    if img.width == 0 || img.height == 0 {
        return;
    }
    if !img.is_mask && matches!(paint.src, Source::Nothing) {
        return;
    }
    let dw = (m.a * m.a + m.b * m.b).sqrt();
    let dh = (m.c * m.c + m.d * m.d).sqrt();
    let Some((dec, key)) = decoded(ctx.r, p, id, (dw, dh)) else {
        return;
    };
    // Axis-aligned images cover whole device pixels and are box-filtered to their exact
    // size when shrunk.
    let axis = m.b.abs() < 1e-4 && m.c.abs() < 1e-4;
    let swapped = m.a.abs() < 1e-4 && m.d.abs() < 1e-4;
    let m = if axis || swapped {
        gridfit(m, swapped)
    } else {
        m
    };
    let (tw, th) = (
        (m.a.abs() + m.b.abs()).round() as u32,
        (m.c.abs() + m.d.abs()).round() as u32,
    );
    let exact = (axis || swapped)
        && tw > 0
        && th > 0
        && tw <= dec.base.w
        && th <= dec.base.h
        && (tw, th) != (dec.base.w, dec.base.h);
    let dec = if exact {
        scaled(ctx.r, key, &dec, tw, th)
    } else {
        dec
    };
    // Sample relative to a whole-pixel origin so tiles of the same page agree exactly.
    let origin = (m.e.floor(), m.f.floor());
    let rel = Matrix {
        e: m.e - origin.0,
        f: m.f - origin.1,
        ..m
    };
    let Some(inv) = rel.invert() else { return };
    let (w, h) = (dec.base.w as f32, dec.base.h as f32);
    let to_img = inv.then(&Matrix::new(w, 0.0, 0.0, -h, 0.0, h));
    let s = (to_img.a * to_img.a + to_img.b * to_img.b)
        .sqrt()
        .max((to_img.c * to_img.c + to_img.d * to_img.d).sqrt());
    let level = if s >= 2.0 && !exact {
        (s.log2().floor() as usize).min(15)
    } else {
        0
    };
    let smooth = !exact && (s >= 1.001 || dec.interpolate);
    let sampler = Sampler {
        img: dec.clone(),
        level,
        to_img,
        origin: (origin.0 as i32, origin.1 as i32),
        smooth,
    };
    let paint = &match &paint.src {
        Source::Tiling(t) if dec.stencil => ctx.untile(paint, &t.clone()),
        _ => paint.clone(),
    };
    let src: Arc<dyn Shader> = if dec.stencil {
        match &paint.src {
            Source::Nothing => return,
            s => Arc::new(Stencil {
                sampler,
                color: s.clone(),
            }),
        }
    } else {
        Arc::new(sampler)
    };
    let r = Resolved {
        src: Source::Shader(src),
        alpha: if dec.stencil { 255 } else { paint.alpha },
        blend: paint.blend,
        smask: paint.smask.clone(),
    };
    let mut e = Edges::new();
    let mut f = Flattener::new(&mut e, m);
    f.move_to(Point::new(0.0, 0.0));
    f.line_to(Point::new(1.0, 0.0));
    f.line_to(Point::new(1.0, 1.0));
    f.line_to(Point::new(0.0, 1.0));
    f.close();
    // Snap near-integer device edges so pixel-aligned images are not blurred at edges.
    for l in &mut e.lines {
        for v in [&mut l.x0, &mut l.y0, &mut l.x1, &mut l.y1] {
            let r = v.round();
            if (*v - r).abs() < 1e-3 {
                *v = r;
            }
        }
    }
    ctx.fill(&e, FillRule::NonZero, &r);
}

struct Sampler {
    img: Arc<Decoded>,
    level: usize,
    /// Device pixels relative to `origin` → base image pixels.
    to_img: Matrix,
    origin: (i32, i32),
    smooth: bool,
}

impl Sampler {
    /// Samples into `out` with `bpp` bytes per pixel.
    fn sample(&self, x: i32, y: i32, out: &mut [u8], bpp: usize) {
        let lv = self.img.level(self.level);
        let (sx, sy) = (
            lv.w as f32 / (self.img.base.w as f32),
            lv.h as f32 / (self.img.base.h as f32),
        );
        let m = self.to_img;
        let p0 = m.apply(Point::new(
            (x - self.origin.0) as f32 + 0.5,
            (y - self.origin.1) as f32 + 0.5,
        ));
        let (du, dv) = (m.a * sx, m.b * sy);
        let (u0, v0) = (p0.x * sx, p0.y * sy);
        let (w, h) = (lv.w as i32, lv.h as i32);
        let n = out.len() / bpp;
        for i in 0..n {
            let u = u0 + du * i as f32;
            let v = v0 + dv * i as f32;
            let o = &mut out[i * bpp..(i + 1) * bpp];
            if self.smooth {
                let (fu, fv) = (u - 0.5, v - 0.5);
                let (xf, yf) = (fu.floor(), fv.floor());
                let (tx, ty) = (fu - xf, fv - yf);
                let x0 = (xf as i32).clamp(0, w - 1) as usize;
                let x1 = (xf as i32 + 1).clamp(0, w - 1) as usize;
                let y0 = (yf as i32).clamp(0, h - 1) as usize;
                let y1 = (yf as i32 + 1).clamp(0, h - 1) as usize;
                let wu = w as usize;
                let d = &lv.data;
                for c in 0..bpp {
                    let a = d[(y0 * wu + x0) * bpp + c] as f32;
                    let b = d[(y0 * wu + x1) * bpp + c] as f32;
                    let cc = d[(y1 * wu + x0) * bpp + c] as f32;
                    let dd = d[(y1 * wu + x1) * bpp + c] as f32;
                    let top = a + (b - a) * tx;
                    let bot = cc + (dd - cc) * tx;
                    o[c] = (top + (bot - top) * ty + 0.5) as u8;
                }
            } else {
                let xi = (u.floor() as i32).clamp(0, w - 1) as usize;
                let yi = (v.floor() as i32).clamp(0, h - 1) as usize;
                let s = (yi * w as usize + xi) * bpp;
                o.copy_from_slice(&lv.data[s..s + bpp]);
            }
        }
    }
}

impl Shader for Sampler {
    fn shade(&self, x: i32, y: i32, out: &mut [u8]) {
        if self.img.gray {
            let n = out.len() / 4;
            self.sample(x, y, &mut out[3 * n..], 1);
            for i in 0..n {
                let g = out[3 * n + i];
                out[i * 4..i * 4 + 4].copy_from_slice(&[g, g, g, 255]);
            }
        } else {
            self.sample(x, y, out, 4);
        }
    }
}

struct Stencil {
    sampler: Sampler,
    color: Source,
}

impl Shader for Stencil {
    fn shade(&self, x: i32, y: i32, out: &mut [u8]) {
        let n = out.len() / 4;
        let mut a = vec![0u8; n];
        self.sampler.sample(x, y, &mut a, 1);
        match &self.color {
            Source::Solid(c) => {
                for (px, &a) in out.as_chunks_mut::<4>().0.iter_mut().zip(&a) {
                    *px = c.map(|v| crate::raster::mask::mul8(v, a));
                }
            }
            Source::Shader(s) => {
                s.shade(x, y, out);
                for (px, &a) in out.as_chunks_mut::<4>().0.iter_mut().zip(&a) {
                    *px = px.map(|v| crate::raster::mask::mul8(v, a));
                }
            }
            Source::Nothing | Source::Tiling(_) => out.fill(0),
        }
    }
}

/// Snaps an axis-aligned (or quarter-turned) image placement to whole pixels.
fn gridfit(m: Matrix, swapped: bool) -> Matrix {
    let snap = |o: f32, l: f32| -> (f32, f32) {
        let (lo, hi) = if l >= 0.0 { (o, o + l) } else { (o + l, o) };
        let lo2 = (lo + 0.01).floor();
        let hi2 = (hi - 0.01).ceil().max(lo2 + 1.0);
        if l >= 0.0 {
            (lo2, hi2 - lo2)
        } else {
            (hi2, lo2 - hi2)
        }
    };
    if swapped {
        let (e, c) = snap(m.e, m.c);
        let (f, b) = snap(m.f, m.b);
        Matrix::new(0.0, b, c, 0.0, e, f)
    } else {
        let (e, a) = snap(m.e, m.a);
        let (f, d) = snap(m.f, m.d);
        Matrix::new(a, 0.0, 0.0, d, e, f)
    }
}

/// The image box-filtered to `w` × `h`, cached.
fn scaled(r: &Inner, key: Key, dec: &Arc<Decoded>, w: u32, h: u32) -> Arc<Decoded> {
    let k = Key::Scaled(Box::new(key), w, h);
    let made = r.images.get_or(&k, false, || {
        let bpp = dec.bpp();
        let data = resample(&dec.base.data, dec.base.w, dec.base.h, bpp, w, h);
        Some(Decoded {
            base: Level { w, h, data },
            stencil: dec.stencil,
            gray: dec.gray,
            interpolate: false,
            mips: OnceLock::new(),
        })
    });
    made.unwrap_or_else(|| dec.clone())
}

/// The decoded image, from the cache or decoded for a display of `target` device pixels.
fn decoded(r: &Inner, p: &Prepared, id: Id, target: (f32, f32)) -> Option<(Arc<Decoded>, Key)> {
    let img = &p.list.images[id as usize];
    let (stream, key): (Arc<Stream>, Key) = match &img.source {
        ImageSource::XObject(o) => {
            let s = r.doc.resolve(o);
            let s = Arc::new(s.as_stream()?.clone());
            match o.as_objref() {
                Some(rf) => (s, Key::Ref(rf, 0)),
                None => (s, Key::Inline(p.id, id)),
            }
        }
        ImageSource::Inline(s) => (s.clone(), Key::Inline(p.id, id)),
    };
    // Reduced decodes only pay off for DCT/JPX; key them by the reduction.
    let reduce = reduction(img.width, img.height, target);
    let keys: Vec<Key> = match &key {
        Key::Ref(rf, _) => (0..=reduce).rev().map(|k| Key::Ref(*rf, k)).collect(),
        k => vec![k.clone()],
    };
    for k in keys.iter().rev() {
        if let Some(d) = r.images.lru.get(k) {
            return Some((d, k.clone()));
        }
    }
    let k = match key {
        Key::Ref(rf, _) => Key::Ref(rf, reduce),
        k => k,
    };
    let d = r
        .images
        .get_or(&k, forks(r, &stream), || decode(r, &stream, reduce))?;
    Some((d, k))
}

/// log2 of the decode reduction for an image shown at `target` pixels.
fn reduction(w: u32, h: u32, target: (f32, f32)) -> u8 {
    let fx = w as f32 / target.0.max(1.0);
    let fy = h as f32 / target.1.max(1.0);
    let f = fx.min(fy);
    if f >= 8.0 {
        3
    } else if f >= 4.0 {
        2
    } else if f >= 2.0 {
        1
    } else {
        0
    }
}

/// Samples with `n` components per pixel at 8 bits (16-bit images keep the high byte), or
/// the raw packed data for other depths.
struct Raw {
    w: u32,
    h: u32,
    n: usize,
    bpc: u32,
    /// Unpacked: one byte per component, row-major.
    data: Vec<u8>,
    /// Alpha straight from the codec (JPX).
    alpha: Option<Vec<u8>>,
    /// Data was already converted to sRGB by the codec.
    rgb: bool,
}

/// Whether decoding `s` may run rayon jobs: the DCT and JPX codecs do, and so does
/// unfiltering the predictor of a large image; its masks count too.
fn forks(r: &Inner, s: &Stream) -> bool {
    let own = |d: &Dict| {
        let parallel = |f: &Object| {
            matches!(
                f.as_name(),
                Some(b"DCTDecode" | b"DCT" | b"JPXDecode" | b"JPX")
            )
        };
        let codec = match &*r.doc.get_in(d, b"Filter") {
            Object::Array(a) => a.iter().any(parallel),
            f => parallel(f),
        };
        let w = int(r, d, b"Width").unwrap_or(0).clamp(0, 1 << 20);
        let h = int(r, d, b"Height").unwrap_or(0).clamp(0, 1 << 20);
        codec || w * h >= 1 << 17
    };
    let mask = |k: &[u8]| {
        r.doc
            .get_in(&s.dict, k)
            .as_stream()
            .is_some_and(|m| own(&m.dict))
    };
    own(&s.dict) || mask(b"SMask") || mask(b"Mask")
}

fn int(r: &Inner, d: &Dict, k: &[u8]) -> Option<i64> {
    r.doc.get_in(d, k).as_int()
}

fn flag(r: &Inner, d: &Dict, k: &[u8]) -> bool {
    r.doc.get_in(d, k).as_bool() == Some(true)
}

pub fn decode(r: &Inner, s: &Stream, reduce: u8) -> Option<Decoded> {
    let d = &s.dict;
    let doc = &r.doc;
    let w = int(r, d, b"Width")?.clamp(0, 1 << 20) as u32;
    let h = int(r, d, b"Height")?.clamp(0, 1 << 20) as u32;
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return None;
    }
    let interpolate = flag(r, d, b"Interpolate");
    let stencil = flag(r, d, b"ImageMask");
    if stencil {
        let raw = samples(r, s, w, h, 1, 1, 0)?;
        let inv = decode_inverted(r, d);
        // Sample 0 paints unless Decode is [1 0].
        let data: Vec<u8> = raw
            .data
            .iter()
            .map(|&v| if (v == 0) != inv { 255 } else { 0 })
            .collect();
        return Some(Decoded {
            base: Level {
                w: raw.w,
                h: raw.h,
                data,
            },
            stencil: true,
            gray: false,
            interpolate,
            mips: OnceLock::new(),
        });
    }
    let cs_obj = d.get(b"ColorSpace").cloned();
    let filter_jpx = is_filter(r, d, b"JPXDecode");
    let cs: Arc<Cs> = match &cs_obj {
        Some(o) => r.resolve_space(o).unwrap_or_else(|| r.gray.clone()),
        None if filter_jpx => r.rgb.clone(),
        None => r.gray.clone(),
    };
    let n = cs.components().max(1);
    let bpc = match int(r, d, b"BitsPerComponent") {
        Some(b) if [1, 2, 4, 8, 16].contains(&b) => b as u32,
        _ => 8,
    };
    let raw = samples(r, s, w, h, n, bpc, reduce)?;
    // A JPX image without /ColorSpace takes its colours from the file: grey for one
    // component (assuming RGB tinted grey images red).
    let jpx_own = cs_obj.is_none() && filter_jpx;
    let (n, cs) = if raw.rgb || jpx_own {
        (
            raw.n,
            if raw.n == 1 {
                r.gray.clone()
            } else {
                r.rgb.clone()
            },
        )
    } else {
        (raw.n, cs)
    };
    let px = raw.w as usize * raw.h as usize;
    // Decode array per component (on raw sample values scaled to 0..1).
    let decode: Vec<(f32, f32)> = match numbers(doc, d, b"Decode") {
        Some(v) if v.len() >= 2 * n && !raw.rgb => {
            (0..n).map(|i| (v[2 * i], v[2 * i + 1])).collect()
        }
        _ => (0..n).map(|i| cs.decode_range(i, raw.bpc)).collect(),
    };
    let has_alpha = d.contains(b"SMask")
        || d.contains(b"Mask")
        || (raw.alpha.is_some() && int(r, d, b"SMaskInData").unwrap_or(0) != 0);
    if n == 1
        && !has_alpha
        && let Some(lut) = gray_lut(&cs, &raw, decode[0])
    {
        return Some(Decoded {
            base: Level {
                w: raw.w,
                h: raw.h,
                data: raw.data.iter().map(|&v| lut[v as usize]).collect(),
            },
            stencil: false,
            gray: true,
            interpolate,
            mips: OnceLock::new(),
        });
    }
    let mut rgba = vec![0u8; px * 4];
    convert(&cs, &raw, &decode, &mut rgba);
    let mut size = (raw.w, raw.h);
    // Alpha sources.
    let mut alpha: Option<Vec<u8>> = None;
    if let Some(a) = &raw.alpha
        && int(r, d, b"SMaskInData").unwrap_or(0) != 0
    {
        alpha = Some(a.clone());
    }
    let mut matte: Option<[u8; 3]> = None;
    if let Some(sm) = d.get(b"SMask") {
        let sm = doc.resolve(sm);
        if let Some(ss) = sm.as_stream() {
            if let Some(m) = numbers(doc, &ss.dict, b"Matte").filter(|m| m.len() == n) {
                matte = Some(rgb8(cs.to_rgb(&m)));
            }
            alpha = soft_mask(r, ss).map(|a| fit(&mut rgba, &mut size, a));
        }
    } else if let Some(mk) = d.get(b"Mask") {
        let mk = doc.resolve(mk);
        match &*mk {
            Object::Stream(ms) => {
                alpha = stencil_alpha(r, ms).map(|a| fit(&mut rgba, &mut size, a))
            }
            Object::Array(a) => {
                let ranges: Vec<i64> = a.iter().filter_map(|o| doc.resolve(o).as_int()).collect();
                if ranges.len() >= 2 * n && raw.bpc <= 8 {
                    let max = (1i64 << raw.bpc) - 1;
                    let scale = |v: u8| -> i64 {
                        if raw.bpc == 8 {
                            i64::from(v)
                        } else {
                            i64::from(v) * max / 255
                        }
                    };
                    let mut al = vec![255u8; px];
                    for (i, a) in al.iter_mut().enumerate() {
                        let px = &raw.data[i * n..(i + 1) * n];
                        if px
                            .iter()
                            .enumerate()
                            .all(|(k, &v)| (ranges[2 * k]..=ranges[2 * k + 1]).contains(&scale(v)))
                        {
                            *a = 0;
                        }
                    }
                    alpha = Some(al);
                }
            }
            _ => {}
        }
    }
    if let Some(a) = alpha {
        for (p, &a) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(&a) {
            if let Some(m) = matte
                && a != 0
            {
                // Un-premultiply against the matte colour: c = m + (c - m) / a.
                for k in 0..3 {
                    let c = p[k] as f32;
                    let mm = m[k] as f32;
                    p[k] = (mm + (c - mm) * 255.0 / a as f32).clamp(0.0, 255.0) as u8;
                }
            }
            p[3] = a;
            if a != 255 {
                for c in &mut p[..3] {
                    *c = crate::raster::mask::mul8(*c, a);
                }
            }
        }
    }
    Some(Decoded {
        base: Level {
            w: size.0,
            h: size.1,
            data: rgba,
        },
        stencil: false,
        gray: false,
        interpolate,
        mips: OnceLock::new(),
    })
}

/// Brings colour and a mask (given at the image size and at its own size `mw` × `mh`) to
/// the finer of both resolutions; returns the mask at the result size.
/// The colour is enlarged to at most [`FIT_PIXELS`] (never below its own size): a tiny
/// image with a huge mask would otherwise grow to gigabytes.
fn fit(rgba: &mut Vec<u8>, size: &mut (u32, u32), (a, mw, mh): (Vec<u8>, u32, u32)) -> Vec<u8> {
    let (w, h) = *size;
    let (mut tw, mut th) = (w.max(mw), h.max(mh));
    let area = f64::from(tw) * f64::from(th);
    if area > FIT_PIXELS as f64 && (tw, th) != (w, h) {
        let k = (FIT_PIXELS as f64 / area).sqrt();
        tw = ((f64::from(tw) * k) as u32).max(w).max(1);
        th = ((f64::from(th) * k) as u32).max(h).max(1);
    }
    if (tw, th) != (w, h) {
        *rgba = resample(rgba, w, h, 4, tw, th);
        *size = (tw, th);
    }
    resample(&a, mw, mh, 1, tw, th)
}

fn decode_inverted(r: &Inner, d: &Dict) -> bool {
    numbers(&r.doc, d, b"Decode").is_some_and(|v| v.first().copied() == Some(1.0))
}

fn is_filter(r: &Inner, d: &Dict, name: &[u8]) -> bool {
    match &*r.doc.get_in(d, b"Filter") {
        Object::Name(n) => n.as_bytes() == name,
        Object::Array(a) => {
            a.last()
                .and_then(|o| r.doc.resolve(o).as_name().map(|n| n == name))
                == Some(true)
        }
        _ => false,
    }
}

/// An `/SMask` image's values as alpha, resampled to `w` × `h`.
fn soft_mask(r: &Inner, s: &Stream) -> Option<(Vec<u8>, u32, u32)> {
    let d = &s.dict;
    let mw = int(r, d, b"Width")?.clamp(1, 1 << 20) as u32;
    let mh = int(r, d, b"Height")?.clamp(1, 1 << 20) as u32;
    let bpc = match int(r, d, b"BitsPerComponent") {
        Some(b) if [1, 2, 4, 8, 16].contains(&b) => b as u32,
        _ => 8,
    };
    let raw = samples(r, s, mw, mh, 1, bpc, 0)?;
    let (lo, hi) = numbers(&r.doc, d, b"Decode")
        .filter(|v| v.len() >= 2)
        .map_or((0.0, 1.0), |v| (v[0], v[1]));
    let max = if raw.bpc >= 8 {
        255.0
    } else {
        ((1u32 << raw.bpc) - 1) as f32
    };
    let lut: Vec<u8> = (0..256)
        .map(|v| {
            let x = (v as f32).min(max) / max;
            ((lo + x * (hi - lo)).clamp(0.0, 1.0) * 255.0 + 0.5) as u8
        })
        .collect();
    let vals: Vec<u8> = raw.data.iter().map(|&v| lut[v as usize]).collect();
    Some((vals, raw.w, raw.h))
}

/// A `/Mask` stencil image as alpha (sample 1 masks out unless `/Decode [1 0]`).
fn stencil_alpha(r: &Inner, s: &Stream) -> Option<(Vec<u8>, u32, u32)> {
    let d = &s.dict;
    let mw = int(r, d, b"Width")?.clamp(1, 1 << 20) as u32;
    let mh = int(r, d, b"Height")?.clamp(1, 1 << 20) as u32;
    let raw = samples(r, s, mw, mh, 1, 1, 0)?;
    let inv = decode_inverted(r, d);
    let vals: Vec<u8> = raw
        .data
        .iter()
        .map(|&v| if (v == 0) != inv { 255 } else { 0 })
        .collect();
    Some((vals, raw.w, raw.h))
}

/// Source taps of one axis: exact area weights when shrinking, the nearest sample when
/// enlarging. Taps of destination `i` are `idx/wt[range[i].0..range[i].1]`.
struct Taps {
    idx: Vec<u32>,
    wt: Vec<f32>,
    range: Vec<(u32, u32)>,
}

fn taps(src: u32, dst: u32) -> Taps {
    let k = src as f64 / dst as f64;
    let mut t = Taps {
        idx: Vec::new(),
        wt: Vec::new(),
        range: Vec::with_capacity(dst as usize),
    };
    for d in 0..dst {
        let start = t.idx.len() as u32;
        if src <= dst {
            t.idx.push((((d as f64 + 0.5) * k) as u32).min(src - 1));
            t.wt.push(1.0);
        } else {
            let (s0, s1) = (d as f64 * k, (d as f64 + 1.0) * k);
            let mut i = s0.floor() as u32;
            while (i as f64) < s1 && i < src {
                let lo = s0.max(i as f64);
                let hi = s1.min(i as f64 + 1.0);
                if hi > lo {
                    t.idx.push(i);
                    t.wt.push(((hi - lo) / k) as f32);
                }
                i += 1;
            }
        }
        t.range.push((start, t.idx.len() as u32));
    }
    t
}

/// One source row filtered horizontally. The taps of a destination pixel are consecutive
/// source pixels.
fn hrow(src: &[u8], tx: &Taps, bpp: usize, out: &mut [f32]) {
    let taps = |a: u32, b: u32| {
        let wt = &tx.wt[a as usize..b as usize];
        let first = tx.idx.get(a as usize).map_or(0, |&i| i as usize);
        (first, wt)
    };
    match bpp {
        1 => {
            for (o, &(a, b)) in out.iter_mut().zip(&tx.range) {
                let (first, wt) = taps(a, b);
                let px = &src[first..first + wt.len()];
                *o = px.iter().zip(wt).map(|(&p, &w)| w * f32::from(p)).sum();
            }
        }
        4 => {
            for (o, &(a, b)) in out.as_chunks_mut::<4>().0.iter_mut().zip(&tx.range) {
                let (first, wt) = taps(a, b);
                let px = src[first * 4..(first + wt.len()) * 4].as_chunks::<4>().0;
                let mut s = [0f32; 4];
                for (p, &w) in px.iter().zip(wt) {
                    for c in 0..4 {
                        s[c] += w * f32::from(p[c]);
                    }
                }
                *o = s;
            }
        }
        _ => {
            for (o, &(a, b)) in out.chunks_exact_mut(bpp).zip(&tx.range) {
                let (first, wt) = taps(a, b);
                let px = src[first * bpp..(first + wt.len()) * bpp].chunks_exact(bpp);
                o.fill(0.0);
                for (p, &w) in px.zip(wt) {
                    for (s, &v) in o.iter_mut().zip(p) {
                        *s += w * f32::from(v);
                    }
                }
            }
        }
    }
}

/// Resamples `bpp`-byte pixels from `sw` × `sh` to `w` × `h` (separable box filter).
pub fn resample(v: &[u8], sw: u32, sh: u32, bpp: usize, w: u32, h: u32) -> Vec<u8> {
    if sw == w && sh == h {
        return v.to_vec();
    }
    let tx = taps(sw, w);
    let ty = taps(sh, h);
    let (srow, drow) = (sw as usize * bpp, w as usize * bpp);
    let mut out = vec![0u8; drow * h as usize];
    let mut acc = vec![0f32; drow];
    // Horizontally filtered rows still needed by the vertical taps, and spare buffers.
    let mut rows: Vec<(u32, Vec<f32>)> = Vec::new();
    let mut spare: Vec<Vec<f32>> = Vec::new();
    for (y, &(a, b)) in ty.range.iter().enumerate() {
        let first = ty.idx[a as usize];
        let mut k = 0;
        while k < rows.len() {
            if rows[k].0 < first {
                spare.push(rows.swap_remove(k).1);
            } else {
                k += 1;
            }
        }
        acc.fill(0.0);
        for t in a as usize..b as usize {
            let sy = ty.idx[t];
            let wy = ty.wt[t];
            let pos = match rows.iter().position(|r| r.0 == sy) {
                Some(p) => p,
                None => {
                    let mut buf = spare.pop().unwrap_or_else(|| vec![0f32; drow]);
                    let src = &v[sy as usize * srow..(sy as usize + 1) * srow];
                    hrow(src, &tx, bpp, &mut buf);
                    rows.push((sy, buf));
                    rows.len() - 1
                }
            };
            for (a, r) in acc.iter_mut().zip(&rows[pos].1) {
                *a += wy * r;
            }
        }
        for (o, a) in out[y * drow..(y + 1) * drow].iter_mut().zip(&acc) {
            *o = (a + 0.5).clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// Image samples unpacked to one byte per component.
fn samples(r: &Inner, s: &Stream, w: u32, h: u32, n: usize, bpc: u32, reduce: u8) -> Option<Raw> {
    let doc = &r.doc;
    let dec = doc.decode(s).ok()?;
    if let Some(f) = &dec.image {
        return match f.name.as_bytes() {
            b"DCTDecode" => {
                // Adobe CMYK JPEGs store inverted inks; a /Decode of [1 0 ...] already
                // says so, and inverting for the marker as well would double it.
                let inverts = numbers(doc, &s.dict, b"Decode")
                    .is_some_and(|v| v.len() == 8 && v.chunks(2).all(|p| p == [1.0, 0.0]));
                jpeg(&dec.data, n, reduce, !inverts, r)
            }
            b"JPXDecode" if raw_space(r, &s.dict) => jpx_components(&dec.data, reduce, n),
            b"JPXDecode" => jpx(&dec.data, reduce),
            b"JBIG2Decode" => {
                let globals = f
                    .parms
                    .get(b"JBIG2Globals")
                    .and_then(|g| doc.resolve(g).as_stream().cloned())
                    .and_then(|g| doc.decode(&g).ok().map(|d| d.data));
                let p = if pagebara_jbig2::probe(&dec.data) {
                    // A whole JBIG2 file rather than the embedded form (as some writers do).
                    let img = pagebara_jbig2::decode(&dec.data, 0).ok()?;
                    let pagebara_jbig2::Pixels::Gray8(px) = img.pixels else {
                        return None;
                    };
                    pagebara_jbig2::Packed {
                        width: img.width,
                        height: img.height,
                        data: px.iter().map(|&v| u8::from(v != 0)).collect(),
                        truncated: img.truncated,
                    }
                } else {
                    pagebara_jbig2::decode_embedded_unpacked(&dec.data, globals.as_deref()).ok()?
                };
                // A page of another size than the image's is placed at its top left on
                // white (as PDFium does) rather than stretched over it.
                let data = if (p.width, p.height) == (w, h) {
                    p.data
                } else {
                    let mut d = vec![1u8; w as usize * h as usize];
                    let cols = p.width.min(w) as usize;
                    for y in 0..p.height.min(h) as usize {
                        let src = &p.data[y * p.width as usize..][..cols];
                        d[y * w as usize..][..cols].copy_from_slice(src);
                    }
                    d
                };
                Some(Raw {
                    w,
                    h,
                    n: 1,
                    bpc: 1,
                    data,
                    alpha: None,
                    rgb: false,
                })
            }
            _ => None,
        };
    }
    unpack(&dec.data, w, h, n, bpc)
}

fn unpack(data: &[u8], w: u32, h: u32, n: usize, bpc: u32) -> Option<Raw> {
    let row_bits = w as usize * n * bpc as usize;
    let stride = row_bits.div_ceil(8);
    let total = w as usize * h as usize * n;
    let mut out = vec![0u8; total];
    let rows = h as usize;
    let per_row = w as usize * n;
    for y in 0..rows {
        let Some(src) = data.get(y * stride..) else {
            break;
        };
        let dst = &mut out[y * per_row..(y + 1) * per_row];
        match bpc {
            8 => {
                let k = per_row.min(src.len());
                dst[..k].copy_from_slice(&src[..k]);
            }
            16 => {
                for (i, d) in dst.iter_mut().enumerate() {
                    *d = src.get(2 * i).copied().unwrap_or(0);
                }
            }
            1 => {
                for (d, &b) in dst
                    .chunks_mut(8)
                    .zip(src.iter().chain(std::iter::repeat(&0)))
                {
                    for (k, o) in d.iter_mut().enumerate() {
                        *o = b >> (7 - k) & 1;
                    }
                }
            }
            2 | 4 => {
                let per = 8 / bpc as usize;
                let mask = (1u8 << bpc) - 1;
                for (i, d) in dst.iter_mut().enumerate() {
                    let byte = src.get(i / per).copied().unwrap_or(0);
                    let shift = 8 - bpc as usize * (i % per + 1);
                    *d = byte >> shift & mask;
                }
            }
            _ => return None,
        }
    }
    Some(Raw {
        w,
        h,
        n,
        bpc,
        data: out,
        alpha: None,
        rgb: false,
    })
}

fn jpeg(data: &[u8], n: usize, reduce: u8, adobe_inverts: bool, _r: &Inner) -> Option<Raw> {
    use pagebara_jpeg::{ColorModel, DecodeOptions, Parallelism, PlanesLayout, Scale, Upsampling};
    let info = pagebara_jpeg::read_info(data).ok()?;
    let scale = match reduce {
        0 => Scale::Full,
        1 => Scale::Half,
        2 => Scale::Quarter,
        _ => Scale::Eighth,
    };
    let scale = if pagebara_jpeg::is_hierarchical(data) {
        Scale::Full
    } else {
        scale
    };
    let opts = DecodeOptions {
        scale,
        parallelism: Parallelism::Auto,
        ..Default::default()
    };
    let mut p = pagebara_jpeg::decode_components(data, &opts).ok()?;
    let (w, h) = (p.width, p.height);
    let px = w as usize * h as usize;
    let comps = info.components.len();
    let mut rgba = vec![0u8; px * 4];
    match (comps, p.layout) {
        (1, _) | (_, PlanesLayout::Gray) => {
            p.layout = PlanesLayout::Gray;
            pagebara_jpeg::planes_to_rgba_into(&p, Upsampling::Bilinear, &mut rgba, w as usize * 4);
            let data: Vec<u8> = rgba.as_chunks::<4>().0.iter().map(|c| c[0]).collect();
            let _ = n;
            Some(Raw {
                w,
                h,
                n: 1,
                bpc: 8,
                data,
                alpha: None,
                rgb: false,
            })
        }
        (4, _) => {
            let ycck = info.color == ColorModel::Ycck;
            p.layout = if ycck {
                PlanesLayout::YCbCr
            } else {
                PlanesLayout::Rgb
            };
            pagebara_jpeg::planes_to_rgba_into(&p, Upsampling::Bilinear, &mut rgba, w as usize * 4);
            let adobe = adobe_inverts && info.adobe_transform.is_some();
            for c in rgba.as_chunks_mut::<4>().0 {
                if ycck {
                    // YCC decodes to the complement of the stored C, M, Y.
                    c[0] = 255 - c[0];
                    c[1] = 255 - c[1];
                    c[2] = 255 - c[2];
                }
                if adobe {
                    *c = c.map(|v| 255 - v);
                }
            }
            Some(Raw {
                w,
                h,
                n: 4,
                bpc: 8,
                data: rgba,
                alpha: None,
                rgb: false,
            })
        }
        _ => {
            if p.planes.len() > 3 {
                p.planes.truncate(3);
            }
            pagebara_jpeg::planes_to_rgba_into(&p, Upsampling::Bilinear, &mut rgba, w as usize * 4);
            let data: Vec<u8> = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|c| [c[0], c[1], c[2]])
                .collect();
            Some(Raw {
                w,
                h,
                n: 3,
                bpc: 8,
                data,
                alpha: None,
                rgb: false,
            })
        }
    }
}

fn jpx(data: &[u8], reduce: u8) -> Option<Raw> {
    use pagebara_jp2::{DecodeOptions, PixelFormat, Pixels};
    let info = pagebara_jp2::read_info(data).ok()?;
    let opts = DecodeOptions {
        reduce: reduce.min(info.levels),
        ..Default::default()
    };
    let img = pagebara_jp2::decode(data, &opts).ok()?;
    let Pixels::U8(px) = img.pixels else {
        return None;
    };
    match img.format {
        PixelFormat::Gray8 => Some(Raw {
            w: img.width,
            h: img.height,
            n: 1,
            bpc: 8,
            data: px,
            alpha: None,
            rgb: false,
        }),
        _ => {
            let alpha = info
                .has_alpha
                .then(|| px.as_chunks::<4>().0.iter().map(|c| c[3]).collect());
            let data = px
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|c| [c[0], c[1], c[2]])
                .collect();
            Some(Raw {
                w: img.width,
                h: img.height,
                n: 3,
                bpc: 8,
                data,
                alpha,
                rgb: matches!(info.color, pagebara_jp2::ColorSpace::Cmyk)
                    || info.components.len() == 4,
            })
        }
    }
}

/// Whether an image's colour space takes a JPX image's component values as they are:
/// `/Indexed` (indices), `/Separation` and `/DeviceN` (tints).
fn raw_space(r: &Inner, d: &Dict) -> bool {
    let cs = r.doc.get_in(d, b"ColorSpace");
    let first = match &*cs {
        Object::Array(a) => a
            .first()
            .and_then(|o| r.doc.resolve(o).as_name().map(|n| n.to_vec())),
        _ => None,
    };
    matches!(
        first.as_deref(),
        Some(b"Indexed" | b"I" | b"Separation" | b"DeviceN")
    )
}

/// A JPX image under such a space: `n` component values per pixel as decoded, the file's
/// own palette or colour specification unapplied (the PDF's colour space overrides it,
/// ISO 32000-2 8.9.5.6).
fn jpx_components(data: &[u8], reduce: u8, n: usize) -> Option<Raw> {
    let dec = pagebara_jp2::Decoder::new(data).ok()?;
    let reduce = reduce.min(pagebara_jp2::read_info(data).ok()?.levels);
    let comps = dec.decode_components(reduce, None).ok()?;
    let first = comps.first()?;
    let (w, h) = (first.width, first.height);
    let px = w as usize * h as usize;
    // Components sampled differently from the first are left out.
    let same: Vec<_> = comps
        .iter()
        .filter(|c| (c.width, c.height) == (w, h))
        .take(n)
        .collect();
    let mut data = vec![0u8; px * n];
    for (k, c) in same.iter().enumerate() {
        for (i, &v) in c.samples.iter().enumerate().take(px) {
            data[i * n + k] = v.min(255) as u8;
        }
    }
    Some(Raw {
        w,
        h,
        n,
        bpc: 8,
        data,
        alpha: None,
        rgb: false,
    })
}

/// The gray value of each 8-bit sample when a one-component space yields only grays.
fn gray_lut(cs: &Cs, raw: &Raw, (lo, hi): (f32, f32)) -> Option<Vec<u8>> {
    let max = if raw.bpc >= 8 {
        255.0
    } else {
        ((1u32 << raw.bpc) - 1) as f32
    };
    let mut lut = Vec::with_capacity(256);
    for v in 0..256 {
        let c = rgb8(cs.to_rgb(&[lo + (v as f32).min(max) / max * (hi - lo)]));
        if c[0] != c[1] || c[1] != c[2] {
            return None;
        }
        lut.push(c[0]);
    }
    Some(lut)
}

/// Samples through the Decode array and colour space to opaque RGBA.
fn convert(cs: &Arc<Cs>, raw: &Raw, decode: &[(f32, f32)], out: &mut [u8]) {
    let n = raw.n;
    let max = if raw.bpc >= 8 {
        255.0
    } else {
        ((1u32 << raw.bpc) - 1) as f32
    };
    let identity = decode.iter().enumerate().all(|(i, &(a, b))| {
        let (lo, hi) = cs.decode_range(i, raw.bpc);
        a == lo && b == hi
    });
    let px = out.as_chunks_mut::<4>().0;
    if n == 1 {
        let (lo, hi) = decode[0];
        let lut: Vec<[u8; 3]> = (0..256)
            .map(|v| rgb8(cs.to_rgb(&[lo + (v as f32).min(max) / max * (hi - lo)])))
            .collect();
        for (o, &v) in px.iter_mut().zip(&raw.data) {
            let c = lut[v as usize];
            *o = [c[0], c[1], c[2], 255];
        }
        return;
    }
    if identity && raw.bpc == 8 {
        match **cs {
            Cs::Rgb => {
                for (o, s) in px.iter_mut().zip(raw.data.as_chunks::<3>().0) {
                    *o = [s[0], s[1], s[2], 255];
                }
                return;
            }
            Cs::Cmyk => {
                for (o, s) in px.iter_mut().zip(raw.data.as_chunks::<4>().0) {
                    let k = 255 - s[3] as u32;
                    let f = |c: u8| ((255 - c as u32) * k / 255) as u8;
                    *o = [f(s[0]), f(s[1]), f(s[2]), 255];
                }
                return;
            }
            _ => {}
        }
    }
    // General path, remembering the last pixel (images repeat colours).
    let mut last: Option<(&[u8], [u8; 3])> = None;
    let mut comps = [0f32; 32];
    for (o, s) in px.iter_mut().zip(raw.data.chunks_exact(n)) {
        let c = match last {
            Some((l, c)) if l == s => c,
            _ => {
                for (k, &v) in s.iter().enumerate() {
                    let (a, b) = decode[k];
                    comps[k] = a + (v as f32).min(max) / max * (b - a);
                }
                let c = rgb8(cs.to_rgb(&comps[..n]));
                last = Some((s, c));
                c
            }
        };
        *o = [c[0], c[1], c[2], 255];
    }
}
