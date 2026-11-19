//! Removing redacted words from what the document says besides its pages: bookmark titles,
//! named destinations, document information, XMP metadata, annotation and field texts,
//! link targets and every other text string.

use pagebara_syntax::{Dict, Document, Name, Object, PdfString, Stream};

/// What replaces a redacted word outside the pages.
pub const REPLACEMENT: &str = "[redacted]";

/// String entries that hold bytes rather than text.
const BINARY: &[&[u8]] = &[
    b"ID",
    b"O",
    b"U",
    b"OE",
    b"UE",
    b"Perms",
    b"Cert",
    b"ByteRange",
];

/// Replaces `secrets` (ignoring case) with [`REPLACEMENT`] in every text string, in
/// destination names and in XMP metadata. Name trees whose keys change are rebuilt in
/// order. Returns how many strings and streams changed.
pub fn scrub(doc: &mut Document, secrets: &[&str]) -> usize {
    let secrets: Vec<String> = secrets
        .iter()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if secrets.is_empty() {
        return 0;
    }
    let mut n = 0;
    for num in doc.object_numbers() {
        let Some(r) = doc.reference(num) else {
            continue;
        };
        let Some(o) = doc.get(r) else { continue };
        let mut o = (*o).clone();
        let mut changed = 0;
        match &mut o {
            Object::Stream(s) => {
                if s.dict.name(b"Type") == Some(b"Metadata")
                    && let Ok(d) = doc.decode(s)
                    && let Some(text) = replace(&String::from_utf8_lossy(&d.data), &secrets)
                {
                    let mut dict = Dict::new();
                    for (k, v) in s.dict.iter() {
                        if !matches!(k.as_bytes(), b"Filter" | b"DecodeParms" | b"Length") {
                            dict.set(k.as_bytes(), v.clone());
                        }
                    }
                    *s = Stream::new(dict, text.into_bytes());
                    changed += 1;
                }
                if !is_signature(&s.dict) {
                    changed += dict(&mut s.dict, &secrets);
                }
            }
            Object::Dict(d) if is_signature(d) => {}
            o => changed += value(o, &secrets, b""),
        }
        if changed > 0 {
            n += changed;
            doc.set(r, o);
        }
    }
    if n > 0 {
        sort_name_trees(doc);
    }
    n
}

fn is_signature(d: &Dict) -> bool {
    d.contains(b"ByteRange") && d.contains(b"Contents")
}

fn dict(d: &mut Dict, secrets: &[String]) -> usize {
    let mut n = 0;
    for (k, v) in d.0.iter_mut() {
        if BINARY.contains(&k.as_bytes()) {
            continue;
        }
        n += value(v, secrets, k.as_bytes());
    }
    n
}

fn value(o: &mut Object, secrets: &[String], key: &[u8]) -> usize {
    match o {
        Object::String(s) => match replace_string(s, secrets) {
            Some(new) => {
                *s = new;
                1
            }
            None => 0,
        },
        // Destination names (`/D` of go-to actions, `/Dest`) as names.
        Object::Name(nm) if matches!(key, b"D" | b"Dest") => {
            let text = String::from_utf8_lossy(nm.as_bytes()).into_owned();
            match replace(&text, secrets) {
                Some(t) => {
                    *nm = Name::new(t.as_bytes());
                    1
                }
                None => 0,
            }
        }
        Object::Array(a) => a.iter_mut().map(|e| value(e, secrets, key)).sum(),
        Object::Dict(d) => {
            let mut n = dict(d, secrets);
            // Keys of a `/Dests` dictionary are destination names.
            if key == b"Dests" {
                for (k, _) in d.0.iter_mut() {
                    let text = String::from_utf8_lossy(k.as_bytes()).into_owned();
                    if let Some(t) = replace(&text, secrets) {
                        *k = Name::new(t.as_bytes());
                        n += 1;
                    }
                }
            }
            n
        }
        _ => 0,
    }
}

fn replace_string(s: &PdfString, secrets: &[String]) -> Option<PdfString> {
    let o = Object::String(s.clone());
    let text = pagebara_content::extract::text_string(&o);
    let new = replace(&text, secrets)?;
    let bytes = match pagebara_syntax::text::pdfdoc_encode(&new) {
        Some(b) if !s.bytes.starts_with(&[0xFE, 0xFF]) => b,
        _ => {
            let mut b = vec![0xFE, 0xFF];
            b.extend(new.encode_utf16().flat_map(u16::to_be_bytes));
            b
        }
    };
    Some(PdfString { bytes, hex: s.hex })
}

/// `text` with each secret replaced, ignoring case; `None` if none occurs.
fn replace(text: &str, secrets: &[String]) -> Option<String> {
    let mut out = text.to_string();
    let mut changed = false;
    for s in secrets {
        while let Some((a, b)) = find_ci(&out, s) {
            out.replace_range(a..b, REPLACEMENT);
            changed = true;
        }
    }
    changed.then_some(out)
}

/// The byte range of `text` whose lowercase form is `needle` (lowercase).
fn find_ci(text: &str, needle: &str) -> Option<(usize, usize)> {
    let mut low = String::with_capacity(text.len());
    // For each byte of `low`, where its character starts in `text`.
    let mut at = Vec::with_capacity(text.len());
    for (i, c) in text.char_indices() {
        let start = low.len();
        low.extend(c.to_lowercase());
        at.resize(at.len() + low.len() - start, i);
    }
    let p = low.find(needle)?;
    let end = p + needle.len();
    Some((at[p], at.get(end).copied().unwrap_or(text.len())))
}

/// Name trees (the catalog's `/Names` entries) as one sorted leaf, since renamed keys may
/// have left their order.
fn sort_name_trees(doc: &mut Document) {
    let Some(names) = doc
        .catalog()
        .and_then(|c| c.as_dict()?.get(b"Names").cloned())
    else {
        return;
    };
    let Some(nd) = doc.resolve(&names).as_dict().cloned() else {
        return;
    };
    for (_, root) in nd.iter() {
        let Some(r) = root.as_objref() else { continue };
        let mut pairs = Vec::new();
        collect(doc, root, &mut pairs, 0);
        let sorted = pairs.windows(2).all(|w| w[0].0 <= w[1].0);
        let leaf = doc
            .get(r)
            .is_some_and(|o| o.as_dict().is_some_and(|d| !d.contains(b"Kids")));
        if sorted && leaf {
            continue;
        }
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        let mut d = Dict::new();
        let mut arr = Vec::new();
        for (k, v) in pairs {
            arr.push(Object::String(PdfString {
                bytes: k,
                hex: false,
            }));
            arr.push(v);
        }
        d.set(b"Names", Object::Array(arr));
        doc.set(r, d);
    }
}

fn collect(doc: &Document, node: &Object, out: &mut Vec<(Vec<u8>, Object)>, depth: u32) {
    if depth > 32 {
        return;
    }
    let n = doc.resolve(node);
    let Some(d) = n.as_dict() else { return };
    if let Some(a) = doc.get_in(d, b"Names").as_array() {
        for pair in a.as_chunks::<2>().0 {
            if let Some(k) = doc.resolve(&pair[0]).as_string() {
                out.push((k.to_vec(), pair[1].clone()));
            }
        }
    }
    for k in doc.get_in(d, b"Kids").as_array().unwrap_or_default() {
        collect(doc, k, out, depth + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_ignoring_case() {
        let s = vec!["jane doe".to_string()];
        assert_eq!(
            replace("Report on JANE DOE and Jane Doe", &s).as_deref(),
            Some("Report on [redacted] and [redacted]")
        );
        assert_eq!(replace("nothing", &s), None);
        let s = vec!["straße".to_string()];
        assert_eq!(replace("İ STRAßE x", &s).as_deref(), Some("İ [redacted] x"));
    }
}
