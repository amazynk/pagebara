//! `/Filter` and `/DecodeParms` of a stream mapped onto [`pagebara_filters`].

use pagebara_filters::{CcittParams, Filter, Limits, Predictor};

use crate::object::{Dict, Name, Object, Stream, StreamData};
use crate::{Error, Result};

/// An image codec ending a filter chain, decoded by the image layer.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageFilter {
    /// Canonical name: `DCTDecode`, `JPXDecode` or `JBIG2Decode`.
    pub name: Name,
    pub parms: Dict,
}

#[derive(Debug, Default, PartialEq)]
pub struct DecodedStream {
    pub data: Vec<u8>,
    /// Set when `data` is still encoded with an image codec.
    pub image: Option<ImageFilter>,
    pub damaged: bool,
}

/// The byte filters of a chain and the image codec ending it. `resolve` follows references.
pub fn chain(
    dict: &Dict,
    resolve: &dyn Fn(&Object) -> Object,
) -> Result<(Vec<Filter>, Option<ImageFilter>)> {
    let names = match dict.get(b"Filter").map(resolve) {
        None | Some(Object::Null) => return Ok((Vec::new(), None)),
        Some(Object::Name(n)) => vec![n],
        Some(Object::Array(a)) => a
            .iter()
            .filter_map(|o| match resolve(o) {
                Object::Name(n) => Some(n),
                _ => None,
            })
            .collect(),
        Some(_) => return Err(Error::Filter("/Filter is not a name or array")),
    };
    let parms: Vec<Option<Dict>> = match dict.get(b"DecodeParms").map(resolve) {
        Some(Object::Dict(d)) => vec![Some(d)],
        Some(Object::Array(a)) => a
            .iter()
            .map(|o| match resolve(o) {
                Object::Dict(d) => Some(d),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let mut out = Vec::with_capacity(names.len());
    for (i, n) in names.iter().enumerate() {
        let p = parms.get(i).cloned().flatten().unwrap_or_default();
        let int = |k: &[u8], d: i64| p.get(k).map(resolve).and_then(|o| o.as_int()).unwrap_or(d);
        let flag = |k: &[u8]| {
            p.get(k)
                .map(resolve)
                .and_then(|o| o.as_bool())
                .unwrap_or(false)
        };
        let predictor = || {
            Predictor::new(
                int(b"Predictor", 1),
                int(b"Colors", 1).clamp(1, 255) as u8,
                int(b"BitsPerComponent", 8).clamp(1, 255) as u8,
                int(b"Columns", 1).clamp(1, i64::from(u32::MAX)) as u32,
            )
        };
        let f = match n.as_bytes() {
            b"ASCIIHexDecode" | b"AHx" => Filter::AsciiHex,
            b"ASCII85Decode" | b"A85" => Filter::Ascii85,
            b"LZWDecode" | b"LZW" => Filter::Lzw {
                early_change: int(b"EarlyChange", 1) != 0,
                predictor: predictor(),
            },
            b"FlateDecode" | b"Fl" => Filter::Flate {
                predictor: predictor(),
            },
            b"RunLengthDecode" | b"RL" => Filter::RunLength,
            b"CCITTFaxDecode" | b"CCF" => Filter::Ccitt(CcittParams {
                k: int(b"K", 0).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
                columns: int(b"Columns", 1728).clamp(1, 1 << 20) as u32,
                rows: int(b"Rows", 0).clamp(0, 1 << 24) as u32,
                encoded_byte_align: flag(b"EncodedByteAlign"),
                black_is_1: flag(b"BlackIs1"),
            }),
            img @ (b"DCTDecode" | b"DCT" | b"JPXDecode" | b"JBIG2Decode") => {
                let name: &[u8] = match img {
                    b"DCT" => b"DCTDecode",
                    x => x,
                };
                let image = ImageFilter {
                    name: Name::new(name),
                    parms: p,
                };
                return Ok((out, Some(image)));
            }
            // Identity crypt filters only; real ones are applied by decryption.
            b"Crypt" if p.name(b"Name").is_none_or(|n| n == b"Identity") => continue,
            _ => return Err(Error::UnsupportedFilter(n.clone())),
        };
        out.push(f);
    }
    Ok((out, None))
}

pub fn decode(
    raw: &[u8],
    dict: &Dict,
    resolve: &dyn Fn(&Object) -> Object,
) -> Result<DecodedStream> {
    let (filters, image) = chain(dict, resolve)?;
    let d = pagebara_filters::decode(raw, &filters, Limits::default())?;
    Ok(DecodedStream {
        data: d.data,
        image,
        damaged: d.damaged,
    })
}

/// Decodes a stream whose filter parameters are all direct (xref and object streams of the
/// file structure itself).
pub fn decode_direct(src: &[u8], s: &Stream) -> Result<Vec<u8>> {
    let raw = match &s.data {
        StreamData::Source(r) => &src[r.clone()],
        StreamData::Owned(b) => b,
    };
    Ok(decode(raw, &s.dict, &Object::clone)?.data)
}
