//! Finding and removing stamps: ours by their marker, other tools' by their pagination
//! artifact marks (`/Subtype /Watermark`, `/Header`, `/Footer`) or a layer named
//! "Watermark".

use pagebara_edit::{MarkedContent, Target};
use pagebara_syntax::{Document, ObjRef, Object};
use serde::Serialize;

use crate::draw::{self, MARK, SPEC};
use crate::{Kind, Result, oc};

/// A stamp on a page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Found {
    /// 0-based.
    pub page: usize,
    pub kind: Kind,
    /// Made by another tool (found by its marks).
    pub foreign: bool,
}

/// What [`remove`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Removed {
    pub removed: usize,
    pub pages: Vec<usize>,
}

/// The kind of foreign stamp a marked-content sequence belongs to.
fn foreign_kind(doc: &Document, m: &MarkedContent, groups: &[(ObjRef, String)]) -> Option<Kind> {
    if m.tag.as_bytes() == b"Artifact"
        && let Some(p) = &m.properties
    {
        let sub = doc.get_in(p, b"Subtype");
        match sub.as_name() {
            Some(b"Watermark") => return Some(Kind::Watermark),
            Some(b"Header" | b"Footer") => return Some(Kind::HeaderFooter),
            _ => {}
        }
    }
    let r = m.optional_content.as_ref()?.as_objref()?;
    groups
        .iter()
        .any(|(g, _)| *g == r)
        .then_some(Kind::Watermark)
}

/// Layers other tools name for watermarks.
fn watermark_groups(doc: &Document) -> Vec<(ObjRef, String)> {
    oc::groups(doc)
        .into_iter()
        .filter(|(_, n, mark)| mark.is_none() && n.trim().eq_ignore_ascii_case("watermark"))
        .map(|(r, n, _)| (r, n))
        .collect()
}

/// Whether the page's content could hold foreign stamps (to skip parsing the rest).
fn may_have_foreign(doc: &Document, page: ObjRef) -> bool {
    let Some(d) = doc.get(page).and_then(|o| o.as_dict().cloned()) else {
        return false;
    };
    let mut c = Vec::new();
    for o in draw::contents(doc, &d) {
        if draw::stream_kind(doc, &o).is_some() {
            continue;
        }
        if let Some(s) = doc.resolve(&o).as_stream()
            && let Ok(dec) = doc.decode(s)
        {
            c.extend_from_slice(&dec.data);
        }
    }
    let has = |n: &[u8]| c.windows(n.len()).any(|w| w == n);
    has(b"Artifact") || has(b"/OC")
}

/// Foreign stamps on page `i`: the top-level objects inside their marks.
fn foreign_on(
    doc: &Document,
    i: usize,
    page: ObjRef,
    kinds: &[Kind],
    groups: &[(ObjRef, String)],
) -> Vec<(Target, Kind)> {
    if !may_have_foreign(doc, page) {
        return Vec::new();
    }
    let Ok(objs) = pagebara_edit::parse(doc, i) else {
        return Vec::new();
    };
    objs.top_level()
        .filter_map(|o| {
            let k = o.marked.iter().find_map(|m| foreign_kind(doc, m, groups))?;
            kinds.contains(&k).then_some((Target::Object(o.id), k))
        })
        .collect()
}

/// Stamps on each page: ours always, foreign ones when `foreign` is set.
pub fn find(doc: &Document, foreign: bool) -> Vec<Found> {
    let groups = watermark_groups(doc);
    let mut out = Vec::new();
    for (i, f) in draw::frames(doc).iter().enumerate() {
        let Some(d) = doc.get(f.r).and_then(|o| o.as_dict().cloned()) else {
            continue;
        };
        for o in draw::contents(doc, &d) {
            if let Some(kind) = draw::stream_kind(doc, &o) {
                out.push(Found {
                    page: i,
                    kind,
                    foreign: false,
                });
            }
        }
        if foreign {
            let mut kinds: Vec<Kind> = foreign_on(doc, i, f.r, &Kind::ALL, &groups)
                .into_iter()
                .map(|t| t.1)
                .collect();
            kinds.dedup();
            out.extend(kinds.into_iter().map(|kind| Found {
                page: i,
                kind,
                foreign: true,
            }));
        }
    }
    out
}

/// Removes our stamps of `kinds` (and, with `foreign`, other tools' of those kinds). Our
/// layers go too once nothing is drawn in them.
pub fn remove(doc: &mut Document, kinds: &[Kind], foreign: bool) -> Result<Removed> {
    crate::check_editable(doc)?;
    let frames = draw::frames(doc);
    let groups = watermark_groups(doc);
    let mut out = Removed::default();
    for (i, f) in frames.iter().enumerate() {
        let mut n = draw::strip(doc, f.r, kinds);
        if foreign {
            let targets: Vec<Target> = foreign_on(doc, i, f.r, kinds, &groups)
                .into_iter()
                .map(|t| t.0)
                .collect();
            if !targets.is_empty() && pagebara_edit::delete(doc, i, &targets).is_ok() {
                n += targets.len();
            }
        }
        if n > 0 {
            out.removed += n;
            out.pages.push(i);
        }
    }
    // Layers left empty.
    let left = find(doc, false);
    for (r, _, mark) in oc::groups(doc) {
        let mine = mark.as_deref().and_then(Kind::from_marker);
        let unused = match mine {
            Some(k) => kinds.contains(&k) && !left.iter().any(|f| f.kind == k),
            None => foreign && kinds.contains(&Kind::Watermark) && groups.iter().any(|g| g.0 == r),
        };
        if unused {
            oc::drop_group(doc, r);
        }
    }
    Ok(out)
}

/// The settings (JSON) of our first stamp of `kind`, for editing it.
pub fn existing(doc: &Document, kind: Kind) -> Option<String> {
    for f in draw::frames(doc) {
        let Some(d) = doc.get(f.r).and_then(|o| o.as_dict().cloned()) else {
            continue;
        };
        let res = doc.get_in(&d, b"Resources");
        let Some(xo) = res.as_dict().map(|r| doc.get_in(r, b"XObject")) else {
            continue;
        };
        let Some(xo) = xo.as_dict() else { continue };
        for (_, v) in xo.iter() {
            let Some(r) = v.as_objref() else { continue };
            let Some(o) = doc.get(r) else { continue };
            let Some(s) = o.as_stream() else { continue };
            if s.dict.name(MARK) == Some(kind.marker())
                && let Some(Object::String(spec)) = s.dict.get(SPEC)
            {
                return Some(String::from_utf8_lossy(&spec.bytes).into_owned());
            }
        }
    }
    None
}
