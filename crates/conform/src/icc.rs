//! ICC profiles written by pagebara (so no third-party profile is shipped): an sRGB display
//! profile (IEC 61966-2-1 primaries and tone curve, adapted to D50) for PDF/A output
//! intents, and a CMYK profile matching the plain CMYK-to-RGB conversion viewers use for
//! `DeviceCMYK`, for `DefaultCMYK` when the output intent is RGB. Both are ICC version 2.

/// What PDF/A looks at in a profile header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub major: u8,
    pub minor: u8,
    /// Device class: `mntr`, `prtr`, `scnr`, …
    pub class: [u8; 4],
    /// Data colour space: `RGB `, `CMYK`, `GRAY`, …
    pub space: [u8; 4],
    /// The size field matches the data and the `acsp` signature is there.
    pub well_formed: bool,
}

impl Header {
    /// Components of the data colour space, when known.
    pub fn components(&self) -> Option<u8> {
        match &self.space {
            b"GRAY" => Some(1),
            b"RGB " | b"Lab " | b"XYZ " => Some(3),
            b"CMYK" => Some(4),
            _ => None,
        }
    }
}

pub fn header(data: &[u8]) -> Option<Header> {
    if data.len() < 128 {
        return None;
    }
    let size = u32::from_be_bytes(data[0..4].try_into().ok()?) as usize;
    Some(Header {
        major: data[8],
        minor: data[9] >> 4,
        class: data[12..16].try_into().ok()?,
        space: data[16..20].try_into().ok()?,
        well_formed: &data[36..40] == b"acsp" && size <= data.len() && size >= 128,
    })
}

fn s15(v: f64) -> [u8; 4] {
    ((v * 65536.0).round() as i32).to_be_bytes()
}

fn xyz(x: f64, y: f64, z: f64) -> Vec<u8> {
    let mut t = b"XYZ \0\0\0\0".to_vec();
    for v in [x, y, z] {
        t.extend_from_slice(&s15(v));
    }
    t
}

fn desc(s: &str) -> Vec<u8> {
    let mut t = b"desc\0\0\0\0".to_vec();
    t.extend_from_slice(&(s.len() as u32 + 1).to_be_bytes());
    t.extend_from_slice(s.as_bytes());
    t.push(0);
    t.extend_from_slice(&[0; 8]);
    t.extend_from_slice(&[0; 3]);
    t.extend_from_slice(&[0; 67]);
    t
}

fn text(s: &str) -> Vec<u8> {
    let mut t = b"text\0\0\0\0".to_vec();
    t.extend_from_slice(s.as_bytes());
    t.push(0);
    t
}

/// The sRGB transfer function, encoded value to linear.
fn srgb_to_linear(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(v: f64) -> f64 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// The sRGB primaries adapted to D50 (Bradford): columns are R, G, B.
const RGB_TO_XYZ_D50: [[f64; 3]; 3] = [
    [0.4360747, 0.3850649, 0.1430804],
    [0.2225045, 0.7168786, 0.0606169],
    [0.0139322, 0.0971045, 0.7141733],
];
const D50: [f64; 3] = [0.9642, 1.0, 0.8249];

fn curve() -> Vec<u8> {
    let n = 1024u32;
    let mut t = b"curv\0\0\0\0".to_vec();
    t.extend_from_slice(&n.to_be_bytes());
    for i in 0..n {
        let v = srgb_to_linear(f64::from(i) / f64::from(n - 1));
        t.extend_from_slice(&((v * 65535.0).round() as u16).to_be_bytes());
    }
    t
}

/// Assembles a profile from its header fields and tags (shared data for equal tags).
fn profile(
    class: &[u8; 4],
    space: &[u8; 4],
    pcs: &[u8; 4],
    tags: &[(&[u8; 4], Vec<u8>)],
) -> Vec<u8> {
    let mut data: Vec<u8> = Vec::new();
    let table_len = 4 + 12 * tags.len();
    let base = 128 + table_len;
    let mut entries = Vec::new();
    let mut placed: Vec<(Vec<u8>, usize)> = Vec::new();
    for (sig, t) in tags {
        let off = match placed.iter().find(|p| p.0 == *t) {
            Some(p) => p.1,
            None => {
                while !data.len().is_multiple_of(4) {
                    data.push(0);
                }
                let off = base + data.len();
                data.extend_from_slice(t);
                placed.push((t.clone(), off));
                off
            }
        };
        entries.push((**sig, off, t.len()));
    }
    while !data.len().is_multiple_of(4) {
        data.push(0);
    }
    let size = base + data.len();
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(&(size as u32).to_be_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&[2, 0x10, 0, 0]);
    out.extend_from_slice(class);
    out.extend_from_slice(space);
    out.extend_from_slice(pcs);
    for v in [2026u16, 1, 1, 0, 0, 0] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend_from_slice(b"acsp");
    out.extend_from_slice(&[0; 24]);
    out.extend_from_slice(&0u32.to_be_bytes());
    for v in D50 {
        out.extend_from_slice(&s15(v));
    }
    out.extend_from_slice(&[0; 48]);
    debug_assert_eq!(out.len(), 128);
    out.extend_from_slice(&(tags.len() as u32).to_be_bytes());
    for (sig, off, len) in entries {
        out.extend_from_slice(&sig);
        out.extend_from_slice(&(off as u32).to_be_bytes());
        out.extend_from_slice(&(len as u32).to_be_bytes());
    }
    out.extend_from_slice(&data);
    out
}

const COPYRIGHT: &str = "No copyright, use freely (generated by pagebara)";

/// An sRGB display profile.
pub fn srgb() -> Vec<u8> {
    let m = RGB_TO_XYZ_D50;
    let c = curve();
    profile(
        b"mntr",
        b"RGB ",
        b"XYZ ",
        &[
            (b"desc", desc("sRGB IEC61966-2.1")),
            (b"cprt", text(COPYRIGHT)),
            (b"wtpt", xyz(D50[0], D50[1], D50[2])),
            (b"rXYZ", xyz(m[0][0], m[1][0], m[2][0])),
            (b"gXYZ", xyz(m[0][1], m[1][1], m[2][1])),
            (b"bXYZ", xyz(m[0][2], m[1][2], m[2][2])),
            (b"rTRC", c.clone()),
            (b"gTRC", c.clone()),
            (b"bTRC", c),
        ],
    )
}

fn lab_f(t: f64) -> f64 {
    if t > 216.0 / 24389.0 {
        t.cbrt()
    } else {
        (24389.0 / 27.0 * t + 16.0) / 116.0
    }
}

fn lab_finv(t: f64) -> f64 {
    if t * t * t > 216.0 / 24389.0 {
        t * t * t
    } else {
        (116.0 * t - 16.0) * 27.0 / 24389.0
    }
}

/// Encoded sRGB to Lab (D50).
fn rgb_to_lab(rgb: [f64; 3]) -> [f64; 3] {
    let lin = rgb.map(srgb_to_linear);
    let m = RGB_TO_XYZ_D50;
    let x: Vec<f64> = (0..3)
        .map(|i| (m[i][0] * lin[0] + m[i][1] * lin[1] + m[i][2] * lin[2]) / D50[i])
        .collect();
    let (fx, fy, fz) = (lab_f(x[0]), lab_f(x[1]), lab_f(x[2]));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn lab_to_rgb(lab: [f64; 3]) -> [f64; 3] {
    let fy = (lab[0] + 16.0) / 116.0;
    let fx = fy + lab[1] / 500.0;
    let fz = fy - lab[2] / 200.0;
    let xyz = [
        lab_finv(fx) * D50[0],
        lab_finv(fy) * D50[1],
        lab_finv(fz) * D50[2],
    ];
    // Inverse of RGB_TO_XYZ_D50.
    let inv = [
        [3.1338561, -1.6168667, -0.4906146],
        [-0.9787684, 1.9161415, 0.0334540],
        [0.0719453, -0.2289914, 1.4052427],
    ];
    let lin: Vec<f64> = (0..3)
        .map(|i| inv[i][0] * xyz[0] + inv[i][1] * xyz[1] + inv[i][2] * xyz[2])
        .collect();
    [
        linear_to_srgb(lin[0]),
        linear_to_srgb(lin[1]),
        linear_to_srgb(lin[2]),
    ]
}

/// Lab as ICC version 2 16-bit values.
fn lab16(lab: [f64; 3]) -> [u16; 3] {
    [
        (lab[0] / 100.0 * 65280.0).round().clamp(0.0, 65535.0) as u16,
        ((lab[1] + 128.0) * 256.0).round().clamp(0.0, 65535.0) as u16,
        ((lab[2] + 128.0) * 256.0).round().clamp(0.0, 65535.0) as u16,
    ]
}

/// A `lut16Type` with linear input and output tables and `clut` from `f`.
fn lut16(inputs: usize, outputs: usize, grid: usize, f: impl Fn(&[f64]) -> Vec<u16>) -> Vec<u8> {
    let mut t = b"mft2\0\0\0\0".to_vec();
    t.extend_from_slice(&[inputs as u8, outputs as u8, grid as u8, 0]);
    for (i, row) in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
        .iter()
        .enumerate()
    {
        let _ = i;
        for v in row {
            t.extend_from_slice(&s15(*v));
        }
    }
    t.extend_from_slice(&2u16.to_be_bytes());
    t.extend_from_slice(&2u16.to_be_bytes());
    for _ in 0..inputs {
        t.extend_from_slice(&[0, 0, 0xff, 0xff]);
    }
    let total = grid.pow(inputs as u32);
    let mut idx = vec![0usize; inputs];
    for n in 0..total {
        let mut rest = n;
        for i in (0..inputs).rev() {
            idx[i] = rest % grid;
            rest /= grid;
        }
        let x: Vec<f64> = idx.iter().map(|&i| i as f64 / (grid - 1) as f64).collect();
        for v in f(&x) {
            t.extend_from_slice(&v.to_be_bytes());
        }
    }
    for _ in 0..outputs {
        t.extend_from_slice(&[0, 0, 0xff, 0xff]);
    }
    t
}

/// A CMYK output profile matching `R = (1 - C)(1 - K)` and the like on an sRGB display.
pub fn cmyk() -> Vec<u8> {
    let a2b = lut16(4, 3, 7, |x| {
        let k = 1.0 - x[3];
        let rgb = [(1.0 - x[0]) * k, (1.0 - x[1]) * k, (1.0 - x[2]) * k];
        lab16(rgb_to_lab(rgb)).to_vec()
    });
    let b2a = lut16(3, 4, 11, |x| {
        let lab = [
            x[0] * 65535.0 / 65280.0 * 100.0,
            x[1] * 65535.0 / 256.0 - 128.0,
            x[2] * 65535.0 / 256.0 - 128.0,
        ];
        let rgb = lab_to_rgb(lab);
        let k = 1.0 - rgb[0].max(rgb[1]).max(rgb[2]);
        let cmy = if k >= 1.0 {
            [0.0; 3]
        } else {
            rgb.map(|c| (1.0 - c - k) / (1.0 - k))
        };
        [cmy[0], cmy[1], cmy[2], k]
            .map(|v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16)
            .to_vec()
    });
    profile(
        b"prtr",
        b"CMYK",
        b"Lab ",
        &[
            (b"desc", desc("Uncalibrated CMYK (pagebara)")),
            (b"cprt", text(COPYRIGHT)),
            (b"wtpt", xyz(D50[0], D50[1], D50[2])),
            (b"A2B0", a2b),
            (b"B2A0", b2a),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_have_valid_headers() {
        let s = srgb();
        let h = header(&s).unwrap();
        assert!(h.well_formed && h.major == 2 && &h.class == b"mntr" && h.components() == Some(3));
        assert_eq!(s.len() % 4, 0);
        let c = cmyk();
        let h = header(&c).unwrap();
        assert!(h.well_formed && h.components() == Some(4) && &h.class == b"prtr");
    }

    #[test]
    fn lab_round_trips() {
        for rgb in [[1.0, 1.0, 1.0], [0.2, 0.5, 0.9], [0.0, 0.0, 0.0]] {
            let back = lab_to_rgb(rgb_to_lab(rgb));
            for i in 0..3 {
                assert!((back[i] - rgb[i]).abs() < 0.002, "{rgb:?} -> {back:?}");
            }
        }
    }
}
