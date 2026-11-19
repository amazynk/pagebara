//! ICC profiles (v2 and v4) to sRGB: matrix/TRC profiles and LUT-based ones (`mft1`,
//! `mft2`, `mAB `), relative colorimetric with black point compensation. A profile is
//! evaluated once on a grid, then images and colours interpolate the grid.

use std::sync::Arc;

/// D50 (the PCS illuminant).
const D50: [f32; 3] = [0.9642, 1.0, 0.8249];

/// D50 XYZ → linear sRGB (Bradford-adapted).
const XYZ_TO_SRGB: [f32; 9] = [
    3.133_856,
    -1.616_867,
    -0.490_614_6,
    -0.978_768_4,
    1.916_141_5,
    0.033_454,
    0.071_945_3,
    -0.228_991_4,
    1.405_242_7,
];

#[derive(Clone, Debug)]
enum Curve {
    Identity,
    Gamma(f32),
    Table(Vec<f32>),
    /// Parametric curve: function type and parameters `g a b c d e f`.
    Param(u16, [f32; 7]),
}

impl Curve {
    fn eval(&self, x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        match self {
            Curve::Identity => x,
            Curve::Gamma(g) => x.powf(*g),
            Curve::Table(t) => {
                let n = t.len();
                if n == 0 {
                    return x;
                }
                let p = x * (n - 1) as f32;
                let i = (p as usize).min(n - 1);
                let j = (i + 1).min(n - 1);
                let f = p - i as f32;
                t[i] + (t[j] - t[i]) * f
            }
            Curve::Param(kind, p) => {
                let [g, a, b, c, d, e, f] = *p;
                let v = match kind {
                    0 => x.powf(g),
                    1 => {
                        if x >= -b / a {
                            (a * x + b).max(0.0).powf(g)
                        } else {
                            0.0
                        }
                    }
                    2 => {
                        if x >= -b / a {
                            (a * x + b).max(0.0).powf(g) + c
                        } else {
                            c
                        }
                    }
                    3 => {
                        if x >= d {
                            (a * x + b).max(0.0).powf(g)
                        } else {
                            c * x
                        }
                    }
                    _ => {
                        if x >= d {
                            (a * x + b).max(0.0).powf(g) + e
                        } else {
                            c * x + f
                        }
                    }
                };
                v.clamp(0.0, 1.0)
            }
        }
    }
}

#[derive(Clone, Debug)]
struct Clut {
    inputs: usize,
    outputs: usize,
    grid: Vec<usize>,
    /// Normalised 0..1.
    data: Vec<f32>,
}

impl Clut {
    fn eval(&self, x: &[f32], out: &mut [f32]) {
        let m = self.inputs;
        let n = self.outputs;
        let mut idx = [0usize; 8];
        let mut frac = [0f32; 8];
        let mut strides = [0usize; 8];
        let mut s = n;
        for i in (0..m).rev() {
            strides[i] = s;
            s *= self.grid[i];
        }
        for i in 0..m {
            let g = self.grid[i].max(1);
            let p = x[i].clamp(0.0, 1.0) * (g - 1) as f32;
            let f = p.floor();
            idx[i] = (f as usize).min(g - 1);
            frac[i] = p - f;
        }
        out[..n].fill(0.0);
        for corner in 0..1usize << m {
            let mut w = 1.0;
            let mut off = 0;
            for i in 0..m {
                let hi = corner >> i & 1 == 1;
                let fw = if hi { frac[i] } else { 1.0 - frac[i] };
                w *= fw;
                let j = (idx[i] + usize::from(hi)).min(self.grid[i] - 1);
                off += j * strides[i];
            }
            if w == 0.0 {
                continue;
            }
            for (k, o) in out[..n].iter_mut().enumerate() {
                *o += w * self.data.get(off + k).copied().unwrap_or(0.0);
            }
        }
    }
}

#[derive(Clone, Debug)]
enum Pcs {
    Xyz,
    Lab,
    /// Legacy 16-bit Lab encoding (lut16 in v2 profiles).
    Lab16,
}

#[derive(Clone, Debug)]
enum Pipeline {
    Matrix {
        trc: Vec<Curve>,
        /// Columns rXYZ, gXYZ, bXYZ (gray: just the white).
        m: [f32; 9],
        gray: bool,
    },
    Lut {
        matrix: Option<[f32; 12]>,
        b: Vec<Curve>,
        m_curves: Vec<Curve>,
        clut: Option<Clut>,
        a: Vec<Curve>,
        /// lutAtoB order (A → CLUT → M → matrix → B) versus lut8/16 (matrix → curves → CLUT
        /// → curves).
        mab: bool,
        pcs: Pcs,
    },
}

/// A parsed profile's transform from device values (0..1) to PCS XYZ (D50).
#[derive(Clone, Debug)]
pub struct Profile {
    pub components: usize,
    pipeline: Pipeline,
    /// Black point compensation: source black in XYZ.
    black: [f32; 3],
}

impl Profile {
    pub fn parse(data: &[u8]) -> Option<Profile> {
        if data.len() < 132 {
            return None;
        }
        let space = &data[16..20];
        let components = match space {
            b"GRAY" => 1,
            b"RGB " => 3,
            b"CMYK" => 4,
            b"Lab " => 3,
            _ => return None,
        };
        let pcs_xyz = &data[20..24] == b"XYZ ";
        let count = be32(data, 128)? as usize;
        let mut tags = Vec::new();
        for i in 0..count.min(256) {
            let o = 132 + i * 12;
            let sig = data.get(o..o + 4)?;
            let off = be32(data, o + 4)? as usize;
            let len = be32(data, o + 8)? as usize;
            let body = data.get(off..off.checked_add(len)?)?;
            tags.push((sig, body));
        }
        let tag = |s: &[u8]| tags.iter().find(|t| t.0 == s).map(|t| t.1);
        let pcs = if pcs_xyz { Pcs::Xyz } else { Pcs::Lab };
        let lut = [b"A2B1", b"A2B0"]
            .iter()
            .find_map(|s| tag(*s).and_then(|t| parse_lut(t, components, pcs.clone())));
        let pipeline = match lut {
            Some(l) => l,
            None if components == 3 => {
                let xyz = |s: &[u8]| tag(s).and_then(parse_xyz);
                let (r, g, b) = (xyz(b"rXYZ")?, xyz(b"gXYZ")?, xyz(b"bXYZ")?);
                let trc = [b"rTRC", b"gTRC", b"bTRC"]
                    .iter()
                    .map(|s| tag(*s).and_then(|t| parse_curve(t).map(|c| c.0)))
                    .collect::<Option<Vec<_>>>()?;
                Pipeline::Matrix {
                    trc,
                    m: [r[0], g[0], b[0], r[1], g[1], b[1], r[2], g[2], b[2]],
                    gray: false,
                }
            }
            None if components == 1 => {
                let trc = parse_curve(tag(b"kTRC")?)?.0;
                Pipeline::Matrix {
                    trc: vec![trc],
                    m: [D50[0], 0.0, 0.0, D50[1], 0.0, 0.0, D50[2], 0.0, 0.0],
                    gray: true,
                }
            }
            None => return None,
        };
        let mut p = Profile {
            components,
            pipeline,
            black: [0.0; 3],
        };
        p.black = p.detect_black();
        Some(p)
    }

    fn detect_black(&self) -> [f32; 3] {
        let cands: &[&[f32]] = match self.components {
            1 => &[&[0.0]],
            3 => &[&[0.0, 0.0, 0.0]],
            _ => &[&[0.0, 0.0, 0.0, 1.0], &[1.0, 1.0, 1.0, 1.0]],
        };
        let mut best = [1.0f32, 1.0, 1.0];
        for c in cands {
            let x = self.to_xyz_raw(c);
            if x[1] < best[1] {
                best = x;
            }
        }
        // Only a dark black point is compensated.
        if best[1] > 0.5 || best.iter().any(|v| !v.is_finite()) {
            [0.0; 3]
        } else {
            [best[1] * D50[0], best[1], best[1] * D50[2]]
        }
    }

    fn to_xyz_raw(&self, input: &[f32]) -> [f32; 3] {
        match &self.pipeline {
            Pipeline::Matrix { trc, m, gray } => {
                if *gray {
                    let y = trc[0].eval(input[0]);
                    return [D50[0] * y, y, D50[2] * y];
                }
                let r = trc[0].eval(input[0]);
                let g = trc[1].eval(input[1]);
                let b = trc[2].eval(input[2]);
                [
                    m[0] * r + m[1] * g + m[2] * b,
                    m[3] * r + m[4] * g + m[5] * b,
                    m[6] * r + m[7] * g + m[8] * b,
                ]
            }
            Pipeline::Lut {
                matrix,
                b,
                m_curves,
                clut,
                a,
                mab,
                pcs,
            } => {
                let mut v = [0f32; 16];
                let n = input.len().min(15);
                v[..n].copy_from_slice(&input[..n]);
                let mut tmp = [0f32; 16];
                if *mab {
                    apply_curves(a, &mut v);
                    if let Some(c) = clut {
                        c.eval(&v[..c.inputs], &mut tmp);
                        v = tmp;
                    }
                    apply_curves(m_curves, &mut v);
                    if let Some(mx) = matrix {
                        v = apply_matrix(mx, &v);
                    }
                    apply_curves(b, &mut v);
                } else {
                    if let Some(mx) = matrix
                        && n == 3
                    {
                        v = apply_matrix(mx, &v);
                    }
                    apply_curves(a, &mut v);
                    if let Some(c) = clut {
                        c.eval(&v[..c.inputs], &mut tmp);
                        v = tmp;
                    }
                    apply_curves(b, &mut v);
                }
                match pcs {
                    Pcs::Xyz => {
                        let s = 65535.0 / 32768.0;
                        [v[0] * s, v[1] * s, v[2] * s]
                    }
                    Pcs::Lab => {
                        lab_to_xyz(v[0] * 100.0, v[1] * 255.0 - 128.0, v[2] * 255.0 - 128.0)
                    }
                    Pcs::Lab16 => {
                        let s = 65535.0 / 65280.0;
                        lab_to_xyz(
                            v[0] * 100.0 * s,
                            v[1] * 255.0 * s - 128.0,
                            v[2] * 255.0 * s - 128.0,
                        )
                    }
                }
            }
        }
    }

    /// Device values to linear... to sRGB-encoded 0..1.
    pub fn to_srgb(&self, input: &[f32]) -> [f32; 3] {
        let mut xyz = self.to_xyz_raw(input);
        let bk = self.black;
        if bk[1] > 0.0 {
            for i in 0..3 {
                xyz[i] = (xyz[i] - bk[i]) * D50[i] / (D50[i] - bk[i]);
            }
        }
        xyz_d50_to_srgb(xyz)
    }
}

fn apply_curves(c: &[Curve], v: &mut [f32; 16]) {
    for (x, c) in v.iter_mut().zip(c) {
        *x = c.eval(*x);
    }
}

fn apply_matrix(m: &[f32; 12], v: &[f32; 16]) -> [f32; 16] {
    let mut o = *v;
    for r in 0..3 {
        o[r] = (m[r * 3] * v[0] + m[r * 3 + 1] * v[1] + m[r * 3 + 2] * v[2] + m[9 + r])
            .clamp(0.0, 1.0);
    }
    o
}

pub fn lab_to_xyz(l: f32, a: f32, b: f32) -> [f32; 3] {
    lab_to_xyz_white(l, a, b, D50)
}

pub fn lab_to_xyz_white(l: f32, a: f32, b: f32, w: [f32; 3]) -> [f32; 3] {
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let g = |t: f32| {
        if t > 6.0 / 29.0 {
            t * t * t
        } else {
            108.0 / 841.0 * (t - 4.0 / 29.0)
        }
    };
    [w[0] * g(fx), w[1] * g(fy), w[2] * g(fz)]
}

/// D50 XYZ to sRGB-encoded 0..1, clipped.
pub fn xyz_d50_to_srgb(x: [f32; 3]) -> [f32; 3] {
    let m = XYZ_TO_SRGB;
    let lin = [
        m[0] * x[0] + m[1] * x[1] + m[2] * x[2],
        m[3] * x[0] + m[4] * x[1] + m[5] * x[2],
        m[6] * x[0] + m[7] * x[1] + m[8] * x[2],
    ];
    lin.map(srgb_encode)
}

pub fn srgb_encode(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.0031308 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// Bradford adaptation of XYZ from white point `w` to D50.
pub fn adapt_to_d50(xyz: [f32; 3], w: [f32; 3]) -> [f32; 3] {
    const B: [f32; 9] = [
        0.8951, 0.2664, -0.1614, -0.7502, 1.7135, 0.0367, 0.0389, -0.0685, 1.0296,
    ];
    const BI: [f32; 9] = [
        0.9869929, -0.1470543, 0.1599627, 0.4323053, 0.5183603, 0.0492912, -0.0085287, 0.0400428,
        0.9684867,
    ];
    let mul = |m: &[f32; 9], v: [f32; 3]| {
        [
            m[0] * v[0] + m[1] * v[1] + m[2] * v[2],
            m[3] * v[0] + m[4] * v[1] + m[5] * v[2],
            m[6] * v[0] + m[7] * v[1] + m[8] * v[2],
        ]
    };
    let src = mul(&B, w);
    let dst = mul(&B, D50);
    let c = mul(&B, xyz);
    let s = [
        c[0] * dst[0] / src[0],
        c[1] * dst[1] / src[1],
        c[2] * dst[2] / src[2],
    ];
    mul(&BI, s)
}

fn be32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

fn be16(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(o..o + 2)?.try_into().ok()?))
}

fn s15(d: &[u8], o: usize) -> Option<f32> {
    Some(be32(d, o)? as i32 as f32 / 65536.0)
}

fn parse_xyz(t: &[u8]) -> Option<[f32; 3]> {
    if t.get(0..4)? != b"XYZ " {
        return None;
    }
    Some([s15(t, 8)?, s15(t, 12)?, s15(t, 16)?])
}

/// A `curv` or `para` element; returns the curve and its byte length (4-byte padded).
fn parse_curve(t: &[u8]) -> Option<(Curve, usize)> {
    match t.get(0..4)? {
        b"curv" => {
            let n = be32(t, 8)? as usize;
            let len = 12 + 2 * n;
            let c = match n {
                0 => Curve::Identity,
                1 => Curve::Gamma(be16(t, 12)? as f32 / 256.0),
                _ => {
                    let mut v = Vec::with_capacity(n.min(1 << 16));
                    for i in 0..n.min(1 << 16) {
                        v.push(be16(t, 12 + 2 * i)? as f32 / 65535.0);
                    }
                    Curve::Table(v)
                }
            };
            Some((c, len.next_multiple_of(4)))
        }
        b"para" => {
            let kind = be16(t, 8)?;
            let np = [1, 3, 4, 5, 7].get(kind as usize).copied()?;
            let mut p = [0f32; 7];
            for (i, v) in p.iter_mut().enumerate().take(np) {
                *v = s15(t, 12 + 4 * i)?;
            }
            Some((Curve::Param(kind, p), (12 + 4 * np).next_multiple_of(4)))
        }
        _ => None,
    }
}

fn curves_at(t: &[u8], off: usize, n: usize) -> Option<Vec<Curve>> {
    let mut o = off;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let (c, len) = parse_curve(t.get(o..)?)?;
        v.push(c);
        o += len;
    }
    Some(v)
}

fn parse_lut(t: &[u8], inputs: usize, pcs: Pcs) -> Option<Pipeline> {
    match t.get(0..4)? {
        kind @ (b"mft1" | b"mft2") => {
            let wide = kind == b"mft2";
            let i = *t.get(8)? as usize;
            let o = *t.get(9)? as usize;
            let g = *t.get(10)? as usize;
            if i != inputs || o < 3 || i == 0 || i > 8 || g < 2 {
                return None;
            }
            let mut m = [0f32; 12];
            for (k, v) in m.iter_mut().take(9).enumerate() {
                *v = s15(t, 12 + 4 * k)?;
            }
            let identity = m[..9] == [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
            let (n_in, n_out, mut p) = if wide {
                (be16(t, 48)? as usize, be16(t, 50)? as usize, 52)
            } else {
                (256, 256, 48)
            };
            let sample = |p: usize| -> Option<f32> {
                if wide {
                    Some(be16(t, p)? as f32 / 65535.0)
                } else {
                    Some(*t.get(p)? as f32 / 255.0)
                }
            };
            let es = if wide { 2 } else { 1 };
            let table = |count: usize, p: &mut usize| -> Option<Curve> {
                let mut v = Vec::with_capacity(count);
                for _ in 0..count {
                    v.push(sample(*p)?);
                    *p += es;
                }
                Some(Curve::Table(v))
            };
            let a = (0..i)
                .map(|_| table(n_in, &mut p))
                .collect::<Option<Vec<_>>>()?;
            let total = g.checked_pow(i as u32)?.checked_mul(o)?;
            if total > 1 << 24 {
                return None;
            }
            let mut data = Vec::with_capacity(total);
            for _ in 0..total {
                data.push(sample(p)?);
                p += es;
            }
            let b = (0..o)
                .map(|_| table(n_out, &mut p))
                .collect::<Option<Vec<_>>>()?;
            let pcs = match pcs {
                Pcs::Lab if wide => Pcs::Lab16,
                p => p,
            };
            Some(Pipeline::Lut {
                matrix: (!identity).then_some(m),
                b,
                m_curves: Vec::new(),
                clut: Some(Clut {
                    inputs: i,
                    outputs: o,
                    grid: vec![g; i],
                    data,
                }),
                a,
                mab: false,
                pcs,
            })
        }
        b"mAB " => {
            let i = *t.get(8)? as usize;
            let o = *t.get(9)? as usize;
            if i != inputs || o != 3 || i > 8 {
                return None;
            }
            let off = |k: usize| be32(t, 12 + 4 * k).map(|v| v as usize);
            let (ob, om, ocl, oa) = (off(0)?, off(2)?, off(3)?, off(4)?);
            let omx = off(1)?;
            let b = curves_at(t, ob, o)?;
            let m_curves = if om != 0 {
                curves_at(t, om, o)?
            } else {
                Vec::new()
            };
            let matrix = if omx != 0 {
                let mut m = [0f32; 12];
                for (k, v) in m.iter_mut().enumerate() {
                    *v = s15(t, omx + 4 * k)?;
                }
                Some(m)
            } else {
                None
            };
            let a = if oa != 0 {
                curves_at(t, oa, i)?
            } else {
                Vec::new()
            };
            let clut = if ocl != 0 {
                let grid: Vec<usize> = (0..i)
                    .map(|k| t.get(ocl + k).map(|&g| g as usize))
                    .collect::<Option<_>>()?;
                if grid.iter().any(|&g| g < 2) {
                    return None;
                }
                let prec = *t.get(ocl + 16)? as usize;
                let total = grid.iter().try_fold(o, |a, &g| a.checked_mul(g))?;
                if total > 1 << 24 {
                    return None;
                }
                let base = ocl + 20;
                let mut data = Vec::with_capacity(total);
                for k in 0..total {
                    data.push(if prec == 1 {
                        *t.get(base + k)? as f32 / 255.0
                    } else {
                        be16(t, base + 2 * k)? as f32 / 65535.0
                    });
                }
                Some(Clut {
                    inputs: i,
                    outputs: o,
                    grid,
                    data,
                })
            } else {
                None
            };
            Some(Pipeline::Lut {
                matrix,
                b,
                m_curves,
                clut,
                a,
                mab: true,
                pcs,
            })
        }
        _ => None,
    }
}

/// A profile sampled on a regular grid to sRGB, for fast conversion.
#[derive(Debug)]
pub struct Transform {
    pub components: usize,
    grid: usize,
    /// sRGB 0..1 triplets.
    table: Vec<[f32; 3]>,
}

impl Transform {
    pub fn new(p: &Profile) -> Arc<Transform> {
        let n = p.components;
        let grid: usize = match n {
            1 => 256,
            3 => 17,
            _ => 11,
        };
        let total = grid.pow(n as u32);
        let mut table = Vec::with_capacity(total);
        let mut idx = vec![0usize; n];
        let mut v = vec![0f32; n];
        for _ in 0..total {
            for k in 0..n {
                v[k] = idx[k] as f32 / (grid - 1) as f32;
            }
            table.push(p.to_srgb(&v));
            // Last component varies fastest.
            for k in (0..n).rev() {
                idx[k] += 1;
                if idx[k] < grid {
                    break;
                }
                idx[k] = 0;
            }
        }
        Arc::new(Transform {
            components: n,
            grid,
            table,
        })
    }

    pub fn eval(&self, c: &[f32]) -> [f32; 3] {
        let n = self.components;
        let g = self.grid;
        let mut idx = [0usize; 4];
        let mut frac = [0f32; 4];
        for i in 0..n {
            let p = c.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0) * (g - 1) as f32;
            let f = p.floor().min((g - 2) as f32);
            idx[i] = f as usize;
            frac[i] = p - f;
        }
        let mut stride = [0usize; 4];
        let mut s = 1;
        for i in (0..n).rev() {
            stride[i] = s;
            s *= g;
        }
        let mut out = [0f32; 3];
        for corner in 0..1usize << n {
            let mut w = 1.0;
            let mut off = 0;
            for i in 0..n {
                let hi = corner >> i & 1;
                w *= if hi == 1 { frac[i] } else { 1.0 - frac[i] };
                off += (idx[i] + hi) * stride[i];
            }
            if w == 0.0 {
                continue;
            }
            let t = self.table[off];
            out[0] += w * t[0];
            out[1] += w * t[1];
            out[2] += w * t[2];
        }
        out
    }
}

#[cfg(test)]
mod probe {
    /// Prints conversions of a profile named by `PAGEBARA_ICC` (for comparing with LittleCMS).
    #[test]
    #[ignore]
    fn print_profile() {
        let Ok(p) = std::env::var("PAGEBARA_ICC") else {
            return;
        };
        let data = std::fs::read(p).unwrap();
        let prof = super::Profile::parse(&data).unwrap();
        let xf = super::Transform::new(&prof);
        for c in [
            [0u8, 0, 0, 0],
            [255, 0, 0, 0],
            [0, 255, 0, 0],
            [0, 0, 255, 0],
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [255, 128, 0, 0],
            [51, 25, 0, 0],
            [128, 128, 128, 128],
        ] {
            let v = c.map(|x| x as f32 / 255.0);
            let a = prof.to_srgb(&v).map(|x| (x * 255.0).round());
            let b = xf.eval(&v).map(|x| (x * 255.0).round());
            println!("{c:?} direct {a:?} grid {b:?}");
        }
    }
}
