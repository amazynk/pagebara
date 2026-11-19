//! Applying redactions: page content, annotations, form fields, the overlay, and what
//! else describes the removed content (structure alternate text, XFA data, thumbnails).

use std::collections::{BTreeMap, HashSet};

use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};

use crate::geom::{Matrix, Point, Rect, Region};
use crate::mark::{self, Mark};
use crate::rewrite::{Res, Rewriter, Stats};
use crate::{Error, Result, fonts, overlay, prune};

/// The default share of a glyph's box a region must cover to remove it.
pub const GLYPH_OVERLAP: f64 = 0.2;

/// How redactions are applied.
#[derive(Clone, Debug, PartialEq)]
pub struct ApplyOptions {
    /// Share of a glyph's box a region must cover for the glyph to be removed. The default
    /// ([`GLYPH_OVERLAP`], a fifth) removes every glyph a region covers noticeably, but not
    /// neighbours whose boxes reach into it through kerning or tight spacing; what a
    /// region covers of those is a sliver of a glyph that stays readable beside it.
    pub glyph_overlap: f64,
    /// Remove annotations (comments, links) that overlap a region.
    pub annotations: bool,
    /// Remove form field widgets that overlap a region and clear their fields' values.
    pub fields: bool,
    /// Draw each mark's fill and overlay text as page content.
    pub overlay: bool,
}

impl Default for ApplyOptions {
    fn default() -> Self {
        ApplyOptions {
            glyph_overlap: GLYPH_OVERLAP,
            annotations: true,
            fields: true,
            overlay: true,
        }
    }
}

/// What was redacted on one page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageReport {
    /// Page index.
    pub page: usize,
    /// The redacted areas, for [`verify`](crate::verify).
    pub regions: Vec<Region>,
    pub stats: Stats,
    pub annotations_removed: usize,
    /// Fully qualified names of fields whose values were cleared.
    pub fields_cleared: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub pages: Vec<PageReport>,
    /// Structure elements that lost alternate or actual text describing removed content.
    pub structure_cleaned: usize,
    /// Resource entries dropped because they named content now removed.
    pub resources_pruned: usize,
    pub xfa_removed: bool,
    /// Glyphs whose outlines left the embedded fonts (nothing shows them any more).
    pub glyphs_blanked: usize,
    /// Fonts that lost glyphs.
    pub fonts_pruned: usize,
    /// Signatures the redaction invalidates.
    pub signatures_invalidated: usize,
    pub warnings: Vec<String>,
}

impl Report {
    /// The redacted areas by page index.
    pub fn regions(&self) -> Vec<(usize, Vec<Region>)> {
        self.pages
            .iter()
            .map(|p| (p.page, p.regions.clone()))
            .collect()
    }

    pub fn stats(&self) -> Stats {
        let mut s = Stats::default();
        for p in &self.pages {
            s.add(&p.stats);
        }
        s
    }
}

/// Applies every redaction mark in the document and removes the marks. Save the result
/// with [`save`](crate::save): an incremental save would keep the original content.
pub fn apply(doc: &mut Document, opts: &ApplyOptions) -> Result<Report> {
    apply_marks(doc, &[], opts)
}

/// Applies `extra` marks (page index, mark) together with the document's own marks.
pub fn apply_marks(
    doc: &mut Document,
    extra: &[(usize, Mark)],
    opts: &ApplyOptions,
) -> Result<Report> {
    if !doc.permissions().modify && !doc.is_owner() {
        return Err(Error::NotPermitted);
    }
    let tree = pagebara_pages::PageTree::load(doc);
    let mut todo: BTreeMap<usize, (Vec<Mark>, Vec<ObjRef>)> = BTreeMap::new();
    for (i, page) in tree.pages().iter().enumerate() {
        for (r, m) in mark::marks(doc, page.r) {
            let e = todo.entry(i).or_default();
            e.0.push(m);
            e.1.push(r);
        }
    }
    for (i, m) in extra {
        if *i >= tree.len() {
            return Err(Error::NoSuchPage(*i));
        }
        todo.entry(*i).or_default().0.push(m.clone());
    }
    let mut report = Report::default();
    let mut tainted = HashSet::new();
    let mut mcids = Vec::new();
    let mut removed = fonts::Removed::new();
    let mut fields_touched = false;
    for (i, (marks, applied)) in todo {
        let page = tree.pages()[i].r;
        let (pr, t, m, r) = redact_page(doc, page, i, &marks, &applied, opts, &mut report)?;
        fields_touched |= !pr.fields_cleared.is_empty();
        tainted.extend(t);
        mcids.extend(m);
        fonts::merge(&mut removed, r);
        report.pages.push(pr);
    }
    if report.pages.is_empty() {
        return Ok(report);
    }
    report.structure_cleaned = clean_structure(doc, &mcids);
    tainted.remove(&0);
    if !tainted.is_empty() {
        report.resources_pruned = prune::prune_xobjects(doc, Some(&tainted));
        let still = still_referenced(doc, &tainted);
        if !still.is_empty() {
            report.warnings.push(format!(
                "{} object(s) with content under a region are also shown elsewhere and stay \
                 there unredacted",
                still.len()
            ));
        }
    }
    if fields_touched {
        report.xfa_removed = remove_xfa(doc);
    }
    let pruned = fonts::prune(doc, &removed, &mut report.warnings);
    report.glyphs_blanked = pruned.glyphs;
    report.fonts_pruned = pruned.fonts;
    report.signatures_invalidated = count_signatures(doc);
    Ok(report)
}

type PageResult = (PageReport, HashSet<u32>, Vec<(i64, i64)>, fonts::Removed);

fn redact_page(
    doc: &mut Document,
    page: ObjRef,
    index: usize,
    marks: &[Mark],
    applied: &[ObjRef],
    opts: &ApplyOptions,
    report: &mut Report,
) -> Result<PageResult> {
    let po = doc.get(page).ok_or(Error::NotAPage)?;
    let mut pd = po.as_dict().ok_or(Error::NotAPage)?.clone();
    let regions: Vec<Region> = marks.iter().flat_map(Mark::regions).collect();
    let mut pr = PageReport {
        page: index,
        regions: regions.clone(),
        ..PageReport::default()
    };
    let content = pagebara_content::page_contents(doc, &pd);
    let res_obj = inherited(doc, &pd, b"Resources");
    let mut res = Res::new(doc, res_obj.as_ref());
    let rotate = inherited(doc, &pd, b"Rotate")
        .and_then(|o| o.as_int())
        .unwrap_or(0)
        .rem_euclid(360) as u16
        / 90
        * 90;
    let sp = doc.get_in(&pd, b"StructParents").as_int();
    let mut rw = Rewriter::new(doc, regions.clone(), opts.glyph_overlap);
    let (new, changed) = rw.rewrite(&content, &mut res, Matrix::IDENTITY, sp);
    pr.stats = rw.stats.clone();
    let tainted = std::mem::take(&mut rw.tainted);
    let mcids = std::mem::take(&mut rw.mcids);
    let glyphs = std::mem::take(&mut rw.removed);
    report.warnings.append(&mut rw.warnings);
    drop(rw);

    let mut data = b"q\n".to_vec();
    data.extend_from_slice(if changed { &new } else { &content });
    data.extend_from_slice(b"\nQ\n");
    if opts.overlay {
        data.extend(overlay::draw(doc, marks, rotate, &mut res));
    }
    let mut sd = Dict::new();
    sd.set(b"Filter", Object::name(b"FlateDecode"));
    let contents = doc.add(Stream::new(sd, pagebara_filters::encode_flate(&data, 6)));
    pd.set(b"Contents", Object::Ref(contents));
    pd.set(b"Resources", Object::Dict(res.dict));
    // A thumbnail shows the old content.
    pd.remove(b"Thumb");
    doc.set(page, pd.clone());

    let mut remove: Vec<ObjRef> = applied.to_vec();
    for a in mark::annots(doc, &pd) {
        if applied.contains(&a) {
            continue;
        }
        let Some(ao) = doc.get(a) else { continue };
        let Some(ad) = ao.as_dict() else { continue };
        if !annot_meets(doc, ad, &regions) {
            continue;
        }
        match ad.name(b"Subtype") {
            Some(b"Widget") if opts.fields => {
                if let Some(name) = clear_field(doc, a) {
                    pr.fields_cleared.push(name);
                }
                remove.push(a);
            }
            Some(b"Widget") => {}
            Some(b"Popup") => {}
            _ if opts.annotations => remove.push(a),
            _ => {}
        }
    }
    let removed = remove_annots(doc, page, &remove);
    pr.annotations_removed = removed.saturating_sub(applied.len());
    Ok((pr, tainted, mcids, glyphs))
}

/// An inheritable page attribute.
pub(crate) fn inherited(doc: &Document, page: &Dict, key: &[u8]) -> Option<Object> {
    let mut d = page.clone();
    for _ in 0..64 {
        if let Some(v) = d.get(key) {
            return Some(v.clone());
        }
        let parent = d.get(b"Parent")?.as_objref()?;
        d = doc.get(parent)?.as_dict()?.clone();
    }
    None
}

/// Whether an annotation's area (its quadrilaterals, or its rectangle) overlaps a region.
pub(crate) fn annot_meets(doc: &Document, d: &Dict, regions: &[Region]) -> bool {
    let quads: Vec<Vec<Point>> = doc
        .get_in(d, b"QuadPoints")
        .as_array()
        .unwrap_or_default()
        .iter()
        .filter_map(|o| doc.resolve(o).as_f64())
        .collect::<Vec<f64>>()
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| (0..4).map(|i| Point::new(c[2 * i], c[2 * i + 1])).collect())
        .collect();
    let polys: Vec<Vec<Point>> = if quads.is_empty() {
        match d.get(b"Rect").and_then(|o| Rect::from_object(doc, o)) {
            Some(r) => vec![r.corners().to_vec()],
            None => return false,
        }
    } else {
        quads
            .into_iter()
            .filter_map(|q| Region::from_points(&q).map(|r| r.points().to_vec()))
            .collect()
    };
    polys.iter().any(|p| {
        regions
            .iter()
            .any(|r| r.overlap_area(p) > 0.0 || r.touches(p) && crate::geom::area(p) < 1e-9)
    })
}

/// Removes annotations from a page with their pop-ups and replies, and deletes them.
/// Returns how many went.
pub(crate) fn remove_annots(doc: &mut Document, page: ObjRef, remove: &[ObjRef]) -> usize {
    let Some(po) = doc.get(page) else { return 0 };
    let Some(pd) = po.as_dict() else { return 0 };
    let all = mark::annots(doc, pd);
    let mut gone: HashSet<ObjRef> = remove.iter().copied().collect();
    // Pop-ups and replies of removed annotations go too.
    loop {
        let before = gone.len();
        for &a in &all {
            if gone.contains(&a) {
                continue;
            }
            let Some(ao) = doc.get(a) else { continue };
            let Some(ad) = ao.as_dict() else { continue };
            let linked = [&b"Parent"[..], b"IRT"].iter().any(|k| {
                ad.get(k)
                    .and_then(Object::as_objref)
                    .is_some_and(|r| gone.contains(&r))
            }) && ad.name(b"Subtype") != Some(b"Widget");
            if linked {
                gone.insert(a);
            }
        }
        if gone.len() == before {
            break;
        }
    }
    let kept: Vec<Object> = doc
        .get_in(pd, b"Annots")
        .as_array()
        .unwrap_or_default()
        .iter()
        .filter(|o| o.as_objref().is_none_or(|r| !gone.contains(&r)))
        .cloned()
        .collect();
    match pd.get(b"Annots") {
        Some(Object::Ref(ar)) => {
            let ar = *ar;
            doc.set(ar, Object::Array(kept));
        }
        _ => {
            let mut pd = pd.clone();
            if kept.is_empty() {
                pd.remove(b"Annots");
            } else {
                pd.set(b"Annots", Object::Array(kept));
            }
            doc.set(page, pd);
        }
    }
    let n = gone.iter().filter(|r| all.contains(r)).count();
    for r in gone {
        doc.delete(r);
    }
    n
}

fn field_attr(doc: &Document, f: ObjRef, key: &[u8]) -> Option<Object> {
    let mut r = f;
    for _ in 0..64 {
        let o = doc.get(r)?;
        let d = o.as_dict()?;
        if let Some(v) = d.get(key) {
            return Some(doc.resolve(v).into_owned());
        }
        r = d.get(b"Parent")?.as_objref()?;
    }
    None
}

trait Owned {
    fn into_owned(self) -> Object;
}

impl Owned for pagebara_syntax::Resolved<'_> {
    fn into_owned(self) -> Object {
        (*self).clone()
    }
}

fn full_name(doc: &Document, f: ObjRef) -> String {
    let mut parts = Vec::new();
    let mut r = Some(f);
    for _ in 0..64 {
        let Some(cur) = r else { break };
        let Some(o) = doc.get(cur) else { break };
        let Some(d) = o.as_dict() else { break };
        if let Some(t) = d.get(b"T") {
            parts.push(pagebara_content::extract::text_string(&doc.resolve(t)));
        }
        r = d.get(b"Parent").and_then(Object::as_objref);
    }
    parts.reverse();
    parts.join(".")
}

/// Clears the value of the field a widget belongs to and detaches the widget; the field's
/// other widgets lose the value's appearance. Returns the field's name.
fn clear_field(doc: &mut Document, widget: ObjRef) -> Option<String> {
    let wo = doc.get(widget)?;
    let wd = wo.as_dict()?.clone();
    let field = if wd.contains(b"T") {
        widget
    } else {
        wd.get(b"Parent")
            .and_then(Object::as_objref)
            .unwrap_or(widget)
    };
    let name = full_name(doc, field);
    let ft = field_attr(doc, field, b"FT");
    let button = ft.as_ref().and_then(Object::as_name) == Some(b"Btn");
    let fo = doc.get(field)?;
    let mut fd = fo.as_dict()?.clone();
    for k in [&b"V"[..], b"DV", b"RV"] {
        fd.remove(k);
    }
    if button {
        fd.set(b"V", Object::name(b"Off"));
    }
    let kids: Vec<ObjRef> = if field == widget {
        Vec::new()
    } else {
        doc.get_in(&fd, b"Kids")
            .as_array()
            .unwrap_or_default()
            .iter()
            .filter_map(Object::as_objref)
            .filter(|&k| k != widget)
            .collect()
    };
    if field != widget {
        fd.set(
            b"Kids",
            Object::Array(kids.iter().map(|&k| Object::Ref(k)).collect()),
        );
    }
    doc.set(field, fd.clone());
    for k in &kids {
        let Some(ko) = doc.get(*k) else { continue };
        let Some(kd) = ko.as_dict() else { continue };
        let mut kd = kd.clone();
        if button {
            kd.set(b"AS", Object::name(b"Off"));
        } else {
            let bbox = kd
                .get(b"Rect")
                .and_then(|o| Rect::from_object(doc, o))
                .map(|r| Rect::new(0.0, 0.0, r.width(), r.height()))
                .unwrap_or_default();
            let mut sd = Dict::new();
            sd.set(b"Type", Object::name(b"XObject"));
            sd.set(b"Subtype", Object::name(b"Form"));
            sd.set(b"BBox", bbox.to_object());
            let empty = doc.add(Stream::new(sd, Vec::new()));
            let mut ap = Dict::new();
            ap.set(b"N", Object::Ref(empty));
            kd.set(b"AP", ap);
        }
        doc.set(*k, kd);
    }
    if kids.is_empty() {
        detach_field(doc, field);
    }
    Some(name)
}

/// Removes a field from its parent's `/Kids` or the form's `/Fields`.
fn detach_field(doc: &mut Document, field: ObjRef) {
    let parent = doc
        .get(field)
        .and_then(|o| o.as_dict()?.get(b"Parent")?.as_objref());
    let strip = |doc: &mut Document, holder: ObjRef, key: &[u8]| {
        let Some(o) = doc.get(holder) else { return };
        let Some(d) = o.as_dict() else { return };
        let mut d = d.clone();
        match d.get(key).cloned() {
            Some(Object::Ref(ar)) => {
                if let Some(a) = doc
                    .get(ar)
                    .and_then(|a| a.as_array().map(<[Object]>::to_vec))
                {
                    let a: Vec<Object> = a
                        .into_iter()
                        .filter(|o| o.as_objref() != Some(field))
                        .collect();
                    doc.set(ar, Object::Array(a));
                }
            }
            Some(Object::Array(a)) => {
                let a: Vec<Object> = a
                    .into_iter()
                    .filter(|o| o.as_objref() != Some(field))
                    .collect();
                d.set(key, Object::Array(a));
                doc.set(holder, d);
            }
            _ => {}
        }
    };
    match parent {
        Some(p) => strip(doc, p, b"Kids"),
        None => {
            let form = doc
                .catalog()
                .and_then(|c| c.as_dict()?.get(b"AcroForm").cloned());
            match form {
                Some(Object::Ref(f)) => strip(doc, f, b"Fields"),
                Some(Object::Dict(mut fd)) => {
                    if let Some(Object::Array(a)) = fd.get(b"Fields").cloned() {
                        let a: Vec<Object> = a
                            .into_iter()
                            .filter(|o| o.as_objref() != Some(field))
                            .collect();
                        fd.set(b"Fields", Object::Array(a));
                        set_catalog_key(doc, b"AcroForm", Object::Dict(fd));
                    } else if let Some(Object::Ref(ar)) = fd.get(b"Fields").cloned()
                        && let Some(a) = doc
                            .get(ar)
                            .and_then(|a| a.as_array().map(<[Object]>::to_vec))
                    {
                        let a: Vec<Object> = a
                            .into_iter()
                            .filter(|o| o.as_objref() != Some(field))
                            .collect();
                        doc.set(ar, Object::Array(a));
                    }
                }
                _ => {}
            }
        }
    }
}

pub(crate) fn set_catalog_key(doc: &mut Document, key: &[u8], v: Object) {
    let Some(root) = doc.trailer().get(b"Root").and_then(Object::as_objref) else {
        return;
    };
    let Some(c) = doc.get(root) else { return };
    let Some(cd) = c.as_dict() else { return };
    let mut cd = cd.clone();
    if v.is_null() {
        cd.remove(key);
    } else {
        cd.set(key, v);
    }
    doc.set(root, cd);
}

/// Removes `/XFA` from the interactive form: its data packets repeat field values.
pub(crate) fn remove_xfa(doc: &mut Document) -> bool {
    let Some(form) = doc
        .catalog()
        .and_then(|c| c.as_dict()?.get(b"AcroForm").cloned())
    else {
        return false;
    };
    match form {
        Object::Ref(r) => {
            let Some(o) = doc.get(r) else { return false };
            let Some(d) = o.as_dict() else { return false };
            if !d.contains(b"XFA") {
                return false;
            }
            let mut d = d.clone();
            d.remove(b"XFA");
            doc.set(r, d);
            true
        }
        Object::Dict(mut d) => {
            if d.remove(b"XFA").is_none() {
                return false;
            }
            set_catalog_key(doc, b"AcroForm", Object::Dict(d));
            true
        }
        _ => false,
    }
}

/// An entry of a number tree.
fn number_tree_get(doc: &Document, node: &Dict, key: i64, depth: u32) -> Option<Object> {
    if depth > 32 {
        return None;
    }
    if let Some(nums) = doc.get_in(node, b"Nums").as_array() {
        for pair in nums.as_chunks::<2>().0 {
            if doc.resolve(&pair[0]).as_int() == Some(key) {
                return Some(pair[1].clone());
            }
        }
    }
    for kid in doc.get_in(node, b"Kids").as_array().unwrap_or_default() {
        let k = doc.resolve(kid);
        let Some(kd) = k.as_dict() else { continue };
        let limits: Vec<i64> = doc
            .get_in(kd, b"Limits")
            .as_array()
            .unwrap_or_default()
            .iter()
            .filter_map(Object::as_int)
            .collect();
        if let [lo, hi] = limits[..]
            && (key < lo || key > hi)
        {
            continue;
        }
        if let Some(v) = number_tree_get(doc, kd, key, depth + 1) {
            return Some(v);
        }
    }
    None
}

/// Strips alternate and actual text from the structure elements of changed marked content
/// and their ancestors.
fn clean_structure(doc: &mut Document, mcids: &[(i64, i64)]) -> usize {
    if mcids.is_empty() {
        return 0;
    }
    let Some(root) = doc
        .catalog()
        .and_then(|c| c.as_dict()?.get(b"StructTreeRoot").cloned())
    else {
        return 0;
    };
    let root = doc.resolve(&root).into_owned();
    let Some(rd) = root.as_dict() else { return 0 };
    let pt = doc.get_in(rd, b"ParentTree");
    let Some(pt) = pt.as_dict() else { return 0 };
    let mut elems: Vec<ObjRef> = Vec::new();
    for &(sp, mcid) in mcids {
        let Some(arr) = number_tree_get(doc, pt, sp, 0) else {
            continue;
        };
        let arr = doc.resolve(&arr);
        if let Some(e) = arr
            .as_array()
            .and_then(|a| a.get(usize::try_from(mcid).ok()?))
            .and_then(Object::as_objref)
        {
            elems.push(e);
        }
    }
    let mut cleaned = 0;
    let mut seen = HashSet::new();
    for e in elems {
        let mut cur = Some(e);
        for _ in 0..64 {
            let Some(r) = cur else { break };
            if !seen.insert(r) {
                break;
            }
            let Some(o) = doc.get(r) else { break };
            let Some(d) = o.as_dict() else { break };
            if d.name(b"Type") == Some(b"StructTreeRoot") {
                break;
            }
            let next = d.get(b"P").and_then(Object::as_objref);
            if [&b"Alt"[..], b"ActualText", b"E"]
                .iter()
                .any(|k| d.contains(k))
            {
                let mut d = d.clone();
                for k in [&b"Alt"[..], b"ActualText", b"E"] {
                    d.remove(k);
                }
                doc.set(r, d);
                cleaned += 1;
            }
            cur = next;
        }
    }
    cleaned
}

/// Tainted objects something still refers to.
fn still_referenced(doc: &Document, tainted: &HashSet<u32>) -> HashSet<u32> {
    let mut found = HashSet::new();
    let mut seen = HashSet::new();
    let mut stack: Vec<Object> = doc.trailer().iter().map(|(_, v)| v.clone()).collect();
    while let Some(o) = stack.pop() {
        match o {
            Object::Ref(r) => {
                if tainted.contains(&r.num) {
                    found.insert(r.num);
                }
                if seen.insert(r.num)
                    && let Some(o) = doc.get(r)
                {
                    stack.push((*o).clone());
                }
            }
            Object::Array(a) => stack.extend(a),
            Object::Dict(d) => stack.extend(d.0.into_iter().map(|(_, v)| v)),
            Object::Stream(s) => stack.extend(s.dict.0.into_iter().map(|(_, v)| v)),
            _ => {}
        }
    }
    found
}

fn count_signatures(doc: &Document) -> usize {
    doc.object_numbers()
        .into_iter()
        .filter_map(|n| doc.get(doc.reference(n)?))
        .filter(|o| {
            o.as_dict()
                .is_some_and(|d| d.contains(b"ByteRange") && d.contains(b"Contents"))
        })
        .count()
}
