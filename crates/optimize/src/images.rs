//! Image work for the lossy presets: downsampling to a target resolution, JPEG for photos
//! and PNG-predicted Flate for graphics, grey conversion, and Group 4 for bilevel images
//! (lossless). Images run in parallel.

use std::collections::HashSet;
use std::sync::Arc;

use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};
use rayon::prelude::*;

use crate::scan::Usage;
use crate::streams::{components, deflate, png_predict, with_flate};
use crate::{Effort, Images, Report};

/// Images are only downsampled when drawn at more than this times the target.
const THRESHOLD: f32 = 1.5;
/// More distinct colours than this in a sample make an image a photo.
const PHOTO_COLORS: usize = 1024;

struct Pixels {
    w: usize,
    h: usize,
    n: usize,
    data: Vec<u8>,
    jpeg: bool,
}

/// What happened to one image.
enum Done {
    Resampled(Stream),
    Reencoded(Stream),
}

fn filters(doc: &Document, d: &Dict) -> Vec<Vec<u8>> {
    match &*doc.get_in(d, b"Filter") {
        Object::Name(n) => vec![n.as_bytes().to_vec()],
        Object::Array(a) => a
            .iter()
            .filter_map(|o| doc.resolve(o).as_name().map(<[u8]>::to_vec))
            .collect(),
        _ => Vec::new(),
    }
}

/// 8-bit samples of a Gray, RGB or CMYK image (without a palette or special space).
fn pixels(doc: &Document, s: &Stream) -> Option<Pixels> {
    let d = &s.dict;
    let w = usize::try_from(doc.get_in(d, b"Width").as_int()?).ok()?;
    let h = usize::try_from(doc.get_in(d, b"Height").as_int()?).ok()?;
    if w == 0 || h == 0 || w * h > 1 << 28 || d.contains(b"SMaskInData") {
        return None;
    }
    let cs = doc.get_in(d, b"ColorSpace");
    let simple = match &*cs {
        Object::Name(_) => true,
        Object::Array(a) => matches!(
            a.first().and_then(Object::as_name),
            Some(b"ICCBased" | b"CalRGB" | b"CalGray")
        ),
        _ => false,
    };
    let n = components(doc, &cs)? as usize;
    if !simple || !matches!(n, 1 | 3 | 4) || doc.get_in(d, b"BitsPerComponent").as_int() != Some(8)
    {
        return None;
    }
    let dec = doc.decode(s).ok()?;
    if dec.damaged {
        return None;
    }
    match dec.image.as_ref().map(|f| f.name.as_bytes()) {
        None => (dec.data.len() >= w * h * n).then(|| Pixels {
            w,
            h,
            n,
            data: dec.data[..w * h * n].to_vec(),
            jpeg: false,
        }),
        Some(b"DCTDecode") if n != 4 => {
            let opts = pagebara_jpeg::DecodeOptions::default();
            let mut p = pagebara_jpeg::decode_components(&dec.data, &opts).ok()?;
            if p.truncated || p.width as usize != w || p.height as usize != h {
                return None;
            }
            let mut rgba = vec![0u8; w * h * 4];
            if n == 1 {
                p.layout = pagebara_jpeg::PlanesLayout::Gray;
            }
            pagebara_jpeg::planes_to_rgba_into(
                &p,
                pagebara_jpeg::Upsampling::Bilinear,
                &mut rgba,
                w * 4,
            );
            let px = rgba.as_chunks::<4>().0;
            let data = if n == 1 {
                px.iter().map(|c| c[0]).collect()
            } else {
                px.iter().flat_map(|c| [c[0], c[1], c[2]]).collect()
            };
            Some(Pixels {
                w,
                h,
                n,
                data,
                jpeg: true,
            })
        }
        _ => None,
    }
}

fn is_photo(p: &Pixels) -> bool {
    if p.jpeg {
        return true;
    }
    let step = (p.w * p.h / 65536).max(1);
    let mut seen = HashSet::new();
    for px in p.data.chunks_exact(p.n).step_by(step) {
        let mut k = 0u32;
        for &v in px.iter().take(4) {
            k = k << 8 | u32::from(v);
        }
        seen.insert(k);
        if seen.len() > PHOTO_COLORS {
            return true;
        }
    }
    false
}

fn to_gray(p: &mut Pixels) {
    let gray: Vec<u8> = match p.n {
        3 => p
            .data
            .as_chunks::<3>()
            .0
            .iter()
            .map(|c| {
                ((299 * u32::from(c[0]) + 587 * u32::from(c[1]) + 114 * u32::from(c[2]) + 500)
                    / 1000) as u8
            })
            .collect(),
        4 => p
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| {
                let ink = (30 * u32::from(c[0]) + 59 * u32::from(c[1]) + 11 * u32::from(c[2]))
                    / 100
                    + u32::from(c[3]);
                255 - ink.min(255) as u8
            })
            .collect(),
        _ => return,
    };
    p.data = gray;
    p.n = 1;
}

/// Codes `p` for `s`'s dictionary, JPEG for photos and Flate otherwise.
fn encode(
    s: &Stream,
    p: &Pixels,
    photo: bool,
    quality: u8,
    effort: Effort,
    gray: bool,
) -> Option<Stream> {
    let mut base = s.dict.clone();
    base.set(b"Width", p.w as i64);
    base.set(b"Height", p.h as i64);
    base.set(b"BitsPerComponent", 8);
    if gray {
        base.set(b"ColorSpace", Object::name(b"DeviceGray"));
        base.remove(b"Decode");
    }
    let plain = Stream::new(base, Vec::<u8>::new());
    if photo && p.n != 4 && !s.dict.contains(b"Decode") {
        let samples = if p.n == 1 {
            pagebara_jpeg::encode::Samples::Gray(&p.data)
        } else {
            pagebara_jpeg::encode::Samples::Rgb(&p.data)
        };
        let opts = pagebara_jpeg::encode::EncodeOptions {
            quality,
            ..Default::default()
        };
        let data = pagebara_jpeg::encode::encode(p.w as u32, p.h as u32, samples, &opts).ok()?;
        let mut d = plain.dict.clone();
        for k in [&b"Filter"[..], b"DecodeParms", b"Length", b"DL"] {
            d.remove(k);
        }
        d.set(b"Filter", Object::name(b"DCTDecode"));
        return Some(Stream::new(d, data));
    }
    let pred = png_predict(&p.data, p.n as u8, 8, p.w as u32)?;
    let mut parms = Dict::new();
    parms.set(b"Predictor", 15);
    parms.set(b"Colors", p.n as i64);
    parms.set(b"BitsPerComponent", 8);
    parms.set(b"Columns", p.w as i64);
    Some(with_flate(&plain, deflate(&pred, effort), Some(parms)))
}

/// Bilevel samples as Group 4, when that is smaller.
fn g4(doc: &Document, s: &Stream) -> Option<Stream> {
    let d = &s.dict;
    let mask = doc.get_in(d, b"ImageMask").as_bool() == Some(true);
    let bpc = doc.get_in(d, b"BitsPerComponent").as_int();
    let one = mask || (bpc == Some(1) && components(doc, &doc.get_in(d, b"ColorSpace")) == Some(1));
    let f = filters(doc, d);
    if !one
        || f.iter().any(|n| {
            matches!(
                n.as_slice(),
                b"CCITTFaxDecode" | b"CCF" | b"JBIG2Decode" | b"DCTDecode" | b"JPXDecode"
            )
        })
    {
        return None;
    }
    let w = u32::try_from(doc.get_in(d, b"Width").as_int()?).ok()?;
    let h = u32::try_from(doc.get_in(d, b"Height").as_int()?).ok()?;
    let dec = doc.decode(s).ok()?;
    if dec.damaged || dec.image.is_some() || dec.data.len() < (w as usize).div_ceil(8) * h as usize
    {
        return None;
    }
    // G4 codes long white runs best: the more common value goes in as white. BlackIs1
    // false makes the decoder invert again, so the samples come back as they were.
    let rows = &dec.data[..(w as usize).div_ceil(8) * h as usize];
    let ones: usize = rows.iter().map(|b| b.count_ones() as usize).sum();
    let black_is_1 = ones * 2 <= rows.len() * 8;
    let data = if black_is_1 {
        crate::g4::encode_g4(rows, w, h)
    } else {
        let inv: Vec<u8> = rows.iter().map(|b| !b).collect();
        crate::g4::encode_g4(&inv, w, h)
    };
    if data.len() >= doc.raw(s).len() {
        return None;
    }
    let mut nd = d.clone();
    for k in [&b"Filter"[..], b"DecodeParms", b"Length", b"DL"] {
        nd.remove(k);
    }
    let mut parms = Dict::new();
    parms.set(b"K", -1);
    parms.set(b"Columns", i64::from(w));
    parms.set(b"Rows", i64::from(h));
    if black_is_1 {
        parms.set(b"BlackIs1", true);
    }
    nd.set(b"Filter", Object::name(b"CCITTFaxDecode"));
    nd.set(b"DecodeParms", parms);
    Some(Stream::new(nd, data))
}

fn process_one(
    doc: &Document,
    num: u32,
    s: &Stream,
    o: &Images,
    gray: bool,
    ppi: Option<f32>,
    masks: &HashSet<u32>,
) -> Option<Done> {
    if o.bitonal_g4
        && let Some(st) = g4(doc, s)
    {
        return Some(Done::Reencoded(st));
    }
    let target = o
        .target_ppi
        .filter(|t| ppi.is_some_and(|p| p.is_finite() && p > t * THRESHOLD));
    let is_mask = masks.contains(&num);
    let gray = gray && !is_mask;
    let rejpeg = o.recompress_jpeg && !is_mask;
    if target.is_none() && !rejpeg && !gray {
        return None;
    }
    let mut p = pixels(doc, s)?;
    if rejpeg && target.is_none() && !gray && !p.jpeg {
        return None;
    }
    let to_gray_now = gray && p.n > 1 && !s.dict.contains(b"Decode");
    if to_gray_now {
        to_gray(&mut p);
    }
    let mut resampled = false;
    if let (Some(t), Some(ppi)) = (target, ppi) {
        let k = t / ppi;
        let (dw, dh) = (
            ((p.w as f32 * k).round() as usize).max(1),
            ((p.h as f32 * k).round() as usize).max(1),
        );
        if dw < p.w && dh < p.h {
            p.data = crate::resample::resample(&p.data, p.w, p.h, p.n, dw, dh, o.filter);
            p.w = dw;
            p.h = dh;
            resampled = true;
        }
    }
    if !resampled && !to_gray_now && !(rejpeg && p.jpeg) {
        return None;
    }
    let photo = !is_mask && is_photo(&p);
    let st = encode(s, &p, photo, o.jpeg_quality, Effort::Best, to_gray_now)?;
    let smaller = doc.raw(&st).len() < doc.raw(s).len();
    if !smaller && !to_gray_now && !resampled {
        return None;
    }
    if !smaller && resampled && !to_gray_now {
        return None;
    }
    Some(if resampled {
        Done::Resampled(st)
    } else {
        Done::Reencoded(st)
    })
}

pub(crate) fn process(
    doc: &mut Document,
    o: &Images,
    gray: bool,
    usage: Option<&Usage>,
    report: &mut Report,
) {
    let mut jobs: Vec<(ObjRef, Arc<Object>)> = Vec::new();
    let mut masks = HashSet::new();
    for num in doc.object_numbers() {
        let Some(r) = doc.reference(num) else {
            continue;
        };
        let Some(obj) = doc.get(r) else { continue };
        let Some(s) = obj.as_stream() else { continue };
        if s.dict.name(b"Subtype") != Some(b"Image") {
            continue;
        }
        for k in [&b"SMask"[..], b"Mask"] {
            if let Some(m) = s.dict.get(k).and_then(Object::as_objref) {
                masks.insert(m.num);
            }
        }
        jobs.push((r, obj));
    }
    let d: &Document = doc;
    let done: Vec<(ObjRef, Done)> = jobs
        .par_iter()
        .filter_map(|(r, obj)| {
            let s = obj.as_stream()?;
            let ppi = usage.and_then(|u| u.images.get(&r.num).copied());
            Some((*r, process_one(d, r.num, s, o, gray, ppi, &masks)?))
        })
        .collect();
    for (r, done) in done {
        match done {
            Done::Resampled(s) => {
                report.images_resampled += 1;
                doc.set(r, s);
            }
            Done::Reencoded(s) => {
                report.images_reencoded += 1;
                doc.set(r, s);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn group4_beats_flate_on_text_like_scans() {
        let mut bits = vec![0xffu8; 25 * 200];
        let mut s = 12345u32;
        for _ in 0..60 {
            s = s.wrapping_mul(1103515245).wrapping_add(12345);
            let (x0, y0, w, h) = (
                (s >> 8) % 180,
                (s >> 16) % 185,
                2 + (s >> 4) % 18,
                6 + (s >> 24) % 9,
            );
            for y in y0..y0 + h {
                for x in x0..x0 + w {
                    bits[y as usize * 25 + x as usize / 8] &= !(0x80 >> (x % 8));
                }
            }
        }
        let inv: Vec<u8> = bits.iter().map(|b| !b).collect();
        let g = crate::g4::encode_g4(&inv, 200, 200);
        let f1 = pagebara_filters::encode_flate(&bits, 10);
        assert!(g.len() < f1.len(), "g4 {} flate {}", g.len(), f1.len());
    }
}
