//! Checking a saved redacted file, independently of how it was made.
//!
//! The file is read back and, with every optional content group shown:
//! - the text of each page is extracted and no glyph (other than the overlay's own text)
//!   may have [`GLYPH_OVERLAP`] or more of its box in a region;
//! - every image drawn over a region must have uniform pixels there (in its masks too);
//! - no annotation may overlap a region;
//! - the secrets must not occur anywhere: not in the file bytes, not in any object's
//!   strings or decoded streams, not in the strings of content streams (joined, so kerned
//!   text is found), not in streams found by scanning the file outside its cross-reference
//!   index, and not in any page's extracted text;
//! - the file must hold a single revision.

use std::collections::HashSet;

use pagebara_content::display::{Cmd, ImageSource, NONE};
use pagebara_content::oc::OptionalContent;
use pagebara_content::{ContentParser, DisplayList, Interpreter, Op, extract_text};
use pagebara_syntax::{Document, Object, Stream};

use crate::apply::GLYPH_OVERLAP;
use crate::geom::{Matrix, Point, Rect, Region};
use crate::image;
use crate::overlay::is_overlay;

/// Where a secret was found.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Location {
    /// In the file's bytes as stored.
    File { offset: usize },
    /// In a string of an object.
    Object { num: u32 },
    /// In an object's decoded stream data.
    Stream { num: u32 },
    /// In the strings a content stream shows.
    ContentText { num: u32 },
    /// In a stream found by scanning the file, outside its index.
    LooseStream { offset: usize },
    /// In the extracted text of a page.
    PageText { page: usize },
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextLeak {
    pub page: usize,
    pub text: String,
    pub bbox: Rect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageLeak {
    pub page: usize,
    /// The image object, `None` for inline images.
    pub object: Option<u32>,
    pub reason: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotationLeak {
    pub page: usize,
    pub object: u32,
    pub subtype: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SecretHit {
    pub secret: String,
    pub location: Location,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct VerifyReport {
    pub text_in_regions: Vec<TextLeak>,
    pub images_in_regions: Vec<ImageLeak>,
    pub annotations_in_regions: Vec<AnnotationLeak>,
    pub secrets_found: Vec<SecretHit>,
    /// Revisions in the file; more than one means earlier versions can be recovered.
    pub revisions: usize,
    /// Problems that kept a check from running.
    pub errors: Vec<String>,
}

impl VerifyReport {
    /// Nothing was found and every check ran.
    pub fn is_clean(&self) -> bool {
        self.text_in_regions.is_empty()
            && self.images_in_regions.is_empty()
            && self.annotations_in_regions.is_empty()
            && self.secrets_found.is_empty()
            && self.revisions <= 1
            && self.errors.is_empty()
    }
}

/// Verifies a saved file against the redacted `regions` (page index, areas) and the
/// `secrets` that must be gone. Secrets match ignoring ASCII case, and in text also
/// ignoring white space.
pub fn verify(
    bytes: &[u8],
    password: Option<&str>,
    regions: &[(usize, Vec<Region>)],
    secrets: &[&str],
) -> VerifyReport {
    let mut rep = VerifyReport {
        revisions: count(bytes, b"startxref"),
        ..VerifyReport::default()
    };
    let doc = match Document::open(bytes.to_vec()) {
        Ok(d) => d,
        Err(pagebara_syntax::Error::PasswordRequired(locked)) => {
            match password.map(|p| locked.unlock(p)) {
                Some(Ok(d)) => d,
                _ => {
                    rep.errors.push("the file needs a password".into());
                    return rep;
                }
            }
        }
        Err(e) => {
            rep.errors.push(format!("the file does not open: {e}"));
            return rep;
        }
    };
    let secrets: Vec<&str> = secrets
        .iter()
        .copied()
        .filter(|s| !s.trim().is_empty())
        .collect();
    let tree = pagebara_pages::PageTree::load(&doc);
    let mut texts = Vec::new();
    for (i, page) in tree.pages().iter().enumerate() {
        let Some(po) = doc.get(page.r) else { continue };
        let Some(pd) = po.as_dict() else { continue };
        let res = crate::apply::inherited(&doc, pd, b"Resources")
            .and_then(|o| doc.resolve(&o).as_dict().cloned())
            .unwrap_or_default();
        let list = Interpreter::new(&doc)
            .with_optional_content(OptionalContent::default())
            .page(pd, &res);
        let text = extract_text(&doc, &list);
        let page_regions: Vec<&Region> = regions
            .iter()
            .filter(|(p, _)| *p == i)
            .flat_map(|(_, r)| r)
            .collect();
        if !page_regions.is_empty() {
            check_page(&doc, i, pd, &list, &text, &page_regions, &mut rep);
        }
        texts.push(normalize(text.text.as_bytes()));
    }
    let mut hits: HashSet<SecretHit> = HashSet::new();
    for s in &secrets {
        let n = normalize(s.as_bytes());
        for (i, t) in texts.iter().enumerate() {
            if find(t, &n) {
                hits.insert(hit(s, Location::PageText { page: i }));
            }
        }
    }
    scan_bytes(&doc, bytes, &secrets, &mut hits);
    let mut hits: Vec<SecretHit> = hits.into_iter().collect();
    hits.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    rep.secrets_found = hits;
    rep
}

fn hit(s: &str, location: Location) -> SecretHit {
    SecretHit {
        secret: s.to_string(),
        location,
    }
}

fn count(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

/// Lowercase ASCII without white space.
fn normalize(b: &[u8]) -> Vec<u8> {
    let s = String::from_utf8_lossy(b).to_lowercase();
    s.bytes().filter(|b| !b.is_ascii_whitespace()).collect()
}

fn find(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

fn find_ci(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
}

/// The encodings a secret can be stored in.
fn variants(s: &str) -> Vec<Vec<u8>> {
    let mut v = vec![s.as_bytes().to_vec()];
    let utf16: Vec<u8> = s.encode_utf16().flat_map(u16::to_be_bytes).collect();
    v.push(utf16);
    if let Some(p) = pagebara_syntax::text::pdfdoc_encode(s)
        && !v.contains(&p)
    {
        v.push(p);
    }
    v
}

fn check_page(
    doc: &Document,
    page: usize,
    pd: &pagebara_syntax::Dict,
    list: &DisplayList,
    text: &pagebara_content::TextPage,
    regions: &[&Region],
    rep: &mut VerifyReport,
) {
    // Glyphs and images of the overlay itself.
    let mut overlay_glyphs = HashSet::new();
    let mut depth = 0u32;
    let mut stack = Vec::new();
    for cmd in &list.cmds {
        match *cmd {
            Cmd::BeginMarked { marked } => {
                let o = list.marked[marked as usize]
                    .properties
                    .as_ref()
                    .is_some_and(is_overlay);
                stack.push(o);
                depth += u32::from(o);
            }
            Cmd::EndMarked => {
                if stack.pop() == Some(true) {
                    depth -= 1;
                }
            }
            Cmd::Text { run } if depth > 0 => {
                let r = &list.runs[run as usize];
                overlay_glyphs.extend(r.glyphs.0..r.glyphs.1);
            }
            Cmd::Image { image, ctm, .. } if depth == 0 => {
                let m = list.matrix(ctm);
                let m = Matrix::new(
                    f64::from(m.a),
                    f64::from(m.b),
                    f64::from(m.c),
                    f64::from(m.d),
                    f64::from(m.e),
                    f64::from(m.f),
                );
                let img = &list.images[image as usize];
                check_image(doc, page, &img.source, &m, regions, rep);
            }
            _ => {}
        }
    }
    for c in &text.chars {
        if c.glyph == NONE || overlay_glyphs.contains(&c.glyph) {
            continue;
        }
        let q = c.quad.map(|p| Point::new(f64::from(p.x), f64::from(p.y)));
        if regions.iter().any(|r| r.hits(&q, GLYPH_OVERLAP)) {
            rep.text_in_regions.push(TextLeak {
                page,
                text: text.char_text(c).to_string(),
                bbox: Rect::from_points(&q),
            });
        }
    }
    let owned: Vec<Region> = regions.iter().map(|r| (*r).clone()).collect();
    for a in crate::mark::annots(doc, pd) {
        let Some(ao) = doc.get(a) else { continue };
        let Some(ad) = ao.as_dict() else { continue };
        if ad.name(b"Subtype") == Some(b"Popup") || !crate::apply::annot_meets(doc, ad, &owned) {
            continue;
        }
        rep.annotations_in_regions.push(AnnotationLeak {
            page,
            object: a.num,
            subtype: String::from_utf8_lossy(ad.name(b"Subtype").unwrap_or_default()).into_owned(),
        });
    }
}

fn check_image(
    doc: &Document,
    page: usize,
    src: &ImageSource,
    ctm: &Matrix,
    regions: &[&Region],
    rep: &mut VerifyReport,
) {
    let (stream, num): (Stream, Option<u32>) = match src {
        ImageSource::XObject(o) => match doc.resolve(o).as_stream() {
            Some(s) => (s.clone(), o.as_objref().map(|r| r.num)),
            None => return,
        },
        ImageSource::Inline(s) => ((**s).clone(), None),
    };
    let quad = Rect::new(0.0, 0.0, 1.0, 1.0)
        .corners()
        .map(|p| ctm.apply(p));
    if !regions.iter().any(|r| r.touches(&quad)) {
        return;
    }
    let mut leak = |reason| {
        rep.images_in_regions.push(ImageLeak {
            page,
            object: num,
            reason,
        })
    };
    let mut streams = vec![(stream.clone(), "image pixels under a region differ")];
    for (k, reason) in [
        (&b"SMask"[..], "soft mask under a region differs"),
        (b"Mask", "mask under a region differs"),
    ] {
        if let Some(s) = stream
            .dict
            .get(k)
            .and_then(|m| doc.resolve(m).as_stream().cloned())
        {
            streams.push((s, reason));
        }
    }
    for (s, reason) in streams {
        let Some(img) = image::decode(doc, &s, None) else {
            leak("image under a region cannot be decoded to check it");
            return;
        };
        let Some(mask) = image::coverage(img.w, img.h, ctm, regions) else {
            continue;
        };
        let mut first: Option<Vec<u32>> = None;
        let mut uniform = true;
        'px: for y in 0..img.h {
            for x in 0..img.w {
                if !mask[(y * img.w + x) as usize] {
                    continue;
                }
                let px: Vec<u32> = (0..img.n).map(|k| img.get(x, y, k)).collect();
                match &first {
                    None => first = Some(px),
                    Some(f) if *f != px => {
                        uniform = false;
                        break 'px;
                    }
                    _ => {}
                }
            }
        }
        if !uniform {
            leak(reason);
        }
    }
}

/// Searches the secrets in the raw file, in every object, and in streams found by
/// scanning the file.
fn scan_bytes(doc: &Document, bytes: &[u8], secrets: &[&str], hits: &mut HashSet<SecretHit>) {
    // (secret, encodings, normalised form)
    type Needle<'a> = (&'a str, Vec<Vec<u8>>, Vec<u8>);
    let all: Vec<Needle<'_>> = secrets
        .iter()
        .map(|s| (*s, variants(s), normalize(s.as_bytes())))
        .collect();
    let search = |data: &[u8], loc: &dyn Fn(usize) -> Location, hits: &mut HashSet<SecretHit>| {
        for (s, vs, _) in &all {
            for v in vs {
                if let Some(at) = find_ci(data, v) {
                    hits.insert(hit(s, loc(at)));
                    break;
                }
            }
        }
    };
    search(bytes, &|at| Location::File { offset: at }, hits);
    for num in doc.object_numbers() {
        let Some(r) = doc.reference(num) else {
            continue;
        };
        let Some(o) = doc.get(r) else { continue };
        let mut strings = Vec::new();
        collect_strings(&o, &mut strings, 0);
        for st in &strings {
            search(st, &|_| Location::Object { num }, hits);
        }
        if let Object::Stream(s) = &*o
            && let Ok(d) = doc.decode(s)
        {
            search(&d.data, &|_| Location::Stream { num }, hits);
            if s.dict.name(b"Subtype") != Some(b"Image") && d.image.is_none() {
                let shown = normalize(&shown_strings(&d.data));
                for (s, _, n) in &all {
                    if find(&shown, n) {
                        hits.insert(hit(s, Location::ContentText { num }));
                    }
                }
            }
        }
    }
    // Streams anywhere in the file, indexed or not.
    let mut at = 0;
    while let Some(p) = find_ci(&bytes[at..], b"stream") {
        let start = at + p + 6;
        at = start;
        if bytes[..start - 6].ends_with(b"end") {
            continue;
        }
        let body = match bytes.get(start..) {
            Some(b) if b.starts_with(b"\r\n") => start + 2,
            Some(b) if b.starts_with(b"\n") || b.starts_with(b"\r") => start + 1,
            _ => continue,
        };
        let Some(end) = find_ci(&bytes[body..], b"endstream").map(|e| body + e) else {
            break;
        };
        let raw = &bytes[body..end];
        let d = pagebara_flate::decode_lenient(raw, 256 << 20);
        if !d.data.is_empty() {
            search(&d.data, &|_| Location::LooseStream { offset: body }, hits);
        }
        at = end;
    }
}

fn collect_strings(o: &Object, out: &mut Vec<Vec<u8>>, depth: u32) {
    if depth > 64 {
        return;
    }
    match o {
        Object::String(s) => out.push(s.bytes.clone()),
        // Name values too: destination names can carry words.
        Object::Name(n) => out.push(n.as_bytes().to_vec()),
        Object::Array(a) => a.iter().for_each(|e| collect_strings(e, out, depth + 1)),
        Object::Dict(d) => d
            .iter()
            .for_each(|(_, v)| collect_strings(v, out, depth + 1)),
        Object::Stream(s) => s
            .dict
            .iter()
            .for_each(|(_, v)| collect_strings(v, out, depth + 1)),
        _ => {}
    }
}

/// The bytes of the strings shown by a content stream, joined.
fn shown_strings(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut p = ContentParser::new(data);
    while let Some(i) = p.next() {
        let items = match i.op {
            Op::ShowText | Op::NextLineShow | Op::NextLineSpacingShow => {
                i.args.last().map(std::slice::from_ref).unwrap_or_default()
            }
            Op::ShowTextArray => i.args.last().map(|a| i.array(a)).unwrap_or_default(),
            _ => continue,
        };
        for it in items {
            it.string_into(&mut out);
        }
    }
    out
}
