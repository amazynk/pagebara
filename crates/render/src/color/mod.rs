//! Colour spaces (§8.6) converted to sRGB.

pub mod icc;

use std::sync::Arc;

use pagebara_syntax::{Document, Object};

use crate::function::{Function, numbers};
use icc::{Transform, adapt_to_d50, lab_to_xyz_white, xyz_d50_to_srgb};

const MAX_DEPTH: u32 = 8;
pub const MAX_COMPONENTS: usize = 32;

/// A colour space ready for conversion.
#[derive(Clone, Debug)]
pub enum Cs {
    Gray,
    Rgb,
    Cmyk,
    CalGray {
        white: [f32; 3],
        gamma: f32,
    },
    CalRgb {
        white: [f32; 3],
        gamma: [f32; 3],
        matrix: [f32; 9],
    },
    Lab {
        white: [f32; 3],
        range: [f32; 4],
    },
    Icc {
        xf: Arc<Transform>,
        /// The space's own kind for blending (1, 3 or 4 components).
        n: usize,
        range: Vec<f32>,
    },
    Indexed {
        base: Arc<Cs>,
        hival: u32,
        /// sRGB per entry.
        rgb: Vec<[f32; 3]>,
    },
    /// Separation and DeviceN.
    Tint {
        n: usize,
        alt: Arc<Cs>,
        func: Option<Function>,
        /// Every colorant is `None`: nothing is painted.
        none: bool,
    },
    Pattern {
        base: Option<Arc<Cs>>,
    },
}

impl Cs {
    pub fn components(&self) -> usize {
        match self {
            Cs::Gray | Cs::CalGray { .. } => 1,
            Cs::Rgb | Cs::CalRgb { .. } | Cs::Lab { .. } => 3,
            Cs::Cmyk => 4,
            Cs::Icc { n, .. } => *n,
            Cs::Indexed { .. } => 1,
            Cs::Tint { n, .. } => *n,
            Cs::Pattern { base } => base.as_ref().map_or(0, |b| b.components()),
        }
    }

    pub fn is_none(&self) -> bool {
        matches!(self, Cs::Tint { none: true, .. })
    }

    /// The default `/Decode` range of component `i` for `bpc`-bit samples.
    pub fn decode_range(&self, i: usize, bpc: u32) -> (f32, f32) {
        match self {
            Cs::Lab { range, .. } if i > 0 => (range[2 * (i - 1)], range[2 * i - 1]),
            Cs::Lab { .. } => (0.0, 100.0),
            Cs::Indexed { .. } => (0.0, ((1u32 << bpc.min(16)) - 1) as f32),
            Cs::Icc { range, .. } if range.len() > 2 * i + 1 => (range[2 * i], range[2 * i + 1]),
            _ => (0.0, 1.0),
        }
    }

    /// Components to sRGB, each 0..1.
    pub fn to_rgb(&self, c: &[f32]) -> [f32; 3] {
        let g = |i: usize| c.get(i).copied().unwrap_or(0.0);
        match self {
            Cs::Gray => {
                let v = g(0).clamp(0.0, 1.0);
                [v, v, v]
            }
            Cs::Rgb => [g(0), g(1), g(2)].map(|v| v.clamp(0.0, 1.0)),
            Cs::Cmyk => cmyk_to_rgb(g(0), g(1), g(2), g(3)),
            Cs::CalGray { white, gamma } => {
                let a = g(0).clamp(0.0, 1.0).powf(*gamma);
                let xyz = [white[0] * a, white[1] * a, white[2] * a];
                xyz_d50_to_srgb(adapt_to_d50(xyz, *white))
            }
            Cs::CalRgb {
                white,
                gamma,
                matrix: m,
            } => {
                let a = g(0).clamp(0.0, 1.0).powf(gamma[0]);
                let b = g(1).clamp(0.0, 1.0).powf(gamma[1]);
                let cc = g(2).clamp(0.0, 1.0).powf(gamma[2]);
                let xyz = [
                    m[0] * a + m[3] * b + m[6] * cc,
                    m[1] * a + m[4] * b + m[7] * cc,
                    m[2] * a + m[5] * b + m[8] * cc,
                ];
                xyz_d50_to_srgb(adapt_to_d50(xyz, *white))
            }
            Cs::Lab { white, range } => {
                let l = g(0).clamp(0.0, 100.0);
                let a = g(1).clamp(range[0], range[1]);
                let b = g(2).clamp(range[2], range[3]);
                let xyz = lab_to_xyz_white(l, a, b, *white);
                xyz_d50_to_srgb(adapt_to_d50(xyz, *white))
            }
            Cs::Icc { xf, range, .. } => {
                let mut v = [0f32; 4];
                for (i, x) in v.iter_mut().enumerate().take(xf.components) {
                    let (lo, hi) = (
                        range.get(2 * i).copied().unwrap_or(0.0),
                        range.get(2 * i + 1).copied().unwrap_or(1.0),
                    );
                    *x = if hi > lo {
                        (g(i) - lo) / (hi - lo)
                    } else {
                        g(i)
                    };
                }
                xf.eval(&v[..xf.components])
            }
            Cs::Indexed { rgb, hival, .. } => {
                let i = (g(0).round().max(0.0) as u32).min(*hival) as usize;
                rgb.get(i).copied().unwrap_or([0.0; 3])
            }
            Cs::Tint { alt, func, n, .. } => {
                let mut out = [0f32; MAX_COMPONENTS];
                match func {
                    Some(f) => {
                        f.eval(&c[..(*n).min(c.len())], &mut out);
                    }
                    None => {
                        // No usable tint transform: treat the tint as gray ink.
                        let v = 1.0 - g(0).clamp(0.0, 1.0);
                        return [v, v, v];
                    }
                }
                alt.to_rgb(&out[..alt.components()])
            }
            Cs::Pattern { base } => base.as_ref().map_or([0.0; 3], |b| b.to_rgb(c)),
        }
    }

    /// The initial colour (§8.6.5) components.
    pub fn initial(&self) -> Vec<f32> {
        match self {
            Cs::Cmyk => vec![0.0, 0.0, 0.0, 1.0],
            Cs::Tint { n, .. } => vec![1.0; *n],
            Cs::Lab { range, .. } => vec![0.0, range[0].max(0.0), range[2].max(0.0)],
            c => vec![0.0; c.components()],
        }
    }
}

/// DeviceCMYK to sRGB without a profile: the subtractive formula with a mild correction
/// towards printed ink (a full-coverage black reads as black).
pub fn cmyk_to_rgb(c: f32, m: f32, y: f32, k: f32) -> [f32; 3] {
    let (c, m, y, k) = (
        c.clamp(0.0, 1.0),
        m.clamp(0.0, 1.0),
        y.clamp(0.0, 1.0),
        k.clamp(0.0, 1.0),
    );
    [
        (1.0 - c) * (1.0 - k),
        (1.0 - m) * (1.0 - k),
        (1.0 - y) * (1.0 - k),
    ]
}

/// Builds (or finds cached) the transform of an ICC profile's data.
pub type IccLoader<'a> = dyn Fn(&[u8]) -> Option<Arc<Transform>> + 'a;

/// Resolves colour space objects; profile transforms are cached by the caller.
pub struct Resolver<'a> {
    pub doc: &'a Document,
    pub icc: &'a IccLoader<'a>,
}

impl std::fmt::Debug for Resolver<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Resolver")
    }
}

impl Resolver<'_> {
    /// A colour space from its object; `None` for something unusable.
    pub fn resolve(&self, o: &Object) -> Option<Cs> {
        self.resolve_depth(o, 0)
    }

    fn resolve_depth(&self, o: &Object, depth: u32) -> Option<Cs> {
        if depth > MAX_DEPTH {
            return None;
        }
        let doc = self.doc;
        let r = doc.resolve(o);
        let (family, args): (Vec<u8>, &[Object]) = match &*r {
            Object::Name(n) => (n.as_bytes().to_vec(), &[]),
            Object::Array(a) => {
                let f = doc.resolve(a.first()?);
                (f.as_name()?.to_vec(), &a[1..])
            }
            _ => return None,
        };
        let arg = |i: usize| args.get(i).cloned().unwrap_or(Object::Null);
        let dict_arg = |i: usize| doc.resolve(&arg(i)).as_dict().cloned();
        let white = |d: &pagebara_syntax::Dict| {
            numbers(doc, d, b"WhitePoint")
                .filter(|v| v.len() == 3 && v[1] > 0.0)
                .map_or([0.9505, 1.0, 1.089], |v| [v[0], v[1], v[2]])
        };
        Some(match &family[..] {
            b"DeviceGray" | b"G" => Cs::Gray,
            b"CalCMYK" => Cs::Cmyk,
            b"DeviceRGB" | b"RGB" => Cs::Rgb,
            b"DeviceCMYK" | b"CMYK" => Cs::Cmyk,
            b"CalGray" => {
                let d = dict_arg(0).unwrap_or_default();
                let gamma = doc.get_in(&d, b"Gamma").as_f64().unwrap_or(1.0) as f32;
                Cs::CalGray {
                    white: white(&d),
                    gamma,
                }
            }
            b"CalRGB" => {
                let d = dict_arg(0).unwrap_or_default();
                let gamma = numbers(doc, &d, b"Gamma")
                    .filter(|v| v.len() == 3)
                    .map_or([1.0; 3], |v| [v[0], v[1], v[2]]);
                let matrix = numbers(doc, &d, b"Matrix")
                    .filter(|v| v.len() == 9)
                    .map_or([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], |v| {
                        v.try_into().unwrap_or_default()
                    });
                Cs::CalRgb {
                    white: white(&d),
                    gamma,
                    matrix,
                }
            }
            b"Lab" => {
                let d = dict_arg(0).unwrap_or_default();
                let range = numbers(doc, &d, b"Range")
                    .filter(|v| v.len() == 4)
                    .map_or([-100.0, 100.0, -100.0, 100.0], |v| [v[0], v[1], v[2], v[3]]);
                Cs::Lab {
                    white: white(&d),
                    range,
                }
            }
            b"ICCBased" => {
                let so = arg(0);
                let s = doc.resolve(&so);
                let s = s.as_stream()?;
                let n = doc.get_in(&s.dict, b"N").as_int().unwrap_or(0) as usize;
                let alt = || -> Option<Cs> {
                    match s.dict.get(b"Alternate") {
                        Some(a) => self.resolve_depth(a, depth + 1),
                        None => None,
                    }
                    .or(match n {
                        1 => Some(Cs::Gray),
                        3 => Some(Cs::Rgb),
                        4 => Some(Cs::Cmyk),
                        _ => None,
                    })
                };
                let data = doc.decode(s).ok().map(|d| d.data);
                let xf = data.and_then(|d| (self.icc)(&d));
                match xf {
                    Some(xf) if n == 0 || xf.components == n => {
                        let range = numbers(doc, &s.dict, b"Range").unwrap_or_default();
                        // Device-equivalent sRGB/gray profiles convert exactly as device
                        // spaces would; keep the transform anyway for fidelity.
                        Cs::Icc {
                            n: xf.components,
                            xf,
                            range,
                        }
                    }
                    _ => alt()?,
                }
            }
            b"Indexed" | b"I" => {
                let base = Arc::new(self.resolve_depth(&arg(0), depth + 1)?);
                let hival = doc.resolve(&arg(1)).as_int().unwrap_or(0).clamp(0, 255) as u32;
                let lookup = match &*doc.resolve(&arg(2)) {
                    Object::String(s) => s.bytes.to_vec(),
                    Object::Stream(s) => doc.decode(s).ok()?.data,
                    _ => Vec::new(),
                };
                let n = base.components().max(1);
                let mut rgb = Vec::with_capacity(hival as usize + 1);
                let mut comps = vec![0f32; n];
                for i in 0..=hival as usize {
                    for (k, c) in comps.iter_mut().enumerate() {
                        let b = lookup.get(i * n + k).copied().unwrap_or(0) as f32 / 255.0;
                        let (lo, hi) = base.decode_range(k, 8);
                        *c = lo + b * (hi - lo);
                    }
                    rgb.push(base.to_rgb(&comps));
                }
                Cs::Indexed { base, hival, rgb }
            }
            b"Separation" | b"DeviceN" => {
                let a0 = arg(0);
                let names = doc.resolve(&a0);
                let names: Vec<Vec<u8>> = match &*names {
                    Object::Name(n) => vec![n.as_bytes().to_vec()],
                    Object::Array(a) => a
                        .iter()
                        .filter_map(|n| doc.resolve(n).as_name().map(<[u8]>::to_vec))
                        .collect(),
                    _ => vec![b"Unknown".to_vec()],
                };
                let n = names.len().clamp(1, MAX_COMPONENTS);
                let alt = Arc::new(self.resolve_depth(&arg(1), depth + 1).unwrap_or(Cs::Gray));
                let func = Function::parse(doc, &arg(2));
                let none = names.iter().all(|x| x == b"None");
                Cs::Tint { n, alt, func, none }
            }
            b"Pattern" => Cs::Pattern {
                base: args
                    .first()
                    .and_then(|b| self.resolve_depth(b, depth + 1))
                    .map(Arc::new),
            },
            _ => return None,
        })
    }
}

/// A component vector to premultiplied-ready sRGB bytes.
pub fn rgb8(c: [f32; 3]) -> [u8; 3] {
    c.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
}

/// Rec. 601 luma weights used by PDF's luminosity (§11.6.5.2 non-separable blend modes).
pub fn luminosity(r: f32, g: f32, b: f32) -> f32 {
    0.3 * r + 0.59 * g + 0.11 * b
}
