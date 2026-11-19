//! Converting a document to a PDF/A part.

use pagebara_i18n::{tr, trn};
use pagebara_syntax::{Dict, Document, ObjRef, Object, PdfString, Stream};

use super::fonts;
use super::validate::{info_of, metadata};
use crate::content::{Device, Scanner};
use crate::{Profile, Report, icc, xmp};

/// The converted file, what was changed, and the validation of the result.
#[derive(Debug)]
pub struct Conversion {
    pub bytes: Vec<u8>,
    /// One line per change, for the user.
    pub changes: Vec<String>,
    pub report: Report,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvertError {
    /// Protected and the owner password was not given.
    Protected,
    /// PDF/A-1 does not allow transparency; the pages that use it.
    Transparency(Vec<usize>),
    NotPdfA,
}

impl std::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConvertError::Protected => f.write_str(&tr!(
                "the document is protected; unlock it with the owner password first"
            )),
            ConvertError::Transparency(p) => {
                let pages = p
                    .iter()
                    .take(8)
                    .map(|p| (p + 1).to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                f.write_str(&trn!(
                    "PDF/A-1 does not allow transparency, which page {} uses; save as PDF/A-2b instead",
                    "PDF/A-1 does not allow transparency, which pages {} use; save as PDF/A-2b instead",
                    p.len(),
                    pages
                ))
            }
            ConvertError::NotPdfA => f.write_str(&tr!("not a PDF/A profile")),
        }
    }
}

impl std::error::Error for ConvertError {}

fn edit(doc: &mut Document, r: ObjRef, f: impl FnOnce(&mut Dict) -> bool) -> bool {
    let Some(o) = doc.get(r) else { return false };
    let mut o = (*o).clone();
    let changed = match &mut o {
        Object::Dict(d) => f(d),
        Object::Stream(s) => f(&mut s.dict),
        _ => false,
    };
    if changed {
        doc.set(r, o);
    }
    changed
}

fn forbidden_action(part: u8, s: &[u8]) -> bool {
    matches!(
        s,
        b"Launch" | b"Sound" | b"Movie" | b"ResetForm" | b"ImportData" | b"JavaScript"
    ) || (part > 1
        && matches!(
            s,
            b"Hide" | b"SetOCGState" | b"Rendition" | b"Trans" | b"GoTo3DView"
        ))
}

/// Whether an action (dictionary or reference) may stay.
fn action_ok(doc: &Document, part: u8, a: &Object) -> bool {
    let a = doc.resolve(a);
    let Some(d) = a.as_dict() else { return true };
    let Some(s) = d.name(b"S") else { return true };
    if forbidden_action(part, s) {
        return false;
    }
    if s == b"Named" {
        return doc
            .get_in(d, b"N")
            .as_name()
            .is_some_and(|n| matches!(n, b"NextPage" | b"PrevPage" | b"FirstPage" | b"LastPage"));
    }
    true
}

/// Pages using transparency, for PDF/A-1.
fn transparency_pages(doc: &Document) -> Vec<usize> {
    let mut out = Vec::new();
    let tree = pagebara_pages::PageTree::load(doc);
    for (i, p) in tree.pages().iter().enumerate() {
        let Some(po) = doc.get(p.r) else { continue };
        let Some(pd) = po.as_dict() else { continue };
        let mut stack: Vec<Object> = vec![Object::Dict(pd.clone())];
        let mut seen = std::collections::HashSet::new();
        let mut found = false;
        while let Some(o) = stack.pop() {
            if found {
                break;
            }
            match o {
                Object::Ref(r) => {
                    if seen.insert(r.num)
                        && let Some(o) = doc.get(r)
                    {
                        let is_page = o
                            .as_dict()
                            .is_some_and(|d| d.name(b"Type") == Some(b"Page"));
                        if !is_page || r == p.r {
                            stack.push((*o).clone());
                        }
                    }
                }
                Object::Dict(d) => {
                    found |= uses_transparency(doc, &d);
                    stack.extend(
                        d.0.into_iter()
                            .filter(|(k, _)| {
                                !matches!(k.as_bytes(), b"Parent" | b"P" | b"Dest" | b"A")
                            })
                            .map(|(_, v)| v),
                    );
                }
                Object::Stream(s) => {
                    found |= uses_transparency(doc, &s.dict);
                    stack.extend(s.dict.0.into_iter().map(|(_, v)| v));
                }
                Object::Array(a) => stack.extend(a),
                _ => {}
            }
        }
        if found {
            out.push(i);
        }
    }
    out
}

fn uses_transparency(doc: &Document, d: &Dict) -> bool {
    let smask = doc.get_in(d, b"SMask");
    let smask = !smask.is_null() && smask.as_name() != Some(b"None");
    let alpha = |k: &[u8]| {
        doc.get_in(d, k)
            .as_f64()
            .is_some_and(|v| (v - 1.0).abs() > 1e-6)
    };
    let bm = match &*doc.get_in(d, b"BM") {
        Object::Name(n) => n.as_bytes() != b"Normal" && n.as_bytes() != b"Compatible",
        Object::Array(a) => a
            .iter()
            .any(|b| !matches!(b.as_name(), Some(b"Normal" | b"Compatible"))),
        _ => false,
    };
    let group = doc
        .get_in(d, b"Group")
        .as_dict()
        .is_some_and(|g| g.name(b"S") == Some(b"Transparency"));
    let annot = d.contains(b"Rect") && alpha(b"CA");
    (smask
        && (d.name(b"Subtype") == Some(b"Image")
            || d.name(b"Type") == Some(b"ExtGState")
            || d.contains(b"CA")
            || d.contains(b"ca")
            || d.contains(b"BM")))
        || ((alpha(b"CA") || alpha(b"ca")) && !d.contains(b"Rect"))
        || bm
        || group
        || annot
}

fn now_pdf() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "D:{y:04}{m:02}{d:02}{:02}{:02}{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

fn text(s: &str) -> Object {
    if s.is_ascii() {
        Object::String(PdfString {
            bytes: s.as_bytes().to_vec(),
            hex: false,
        })
    } else {
        let mut b = vec![0xfe, 0xff];
        for u in s.encode_utf16() {
            b.extend_from_slice(&u.to_be_bytes());
        }
        Object::String(PdfString {
            bytes: b,
            hex: true,
        })
    }
}

/// Writes XMP metadata matching the document information, with the PDF/A (and PDF/UA)
/// identification, and dates the change.
pub(crate) fn sync_metadata(
    doc: &mut Document,
    pdfa: Option<(u8, &str)>,
    pdfua: bool,
    changes: &mut Vec<String>,
) {
    let now = now_pdf();
    let mut info_dict = doc
        .trailer()
        .get(b"Info")
        .and_then(|i| doc.resolve(i).as_dict().cloned())
        .unwrap_or_default();
    // Keep a title written only in the old XMP.
    if !info_dict.contains(b"Title")
        && let Some((_, data)) = metadata(doc)
        && let Some(t) = xmp::parse(&data).info.title.filter(|t| !t.is_empty())
    {
        info_dict.set(b"Title", text(&t));
    }
    info_dict.set(b"ModDate", Object::string(now.as_bytes()));
    if !info_dict.contains(b"CreationDate") {
        info_dict.set(b"CreationDate", Object::string(now.as_bytes()));
    }
    info_dict.set(b"Producer", text("pagebara"));
    match doc.trailer().get(b"Info").and_then(Object::as_objref) {
        Some(r) => doc.set(r, info_dict),
        None => {
            let r = doc.add(info_dict);
            let mut t = doc.trailer().clone();
            t.set(b"Info", r);
            doc.set_trailer(t);
        }
    }
    let info = info_of(doc);
    let iso_now = xmp::pdf_date_to_iso(&now).unwrap_or_default();
    let packet = xmp::write(&info, pdfa, pdfua, &iso_now);
    let mut md = Dict::new();
    md.set(b"Type", Object::name(b"Metadata"));
    md.set(b"Subtype", Object::name(b"XML"));
    let m = doc.add(Stream::new(md, packet));
    if let Some(c) = doc.trailer().get(b"Root").and_then(Object::as_objref) {
        edit(doc, c, |d| {
            d.set(b"Metadata", m);
            true
        });
    }
    changes.push(tr!("Wrote XMP metadata matching the document information."));
}

/// Converts `doc` to the PDF/A part of `profile` and validates the result.
pub fn convert(doc: &Document, profile: Profile) -> Result<Conversion, ConvertError> {
    let part = profile.part().ok_or(ConvertError::NotPdfA)?;
    let mut d = doc.clone();
    let mut changes = Vec::new();
    if d.is_encrypted() {
        d.remove_security().map_err(|_| ConvertError::Protected)?;
        changes.push(tr!("Removed the password protection."));
    }
    if part == 1 {
        let t = transparency_pages(&d);
        if !t.is_empty() {
            return Err(ConvertError::Transparency(t));
        }
    }
    let cat_ref = d.trailer().get(b"Root").and_then(Object::as_objref);
    // Forms: appearances instead of NeedAppearances, no XFA.
    let cat = cat_ref
        .and_then(|c| d.get(c))
        .and_then(|c| c.as_dict().cloned())
        .unwrap_or_default();
    if let Some(form) = d.get_in(&cat, b"AcroForm").as_dict().cloned() {
        let mut f = pagebara_forms::Form::load(&d);
        if f.generate_appearances(&mut d, false).is_ok() {
            changes.push(tr!("Generated form field appearances."));
        }
        let cat = cat_ref
            .and_then(|c| d.get(c))
            .and_then(|c| c.as_dict().cloned())
            .unwrap_or_default();
        let fo = cat.get(b"AcroForm").cloned();
        let mut form = d
            .get_in(&cat, b"AcroForm")
            .as_dict()
            .cloned()
            .unwrap_or(form);
        let had = form.remove(b"NeedAppearances").is_some() | form.remove(b"XFA").is_some();
        if had {
            match fo.as_ref().and_then(Object::as_objref) {
                Some(r) => d.set(r, form),
                None => {
                    if let Some(c) = cat_ref {
                        edit(&mut d, c, |cd| {
                            cd.set(b"AcroForm", form);
                            true
                        });
                    }
                }
            }
            changes.push(tr!("Removed NeedAppearances and XFA from the form."));
        }
    }
    // Catalog: scripts, actions, attachments, optional content.
    if let Some(c) = cat_ref {
        let open_ok = d
            .get(c)
            .and_then(|o| o.as_dict().and_then(|cd| cd.get(b"OpenAction").cloned()))
            .is_none_or(|a| action_ok(&d, part, &a));
        let names_ref = d.get(c).and_then(|o| {
            o.as_dict()
                .and_then(|cd| cd.get(b"Names").and_then(Object::as_objref))
        });
        let strip_files = part < 3;
        let fix_names = |nd: &mut Dict| {
            let mut ch = nd.remove(b"JavaScript").is_some();
            if strip_files {
                ch |= nd.remove(b"EmbeddedFiles").is_some();
            }
            ch
        };
        if let Some(nr) = names_ref
            && edit(&mut d, nr, fix_names)
        {
            changes.push(tr!("Removed scripts or attached files."));
        }
        let ch = edit(&mut d, c, |cd| {
            let mut ch = cd.remove(b"AA").is_some();
            if !open_ok {
                ch |= cd.remove(b"OpenAction").is_some();
            }
            if let Some(Object::Dict(nd)) = cd.get_mut(b"Names") {
                ch |= fix_names(nd);
            }
            if part == 1 {
                ch |= cd.remove(b"OCProperties").is_some();
            }
            if part < 3 {
                ch |= cd.remove(b"AF").is_some();
            }
            ch |= cd.remove(b"Collection").is_some() && part < 3;
            if let Some(Object::Dict(p)) = cd.get_mut(b"Perms") {
                let keep: Vec<_> =
                    p.0.iter()
                        .filter(|(k, _)| matches!(k.as_bytes(), b"UR3" | b"DocMDP"))
                        .cloned()
                        .collect();
                ch |= keep.len() != p.len();
                p.0 = keep;
            }
            ch
        });
        if ch {
            changes.push(tr!(
                "Removed document actions and parts PDF/A does not allow."
            ));
        }
    }
    // Every object.
    let pages: std::collections::HashMap<u32, usize> = pagebara_pages::PageTree::load(&d)
        .refs()
        .iter()
        .enumerate()
        .map(|(i, r)| (r.num, i))
        .collect();
    let mut lzw = 0;
    let mut annots_fixed = 0;
    let mut empty_appearance = 0;
    let mut file_specs = Vec::new();
    for num in d.object_numbers() {
        let Some(r) = d.reference(num) else { continue };
        let Some(o) = d.get(r) else { continue };
        let mut o2 = (*o).clone();
        let mut changed = false;
        if let Object::Stream(s) = &mut o2 {
            if matches!(s.dict.name(b"Type"), Some(b"XRef" | b"ObjStm")) {
                continue;
            }
            for k in [&b"F"[..], b"FFilter", b"FDecodeParms"] {
                changed |= s.dict.remove(k).is_some();
            }
            let filters = match &*d.get_in(&s.dict, b"Filter") {
                Object::Name(n) => vec![n.as_bytes().to_vec()],
                Object::Array(a) => a
                    .iter()
                    .filter_map(|o| d.resolve(o).as_name().map(<[u8]>::to_vec))
                    .collect(),
                _ => Vec::new(),
            };
            if filters
                .iter()
                .any(|f| matches!(f.as_slice(), b"LZWDecode" | b"LZW" | b"Crypt"))
                && let Ok(dec) = d.decode(s)
            {
                let mut nd = s.dict.clone();
                for k in [&b"Filter"[..], b"DecodeParms", b"Length", b"DL"] {
                    nd.remove(k);
                }
                // An image codec ending the chain stays.
                if let Some(img) = &dec.image {
                    nd.set(b"Filter", Object::Name(img.name.clone()));
                    if !img.parms.is_empty() {
                        nd.set(b"DecodeParms", img.parms.clone());
                    }
                    *s = Stream::new(nd, dec.data);
                } else {
                    *s = fonts::flate_stream(nd, &dec.data);
                }
                changed = true;
                lzw += 1;
            }
        }
        let dict = match &mut o2 {
            Object::Dict(dd) => dd,
            Object::Stream(s) => &mut s.dict,
            _ => continue,
        };
        let ty = dict.name(b"Type").map(<[u8]>::to_vec);
        let sub = dict.name(b"Subtype").map(<[u8]>::to_vec);
        match sub.as_deref() {
            Some(b"Image") => {
                for k in [&b"Alternates"[..], b"OPI", b"Interpolate"] {
                    changed |= dict.remove(k).is_some();
                }
                if let Some(i) = dict.name(b"Intent")
                    && !matches!(
                        i,
                        b"RelativeColorimetric"
                            | b"AbsoluteColorimetric"
                            | b"Perceptual"
                            | b"Saturation"
                    )
                {
                    dict.remove(b"Intent");
                    changed = true;
                }
            }
            Some(b"Form") if o.as_stream().is_some() => {
                for k in [&b"OPI"[..], b"PS", b"Subtype2", b"Ref"] {
                    changed |= dict.remove(k).is_some();
                }
            }
            _ => {}
        }
        if ty.as_deref() == Some(b"ExtGState")
            || dict.contains(b"TR")
            || dict.contains(b"HTP")
            || dict.contains(b"TR2")
        {
            changed |= dict.remove(b"TR").is_some();
            changed |= dict.remove(b"HTP").is_some();
            if dict
                .get(b"TR2")
                .is_some_and(|t| d.resolve(t).as_name() != Some(b"Default"))
            {
                dict.set(b"TR2", Object::name(b"Default"));
                changed = true;
            }
            if let Some(ri) = dict.name(b"RI")
                && !matches!(
                    ri,
                    b"RelativeColorimetric"
                        | b"AbsoluteColorimetric"
                        | b"Perceptual"
                        | b"Saturation"
                )
            {
                dict.remove(b"RI");
                changed = true;
            }
        }
        if ty.as_deref() == Some(b"Page") {
            changed |= dict.remove(b"AA").is_some();
            if part > 1 {
                changed |= dict.remove(b"PresSteps").is_some();
            }
        }
        if dict.contains(b"FT") || sub.as_deref() == Some(b"Widget") {
            changed |= dict.remove(b"AA").is_some();
            if sub.as_deref() == Some(b"Widget") {
                changed |= dict.remove(b"A").is_some();
            }
        }
        // Actions in links, outlines and chains.
        for k in [&b"A"[..], b"Next"] {
            if let Some(a) = dict.get(k).cloned()
                && !action_ok(&d, part, &a)
            {
                dict.remove(k);
                changed = true;
            }
        }
        if dict.name(b"S").is_some_and(|s| forbidden_action(part, s))
            && ty.as_deref() != Some(b"StructElem")
        {
            // An action object on its own: make it do nothing harmful.
            dict.set(b"S", Object::name(b"GoTo"));
            dict.set(b"D", Vec::<Object>::new());
            dict.remove(b"JS");
            changed = true;
        }
        // Annotations.
        let is_annot = ty.as_deref() == Some(b"Annot")
            || (dict.contains(b"Rect") && sub.is_some() && dict.contains(b"P"));
        if is_annot {
            let f = d.get_in(dict, b"F").as_int().unwrap_or(0);
            let popup = sub.as_deref() == Some(b"Popup");
            let mut nf = f & !(1 | 2 | 32 | 256);
            if !popup {
                nf |= 4;
            }
            if nf != f {
                dict.set(b"F", nf);
                changed = true;
                annots_fixed += 1;
            }
            if part == 1 && dict.contains(b"CA") {
                dict.remove(b"CA");
                changed = true;
            }
            let ap = d.get_in(dict, b"AP").as_dict().cloned();
            match ap {
                Some(mut ap) if ap.len() > 1 || !ap.contains(b"N") => {
                    ap.0.retain(|(k, _)| k.as_bytes() == b"N");
                    if ap.is_empty() {
                        dict.remove(b"AP");
                    } else {
                        dict.set(b"AP", ap);
                    }
                    changed = true;
                }
                None if part > 1 && !matches!(sub.as_deref(), Some(b"Popup" | b"Link")) => {
                    let rect = pagebara_content::Rect::from_object(&d.get_in(dict, b"Rect"));
                    if let Some(rc) = rect.filter(|r| r.width() != 0.0 || r.height() != 0.0) {
                        let mut fd = Dict::new();
                        fd.set(b"Type", Object::name(b"XObject"));
                        fd.set(b"Subtype", Object::name(b"Form"));
                        fd.set(
                            b"BBox",
                            vec![
                                Object::Int(0),
                                Object::Int(0),
                                Object::Real(f64::from(rc.width())),
                                Object::Real(f64::from(rc.height())),
                            ],
                        );
                        let form = d.add(Stream::new(fd, Vec::<u8>::new()));
                        let mut ap = Dict::new();
                        ap.set(b"N", form);
                        dict.set(b"AP", ap);
                        changed = true;
                        empty_appearance += 1;
                    }
                }
                _ => {}
            }
        }
        // Attached files (part 3 keeps them, described).
        if (ty.as_deref() == Some(b"Filespec") || dict.contains(b"EF")) && part == 3 {
            if !dict.contains(b"AFRelationship") {
                dict.set(b"AFRelationship", Object::name(b"Unspecified"));
                changed = true;
            }
            file_specs.push(r);
        }
        // Optional content configurations.
        if part > 1 && dict.contains(b"OCGs") && dict.contains(b"D") {
            let fix = |c: &mut Dict, i: usize| {
                let mut ch = c.remove(b"AS").is_some();
                if !c.contains(b"Name") {
                    c.set(
                        b"Name",
                        Object::string(format!("Layers {}", i + 1).as_bytes()),
                    );
                    ch = true;
                }
                ch
            };
            if let Some(Object::Dict(c)) = dict.get_mut(b"D") {
                changed |= fix(c, 0);
            }
            if let Some(Object::Array(cs)) = dict.get_mut(b"Configs") {
                for (i, c) in cs.iter_mut().enumerate() {
                    if let Object::Dict(c) = c {
                        changed |= fix(c, i + 1);
                    }
                }
            }
        }
        if changed {
            d.set(r, o2);
        }
    }
    if lzw > 0 {
        changes.push(trn!(
            "Recompressed {n} LZW stream with Flate.",
            "Recompressed {n} LZW streams with Flate.",
            lzw
        ));
    }
    if annots_fixed > 0 {
        changes.push(trn!(
            "Set {n} annotation to print and show.",
            "Set {n} annotations to print and show.",
            annots_fixed
        ));
    }
    if empty_appearance > 0 {
        changes.push(trn!(
            "Gave {n} annotation without an appearance an empty one.",
            "Gave {n} annotations without an appearance an empty one.",
            empty_appearance
        ));
    }
    // Annotations PDF/A does not allow.
    for pnum in pages.keys() {
        let Some(pr) = d.reference(*pnum) else {
            continue;
        };
        let Some(po) = d.get(pr) else { continue };
        let Some(pd) = po.as_dict() else { continue };
        let annots = d.get_in(pd, b"Annots");
        let Some(list) = annots.as_array() else {
            continue;
        };
        let keep: Vec<Object> = list
            .iter()
            .filter(|a| {
                let ao = d.resolve(a);
                let st = ao
                    .as_dict()
                    .and_then(|x| x.name(b"Subtype"))
                    .unwrap_or_default();
                let bad = matches!(st, b"Sound" | b"Movie" | b"Screen" | b"3D" | b"RichMedia")
                    || (part < 3 && st == b"FileAttachment");
                !bad
            })
            .cloned()
            .collect();
        if keep.len() != list.len() {
            let mut pd = pd.clone();
            pd.set(b"Annots", keep);
            d.set(pr, pd);
            changes.push(tr!("Removed annotations PDF/A does not allow."));
        }
    }
    if part == 3 && !file_specs.is_empty() {
        for &fs in &file_specs {
            let ef = d
                .get(fs)
                .and_then(|o| o.as_dict().and_then(|x| x.get(b"EF").cloned()));
            if let Some(ef) = ef.and_then(|e| d.resolve(&e).as_dict().cloned()) {
                for (_, v) in ef.iter() {
                    if let Some(er) = v.as_objref() {
                        edit(&mut d, er, |sd| {
                            if sd.contains(b"Subtype") {
                                return false;
                            }
                            sd.set(b"Subtype", Object::name(b"application/octet-stream"));
                            true
                        });
                    }
                }
            }
        }
        if let Some(c) = cat_ref {
            edit(&mut d, c, |cd| {
                let mut af: Vec<Object> = cd
                    .get(b"AF")
                    .and_then(Object::as_array)
                    .map(<[Object]>::to_vec)
                    .unwrap_or_default();
                for &fs in &file_specs {
                    if !af.contains(&Object::Ref(fs)) {
                        af.push(Object::Ref(fs));
                    }
                }
                cd.set(b"AF", af);
                true
            });
        }
    }
    // Fonts, each an object of its own so it can be replaced.
    indirect_fonts(&mut d);
    let facts = Scanner::run(&d);
    let mut embedded = Vec::new();
    for f in &facts.fonts {
        let sub = f.dict.name(b"Subtype").unwrap_or_default();
        if sub == b"Type3" {
            continue;
        }
        let descendant = (sub == b"Type0")
            .then(|| {
                d.get_in(&f.dict, b"DescendantFonts")
                    .as_array()
                    .and_then(|a| a.first().cloned())
            })
            .flatten();
        let desc_dict = descendant
            .as_ref()
            .and_then(|x| d.resolve(x).as_dict().cloned());
        let has_program =
            pagebara_font::pdf::font_file_ref(&d, desc_dict.as_ref().unwrap_or(&f.dict)).is_some();
        let name = String::from_utf8_lossy(f.dict.name(b"BaseFont").unwrap_or(b"?")).into_owned();
        if !has_program && (part == 1 || f.visible) {
            match (f.r, descendant.is_none()) {
                (Some(r), true) => match fonts::embed_simple(&mut d, f) {
                    Ok((nd, used)) => {
                        d.set(r, nd);
                        embedded.push(tr!("{} (as {})", name, used));
                    }
                    Err(e) => changes.push(tr!("Could not embed {}.", e)),
                },
                _ => changes.push(tr!(
                    "Could not embed {}: composite fonts need the original font.",
                    name
                )),
            }
            continue;
        }
        if let Some(r) = f.r {
            let two = matches!(
                d.get_in(&f.dict, b"Encoding").as_name(),
                Some(b"Identity-H" | b"Identity-V")
            );
            if profile.unicode() && (sub != b"Type0" || two) {
                let mut s = String::new();
                let missing = f.codes.iter().any(|&c| {
                    s.clear();
                    !f.font.unicode(&d, c, &mut s) || !crate::real_unicode(&s)
                });
                let lacks_map = !f.dict.contains(b"ToUnicode");
                if (missing || lacks_map)
                    && let Some(cmap) = fonts::to_unicode(&d, f, two)
                {
                    let tu = d.add(fonts::flate_stream(Dict::new(), &cmap));
                    edit(&mut d, r, |fd| {
                        fd.set(b"ToUnicode", tu);
                        true
                    });
                }
            }
        }
        // Character and CID sets are optional from part 2 on, and wrong ones fail.
        let fd_ref = desc_dict
            .as_ref()
            .unwrap_or(&f.dict)
            .get(b"FontDescriptor")
            .and_then(Object::as_objref);
        if let Some(fdr) = fd_ref {
            if part > 1 {
                edit(&mut d, fdr, |fd| {
                    fd.remove(b"CIDSet").is_some() | fd.remove(b"CharSet").is_some()
                });
            } else if let Some(desc) = &desc_dict
                && desc.name(b"Subtype") == Some(b"CIDFontType2")
                && d.get_in(desc, b"CIDToGIDMap").as_name() == Some(b"Identity")
                && let Some(p) = pagebara_font::pdf::font_file_ref(&d, desc)
                && let Some(prog) = d
                    .get(p)
                    .and_then(|o| o.as_stream().and_then(|s| d.decode(s).ok()))
                && let Some(pagebara_font::Font::Sfnt(sf)) = pagebara_font::Font::parse(prog.data)
            {
                let set = d.add(fonts::flate_stream(
                    Dict::new(),
                    &fonts::cid_set(sf.num_glyphs()),
                ));
                edit(&mut d, fdr, |fd| {
                    fd.set(b"CIDSet", set);
                    true
                });
            }
        }
    }
    if !embedded.is_empty() {
        changes.push(tr!("Embedded fonts: {}.", embedded.join(", ")));
    }
    // Colour: an output intent, and default spaces for device colour it does not cover.
    let cat = cat_ref
        .and_then(|c| d.get(c))
        .and_then(|c| c.as_dict().cloned())
        .unwrap_or_default();
    let mut oi_n = None;
    for oi in d
        .get_in(&cat, b"OutputIntents")
        .as_array()
        .unwrap_or_default()
    {
        let oi = d.resolve(oi);
        let Some(oi) = oi.as_dict() else { continue };
        if oi.name(b"S") == Some(b"GTS_PDFA1")
            && let Some(p) = oi.get(b"DestOutputProfile")
            && let Some(h) = d
                .resolve(p)
                .as_stream()
                .and_then(|s| d.decode(s).ok())
                .and_then(|x| icc::header(&x.data))
            && h.well_formed
            && (part > 1 || h.major < 4)
        {
            oi_n = h.components();
        }
    }
    let mut profile_refs: std::collections::HashMap<u8, ObjRef> = std::collections::HashMap::new();
    let mut add_profile = |d: &mut Document, n: u8| -> ObjRef {
        *profile_refs.entry(n).or_insert_with(|| {
            let data = if n == 4 { icc::cmyk() } else { icc::srgb() };
            let mut sd = Dict::new();
            sd.set(b"N", i64::from(n));
            if n == 3 {
                sd.set(b"Alternate", Object::name(b"DeviceRGB"));
            } else {
                sd.set(b"Alternate", Object::name(b"DeviceCMYK"));
            }
            d.add(fonts::flate_stream(sd, &data))
        })
    };
    if oi_n.is_none() {
        let pr = add_profile(&mut d, 3);
        let mut oi = Dict::new();
        oi.set(b"Type", Object::name(b"OutputIntent"));
        oi.set(b"S", Object::name(b"GTS_PDFA1"));
        oi.set(
            b"OutputConditionIdentifier",
            Object::string(b"sRGB IEC61966-2.1"),
        );
        oi.set(b"Info", Object::string(b"sRGB IEC61966-2.1"));
        oi.set(b"DestOutputProfile", pr);
        let oi = d.add(oi);
        if let Some(c) = cat_ref {
            edit(&mut d, c, |cd| {
                cd.set(b"OutputIntents", vec![Object::Ref(oi)]);
                true
            });
        }
        oi_n = Some(3);
        changes.push(tr!("Added an sRGB output intent."));
    }
    let facts = Scanner::run(&d);
    let mut defaults: Vec<(&'static [u8], ObjRef)> = Vec::new();
    for dev in [Device::Rgb, Device::Cmyk] {
        let n = if dev == Device::Rgb { 3 } else { 4 };
        if facts.device.contains_key(&dev) && oi_n != Some(n) {
            let pr = add_profile(&mut d, n);
            let cs = d.add(vec![Object::name(b"ICCBased"), Object::Ref(pr)]);
            defaults.push((dev.default_key(), cs));
        }
    }
    if !defaults.is_empty() {
        add_default_spaces(&mut d, &defaults);
        changes.push(tr!("Gave device colour a calibrated default colour space."));
    }
    // Metadata, then the file.
    sync_metadata(
        &mut d,
        Some((part, profile.conformance())),
        false,
        &mut changes,
    );
    let bytes = pagebara_optimize::write(
        &d,
        &pagebara_optimize::WriteOptions {
            object_streams: part > 1,
            level: 9,
            linearize: false,
            min_version: (1, 4),
        },
    );
    let out = Document::open(bytes.clone()).map_err(|_| ConvertError::NotPdfA)?;
    let report = super::validate(&out, profile);
    Ok(Conversion {
        bytes,
        changes,
        report,
    })
}

/// Embeds substitutes for the simple fonts that visible text uses and that are not
/// embedded. Returns a line per font embedded or not embeddable.
pub(crate) fn embed_missing_fonts(doc: &mut Document) -> Vec<String> {
    indirect_fonts(doc);
    let facts = Scanner::run(doc);
    let mut out = Vec::new();
    for f in &facts.fonts {
        let sub = f.dict.name(b"Subtype").unwrap_or_default();
        if sub == b"Type3"
            || sub == b"Type0"
            || !f.visible
            || pagebara_font::pdf::font_file_ref(doc, &f.dict).is_some()
        {
            continue;
        }
        let name = String::from_utf8_lossy(f.dict.name(b"BaseFont").unwrap_or(b"?")).into_owned();
        let Some(r) = f.r else { continue };
        match fonts::embed_simple(doc, f) {
            Ok((nd, used)) => {
                doc.set(r, nd);
                out.push(tr!("Embedded font {} (as {}).", name, used));
            }
            Err(e) => out.push(tr!("Could not embed {}.", e)),
        }
    }
    out
}

/// Moves font dictionaries written directly in resources into objects of their own.
fn indirect_fonts(doc: &mut Document) {
    fn fix_fonts(doc: &mut Document, fonts: &mut Dict) -> bool {
        let mut changed = false;
        for (_, v) in fonts.0.iter_mut() {
            if let Object::Dict(fd) = v {
                let r = doc.add(fd.clone());
                *v = Object::Ref(r);
                changed = true;
            }
        }
        changed
    }
    fn fix_res(doc: &mut Document, res: &mut Dict) -> bool {
        if let Some(Object::Ref(r)) = res.get(b"Font") {
            let r = *r;
            if let Some(mut fonts) = doc.get(r).and_then(|o| o.as_dict().cloned())
                && fix_fonts(doc, &mut fonts)
            {
                doc.set(r, fonts);
            }
            return false;
        }
        let Some(Object::Dict(fonts)) = res.get_mut(b"Font") else {
            return false;
        };
        let mut fonts = fonts.clone();
        let mut changed = false;
        for (_, v) in fonts.0.iter_mut() {
            if let Object::Dict(fd) = v {
                let r = doc.add(fd.clone());
                *v = Object::Ref(r);
                changed = true;
            }
        }
        if changed {
            res.set(b"Font", fonts);
        }
        changed
    }
    for num in doc.object_numbers() {
        let Some(r) = doc.reference(num) else {
            continue;
        };
        let Some(o) = doc.get(r) else { continue };
        let mut o = (*o).clone();
        let d = match &mut o {
            Object::Dict(d) => d,
            Object::Stream(s) => &mut s.dict,
            _ => continue,
        };
        let mut changed = false;
        if d.contains(b"Font") && !d.contains(b"Type") {
            // A resource dictionary of its own.
            changed |= fix_res(doc, d);
        }
        if let Some(Object::Dict(res)) = d.get_mut(b"Resources") {
            let mut res = res.clone();
            if fix_res(doc, &mut res) {
                d.set(b"Resources", res);
                changed = true;
            }
        }
        if let Some(Object::Dict(dr)) = d.get_mut(b"DR") {
            let mut dr = dr.clone();
            if fix_res(doc, &mut dr) {
                d.set(b"DR", dr);
                changed = true;
            }
        }
        if changed {
            doc.set(r, o);
        }
    }
}

/// Adds `defaults` (`DefaultRGB`, `DefaultCMYK`) to every resource dictionary content uses.
fn add_default_spaces(doc: &mut Document, defaults: &[(&'static [u8], ObjRef)]) {
    let set = |res: &mut Dict, doc: &Document| {
        let mut cs = doc
            .get_in(res, b"ColorSpace")
            .as_dict()
            .cloned()
            .unwrap_or_default();
        for (k, v) in defaults {
            cs.set(k, *v);
        }
        res.set(b"ColorSpace", cs);
    };
    // Pages get resources of their own so inherited ones need no care.
    for page in pagebara_pages::PageTree::load(doc).refs() {
        let Some(po) = doc.get(page) else { continue };
        let Some(pd) = po.as_dict() else { continue };
        let mut pd = pd.clone();
        let mut res = crate::content::inherited_resources(doc, page);
        set(&mut res, doc);
        pd.set(b"Resources", res);
        doc.set(page, pd);
    }
    for num in doc.object_numbers() {
        let Some(r) = doc.reference(num) else {
            continue;
        };
        let Some(o) = doc.get(r) else { continue };
        let mut o = (*o).clone();
        let d = match &mut o {
            Object::Stream(s)
                if s.dict.name(b"Subtype") == Some(b"Form") || s.dict.contains(b"PatternType") =>
            {
                &mut s.dict
            }
            Object::Dict(dd) if dd.name(b"Subtype") == Some(b"Type3") => dd,
            _ => continue,
        };
        // Streams without resources use their parent's, which get the spaces too.
        let Some(mut res) = doc.get_in(d, b"Resources").as_dict().cloned() else {
            continue;
        };
        set(&mut res, doc);
        d.set(b"Resources", res);
        doc.set(r, o);
    }
}
