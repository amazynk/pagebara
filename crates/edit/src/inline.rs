//! Inline images (§8.9.7) as image XObject dictionaries.

use pagebara_content::Instr;
use pagebara_syntax::{Dict, Document, Name, Object, Stream};

pub fn expand_key(k: &[u8]) -> &[u8] {
    match k {
        b"BPC" => b"BitsPerComponent",
        b"CS" => b"ColorSpace",
        b"D" => b"Decode",
        b"DP" => b"DecodeParms",
        b"F" => b"Filter",
        b"H" => b"Height",
        b"IM" => b"ImageMask",
        b"I" => b"Interpolate",
        b"W" => b"Width",
        b"L" => b"Length",
        _ => k,
    }
}

fn expand_space(n: &[u8]) -> Option<&'static [u8]> {
    Some(match n {
        b"G" | b"DeviceGray" => b"DeviceGray",
        b"RGB" | b"DeviceRGB" => b"DeviceRGB",
        b"CMYK" | b"DeviceCMYK" => b"DeviceCMYK",
        b"I" | b"Indexed" => b"Indexed",
        _ => return None,
    })
}

fn expand_filter(n: &[u8]) -> Option<&'static [u8]> {
    Some(match n {
        b"AHx" => b"ASCIIHexDecode",
        b"A85" => b"ASCII85Decode",
        b"LZW" => b"LZWDecode",
        b"Fl" => b"FlateDecode",
        b"RL" => b"RunLengthDecode",
        b"CCF" => b"CCITTFaxDecode",
        b"DCT" => b"DCTDecode",
        _ => return None,
    })
}

fn expand_names(v: &mut Object, f: fn(&[u8]) -> Option<&'static [u8]>) {
    match v {
        Object::Name(n) => {
            if let Some(full) = f(n.as_bytes()) {
                *n = Name::new(full);
            }
        }
        Object::Array(a) => a.iter_mut().for_each(|e| expand_names(e, f)),
        _ => {}
    }
}

/// The colour space of an inline image in full names, a resource it names resolved.
fn space(doc: &Document, res: &Dict, mut v: Object) -> Object {
    let named = |n: &[u8]| {
        doc.get_in(res, b"ColorSpace")
            .as_dict()
            .and_then(|d| d.get(n).cloned())
    };
    if let Object::Name(n) = &v {
        if let Some(full) = expand_space(n.as_bytes()) {
            return Object::name(full);
        }
        return named(n.as_bytes()).unwrap_or(v);
    }
    if let Object::Array(a) = &mut v {
        for (k, e) in a.iter_mut().enumerate() {
            if let Object::Name(n) = e {
                if let Some(full) = expand_space(n.as_bytes()) {
                    *e = Object::name(full);
                } else if k == 1
                    && let Some(raw) = named(n.as_bytes())
                {
                    *e = raw;
                }
            }
        }
    }
    v
}

/// The inline image of a `BI` instruction as an image XObject (its data as written).
pub fn to_xobject(doc: &Document, res: &Dict, i: &Instr<'_, '_>) -> Stream {
    let mut d = Dict::new();
    d.set(b"Type", Object::name(b"XObject"));
    d.set(b"Subtype", Object::name(b"Image"));
    for kv in i.args.as_chunks::<2>().0 {
        let Some(k) = kv[0].name() else { continue };
        let key = expand_key(&k);
        if key == b"Length" {
            continue;
        }
        let mut v = i.object(&kv[1]);
        match key {
            b"ColorSpace" => v = space(doc, res, v),
            b"Filter" => expand_names(&mut v, expand_filter),
            _ => {}
        }
        d.set(key, v);
    }
    Stream::new(d, i.data)
}
