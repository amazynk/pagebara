//! Lossless recompression: streams coded with general-purpose filters are decoded and
//! Flate-coded again at the chosen effort, images with PNG predictors when that is smaller.

use std::sync::atomic::{AtomicBool, Ordering};

use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};
use rayon::prelude::*;

use crate::Effort;

/// Streams below this are left as they are.
const MIN_LEN: usize = 64;
/// Zopfli is only worth its time on streams up to this size.
const ZOPFLI_MAX: usize = 4 << 20;

pub(crate) fn deflate(data: &[u8], effort: Effort) -> Vec<u8> {
    match effort {
        Effort::Fast => pagebara_filters::encode_flate(data, 6),
        Effort::Best => pagebara_filters::encode_flate(data, 10),
        Effort::Maximum if data.len() <= ZOPFLI_MAX => {
            let iterations = if data.len() < 1 << 20 { 15 } else { 5 };
            let opts = zopfli::Options {
                iteration_count: std::num::NonZeroU64::new(iterations)
                    .unwrap_or(std::num::NonZeroU64::MIN),
                ..zopfli::Options::default()
            };
            let mut out = Vec::new();
            match zopfli::compress(opts, zopfli::Format::Zlib, data, &mut out) {
                Ok(()) => out,
                Err(_) => pagebara_filters::encode_flate(data, 10),
            }
        }
        Effort::Maximum => pagebara_filters::encode_flate(data, 10),
    }
}

/// Whether every filter of the stream is a general-purpose one we can write back.
fn general_filters(doc: &Document, d: &Dict) -> bool {
    let names: Vec<Vec<u8>> = match &*doc.get_in(d, b"Filter") {
        Object::Null => return true,
        Object::Name(n) => vec![n.as_bytes().to_vec()],
        Object::Array(a) => a
            .iter()
            .filter_map(|o| doc.resolve(o).as_name().map(<[u8]>::to_vec))
            .collect(),
        _ => return false,
    };
    names.iter().all(|n| {
        matches!(
            n.as_slice(),
            b"FlateDecode"
                | b"Fl"
                | b"LZWDecode"
                | b"LZW"
                | b"ASCIIHexDecode"
                | b"AHx"
                | b"ASCII85Decode"
                | b"A85"
                | b"RunLengthDecode"
                | b"RL"
        )
    })
}

/// Samples per pixel and bits per sample of an image, for PNG prediction.
fn image_layout(doc: &Document, d: &Dict) -> Option<(u8, u8, u32)> {
    if d.name(b"Subtype") != Some(b"Image") {
        return None;
    }
    let w = doc.get_in(d, b"Width").as_int()?;
    let bpc = if doc.get_in(d, b"ImageMask").as_bool() == Some(true) {
        1
    } else {
        doc.get_in(d, b"BitsPerComponent").as_int()?
    };
    let colors = if doc.get_in(d, b"ImageMask").as_bool() == Some(true) {
        1
    } else {
        components(doc, &doc.get_in(d, b"ColorSpace"))?
    };
    if !(1..=16).contains(&bpc) || w <= 0 || w > 1 << 20 {
        return None;
    }
    Some((colors, bpc as u8, w as u32))
}

/// Components of a colour space, when known.
pub(crate) fn components(doc: &Document, cs: &Object) -> Option<u8> {
    match cs {
        Object::Name(n) => match n.as_bytes() {
            b"DeviceGray" | b"CalGray" | b"G" => Some(1),
            b"DeviceRGB" | b"CalRGB" | b"Lab" | b"RGB" => Some(3),
            b"DeviceCMYK" | b"CMYK" => Some(4),
            _ => None,
        },
        Object::Array(a) => {
            let fam = a.first().and_then(Object::as_name)?;
            match fam {
                b"ICCBased" => {
                    let s = doc.resolve(a.get(1)?);
                    doc.get_in(&s.as_stream()?.dict, b"N")
                        .as_int()
                        .and_then(|n| u8::try_from(n).ok())
                }
                b"Indexed" | b"I" | b"Separation" => Some(1),
                b"DeviceN" => a
                    .get(1)
                    .map(|n| doc.resolve(n))
                    .and_then(|n| n.as_array().map(|n| n.len() as u8)),
                b"CalRGB" | b"Lab" => Some(3),
                b"CalGray" => Some(1),
                _ => None,
            }
        }
        Object::Ref(_) => components(doc, &doc.resolve(cs)),
        _ => None,
    }
}

/// PNG-filtered rows (adaptive filter choice per row, as predictor 15 expects).
pub(crate) fn png_predict(data: &[u8], colors: u8, bpc: u8, w: u32) -> Option<Vec<u8>> {
    let row_bytes = (w as usize * colors as usize * bpc as usize).div_ceil(8);
    if row_bytes == 0 || !data.len().is_multiple_of(row_bytes) {
        return None;
    }
    let bpp = (colors as usize * bpc as usize).div_ceil(8).max(1);
    let rows = data.len() / row_bytes;
    let mut out = Vec::with_capacity(data.len() + rows);
    let zero = vec![0u8; row_bytes];
    let mut cand = vec![vec![0u8; row_bytes]; 5];
    for y in 0..rows {
        let cur = &data[y * row_bytes..(y + 1) * row_bytes];
        let up = if y == 0 {
            &zero[..]
        } else {
            &data[(y - 1) * row_bytes..y * row_bytes]
        };
        for i in 0..row_bytes {
            let a = if i >= bpp { cur[i - bpp] } else { 0 };
            let b = up[i];
            let c = if i >= bpp { up[i - bpp] } else { 0 };
            cand[0][i] = cur[i];
            cand[1][i] = cur[i].wrapping_sub(a);
            cand[2][i] = cur[i].wrapping_sub(b);
            cand[3][i] = cur[i].wrapping_sub(((u16::from(a) + u16::from(b)) / 2) as u8);
            cand[4][i] = cur[i].wrapping_sub(paeth(a, b, c));
        }
        let cost =
            |r: &[u8]| -> u64 { r.iter().map(|&v| u64::from((v as i8).unsigned_abs())).sum() };
        let best = (0..5).min_by_key(|&k| cost(&cand[k])).unwrap_or(0);
        out.push(best as u8);
        out.extend_from_slice(&cand[best]);
    }
    Some(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let (pa, pb, pc) = (
        (p - i16::from(a)).abs(),
        (p - i16::from(b)).abs(),
        (p - i16::from(c)).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// `s` with its data replaced by `data` coded with Flate (and `parms`).
pub(crate) fn with_flate(s: &Stream, data: Vec<u8>, parms: Option<Dict>) -> Stream {
    let mut d = s.dict.clone();
    for k in [&b"Filter"[..], b"DecodeParms", b"Length", b"DL"] {
        d.remove(k);
    }
    d.set(b"Filter", Object::name(b"FlateDecode"));
    if let Some(p) = parms {
        d.set(b"DecodeParms", p);
    }
    Stream::new(d, data)
}

/// The stream recoded smaller, if it can be.
fn recompress(doc: &Document, s: &Stream, effort: Effort) -> Option<Stream> {
    let raw_len = doc.raw(s).len();
    if raw_len < MIN_LEN
        || s.dict.contains(b"F")
        || matches!(
            s.dict.name(b"Type"),
            Some(b"XRef" | b"ObjStm" | b"Metadata")
        )
        || !general_filters(doc, &s.dict)
    {
        return None;
    }
    let dec = doc.decode(s).ok()?;
    if dec.damaged || dec.image.is_some() {
        return None;
    }
    let mut best: Option<(Vec<u8>, Option<Dict>)> = None;
    let plain = deflate(&dec.data, effort);
    if plain.len() < raw_len {
        best = Some((plain, None));
    }
    if let Some((colors, bpc, w)) = image_layout(doc, &s.dict)
        && bpc >= 8
        && let Some(pred) = png_predict(&dec.data, colors, bpc, w)
    {
        let p = deflate(&pred, effort);
        if p.len() < best.as_ref().map_or(raw_len, |b| b.0.len()) {
            let mut parms = Dict::new();
            parms.set(b"Predictor", 15);
            parms.set(b"Colors", i64::from(colors));
            parms.set(b"BitsPerComponent", i64::from(bpc));
            parms.set(b"Columns", i64::from(w));
            best = Some((p, Some(parms)));
        }
    }
    // A few bytes are not worth a rewrite of the object.
    let (data, parms) = best.filter(|b| b.0.len() + 16 < raw_len)?;
    Some(with_flate(s, data, parms))
}

/// Recompresses every stream; returns how many got smaller.
pub(crate) fn recompress_all(
    doc: &mut Document,
    effort: Effort,
    cancel: Option<&AtomicBool>,
) -> usize {
    let streams: Vec<(ObjRef, std::sync::Arc<Object>)> = doc
        .object_numbers()
        .into_iter()
        .filter_map(|n| {
            let r = doc.reference(n)?;
            let o = doc.get(r)?;
            o.as_stream().is_some().then_some((r, o))
        })
        .collect();
    let d: &Document = doc;
    let done: Vec<(ObjRef, Stream)> = streams
        .par_iter()
        .filter_map(|(r, o)| {
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return None;
            }
            Some((*r, recompress(d, o.as_stream()?, effort)?))
        })
        .collect();
    let n = done.len();
    for (r, s) in done {
        doc.set(r, s);
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_prediction_round_trips_through_the_decoder() {
        let w = 37u32;
        let data: Vec<u8> = (0..w * 3 * 11).map(|i| (i * 7 % 251) as u8).collect();
        let pred = png_predict(&data, 3, 8, w).unwrap();
        let p = pagebara_filters::Predictor::new(15, 3, 8, w);
        let enc = pagebara_filters::encode_flate(&pred, 6);
        let dec = pagebara_filters::decode(
            &enc,
            &[pagebara_filters::Filter::Flate { predictor: p }],
            pagebara_filters::Limits::default(),
        )
        .unwrap();
        assert_eq!(dec.data, data);
    }
}
