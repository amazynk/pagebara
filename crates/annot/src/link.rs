//! Link targets: destinations (ISO 32000-2 §12.3.2) and the actions links use (§12.6).

use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::geom::{Rect, num};
use crate::text;

/// The page of a destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DestPage {
    Ref(ObjRef),
    /// Zero-based page number, as remote destinations (and some broken files) give it.
    Index(u32),
}

/// How the target page is shown. `None` coordinates and zoom keep the current value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum View {
    Xyz {
        left: Option<f64>,
        top: Option<f64>,
        zoom: Option<f64>,
    },
    Fit,
    FitH {
        top: Option<f64>,
    },
    FitV {
        left: Option<f64>,
    },
    FitR(Rect),
    FitB,
    FitBH {
        top: Option<f64>,
    },
    FitBV {
        left: Option<f64>,
    },
}

/// An explicit destination.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Destination {
    pub page: DestPage,
    pub view: View,
}

impl Destination {
    /// The top of `page`, at the current zoom.
    pub fn page(page: ObjRef) -> Self {
        Destination {
            page: DestPage::Ref(page),
            view: View::Xyz {
                left: None,
                top: None,
                zoom: None,
            },
        }
    }

    pub fn from_object(doc: &Document, o: &Object) -> Option<Destination> {
        let a = o.as_array()?;
        let page = match a.first()? {
            Object::Ref(r) => DestPage::Ref(*r),
            o => DestPage::Index(u32::try_from(o.as_int()?).ok()?),
        };
        let n = |i: usize| a.get(i).and_then(|o| doc.resolve(o).as_f64());
        let view = match a.get(1).and_then(Object::as_name)? {
            b"XYZ" => View::Xyz {
                left: n(2),
                top: n(3),
                // 0 means "unchanged", like null.
                zoom: n(4).filter(|&z| z > 0.0),
            },
            b"Fit" => View::Fit,
            b"FitH" => View::FitH { top: n(2) },
            b"FitV" => View::FitV { left: n(2) },
            b"FitR" => View::FitR(Rect::new(n(2)?, n(3)?, n(4)?, n(5)?)),
            b"FitB" => View::FitB,
            b"FitBH" => View::FitBH { top: n(2) },
            b"FitBV" => View::FitBV { left: n(2) },
            _ => return None,
        };
        Some(Destination { page, view })
    }

    pub fn to_object(&self) -> Object {
        let page = match self.page {
            DestPage::Ref(r) => Object::Ref(r),
            DestPage::Index(i) => Object::Int(i64::from(i)),
        };
        let opt = |v: Option<f64>| v.map_or(Object::Null, num);
        let (name, args): (&[u8], Vec<Object>) = match self.view {
            View::Xyz { left, top, zoom } => (b"XYZ", vec![opt(left), opt(top), opt(zoom)]),
            View::Fit => (b"Fit", vec![]),
            View::FitH { top } => (b"FitH", vec![opt(top)]),
            View::FitV { left } => (b"FitV", vec![opt(left)]),
            View::FitR(r) => (b"FitR", vec![num(r.x0), num(r.y0), num(r.x1), num(r.y1)]),
            View::FitB => (b"FitB", vec![]),
            View::FitBH { top } => (b"FitBH", vec![opt(top)]),
            View::FitBV { left } => (b"FitBV", vec![opt(left)]),
        };
        let mut v = vec![page, Object::name(name)];
        v.extend(args);
        Object::Array(v)
    }
}

/// A destination as links give it: explicit, or by name (looked up with [`named_destination`]).
#[derive(Clone, Debug, PartialEq)]
pub enum Dest {
    Explicit(Destination),
    Named(String),
}

impl Dest {
    fn from_object(doc: &Document, o: &Object) -> Option<Dest> {
        let o = doc.resolve(o);
        match &*o {
            Object::Name(n) => Some(Dest::Named(String::from_utf8_lossy(n.as_bytes()).into())),
            Object::String(s) => Some(Dest::Named(String::from_utf8_lossy(&s.bytes).into())),
            // A destination dictionary (from /Dests) holds the array under /D.
            Object::Dict(d) => Dest::from_object(doc, d.get(b"D")?),
            o => Destination::from_object(doc, o).map(Dest::Explicit),
        }
    }

    fn to_object(&self) -> Object {
        match self {
            Dest::Explicit(d) => d.to_object(),
            Dest::Named(n) => Object::string(n.as_bytes()),
        }
    }
}

/// What activating a link does.
#[derive(Clone, Debug, PartialEq)]
pub enum LinkAction {
    Uri(String),
    /// A place in this document (`/Dest`, or a GoTo action).
    GoTo(Dest),
    /// A place in another PDF file. Read only.
    GoToR {
        file: String,
        dest: Option<Dest>,
        new_window: Option<bool>,
    },
    /// Opening a file or application. Read only.
    Launch {
        file: String,
        new_window: Option<bool>,
    },
    /// Any other action, by its `/S` type. Read only.
    Other(String),
}

impl LinkAction {
    /// Kinds this crate writes; the others are kept as they are in the file.
    pub fn is_writable(&self) -> bool {
        matches!(self, LinkAction::Uri(_) | LinkAction::GoTo(_))
    }
}

/// The link's target from `/Dest`, else `/A`.
pub(crate) fn read(doc: &Document, d: &Dict) -> Option<LinkAction> {
    if let Some(dest) = d.get(b"Dest") {
        return Dest::from_object(doc, dest).map(LinkAction::GoTo);
    }
    let a = doc.get_in(d, b"A");
    let a = a.as_dict()?;
    let new_window = doc.get_in(a, b"NewWindow").as_bool();
    Some(match a.name(b"S")? {
        b"URI" => LinkAction::Uri(
            doc.get_in(a, b"URI")
                .as_string()
                .map(|u| String::from_utf8_lossy(u).into_owned())?,
        ),
        b"GoTo" => LinkAction::GoTo(Dest::from_object(doc, a.get(b"D")?)?),
        b"GoToR" => LinkAction::GoToR {
            file: file_spec(doc, a.get(b"F")?)?,
            dest: a.get(b"D").and_then(|o| Dest::from_object(doc, o)),
            new_window,
        },
        b"Launch" => {
            // Without a file specification, the Windows-specific parameters name the file.
            let win = doc.get_in(a, b"Win");
            let file = match a.get(b"F") {
                Some(f) => file_spec(doc, f),
                None => win.as_dict().and_then(|w| s_of(doc, w, b"F")),
            };
            LinkAction::Launch {
                file: file.unwrap_or_default(),
                new_window,
            }
        }
        other => LinkAction::Other(String::from_utf8_lossy(other).into_owned()),
    })
}

fn s_of(doc: &Document, d: &Dict, k: &[u8]) -> Option<String> {
    doc.get_in(d, k).as_string().map(text::decode)
}

/// A file specification string or dictionary (§7.11), preferring the Unicode name.
fn file_spec(doc: &Document, o: &Object) -> Option<String> {
    let o = doc.resolve(o);
    if let Some(s) = o.as_string() {
        return Some(text::decode(s));
    }
    let d = o.as_dict()?;
    [&b"UF"[..], b"F", b"Unix", b"DOS", b"Mac"]
        .iter()
        .find_map(|k| s_of(doc, d, k))
}

/// Writes a writable action into `d` as `/A`, replacing `/A` and `/Dest`.
pub(crate) fn write(d: &mut Dict, action: &LinkAction) {
    d.remove(b"Dest");
    let mut a = Dict::new();
    match action {
        LinkAction::Uri(u) => {
            a.set(b"S", Object::name(b"URI"));
            a.set(b"URI", Object::string(u.as_bytes()));
        }
        LinkAction::GoTo(dest) => {
            a.set(b"S", Object::name(b"GoTo"));
            a.set(b"D", dest.to_object());
        }
        _ => return,
    }
    d.set(b"A", a);
}

/// Looks `name` up in the catalog's `/Dests` and its `/Names` destination tree.
pub fn named_destination(doc: &Document, name: &str) -> Option<Destination> {
    let cat = doc.catalog()?;
    let cat = cat.as_dict()?;
    let found = doc
        .get_in(cat, b"Dests")
        .as_dict()
        .and_then(|d| d.get(name.as_bytes()).cloned())
        .or_else(|| {
            let names = doc.get_in(cat, b"Names");
            let tree = doc.get_in(names.as_dict()?, b"Dests");
            name_tree(doc, tree.as_dict()?, name.as_bytes(), 0)
        })?;
    match Dest::from_object(doc, &found)? {
        Dest::Explicit(d) => Some(d),
        Dest::Named(_) => None,
    }
}

fn name_tree(doc: &Document, node: &Dict, key: &[u8], depth: u32) -> Option<Object> {
    if depth > 32 {
        return None;
    }
    if let Some(names) = doc.get_in(node, b"Names").as_array() {
        for [k, v] in names.as_chunks::<2>().0 {
            if doc.resolve(k).as_string() == Some(key) {
                return Some(v.clone());
            }
        }
    }
    let kids = doc.get_in(node, b"Kids");
    for kid in kids.as_array().unwrap_or_default() {
        let kid = doc.resolve(kid);
        let Some(k) = kid.as_dict() else { continue };
        let limits = doc.get_in(k, b"Limits");
        if let Some([lo, hi]) = limits.as_array()
            && let (Some(lo), Some(hi)) = (doc.resolve(lo).as_string(), doc.resolve(hi).as_string())
            && !(lo..=hi).contains(&key)
        {
            continue;
        }
        if let Some(v) = name_tree(doc, k, key, depth + 1) {
            return Some(v);
        }
    }
    None
}
