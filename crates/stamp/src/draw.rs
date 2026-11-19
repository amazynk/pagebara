//! Putting stamps on pages: the page frame as shown (crop box and rotation), form
//! XObjects, and the marked content streams that draw them.

use std::fmt::Write as _;

use pagebara_pages::PageTree;
use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};

use crate::Kind;

/// Inheritance chains deeper than this are treated as broken.
const MAX_DEPTH: usize = 64;

/// The stream that opens a graphics state before the original content.
const OPEN: &[u8] = b"q\n";
/// The key of our marker in the dictionaries of the streams and forms we add.
pub(crate) const MARK: &[u8] = b"PagebaraStamp";
/// The key of a stamp's settings (JSON) in its form XObject.
pub(crate) const SPEC: &[u8] = b"PagebaraSpec";

/// A page as shown.
#[derive(Clone, Debug)]
pub(crate) struct PageFrame {
    pub r: ObjRef,
    /// Shown width and height in points (the crop box, turned by `/Rotate`).
    pub w: f64,
    pub h: f64,
    /// Maps shown coordinates (origin bottom left, y up) to the page's user space.
    pub m: [f64; 6],
}

pub(crate) fn frames(doc: &Document) -> Vec<PageFrame> {
    let tree = PageTree::load(doc);
    tree.pages()
        .iter()
        .map(|p| {
            let c = p.crop_box;
            let (cw, ch) = (c.width(), c.height());
            let (w, h, m) = match p.rotate {
                90 => (ch, cw, [0.0, 1.0, -1.0, 0.0, c.x1, c.y0]),
                180 => (cw, ch, [-1.0, 0.0, 0.0, -1.0, c.x1, c.y1]),
                270 => (ch, cw, [0.0, -1.0, 1.0, 0.0, c.x0, c.y1]),
                _ => (cw, ch, [1.0, 0.0, 0.0, 1.0, c.x0, c.y0]),
            };
            PageFrame { r: p.r, w, h, m }
        })
        .collect()
}

/// `a` then `b`: maps through `a` first.
pub(crate) fn concat(a: [f64; 6], b: [f64; 6]) -> [f64; 6] {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

pub(crate) fn num(v: f64) -> String {
    let r = (v * 10_000.0).round() / 10_000.0;
    if r == r.trunc() {
        format!("{}", r as i64)
    } else {
        let s = format!("{r:.4}");
        s.trim_end_matches('0').to_owned()
    }
}

pub(crate) fn cm(m: [f64; 6]) -> String {
    let mut s = String::new();
    for v in m {
        let _ = write!(s, "{} ", num(v));
    }
    s + "cm"
}

pub(crate) fn rect_object(r: [f64; 4]) -> Object {
    Object::Array(r.iter().map(|&v| Object::Real(v)).collect())
}

/// A compressed stream.
pub(crate) fn flate(mut dict: Dict, data: &[u8]) -> Stream {
    let z = pagebara_filters::encode_flate(data, 6);
    if z.len() + 20 < data.len() {
        dict.set(b"Filter", Object::name(b"FlateDecode"));
        Stream::new(dict, z)
    } else {
        Stream::new(dict, data.to_vec())
    }
}

/// A form XObject marked as ours.
pub(crate) fn form(
    kind: Kind,
    bbox: [f64; 4],
    resources: Dict,
    content: &[u8],
    spec: Option<&str>,
) -> Stream {
    let mut d = Dict::new();
    d.set(b"Type", Object::name(b"XObject"));
    d.set(b"Subtype", Object::name(b"Form"));
    d.set(b"BBox", rect_object(bbox));
    d.set(b"Resources", resources);
    d.set(MARK, Object::name(kind.marker()));
    if let Some(s) = spec {
        d.set(SPEC, Object::string(s.as_bytes()));
    }
    flate(d, content)
}

/// An attribute of the page or its nearest ancestor that has it.
fn inherited(doc: &Document, page: &Dict, key: &[u8]) -> Option<Object> {
    let mut cur = page.clone();
    for _ in 0..MAX_DEPTH {
        let v = doc.get_in(&cur, key);
        if !v.is_null() {
            return Some((*v).clone());
        }
        let parent = doc.get_in(&cur, b"Parent");
        cur = parent.as_dict()?.clone();
    }
    None
}

pub(crate) fn contents(doc: &Document, page: &Dict) -> Vec<Object> {
    match page.get(b"Contents") {
        Some(Object::Array(a)) => a.clone(),
        Some(o @ Object::Ref(_)) => match doc.resolve(o).as_array() {
            Some(a) => a.to_vec(),
            None => vec![o.clone()],
        },
        _ => Vec::new(),
    }
}

/// The artifact properties for a kind of stamp.
pub(crate) fn artifact(subtype: &str) -> String {
    format!("/Artifact <</Type /Pagination /Subtype /{subtype}>> BDC\n")
}

/// What one page gets: XObjects to name in its resources, an optional content group,
/// and the operators that draw them (using those names and `/PagebaraOC` for the group).
pub(crate) struct PageStamp<'a> {
    pub kind: Kind,
    pub behind: bool,
    pub xobjects: &'a [(String, ObjRef)],
    pub group: Option<ObjRef>,
    /// Drawing operators, already marked as artifacts.
    pub body: &'a str,
}

/// The resource name of a kind's group on the page.
pub(crate) fn oc_name(kind: Kind) -> String {
    format!("{}OC", kind.prefix())
}

pub(crate) fn add(doc: &mut Document, page: ObjRef, st: &PageStamp<'_>) {
    let Some(mut d) = doc.get(page).and_then(|o| o.as_dict().cloned()) else {
        return;
    };
    let mut res = inherited(doc, &d, b"Resources")
        .and_then(|o| doc.resolve(&o).as_dict().cloned())
        .unwrap_or_default();
    let mut xo = doc
        .get_in(&res, b"XObject")
        .as_dict()
        .cloned()
        .unwrap_or_default();
    for (name, r) in st.xobjects {
        xo.set(name.as_bytes(), *r);
    }
    if !st.xobjects.is_empty() {
        res.set(b"XObject", xo);
    }
    let mut body = String::new();
    if let Some(g) = st.group {
        let mut props = doc
            .get_in(&res, b"Properties")
            .as_dict()
            .cloned()
            .unwrap_or_default();
        let name = oc_name(st.kind);
        props.set(name.as_bytes(), g);
        res.set(b"Properties", props);
        let _ = writeln!(body, "/OC /{name} BDC");
        body.push_str(st.body);
        body.push_str("EMC\n");
    } else {
        body.push_str(st.body);
    }
    d.set(b"Resources", res);

    let mut list = contents(doc, &d);
    let mut sd = Dict::new();
    sd.set(MARK, Object::name(st.kind.marker()));
    if st.behind {
        let ours = doc.add(flate(sd, format!("q\n{body}Q\n").as_bytes()));
        list.insert(0, ours.into());
    } else {
        let wrapped = list.first().is_some_and(|o| {
            let s = doc.resolve(o);
            s.as_stream()
                .is_some_and(|s| !s.dict.contains(b"Filter") && doc.raw(s) == OPEN)
        });
        if !wrapped && !list.is_empty() {
            let open = doc.add(Stream::new(Dict::new(), OPEN));
            list.insert(0, open.into());
            let close = doc.add(Stream::new(Dict::new(), &b"Q\n"[..]));
            list.push(close.into());
        }
        let ours = doc.add(flate(sd, format!("q\n{body}Q\n").as_bytes()));
        list.push(ours.into());
    }
    d.set(b"Contents", list);
    doc.set(page, d);
}

/// The kind of one of our content streams.
pub(crate) fn stream_kind(doc: &Document, o: &Object) -> Option<Kind> {
    let s = doc.resolve(o);
    Kind::from_marker(s.as_stream()?.dict.name(MARK)?)
}

/// Takes our stamps of the kinds in `kinds` off a page; returns how many.
pub(crate) fn strip(doc: &mut Document, page: ObjRef, kinds: &[Kind]) -> usize {
    let Some(mut d) = doc.get(page).and_then(|o| o.as_dict().cloned()) else {
        return 0;
    };
    let list = contents(doc, &d);
    let before = list.len();
    let kept: Vec<Object> = list
        .into_iter()
        .filter(|o| stream_kind(doc, o).is_none_or(|k| !kinds.contains(&k)))
        .collect();
    let n = before - kept.len();
    if n == 0 {
        return 0;
    }
    d.set(b"Contents", kept);
    // Their resource names go too (only in the page's own resources).
    if let Some(mut res) = doc.get_in(&d, b"Resources").as_dict().cloned() {
        for (key, names) in [
            (
                &b"XObject"[..],
                kinds
                    .iter()
                    .map(|k| k.prefix().to_owned())
                    .collect::<Vec<_>>(),
            ),
            (b"Properties", kinds.iter().map(|k| oc_name(*k)).collect()),
        ] {
            if let Some(mut sub) = doc.get_in(&res, key).as_dict().cloned() {
                sub.0.retain(|(k, _)| {
                    let k = String::from_utf8_lossy(k.as_bytes());
                    !names.iter().any(|p| k.starts_with(p.as_str()))
                });
                res.set(key, sub);
            }
        }
        d.set(b"Resources", res);
    }
    doc.set(page, d);
    n
}
