//! The few page-dictionary operations annotations need: boxes, `/Annots`, and drawing on top
//! of the page content.

use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};

use crate::geom::Rect;
use crate::{Error, Result};

/// Inheritance chains deeper than this are treated as broken.
const MAX_DEPTH: usize = 64;

pub(crate) fn page_dict(doc: &Document, page: ObjRef) -> Result<Dict> {
    let o = doc.get(page).ok_or(Error::NotAPage)?;
    let d = o.as_dict().ok_or(Error::NotAPage)?;
    match d.name(b"Type") {
        Some(b"Page") | None => Ok(d.clone()),
        Some(_) => Err(Error::NotAPage),
    }
}

/// An attribute of the page or, if absent, of its nearest ancestor that has it.
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

/// The visible area of a page: its crop box, else its media box (US Letter if neither).
pub fn page_box(doc: &Document, page: &Dict) -> Rect {
    let media = inherited(doc, page, b"MediaBox")
        .and_then(|o| Rect::from_object(&o))
        .unwrap_or(Rect::new(0.0, 0.0, 612.0, 792.0));
    inherited(doc, page, b"CropBox")
        .and_then(|o| Rect::from_object(&o))
        .map_or(media, |c| {
            Rect::new(
                c.x0.max(media.x0),
                c.y0.max(media.y0),
                c.x1.min(media.x1),
                c.y1.min(media.y1),
            )
        })
}

/// The page's `/Rotate` (inherited), as 0, 90, 180 or 270 degrees clockwise.
pub fn page_rotation(doc: &Document, page: &Dict) -> u16 {
    inherited(doc, page, b"Rotate")
        .and_then(|o| o.as_int())
        .map_or(0, crate::appearance::normalize_rotation)
}

/// The entries of the page's `/Annots`.
pub(crate) fn annots(doc: &Document, page: &Dict) -> Vec<Object> {
    doc.get_in(page, b"Annots")
        .as_array()
        .map(<[Object]>::to_vec)
        .unwrap_or_default()
}

/// Replaces the page's `/Annots`, in place when it is an indirect array.
pub(crate) fn set_annots(doc: &mut Document, page: ObjRef, list: Vec<Object>) -> Result<()> {
    let mut d = page_dict(doc, page)?;
    match d.get(b"Annots").and_then(Object::as_objref) {
        Some(r) if doc.get(r).is_some_and(|o| o.as_array().is_some()) => doc.set(r, list),
        _ => {
            if list.is_empty() {
                d.remove(b"Annots");
            } else {
                d.set(b"Annots", list);
            }
            doc.set(page, d);
        }
    }
    Ok(())
}

/// A copy of the page's effective resources with sub-dictionaries `keys` made direct, so
/// adding to them never touches resources shared with other pages.
fn own_resources(doc: &Document, page: &Dict, keys: &[&[u8]]) -> Dict {
    let mut res = inherited(doc, page, b"Resources")
        .and_then(|o| doc.resolve(&o).as_dict().cloned())
        .unwrap_or_default();
    for k in keys {
        let sub = doc.get_in(&res, k).as_dict().cloned().unwrap_or_default();
        res.set(k, sub);
    }
    res
}

/// Our marker stream that opens a graphics state before the original content.
const OPEN: &[u8] = b"q\n";

/// Paints the form XObject `form` on top of the page content, transformed by `m`.
pub(crate) fn draw_form(doc: &mut Document, page: ObjRef, form: ObjRef, m: [f64; 6]) -> Result<()> {
    let mut d = page_dict(doc, page)?;
    let mut res = own_resources(doc, &d, &[b"XObject"]);
    let xobjects = res
        .get(b"XObject")
        .and_then(Object::as_dict)
        .cloned()
        .unwrap_or_default();
    let name = (0..)
        .map(|i| format!("PagebaraFx{i}"))
        .find(|n| !xobjects.contains(n.as_bytes()))
        .unwrap_or_default();
    let mut xobjects = xobjects;
    xobjects.set(name.as_bytes(), form);
    res.set(b"XObject", xobjects);
    d.set(b"Resources", res);

    let mut contents: Vec<Object> = match d.get(b"Contents") {
        Some(Object::Array(a)) => a.clone(),
        Some(o @ Object::Ref(_)) => match doc.resolve(o).as_array() {
            Some(a) => a.to_vec(),
            None => vec![o.clone()],
        },
        _ => Vec::new(),
    };
    let wrapped = contents.first().is_some_and(|o| {
        let s = doc.resolve(o);
        s.as_stream()
            .is_some_and(|s| !s.dict.contains(b"Filter") && doc.raw(s) == OPEN)
    });
    let mut ops = Vec::new();
    if !wrapped && !contents.is_empty() {
        let open = doc.add(Stream::new(Dict::new(), OPEN));
        contents.insert(0, open.into());
        ops.extend_from_slice(b"Q\n");
    }
    let mut c = crate::content::Content::new();
    c.save().cm(m).do_xobject(&name).restore();
    ops.extend_from_slice(&c.0);
    let ours = doc.add(Stream::new(Dict::new(), ops));
    contents.push(ours.into());
    d.set(b"Contents", contents);
    doc.set(page, d);
    Ok(())
}
