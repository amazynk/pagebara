//! Image samples: decoding to packed rows, overwriting the pixels under regions, and
//! writing the result back as a Flate-compressed image.

use pagebara_syntax::{Dict, Document, Object, Stream};

use crate::geom::{Matrix, Point, Rect, Region};

/// Decoded samples of an image, packed `bpc` bits per component in rows padded to bytes.
#[derive(Clone, Debug)]
pub(crate) struct Samples {
    pub w: u32,
    pub h: u32,
    pub n: u32,
    pub bpc: u32,
    pub data: Vec<u8>,
    /// What the dictionary needs besides the new filter: the colour space and decode array
    /// when an image codec delivered other samples than the dictionary describes, and an
    /// alpha channel a JPX codestream carried.
    pub color_space: Option<Object>,
    pub drop_decode: bool,
    pub alpha: Option<Vec<u8>>,
}

const MAX_PIXELS: u64 = 1 << 28;

impl Samples {
    pub fn stride(&self) -> usize {
        (self.w as usize * self.n as usize * self.bpc as usize).div_ceil(8)
    }

    fn set(&mut self, x: u32, y: u32, value: u32) {
        let stride = self.stride();
        let row = &mut self.data[y as usize * stride..(y as usize + 1) * stride];
        let bpc = self.bpc as usize;
        for k in 0..self.n as usize {
            let bit = (x as usize * self.n as usize + k) * bpc;
            match bpc {
                8 => row[bit / 8] = value as u8,
                16 => {
                    row[bit / 8] = (value >> 8) as u8;
                    row[bit / 8 + 1] = value as u8;
                }
                _ => {
                    let shift = 8 - bpc - bit % 8;
                    let mask = (((1u32 << bpc) - 1) << shift) as u8;
                    let byte = &mut row[bit / 8];
                    *byte = (*byte & !mask) | (((value << shift) as u8) & mask);
                }
            }
        }
    }

    pub fn get(&self, x: u32, y: u32, k: u32) -> u32 {
        let stride = self.stride();
        let row = &self.data[y as usize * stride..];
        let bpc = self.bpc as usize;
        let bit = (x as usize * self.n as usize + k as usize) * bpc;
        match bpc {
            8 => u32::from(row[bit / 8]),
            16 => u32::from(row[bit / 8]) << 8 | u32::from(row[bit / 8 + 1]),
            _ => {
                let shift = 8 - bpc - bit % 8;
                u32::from(row[bit / 8] >> shift) & ((1 << bpc) - 1)
            }
        }
    }

    /// Sets every pixel in `mask` (row-major) to `value`; returns how many.
    pub fn blank(&mut self, mask: &[bool], value: u32) -> usize {
        let mut count = 0;
        for y in 0..self.h {
            for x in 0..self.w {
                if mask[(y * self.w + x) as usize] {
                    self.set(x, y, value);
                    count += 1;
                }
            }
        }
        if let Some(a) = &mut self.alpha {
            for (v, &m) in a.iter_mut().zip(mask) {
                if m {
                    *v = 0;
                }
            }
        }
        count
    }

    /// The image dictionary `orig` rewritten for these samples, with its stream data.
    pub fn encode(&self, orig: &Dict) -> (Dict, Vec<u8>) {
        let mut d = Dict::new();
        for (k, v) in orig.iter() {
            if !matches!(
                k.as_bytes(),
                b"Filter"
                    | b"DecodeParms"
                    | b"Length"
                    | b"F"
                    | b"FFilter"
                    | b"FDecodeParms"
                    | b"DL"
                    | b"SMaskInData"
            ) {
                d.set(k.as_bytes(), v.clone());
            }
        }
        d.set(b"Width", i64::from(self.w));
        d.set(b"Height", i64::from(self.h));
        if !matches!(orig.get(b"ImageMask"), Some(Object::Bool(true))) {
            d.set(b"BitsPerComponent", i64::from(self.bpc));
        }
        if let Some(cs) = &self.color_space {
            d.set(b"ColorSpace", cs.clone());
        }
        if self.drop_decode {
            d.remove(b"Decode");
        }
        d.set(b"Filter", Object::name(b"FlateDecode"));
        (d, pagebara_filters::encode_flate(&self.data, 6))
    }
}

/// Components per pixel of an image colour space; `res` resolves names of inline images.
pub(crate) fn components(doc: &Document, cs: &Object, res: Option<&Dict>) -> Option<u32> {
    let cs = doc.resolve(cs);
    match &*cs {
        Object::Name(n) => match n.as_bytes() {
            b"DeviceGray" | b"G" | b"CalGray" | b"Indexed" | b"I" => Some(1),
            b"DeviceRGB" | b"RGB" | b"CalRGB" | b"Lab" => Some(3),
            b"DeviceCMYK" | b"CMYK" => Some(4),
            name => {
                let spaces = doc.get_in(res?, b"ColorSpace");
                let o = spaces.as_dict()?.get(name)?.clone();
                components(doc, &o, None)
            }
        },
        Object::Array(a) => {
            let family = doc.resolve(a.first()?);
            match family.as_name()? {
                b"ICCBased" => {
                    let s = doc.resolve(a.get(1)?);
                    doc.get_in(s.as_dict()?, b"N")
                        .as_int()
                        .and_then(|n| u32::try_from(n).ok())
                        .filter(|n| (1..=4).contains(n))
                }
                b"Indexed" | b"I" | b"Separation" | b"CalGray" => Some(1),
                b"CalRGB" | b"Lab" => Some(3),
                b"DeviceN" => Some(doc.resolve(a.get(1)?).as_array()?.len() as u32),
                b"DeviceGray" | b"G" => Some(1),
                b"DeviceRGB" | b"RGB" => Some(3),
                b"DeviceCMYK" | b"CMYK" => Some(4),
                _ => None,
            }
        }
        _ => None,
    }
}

fn int(doc: &Document, d: &Dict, k: &[u8]) -> Option<i64> {
    doc.get_in(d, k).as_int()
}

/// Decodes an image (or soft mask) stream; `None` for what cannot be decoded.
pub(crate) fn decode(doc: &Document, s: &Stream, res: Option<&Dict>) -> Option<Samples> {
    let d = &s.dict;
    let w = u32::try_from(int(doc, d, b"Width")?).ok()?;
    let h = u32::try_from(int(doc, d, b"Height")?).ok()?;
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return None;
    }
    let stencil = doc.get_in(d, b"ImageMask").as_bool() == Some(true);
    let (n, bpc) = if stencil {
        (1, 1)
    } else {
        let bpc = int(doc, d, b"BitsPerComponent").unwrap_or(8);
        let bpc = u32::try_from(bpc)
            .ok()
            .filter(|b| [1, 2, 4, 8, 16].contains(b));
        let n = d.get(b"ColorSpace").and_then(|cs| components(doc, cs, res));
        (n.unwrap_or(0), bpc.unwrap_or(0))
    };
    let dec = doc.decode(s).ok()?;
    let Some(f) = &dec.image else {
        if n == 0 || bpc == 0 {
            return None;
        }
        let mut sm = Samples {
            w,
            h,
            n,
            bpc,
            data: dec.data,
            color_space: None,
            drop_decode: false,
            alpha: None,
        };
        // Short data is padded as readers do.
        let len = sm.stride() * h as usize;
        sm.data.resize(len, 0);
        return Some(sm);
    };
    match f.name.as_bytes() {
        b"DCTDecode" => jpeg(&dec.data),
        b"JPXDecode" => jpx(&dec.data, d.contains(b"ColorSpace") && n == 3),
        b"JBIG2Decode" => {
            let globals = f
                .parms
                .get(b"JBIG2Globals")
                .and_then(|g| doc.resolve(g).as_stream().cloned())
                .and_then(|g| doc.decode(&g).ok().map(|d| d.data));
            let p = pagebara_jbig2::decode_embedded_unpacked(&dec.data, globals.as_deref()).ok()?;
            Some(pack_bits(p.width, p.height, &p.data))
        }
        _ => None,
    }
}

fn pack_bits(w: u32, h: u32, px: &[u8]) -> Samples {
    let stride = (w as usize).div_ceil(8);
    let mut data = vec![0u8; stride * h as usize];
    for y in 0..h as usize {
        for x in 0..w as usize {
            if px.get(y * w as usize + x).copied().unwrap_or(0) != 0 {
                data[y * stride + x / 8] |= 0x80 >> (x % 8);
            }
        }
    }
    Samples {
        w,
        h,
        n: 1,
        bpc: 1,
        data,
        color_space: None,
        drop_decode: false,
        alpha: None,
    }
}

fn eight_bit(w: u32, h: u32, n: u32, data: Vec<u8>) -> Samples {
    Samples {
        w,
        h,
        n,
        bpc: 8,
        data,
        color_space: None,
        drop_decode: false,
        alpha: None,
    }
}

/// JPEG samples as the image's colour space expects them (as the renderer reads them).
fn jpeg(data: &[u8]) -> Option<Samples> {
    use pagebara_jpeg::{ColorModel, DecodeOptions, PlanesLayout, Upsampling};
    let info = pagebara_jpeg::read_info(data).ok()?;
    let mut p = pagebara_jpeg::decode_components(data, &DecodeOptions::default()).ok()?;
    let (w, h) = (p.width, p.height);
    let px = w as usize * h as usize;
    let mut rgba = vec![0u8; px * 4];
    match (info.components.len(), p.layout) {
        (1, _) | (_, PlanesLayout::Gray) => {
            p.layout = PlanesLayout::Gray;
            pagebara_jpeg::planes_to_rgba_into(&p, Upsampling::Bilinear, &mut rgba, w as usize * 4);
            let d = rgba.as_chunks::<4>().0.iter().map(|c| c[0]).collect();
            Some(eight_bit(w, h, 1, d))
        }
        (4, _) => {
            let ycck = info.color == ColorModel::Ycck;
            p.layout = if ycck {
                PlanesLayout::YCbCr
            } else {
                PlanesLayout::Rgb
            };
            pagebara_jpeg::planes_to_rgba_into(&p, Upsampling::Bilinear, &mut rgba, w as usize * 4);
            let adobe = info.adobe_transform.is_some();
            for c in rgba.as_chunks_mut::<4>().0 {
                if ycck {
                    c[0] = 255 - c[0];
                    c[1] = 255 - c[1];
                    c[2] = 255 - c[2];
                }
                if adobe {
                    *c = c.map(|v| 255 - v);
                }
            }
            // Stored as read, so the dictionary's decode array applies as before.
            Some(eight_bit(w, h, 4, rgba))
        }
        _ => {
            p.planes.truncate(3);
            pagebara_jpeg::planes_to_rgba_into(&p, Upsampling::Bilinear, &mut rgba, w as usize * 4);
            let d = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|c| [c[0], c[1], c[2]])
                .collect();
            Some(eight_bit(w, h, 3, d))
        }
    }
}

fn jpx(data: &[u8], keep_space: bool) -> Option<Samples> {
    use pagebara_jp2::{DecodeOptions, PixelFormat, Pixels};
    let info = pagebara_jp2::read_info(data).ok()?;
    let img = pagebara_jp2::decode(data, &DecodeOptions::default()).ok()?;
    let Pixels::U8(px) = img.pixels else {
        return None;
    };
    let (w, h) = (img.width, img.height);
    let mut s = match img.format {
        PixelFormat::Gray8 => {
            let mut s = eight_bit(w, h, 1, px);
            s.color_space = Some(Object::name(b"DeviceGray"));
            s
        }
        _ => {
            let alpha: Option<Vec<u8>> = info
                .has_alpha
                .then(|| px.as_chunks::<4>().0.iter().map(|c| c[3]).collect());
            let d = px
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|c| [c[0], c[1], c[2]])
                .collect();
            let mut s = eight_bit(w, h, 3, d);
            s.alpha = alpha;
            let converted =
                matches!(info.color, pagebara_jp2::ColorSpace::Cmyk) || info.components.len() == 4;
            if converted || !keep_space {
                s.color_space = Some(Object::name(b"DeviceRGB"));
            }
            s
        }
    };
    s.drop_decode = s.color_space.is_some();
    Some(s)
}

/// Pixels of a `w` × `h` image placed by `ctm` whose cell overlaps a region with positive
/// area, row-major from the top row; `None` if no pixel does.
pub(crate) fn coverage(w: u32, h: u32, ctm: &Matrix, regions: &[&Region]) -> Option<Vec<bool>> {
    let px_to_page =
        Matrix::new(1.0 / f64::from(w), 0.0, 0.0, -1.0 / f64::from(h), 0.0, 1.0).then(ctm);
    let to_px = px_to_page.invert()?;
    let mut mask: Option<Vec<bool>> = None;
    for r in regions {
        let Some(poly) = r.transform(&to_px) else {
            continue;
        };
        let b = poly.bbox();
        let clamp = |v: f64, max: u32| v.clamp(0.0, f64::from(max)) as u32;
        let (x0, x1) = (clamp(b.x0.floor(), w), clamp(b.x1.ceil(), w));
        let (y0, y1) = (clamp(b.y0.floor(), h), clamp(b.y1.ceil(), h));
        let axes: Vec<(Point, f64)> = poly.edges().collect();
        for y in y0..y1 {
            for x in x0..x1 {
                let cell = Rect::new(
                    f64::from(x),
                    f64::from(y),
                    f64::from(x + 1),
                    f64::from(y + 1),
                );
                if cell_overlaps(&cell, &b, &axes) {
                    mask.get_or_insert_with(|| vec![false; w as usize * h as usize])
                        [(y * w + x) as usize] = true;
                }
            }
        }
    }
    mask
}

/// Separating axis test of a pixel cell against a convex polygon (its bounding box and
/// edge half-planes), requiring a little more than touching.
fn cell_overlaps(cell: &Rect, bbox: &Rect, edges: &[(Point, f64)]) -> bool {
    const EPS: f64 = 1e-6;
    if cell.x1 <= bbox.x0 + EPS
        || cell.x0 >= bbox.x1 - EPS
        || cell.y1 <= bbox.y0 + EPS
        || cell.y0 >= bbox.y1 - EPS
    {
        return false;
    }
    edges.iter().all(|&(n, c)| {
        let len = n.dot(n).sqrt();
        let min = cell
            .corners()
            .iter()
            .map(|p| n.dot(*p))
            .fold(f64::INFINITY, f64::min);
        min < c - EPS * len
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_samples() {
        let mut s = Samples {
            w: 5,
            h: 2,
            n: 1,
            bpc: 1,
            data: vec![0xFF; 2],
            color_space: None,
            drop_decode: false,
            alpha: None,
        };
        let mut mask = vec![false; 10];
        mask[2] = true;
        mask[9] = true;
        assert_eq!(s.blank(&mask, 0), 2);
        assert_eq!(s.data, [0b1101_1111, 0b1111_0111]);
        let mut s = Samples {
            bpc: 4,
            n: 3,
            w: 1,
            h: 1,
            data: vec![0x12, 0x34],
            ..s
        };
        s.set(0, 0, 0xA);
        assert_eq!(s.data, [0xAA, 0xA4]);
        assert_eq!(s.get(0, 0, 2), 0xA);
    }

    #[test]
    fn coverage_of_a_placed_image() {
        // 10 × 10 pixels on (0,0)–(100,100); the region covers x 25..45, y 0..10 (bottom).
        let ctm = Matrix::new(100.0, 0.0, 0.0, 100.0, 0.0, 0.0);
        let r = Region::from_rect(Rect::new(25.0, 0.0, 45.0, 10.0));
        let m = coverage(10, 10, &ctm, &[&r]).unwrap();
        let hit: Vec<usize> = (0..100).filter(|&i| m[i]).collect();
        assert_eq!(hit, [92, 93, 94]);
    }
}
