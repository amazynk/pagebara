//! Shadings painted where regions are: what they hold under a region goes from the file.
//!
//! Mesh shadings (types 4 to 7) lose the triangles and patches wholly inside regions. A
//! triangle with a vertex inside a region is cut at the region's boundary: its colours are
//! interpolated linearly, so the pieces outside draw as the triangle did. A patch with a
//! corner inside is split exactly (de Casteljau, colours bilinearly) until the pieces still
//! meeting a region with a corner are smaller than [`MIN_PATCH`]; pieces wholly inside go.
//! Parts wholly outside keep their original samples; when something was cut, the mesh is
//! written as triangles (type 4) or tensor patches (type 7) with 32-bit coordinates and
//! 16-bit colours (24 and 12 bits for 12-bit originals). Whatever reaches into a region is clipped there like any other paint.
//!
//! Sampled functions (type 0) can hold a picture. A function-based shading (type 1) has
//! its samples zeroed where all the area they take part in (by multilinear interpolation)
//! maps into a region, so nothing outside changes; a mesh with a parametric function has
//! the samples zeroed that no remaining part reaches.

use pagebara_syntax::{Dict, Document, Object, Stream};

use crate::geom::{Matrix, Point, Rect, Region, area};

/// Patches meeting a region with a corner are split until smaller than this (page units).
const MIN_PATCH: f64 = 0.5;
/// Pieces written at most for one patch mesh; beyond it pieces stay whole.
const MAX_PIECES: usize = 1 << 20;
/// Function samples looked at, at most.
const MAX_SAMPLES: usize = 1 << 24;

/// A shading rewritten for a placement.
pub(crate) struct Change {
    pub shading: Object,
    /// Nothing of it is drawn any more.
    pub empty: bool,
    /// Triangles and patches wholly inside regions.
    pub parts: usize,
    /// Function samples zeroed.
    pub samples: usize,
}

/// The shading `o` without what it holds under `regions`, where shading space maps to the
/// page by `m`; `None` when it stays as it is.
pub(crate) fn redact(
    doc: &mut Document,
    o: &Object,
    m: &Matrix,
    regions: &[Region],
) -> Option<Change> {
    if regions.is_empty() {
        return None;
    }
    let so = (*doc.resolve(o)).clone();
    let d = so.as_dict()?;
    match doc.get_in(d, b"ShadingType").as_int()? {
        1 => function_based(doc, d, m, regions),
        4..=7 => mesh(doc, so.as_stream()?, m, regions),
        _ => None,
    }
}

fn nums(doc: &Document, d: &Dict, k: &[u8]) -> Option<Vec<f64>> {
    let a = doc.get_in(d, k);
    Some(
        a.as_array()?
            .iter()
            .filter_map(|o| doc.resolve(o).as_f64())
            .collect(),
    )
}

/// A function-based shading with the samples of its sampled functions zeroed where they
/// only take part in what lies inside regions.
fn function_based(doc: &mut Document, d: &Dict, m: &Matrix, regions: &[Region]) -> Option<Change> {
    let dom = nums(doc, d, b"Domain")
        .filter(|v| v.len() >= 4)
        .unwrap_or_else(|| vec![0.0, 1.0, 0.0, 1.0]);
    let to_page = d
        .get(b"Matrix")
        .and_then(|o| Matrix::from_object(doc, o))
        .unwrap_or_default()
        .then(m);
    let under = |s: &[(f64, f64)]| -> bool {
        let [(x0, x1), (y0, y1)] = s else {
            return false;
        };
        let x = (x0.max(dom[0].min(dom[1])), x1.min(dom[0].max(dom[1])));
        let y = (y0.max(dom[2].min(dom[3])), y1.min(dom[2].max(dom[3])));
        if x.0 > x.1 || y.0 > y.1 {
            return false;
        }
        let pts = [(x.0, y.0), (x.1, y.0), (x.1, y.1), (x.0, y.1)]
            .map(|(x, y)| to_page.apply(Point::new(x, y)));
        regions.iter().any(|r| pts.iter().all(|&p| r.contains(p)))
    };
    let (f, samples) = blank_functions(doc, d.get(b"Function")?, &under)?;
    let mut nd = d.clone();
    nd.set(b"Function", f);
    Some(Change {
        shading: Object::Dict(nd),
        empty: false,
        parts: 0,
        samples,
    })
}

/// Whether a sample whose support (per input, the open interval of inputs it takes part
/// in) is given goes.
type Gone<'a> = dyn Fn(&[(f64, f64)]) -> bool + 'a;

/// The function (or array of functions) `fo` with the samples of sampled functions zeroed
/// whose support is `gone`, and how many; `None` when none is.
fn blank_functions(doc: &mut Document, fo: &Object, gone: &Gone<'_>) -> Option<(Object, usize)> {
    let r = doc.resolve(fo);
    if let Some(a) = r.as_array() {
        let mut a = a.to_vec();
        let mut total = 0;
        for f in &mut a {
            if let Some((nf, n)) = blank_function(doc, f, gone) {
                *f = nf;
                total += n;
            }
        }
        return (total > 0).then_some((Object::Array(a), total));
    }
    blank_function(doc, fo, gone)
}

fn blank_function(doc: &mut Document, fo: &Object, gone: &Gone<'_>) -> Option<(Object, usize)> {
    let so = (*doc.resolve(fo)).clone();
    let s = so.as_stream()?;
    let d = &s.dict;
    if doc.get_in(d, b"FunctionType").as_int() != Some(0) {
        return None;
    }
    let domain = nums(doc, d, b"Domain")?;
    let range = nums(doc, d, b"Range")?;
    let (m, n) = (domain.len() / 2, range.len() / 2);
    let size: Vec<usize> = nums(doc, d, b"Size")?
        .iter()
        .map(|&v| v.max(1.0) as usize)
        .collect();
    let bps = doc.get_in(d, b"BitsPerSample").as_int()? as u32;
    if m == 0 || n == 0 || size.len() < m || ![1, 2, 4, 8, 12, 16, 24, 32].contains(&bps) {
        return None;
    }
    let count = size[..m].iter().try_fold(n, |a, &s| a.checked_mul(s))?;
    if count > MAX_SAMPLES {
        return None;
    }
    let encode = nums(doc, d, b"Encode")
        .filter(|e| e.len() >= 2 * m)
        .unwrap_or_else(|| {
            size[..m]
                .iter()
                .flat_map(|&s| [0.0, (s - 1) as f64])
                .collect()
        });
    let reach = if doc.get_in(d, b"Order").as_int() == Some(3) {
        2.0
    } else {
        1.0
    };
    let supports: Vec<Vec<(f64, f64)>> = (0..m)
        .map(|i| {
            (0..size[i])
                .map(|k| {
                    support(
                        k,
                        size[i],
                        (domain[2 * i], domain[2 * i + 1]),
                        (encode[2 * i], encode[2 * i + 1]),
                        reach,
                    )
                })
                .collect()
        })
        .collect();
    let data = doc.decode(s).ok()?;
    let mut bits = Bits {
        data: &data.data,
        pos: 0,
    };
    let mut v: Vec<u32> = (0..count).map(|_| bits.read(bps).unwrap_or(0)).collect();
    let mut idx = vec![0usize; m];
    let mut sup = vec![(0.0, 0.0); m];
    let mut blanked = 0;
    for chunk in v.chunks_mut(n) {
        for i in 0..m {
            sup[i] = supports[i][idx[i]];
        }
        if gone(&sup) && chunk.iter().any(|&x| x != 0) {
            chunk.fill(0);
            blanked += 1;
        }
        // The first input varies fastest.
        for i in 0..m {
            idx[i] += 1;
            if idx[i] < size[i] {
                break;
            }
            idx[i] = 0;
        }
    }
    if blanked == 0 {
        return None;
    }
    let mut w = Writer::default();
    for x in v {
        w.write(x, bps);
    }
    w.align();
    let mut nd = Dict::new();
    for (k, v) in d.iter() {
        if !matches!(k.as_bytes(), b"Filter" | b"DecodeParms" | b"Length") {
            nd.set(k.as_bytes(), v.clone());
        }
    }
    nd.set(b"Filter", Object::name(b"FlateDecode"));
    let r = doc.add(Stream::new(nd, pagebara_filters::encode_flate(&w.out, 6)));
    Some((Object::Ref(r), blanked))
}

/// The open interval of inputs whose value sample `k` (of `size` along one input) takes
/// part in, interpolating over `reach` samples each way; inputs clamp into `dom` and
/// encoded values into the samples, so the end samples also stand for what lies beyond.
fn support(k: usize, size: usize, dom: (f64, f64), enc: (f64, f64), reach: f64) -> (f64, f64) {
    let all = (f64::NEG_INFINITY, f64::INFINITY);
    let ((d0, d1), (e0, e1)) = (dom, enc);
    if e1 == e0 || d1 == d0 || size < 2 {
        return all;
    }
    let (k, last) = (k as f64, (size - 1) as f64);
    let lo = if k - reach < 0.0 {
        f64::NEG_INFINITY
    } else {
        k - reach
    };
    let hi = if k + reach > last {
        f64::INFINITY
    } else {
        k + reach
    };
    let slope = (d1 - d0) / (e1 - e0);
    let x = |e: f64| {
        if e.is_infinite() {
            e * slope.signum()
        } else {
            d0 + (e - e0) * slope
        }
    };
    let (mut p, mut q) = (x(lo), x(hi));
    if p > q {
        std::mem::swap(&mut p, &mut q);
    }
    if p <= d0.min(d1) {
        p = f64::NEG_INFINITY;
    }
    if q >= d0.max(d1) {
        q = f64::INFINITY;
    }
    (p, q)
}

/// A mesh shading without what it holds under regions, its parametric function too.
fn mesh(doc: &mut Document, s: &Stream, m: &Matrix, regions: &[Region]) -> Option<Change> {
    let p = prune(doc, s, m, regions)?;
    let empty = p.stream.is_none();
    let mut stream = p.stream.unwrap_or_else(|| self::empty(s));
    let mut samples = 0;
    if let Some(f) = s.dict.get(b"Function") {
        let ts = p.ts;
        let unused = |sup: &[(f64, f64)]| {
            let [(lo, hi)] = sup else { return false };
            !ts.iter().any(|&(a, b)| b > *lo && a < *hi)
        };
        if let Some((nf, n)) = blank_functions(doc, f, &unused) {
            stream.dict.set(b"Function", nf);
            samples = n;
        }
    }
    Some(Change {
        shading: Object::Stream(stream),
        empty,
        parts: p.parts,
        samples,
    })
}
/// Raw sample values of one vertex or control point with its colour.
#[derive(Clone, Debug, Default)]
struct V {
    x: u32,
    y: u32,
    c: Vec<u32>,
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn read(&mut self, n: u32) -> Option<u32> {
        if n == 0 || n > 32 || self.pos + n as usize > self.data.len() * 8 {
            return None;
        }
        let mut v = 0u64;
        for _ in 0..n {
            let byte = self.data[self.pos / 8];
            v = v << 1 | u64::from(byte >> (7 - self.pos % 8) & 1);
            self.pos += 1;
        }
        Some(v as u32)
    }

    fn align(&mut self) {
        self.pos = self.pos.next_multiple_of(8);
    }
}

#[derive(Default)]
struct Writer {
    out: Vec<u8>,
    bits: u32,
    acc: u64,
}

impl Writer {
    fn write(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            self.acc = self.acc << 1 | u64::from(v >> i & 1);
            self.bits += 1;
            if self.bits == 8 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.bits = 0;
            }
        }
    }

    fn align(&mut self) {
        if self.bits > 0 {
            self.write(0, 8 - self.bits);
        }
    }
}

struct Format {
    bpc: u32,
    bpcomp: u32,
    bpf: u32,
    decode: Vec<f64>,
    n: usize,
}

impl Format {
    fn point(&self, b: &mut Bits) -> Option<V> {
        Some(V {
            x: b.read(self.bpc)?,
            y: b.read(self.bpc)?,
            c: Vec::new(),
        })
    }

    fn color(&self, b: &mut Bits) -> Option<Vec<u32>> {
        (0..self.n).map(|_| b.read(self.bpcomp)).collect()
    }

    fn vertex(&self, b: &mut Bits) -> Option<V> {
        let mut v = self.point(b)?;
        v.c = self.color(b)?;
        Some(v)
    }

    /// The point in shading space.
    fn at(&self, v: &V) -> Point {
        let max = ((1u64 << self.bpc) - 1) as f64;
        let d = &self.decode;
        Point::new(
            d[0] + f64::from(v.x) / max * (d[1] - d[0]),
            d[2] + f64::from(v.y) / max * (d[3] - d[2]),
        )
    }

    fn put_point(&self, w: &mut Writer, v: &V) {
        w.write(v.x, self.bpc);
        w.write(v.y, self.bpc);
    }

    fn put_color(&self, w: &mut Writer, c: &[u32]) {
        for &v in c {
            w.write(v, self.bpcomp);
        }
    }
}

/// A patch: the twelve boundary points in stream order, four inner ones (type 7) and the
/// corner colours.
#[derive(Clone)]
struct Patch {
    b: [V; 12],
    inner: Option<[V; 4]>,
    c: [Vec<u32>; 4],
}

impl Format {
    /// Bits of coordinates written after cutting: a multiple of the original ones, so the
    /// original points keep their exact values.
    fn fine_bpc(&self) -> u32 {
        if self.bpc % 12 == 0 { 24 } else { 32 }
    }

    /// Bits of colour components written after cutting, likewise.
    fn fine_bpcomp(&self) -> u32 {
        if self.bpcomp == 12 { 12 } else { 16 }
    }

    /// The colour of raw components `c`.
    fn colour(&self, c: &[u32]) -> Vec<f64> {
        let max = ((1u64 << self.bpcomp) - 1) as f64;
        c.iter()
            .enumerate()
            .map(|(i, &v)| {
                let (a, b) = (self.decode[4 + 2 * i], self.decode[5 + 2 * i]);
                a + f64::from(v) / max * (b - a)
            })
            .collect()
    }

    fn vx(&self, v: &V) -> Vx {
        Vx {
            p: self.at(v),
            c: self.colour(&v.c),
        }
    }

    fn put_fine_point(&self, w: &mut Writer, p: Point) {
        let d = &self.decode;
        for (v, a, b) in [(p.x, d[0], d[1]), (p.y, d[2], d[3])] {
            w.write(fine(v, a, b, self.fine_bpc()), self.fine_bpc());
        }
    }

    fn put_fine_colour(&self, w: &mut Writer, c: &[f64]) {
        for (i, &v) in c.iter().enumerate() {
            let (a, b) = (self.decode[4 + 2 * i], self.decode[5 + 2 * i]);
            w.write(fine(v, a, b, self.fine_bpcomp()), self.fine_bpcomp());
        }
    }

    /// A patch as a tensor grid with real points and colours.
    fn grid(&self, p: &Patch) -> Grid {
        let b: Vec<Point> = p.b.iter().map(|v| self.at(v)).collect();
        let inner = match &p.inner {
            Some(i) => i.each_ref().map(|v| self.at(v)),
            // [p11, p21, p12, p22] → stream order p11, p12, p22, p21.
            None => {
                let i = coons_inner(&b);
                [i[0], i[2], i[3], i[1]]
            }
        };
        let mut g = [[Point::default(); 4]; 4];
        for (k, &(i, j)) in BOUNDARY.iter().enumerate() {
            g[i][j] = b[k];
        }
        for (k, &(i, j)) in INNER.iter().enumerate() {
            g[i][j] = inner[k];
        }
        Grid {
            g,
            c: p.c.each_ref().map(|c| self.colour(c)),
        }
    }
}

fn fine(v: f64, a: f64, b: f64, bits: u32) -> u32 {
    let t = if b != a { (v - a) / (b - a) } else { 0.0 };
    (t.clamp(0.0, 1.0) * ((1u64 << bits) - 1) as f64).round() as u32
}

/// Grid positions `(i, j)` of a tensor patch's control points `p_ij` in stream order: the
/// boundary, then the inner points.
const BOUNDARY: [(usize, usize); 12] = [
    (0, 0),
    (0, 1),
    (0, 2),
    (0, 3),
    (1, 3),
    (2, 3),
    (3, 3),
    (3, 2),
    (3, 1),
    (3, 0),
    (2, 0),
    (1, 0),
];
const INNER: [(usize, usize); 4] = [(1, 1), (1, 2), (2, 2), (2, 1)];

/// A vertex with its point in shading space and its colour.
#[derive(Clone, Debug)]
struct Vx {
    p: Point,
    c: Vec<f64>,
}

impl Vx {
    fn mix(&self, o: &Vx, t: f64) -> Vx {
        Vx {
            p: self.p.lerp(o.p, t),
            c: self
                .c
                .iter()
                .zip(&o.c)
                .map(|(a, b)| a + (b - a) * t)
                .collect(),
        }
    }
}

/// The parts of a convex polygon where `n·p <= c` and where `n·p >= c`.
fn halves(poly: &[Vx], n: Point, c: f64) -> (Vec<Vx>, Vec<Vx>) {
    let (mut inner, mut outer) = (Vec::new(), Vec::new());
    for (i, a) in poly.iter().enumerate() {
        let b = &poly[(i + 1) % poly.len()];
        let (da, db) = (n.dot(a.p) - c, n.dot(b.p) - c);
        if da <= 0.0 {
            inner.push(a.clone());
        }
        if da >= 0.0 {
            outer.push(a.clone());
        }
        if (da < 0.0 && db > 0.0) || (da > 0.0 && db < 0.0) {
            let x = a.mix(b, da / (da - db));
            inner.push(x.clone());
            outer.push(x);
        }
    }
    (inner, outer)
}

/// Triangles covering what of triangle `t` lies outside every region (in shading space).
fn cut(t: [Vx; 3], regions: &[Region]) -> Vec<[Vx; 3]> {
    let has_area =
        |p: &[Vx]| p.len() >= 3 && area(&p.iter().map(|v| v.p).collect::<Vec<_>>()) > 1e-12;
    let mut pieces = vec![t.to_vec()];
    for r in regions {
        let mut next = Vec::new();
        for mut cur in pieces {
            for (n, c) in r.edges() {
                let (inner, outer) = halves(&cur, n, c);
                if has_area(&outer) {
                    next.push(outer);
                }
                cur = inner;
                if !has_area(&cur) {
                    break;
                }
            }
        }
        pieces = next;
    }
    pieces
        .iter()
        .flat_map(|p| (1..p.len() - 1).map(|k| [p[0].clone(), p[k].clone(), p[k + 1].clone()]))
        .collect()
}

/// A tensor patch: control points `g[i][j]` (`i` along `u`, `j` along `v`) and the colours
/// at `(u, v)` = (0, 0), (0, 1), (1, 1), (1, 0).
#[derive(Clone)]
struct Grid {
    g: [[Point; 4]; 4],
    c: [Vec<f64>; 4],
}

fn halve(p: [Point; 4]) -> ([Point; 4], [Point; 4]) {
    let [a, b, c, d] = p;
    let (ab, bc, cd) = (a.lerp(b, 0.5), b.lerp(c, 0.5), c.lerp(d, 0.5));
    let (abc, bcd) = (ab.lerp(bc, 0.5), bc.lerp(cd, 0.5));
    let m = abc.lerp(bcd, 0.5);
    ([a, ab, abc, m], [m, bcd, cd, d])
}

fn mid(a: &[f64], b: &[f64]) -> Vec<f64> {
    a.iter().zip(b).map(|(x, y)| (x + y) / 2.0).collect()
}

impl Grid {
    fn corners(&self) -> [Point; 4] {
        [self.g[0][0], self.g[0][3], self.g[3][3], self.g[3][0]]
    }

    /// The halves `u < ½` and `u > ½`.
    fn split_u(&self) -> (Grid, Grid) {
        let (mut a, mut b) = (self.g, self.g);
        for j in 0..4 {
            let (l, r) = halve([self.g[0][j], self.g[1][j], self.g[2][j], self.g[3][j]]);
            for i in 0..4 {
                a[i][j] = l[i];
                b[i][j] = r[i];
            }
        }
        let c = &self.c;
        let (m0, m1) = (mid(&c[0], &c[3]), mid(&c[1], &c[2]));
        (
            Grid {
                g: a,
                c: [c[0].clone(), c[1].clone(), m1.clone(), m0.clone()],
            },
            Grid {
                g: b,
                c: [m0, m1, c[2].clone(), c[3].clone()],
            },
        )
    }

    /// The halves `v < ½` and `v > ½`.
    fn split_v(&self) -> (Grid, Grid) {
        let (mut a, mut b) = (self.g, self.g);
        for i in 0..4 {
            (a[i], b[i]) = halve(self.g[i]);
        }
        let c = &self.c;
        let (m0, m1) = (mid(&c[0], &c[1]), mid(&c[3], &c[2]));
        (
            Grid {
                g: a,
                c: [c[0].clone(), m0.clone(), m1.clone(), c[3].clone()],
            },
            Grid {
                g: b,
                c: [m0, c[1].clone(), c[2].clone(), m1],
            },
        )
    }

    /// Splits until no piece meets a region with a corner (or pieces get small); pieces
    /// wholly inside a region go.
    fn refine(self, m: &Matrix, regions: &[Region], depth: u32, out: &mut Vec<Grid>) {
        let pts: Vec<Point> = self.g.iter().flatten().map(|&p| m.apply(p)).collect();
        if regions.iter().any(|r| pts.iter().all(|&p| r.contains(p))) {
            return;
        }
        let bb = Rect::from_points(&pts);
        let corner_in = self
            .corners()
            .iter()
            .any(|&p| regions.iter().any(|r| deep(r, m.apply(p))));
        if !corner_in
            || depth == 0
            || out.len() >= MAX_PIECES
            || bb.width().max(bb.height()) < MIN_PATCH
        {
            out.push(self);
            return;
        }
        let len = |a: Point, b: Point| {
            let d = m.apply(a).sub(m.apply(b));
            d.dot(d).sqrt()
        };
        let g = &self.g;
        let along_u = len(g[0][0], g[3][0]) + len(g[0][3], g[3][3]);
        let along_v = len(g[0][0], g[0][3]) + len(g[3][0], g[3][3]);
        let (a, b) = if along_u >= along_v {
            self.split_u()
        } else {
            self.split_v()
        };
        a.refine(m, regions, depth - 1, out);
        b.refine(m, regions, depth - 1, out);
    }
}

/// Whether `p` lies inside `r`, off its boundary (where cut parts end).
fn deep(r: &Region, p: Point) -> bool {
    r.edges()
        .all(|(n, c)| n.dot(p) < c - 1e-6 * n.dot(n).sqrt())
}

/// What is kept of a mesh part.
enum Part<T, F> {
    Raw(T),
    Cut(Vec<F>),
}

/// A mesh without what lies inside regions: the new stream (`None` when nothing is left),
/// the parts that went, and the parametric values the rest spans.
struct Pruned {
    stream: Option<Stream>,
    parts: usize,
    ts: Vec<(f64, f64)>,
}

fn span(ts: &mut Vec<(f64, f64)>, cs: impl Iterator<Item = f64>) {
    let (lo, hi) = cs.fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), t| {
        (a.min(t), b.max(t))
    });
    if lo <= hi {
        ts.push((lo, hi));
    }
}

/// The mesh shading `s` without what lies inside `regions` (where shading space maps to
/// the page by `m`); `None` when nothing does.
fn prune(doc: &Document, s: &Stream, m: &Matrix, regions: &[Region]) -> Option<Pruned> {
    let d = &s.dict;
    let ty = doc.get_in(d, b"ShadingType").as_int().unwrap_or(0);
    if !(4..=7).contains(&ty) {
        return None;
    }
    let data = doc.decode(s).ok()?;
    let int = |k: &[u8]| doc.get_in(d, k).as_int().unwrap_or(0) as u32;
    let decode = nums(doc, d, b"Decode")?;
    if decode.len() < 6 || decode.len() % 2 != 0 {
        return None;
    }
    let f = Format {
        bpc: int(b"BitsPerCoordinate"),
        bpcomp: int(b"BitsPerComponent"),
        bpf: int(b"BitsPerFlag"),
        n: (decode.len() - 4) / 2,
        decode,
    };
    if !(1..=32).contains(&f.bpc) || !(1..=16).contains(&f.bpcomp) {
        return None;
    }
    let inv = m.invert()?;
    let local: Vec<Region> = regions.iter().filter_map(|r| r.transform(&inv)).collect();
    let wholly = |pts: &[Point]| {
        regions
            .iter()
            .any(|r| pts.iter().all(|&p| r.contains(m.apply(p))))
    };
    let any_in = |pts: &[Point]| {
        pts.iter()
            .any(|&p| regions.iter().any(|r| deep(r, m.apply(p))))
    };
    let param = d.contains(b"Function");
    let mut bits = Bits {
        data: &data.data,
        pos: 0,
    };
    let mut w = Writer::default();
    let mut dict = Dict::new();
    for (k, v) in d.iter() {
        if !matches!(k.as_bytes(), b"Filter" | b"DecodeParms" | b"Length") {
            dict.set(k.as_bytes(), v.clone());
        }
    }
    let (mut kept, mut gone, mut cuts) = (0usize, 0usize, false);
    let mut ts = Vec::new();
    if ty == 4 || ty == 5 {
        let tris = if ty == 4 {
            free_form(&f, &mut bits)
        } else {
            lattice(&f, &mut bits, int(b"VerticesPerRow") as usize)
        };
        let mut parts = Vec::new();
        for t in tris {
            let pts = t.each_ref().map(|v| f.at(v));
            if wholly(&pts) {
                gone += 1;
            } else if any_in(&pts) {
                cuts = true;
                let pieces = cut(t.each_ref().map(|v| f.vx(v)), &local);
                if pieces.is_empty() {
                    gone += 1;
                } else {
                    parts.push(Part::Cut(pieces));
                }
            } else {
                parts.push(Part::Raw(t));
            }
        }
        let bpf = if ty == 4 && (1..=8).contains(&f.bpf) && !cuts {
            f.bpf
        } else {
            8
        };
        let tri = |w: &mut Writer, t: &[Vx; 3]| {
            for v in t {
                w.write(0, bpf);
                f.put_fine_point(w, v.p);
                f.put_fine_colour(w, &v.c);
                w.align();
            }
        };
        for p in &parts {
            match p {
                Part::Raw(t) if !cuts => {
                    for v in t {
                        w.write(0, bpf);
                        f.put_point(&mut w, v);
                        f.put_color(&mut w, &v.c);
                        w.align();
                    }
                    if param {
                        span(&mut ts, t.iter().map(|v| f.colour(&v.c)[0]));
                    }
                    kept += 1;
                }
                Part::Raw(t) => {
                    let t = t.each_ref().map(|v| f.vx(v));
                    tri(&mut w, &t);
                    if param {
                        span(&mut ts, t.iter().map(|v| v.c[0]));
                    }
                    kept += 1;
                }
                Part::Cut(pieces) => {
                    for t in pieces {
                        tri(&mut w, t);
                        if param {
                            span(&mut ts, t.iter().map(|v| v.c[0]));
                        }
                        kept += 1;
                    }
                }
            }
        }
        dict.set(b"ShadingType", Object::Int(4));
        dict.set(b"BitsPerFlag", Object::Int(i64::from(bpf)));
        dict.remove(b"VerticesPerRow");
    } else {
        if !(1..=8).contains(&f.bpf) {
            return None;
        }
        let mut parts = Vec::new();
        let mut pieces = 0;
        for p in patches(&f, &mut bits, ty == 7) {
            let mut pts: Vec<Point> = p.b.iter().map(|v| f.at(v)).collect();
            match &p.inner {
                Some(i) => pts.extend(i.iter().map(|v| f.at(v))),
                None => pts.extend(coons_inner(&pts)),
            }
            if wholly(&pts) {
                gone += 1;
                continue;
            }
            let corners = [0, 3, 6, 9].map(|k| pts[k]);
            if any_in(&corners) && pieces < MAX_PIECES {
                cuts = true;
                let mut out = Vec::new();
                f.grid(&p).refine(m, regions, 24, &mut out);
                pieces += out.len();
                if out.is_empty() {
                    gone += 1;
                } else {
                    parts.push(Part::Cut(out));
                }
            } else {
                parts.push(Part::Raw(p));
            }
        }
        let fine = |w: &mut Writer, ts: &mut Vec<(f64, f64)>, g: &Grid| {
            w.write(0, 8);
            for &(i, j) in BOUNDARY.iter().chain(&INNER) {
                f.put_fine_point(w, g.g[i][j]);
            }
            for c in &g.c {
                f.put_fine_colour(w, c);
            }
            if param {
                span(ts, g.c.iter().map(|c| c[0]));
            }
        };
        for p in &parts {
            match p {
                Part::Raw(p) if !cuts => {
                    w.write(0, f.bpf);
                    for v in p.b.iter().chain(p.inner.iter().flatten()) {
                        f.put_point(&mut w, v);
                    }
                    for c in &p.c {
                        f.put_color(&mut w, c);
                    }
                    if param {
                        span(&mut ts, p.c.iter().map(|c| f.colour(c)[0]));
                    }
                    kept += 1;
                }
                Part::Raw(p) => {
                    fine(&mut w, &mut ts, &f.grid(p));
                    kept += 1;
                }
                Part::Cut(gs) => {
                    for g in gs {
                        fine(&mut w, &mut ts, g);
                        kept += 1;
                    }
                }
            }
        }
        if cuts {
            dict.set(b"ShadingType", Object::Int(7));
            dict.set(b"BitsPerFlag", Object::Int(8));
        }
    }
    if gone == 0 && !cuts {
        return None;
    }
    if cuts {
        dict.set(b"BitsPerCoordinate", Object::Int(i64::from(f.fine_bpc())));
        dict.set(b"BitsPerComponent", Object::Int(i64::from(f.fine_bpcomp())));
    }
    if kept == 0 {
        return Some(Pruned {
            stream: None,
            parts: gone,
            ts,
        });
    }
    w.align();
    dict.set(b"Filter", Object::name(b"FlateDecode"));
    Some(Pruned {
        stream: Some(Stream::new(dict, pagebara_filters::encode_flate(&w.out, 6))),
        parts: gone,
        ts,
    })
}

/// A copy of the mesh shading `s` with no parts.
pub(crate) fn empty(s: &Stream) -> Stream {
    let mut d = s.dict.clone();
    for k in [&b"Filter"[..], b"DecodeParms", b"Length"] {
        d.remove(k);
    }
    Stream::new(d, Vec::new())
}

fn free_form(f: &Format, b: &mut Bits) -> Vec<[V; 3]> {
    let mut out = Vec::new();
    let read = |b: &mut Bits| -> Option<(u32, V)> {
        let flag = b.read(f.bpf)?;
        let v = f.vertex(b)?;
        b.align();
        Some((flag, v))
    };
    let mut tri: Option<[V; 3]> = None;
    while let Some((flag, v)) = read(b) {
        tri = match (flag, tri.take()) {
            (0, _) => {
                let Some((_, v2)) = read(b) else { break };
                let Some((_, v3)) = read(b) else { break };
                Some([v, v2, v3])
            }
            (1, Some([_, vb, vc])) => Some([vb, vc, v]),
            (_, Some([va, _, vc])) => Some([va, vc, v]),
            (_, None) => None,
        };
        if let Some(t) = &tri {
            out.push(t.clone());
        }
        if out.len() > 4_000_000 {
            break;
        }
    }
    out
}

fn lattice(f: &Format, b: &mut Bits, per_row: usize) -> Vec<[V; 3]> {
    if per_row < 2 {
        return Vec::new();
    }
    let mut rows: Vec<Vec<V>> = Vec::new();
    'outer: loop {
        let mut row = Vec::with_capacity(per_row);
        for _ in 0..per_row {
            let Some(v) = f.vertex(b) else {
                break 'outer;
            };
            b.align();
            row.push(v);
        }
        rows.push(row);
        if rows.len() * per_row > 4_000_000 {
            break;
        }
    }
    let mut out = Vec::new();
    for w in rows.windows(2) {
        for j in 0..per_row - 1 {
            out.push([w[0][j].clone(), w[1][j].clone(), w[0][j + 1].clone()]);
            out.push([w[1][j + 1].clone(), w[1][j].clone(), w[0][j + 1].clone()]);
        }
    }
    out
}

fn patches(f: &Format, b: &mut Bits, tensor: bool) -> Vec<Patch> {
    let mut out: Vec<Patch> = Vec::new();
    while let Some(flag) = b.read(f.bpf) {
        let mut p = Patch {
            b: Default::default(),
            inner: None,
            c: Default::default(),
        };
        let start = match (flag, out.last()) {
            (0, _) => 0,
            (1..=3, Some(prev)) => {
                let (pts, cols) = match flag {
                    1 => ([3, 4, 5, 6], [1, 2]),
                    2 => ([6, 7, 8, 9], [2, 3]),
                    _ => ([9, 10, 11, 0], [3, 0]),
                };
                for (k, i) in pts.into_iter().enumerate() {
                    p.b[k] = prev.b[i].clone();
                }
                p.c[0] = prev.c[cols[0]].clone();
                p.c[1] = prev.c[cols[1]].clone();
                4
            }
            _ => break,
        };
        let mut ok = true;
        for q in &mut p.b[start..] {
            match f.point(b) {
                Some(v) => *q = v,
                None => ok = false,
            }
        }
        if tensor {
            let mut inner: [V; 4] = Default::default();
            for q in &mut inner {
                match f.point(b) {
                    Some(v) => *q = v,
                    None => ok = false,
                }
            }
            p.inner = Some(inner);
        }
        for c in &mut p.c[if start == 0 { 0 } else { 2 }..] {
            match f.color(b) {
                Some(v) => *c = v,
                None => ok = false,
            }
        }
        if !ok {
            break;
        }
        out.push(p);
        if out.len() > 1_000_000 {
            break;
        }
    }
    out
}

/// The inner control points of a Coons patch from its boundary in stream order.
fn coons_inner(b: &[Point]) -> [Point; 4] {
    // Grid position (row, column) of each boundary point in stream order.
    const AT: [(usize, usize); 12] = [
        (0, 0),
        (1, 0),
        (2, 0),
        (3, 0),
        (3, 1),
        (3, 2),
        (3, 3),
        (2, 3),
        (1, 3),
        (0, 3),
        (0, 2),
        (0, 1),
    ];
    let mut g = [[Point::default(); 4]; 4];
    for (k, &(i, j)) in AT.iter().enumerate() {
        g[i][j] = b[k];
    }
    let inner = |i: usize, j: usize| {
        let (a, c) = (if i == 1 { 0 } else { 3 }, if j == 1 { 0 } else { 3 });
        let (a2, c2) = (3 - a, 3 - c);
        let terms = [
            (-4.0, g[a][c]),
            (6.0, g[a][j]),
            (6.0, g[i][c]),
            (-2.0, g[a][c2]),
            (-2.0, g[a2][c]),
            (3.0, g[a2][j]),
            (3.0, g[i][c2]),
            (-1.0, g[a2][c2]),
        ];
        let (x, y) = terms
            .iter()
            .fold((0.0, 0.0), |(x, y), (k, p)| (x + k * p.x, y + k * p.y));
        Point::new(x / 9.0, y / 9.0)
    };
    [inner(1, 1), inner(1, 2), inner(2, 1), inner(2, 2)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coons_inner_points_of_a_flat_patch() {
        // A square whose edges are straight thirds: inner points sit at the thirds too.
        let pts: Vec<Point> = [
            (0, 0),
            (0, 1),
            (0, 2),
            (0, 3),
            (1, 3),
            (2, 3),
            (3, 3),
            (3, 2),
            (3, 1),
            (3, 0),
            (2, 0),
            (1, 0),
        ]
        .iter()
        .map(|&(x, y)| Point::new(f64::from(x), f64::from(y)))
        .collect();
        let i = coons_inner(&pts);
        let want = [(1.0, 1.0), (2.0, 1.0), (1.0, 2.0), (2.0, 2.0)];
        for (p, (x, y)) in i.iter().zip(want) {
            assert!((p.x - x).abs() < 1e-9 && (p.y - y).abs() < 1e-9, "{p:?}");
        }
    }
}
