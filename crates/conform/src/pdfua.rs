//! PDF/UA-1: the machine-checkable failure conditions of the Matterhorn Protocol (ids
//! `checkpoint-condition`), safe automatic fix-ups, and the edits behind the guided fixes
//! (alternative text, heading levels, table header cells).

use std::collections::{HashMap, HashSet};

use pagebara_content::{ContentParser, Op};
use pagebara_i18n::{tr, trn};
use pagebara_syntax::{Dict, Document, ObjRef, Object, PdfString, Stream};

use crate::content::Scanner;
use crate::{Fix, Issues, Profile, Report, xmp};

const STANDARD: [&[u8]; 47] = [
    b"Document",
    b"Part",
    b"Art",
    b"Sect",
    b"Div",
    b"BlockQuote",
    b"Caption",
    b"TOC",
    b"TOCI",
    b"Index",
    b"NonStruct",
    b"Private",
    b"P",
    b"H",
    b"H1",
    b"H2",
    b"H3",
    b"H4",
    b"H5",
    b"H6",
    b"L",
    b"LI",
    b"Lbl",
    b"LBody",
    b"Table",
    b"TR",
    b"TH",
    b"TD",
    b"THead",
    b"TBody",
    b"TFoot",
    b"Span",
    b"Quote",
    b"Note",
    b"Reference",
    b"BibEntry",
    b"Code",
    b"Link",
    b"Annot",
    b"Ruby",
    b"Warichu",
    b"RB",
    b"RT",
    b"RP",
    b"WT",
    b"WP",
    b"Figure",
];
const MORE_STANDARD: [&[u8]; 2] = [b"Formula", b"Form"];

fn is_standard(t: &[u8]) -> bool {
    STANDARD.contains(&t) || MORE_STANDARD.contains(&t)
}

/// A structure element, for the guided fixes.
#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    pub obj: ObjRef,
    /// `/S` as written.
    pub tag: String,
    /// The standard type it maps to.
    pub role: String,
    pub page: Option<usize>,
    pub alt: Option<String>,
    /// Nesting depth below the structure tree root.
    pub depth: usize,
}

/// The structure elements in document order.
pub fn elements(doc: &Document) -> Vec<Element> {
    let mut out = Vec::new();
    let Some(cat) = doc.catalog() else { return out };
    let Some(cat) = cat.as_dict() else { return out };
    let root = doc.get_in(cat, b"StructTreeRoot");
    let Some(root) = root.as_dict() else {
        return out;
    };
    let role_map = doc
        .get_in(root, b"RoleMap")
        .as_dict()
        .cloned()
        .unwrap_or_default();
    let pages: HashMap<u32, usize> = pagebara_pages::PageTree::load(doc)
        .refs()
        .iter()
        .enumerate()
        .map(|(i, r)| (r.num, i))
        .collect();
    let mut seen = HashSet::new();
    let mut stack: Vec<(Object, usize, Option<usize>)> = Vec::new();
    if let Some(k) = root.get(b"K") {
        push_kids(doc, k, 0, None, &mut stack);
    }
    while let Some((o, depth, page)) = stack.pop() {
        let Some(r) = o.as_objref() else { continue };
        if !seen.insert(r.num) || out.len() > 1 << 20 {
            continue;
        }
        let Some(eo) = doc.get(r) else { continue };
        let Some(d) = eo.as_dict() else { continue };
        let Some(tag) = d.name(b"S") else { continue };
        let page = d
            .get(b"Pg")
            .and_then(Object::as_objref)
            .and_then(|p| pages.get(&p.num).copied())
            .or(page);
        let alt = doc.get_in(d, b"Alt");
        let alt = alt
            .as_string()
            .map(|_| pagebara_content::extract::text_string(&alt))
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                let at = doc.get_in(d, b"ActualText");
                at.as_string()
                    .map(|_| pagebara_content::extract::text_string(&at))
                    .filter(|s| !s.trim().is_empty())
            });
        out.push(Element {
            obj: r,
            tag: String::from_utf8_lossy(tag).into_owned(),
            role: String::from_utf8_lossy(&map_role(doc, &role_map, tag)).into_owned(),
            page,
            alt,
            depth,
        });
        if let Some(k) = d.get(b"K") {
            push_kids(doc, k, depth + 1, page, &mut stack);
        }
    }
    out
}

fn push_kids(
    doc: &Document,
    k: &Object,
    depth: usize,
    page: Option<usize>,
    stack: &mut Vec<(Object, usize, Option<usize>)>,
) {
    let kids: Vec<Object> = match &*doc.resolve(k) {
        Object::Array(a) => a.clone(),
        _ => vec![k.clone()],
    };
    for kid in kids.into_iter().rev() {
        if kid.as_objref().is_some() {
            stack.push((kid, depth, page));
        }
    }
}

/// Whether every structure element owning marked content has a language of its own or
/// from an ancestor (and there is a structure tree at all).
fn structure_has_languages(doc: &Document, cat: &Dict) -> bool {
    let root = doc.get_in(cat, b"StructTreeRoot");
    let Some(root) = root.as_dict() else {
        return false;
    };
    let has_lang = |d: &Dict| {
        let l = doc.get_in(d, b"Lang");
        l.as_string()
            .is_some_and(|_| !pagebara_content::extract::text_string(&l).trim().is_empty())
    };
    let mut seen = HashSet::new();
    let mut stack: Vec<(Object, bool)> = vec![(root.get(b"K").cloned().unwrap_or_default(), false)];
    let mut content = false;
    while let Some((k, inherited)) = stack.pop() {
        let o = doc.resolve(&k);
        match &*o {
            Object::Array(a) => stack.extend(a.iter().map(|k| (k.clone(), inherited))),
            Object::Int(_) => {
                content = true;
                if !inherited {
                    return false;
                }
            }
            Object::Dict(d) => {
                if let Some(r) = k.as_objref()
                    && (!seen.insert(r.num) || seen.len() > 1 << 20)
                {
                    continue;
                }
                match d.name(b"Type") {
                    Some(b"MCR") => {
                        content = true;
                        if !inherited {
                            return false;
                        }
                    }
                    Some(b"OBJR") => {}
                    _ => {
                        if let Some(kids) = d.get(b"K") {
                            stack.push((kids.clone(), inherited || has_lang(d)));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    content
}

/// The `/StructParent` keys whose structure element has a non-empty `/Alt`, from the
/// parent tree (a number tree).
fn described_parents(doc: &Document, root: &Dict) -> HashSet<i64> {
    let mut out = HashSet::new();
    let mut stack = vec![(doc.get_in(root, b"ParentTree").as_dict().cloned(), 0)];
    while let Some((Some(node), depth)) = stack.pop() {
        if depth > 32 {
            continue;
        }
        if let Some(nums) = doc.get_in(&node, b"Nums").as_array() {
            for pair in nums.as_chunks::<2>().0 {
                let (Some(k), elem) = (pair[0].as_int(), doc.resolve(&pair[1])) else {
                    continue;
                };
                let alt = elem.as_dict().map(|e| doc.get_in(e, b"Alt"));
                if alt.is_some_and(|a| {
                    a.as_string().is_some_and(|_| {
                        !pagebara_content::extract::text_string(&a).trim().is_empty()
                    })
                }) {
                    out.insert(k);
                }
            }
        }
        for kid in doc.get_in(&node, b"Kids").as_array().unwrap_or_default() {
            stack.push((doc.resolve(kid).as_dict().cloned(), depth + 1));
        }
    }
    out
}

/// The standard type a type maps to, or the type itself when unmapped.
fn map_role(doc: &Document, role_map: &Dict, tag: &[u8]) -> Vec<u8> {
    let mut t = tag.to_vec();
    for _ in 0..16 {
        if is_standard(&t) {
            return t;
        }
        match doc.get_in(role_map, &t).as_name() {
            Some(n) if n != t.as_slice() => t = n.to_vec(),
            _ => break,
        }
    }
    t
}

/// Checks `doc` against PDF/UA-1.
pub fn check(doc: &Document) -> Report {
    let mut is = Issues::new("ISO 14289-1 (Matterhorn Protocol 1.1)");
    let cat = doc
        .catalog()
        .and_then(|c| c.as_dict().cloned())
        .unwrap_or_default();
    let root = doc.get_in(&cat, b"StructTreeRoot").as_dict().cloned();
    let mark = doc
        .get_in(&cat, b"MarkInfo")
        .as_dict()
        .cloned()
        .unwrap_or_default();
    is.check(
        "01-001",
        doc.get_in(&mark, b"Marked").as_bool() != Some(true) && root.is_some(),
        None,
        None,
        || tr!("the document is not marked as tagged (MarkInfo /Marked)"),
    );
    is.check("01-002", root.is_none(), None, None, || {
        tr!("the document has no structure tree (it is not tagged)")
    });
    is.check(
        "01-007",
        doc.get_in(&mark, b"Suspects").as_bool() == Some(true),
        None,
        None,
        || tr!("the tags are marked as suspect"),
    );
    let facts = Scanner::run(doc);
    if root.is_some() {
        for &p in &facts.untagged {
            is.check("01-005", true, Some(p), None, || {
                tr!("content is neither tagged nor marked as an artifact")
            });
        }
    }
    // Role mapping.
    if let Some(root) = &root {
        let rm = doc
            .get_in(root, b"RoleMap")
            .as_dict()
            .cloned()
            .unwrap_or_default();
        for (k, v) in rm.iter() {
            let std_remapped = is_standard(k.as_bytes());
            is.check("02-004", std_remapped, None, None, || {
                tr!(
                    "standard type {} is mapped to another type",
                    String::from_utf8_lossy(k.as_bytes())
                )
            });
            let end = map_role(doc, &rm, k.as_bytes());
            let circular = !is_standard(&end) && doc.get_in(&rm, &end).as_name().is_some();
            is.check("02-003", circular, None, None, || {
                tr!(
                    "the role map of {} is circular",
                    String::from_utf8_lossy(k.as_bytes())
                )
            });
            let _ = v;
        }
    }
    let els = elements(doc);
    let mut heading_levels = Vec::new();
    let mut tables: HashMap<usize, bool> = HashMap::new();
    let mut table_stack: Vec<(usize, usize)> = Vec::new();
    for (i, e) in els.iter().enumerate() {
        let role = e.role.as_bytes();
        is.check("02-001", !is_standard(role), e.page, Some(e.obj), || {
            tr!(
                "structure type {} is not standard and not role mapped",
                e.tag
            )
        });
        if role == b"Figure" {
            is.check("13-004", e.alt.is_none(), e.page, Some(e.obj), || {
                tr!("a figure has no alternative text")
            });
        }
        if role == b"Formula" {
            is.check("17-002", e.alt.is_none(), e.page, Some(e.obj), || {
                tr!("a formula has no alternative text")
            });
        }
        if let Some(l) = role
            .strip_prefix(b"H")
            .and_then(|n| std::str::from_utf8(n).ok()?.parse::<u8>().ok())
        {
            heading_levels.push((l, e.page, e.obj));
        }
        while table_stack.last().is_some_and(|t| t.1 >= e.depth) {
            table_stack.pop();
        }
        if role == b"Table" {
            tables.insert(i, false);
            table_stack.push((i, e.depth));
        }
        if role == b"TH"
            && let Some(t) = table_stack.last()
        {
            tables.insert(t.0, true);
        }
    }
    let mut prev = 0u8;
    for (i, &(l, page, obj)) in heading_levels.iter().enumerate() {
        if i == 0 {
            is.check("14-002", l != 1, page, Some(obj), || {
                tr!("the first heading is H{}, not H1", l)
            });
        } else {
            is.check("14-003", l > prev + 1, page, Some(obj), || {
                tr!("heading level skips from H{} to H{}", prev, l)
            });
        }
        prev = l;
    }
    for (&t, &has_th) in &tables {
        let e = &els[t];
        is.check("15-003", !has_th, e.page, Some(e.obj), || {
            tr!("a table has no header cells (TH)")
        });
    }
    // Metadata and the document.
    let meta = crate::pdfa::metadata_of(doc);
    is.check("06-001", meta.is_none(), None, None, || {
        tr!("the catalog has no XMP metadata")
    });
    let x = meta.as_ref().map(|m| xmp::parse(m));
    is.check(
        "06-002",
        meta.is_some() && x.as_ref().and_then(|x| x.pdfua_part.as_deref()) != Some("1"),
        None,
        None,
        || tr!("the metadata does not identify PDF/UA-1 (pdfuaid:part)"),
    );
    is.check(
        "06-003",
        meta.is_some()
            && x.as_ref()
                .and_then(|x| x.info.title.as_deref())
                .is_none_or(str::is_empty),
        None,
        None,
        || tr!("the metadata has no title (dc:title)"),
    );
    let vp = doc
        .get_in(&cat, b"ViewerPreferences")
        .as_dict()
        .cloned()
        .unwrap_or_default();
    is.check(
        "07-001",
        doc.get_in(&vp, b"DisplayDocTitle").as_bool() != Some(true),
        None,
        None,
        || tr!("the viewer is not told to show the title (DisplayDocTitle)"),
    );
    let lang = doc.get_in(&cat, b"Lang");
    let lang = lang
        .as_string()
        .map(|_| pagebara_content::extract::text_string(&lang));
    // Without a document language, every tagged content item needs one from its
    // structure elements.
    is.check(
        "11-006",
        lang.as_deref().is_none_or(|l| l.trim().is_empty()) && !structure_has_languages(doc, &cat),
        None,
        None,
        || tr!("the document has no natural language (Lang)"),
    );
    if let Some(l) = &lang {
        let valid = !l.is_empty()
            && l.split('-').all(|p| {
                !p.is_empty() && p.len() <= 8 && p.chars().all(|c| c.is_ascii_alphanumeric())
            })
            && l.split('-')
                .next()
                .is_some_and(|p| p.chars().all(|c| c.is_ascii_alphabetic()));
        is.check("11-005", !valid, None, None, || {
            tr!(
                "the language {} is not a valid language tag",
                format!("{l:?}")
            )
        });
    }
    let form = doc.get_in(&cat, b"AcroForm");
    is.check(
        "25-001",
        form.as_dict().is_some_and(|f| {
            f.contains(b"XFA") && doc.get_in(&cat, b"NeedsRendering").as_bool() == Some(true)
        }),
        None,
        None,
        || tr!("dynamic XFA forms are not allowed"),
    );
    if doc.is_encrypted() {
        let p = doc.permissions();
        is.check("26-002", !p.extract_for_accessibility, None, None, || {
            tr!("the security settings block assistive technology")
        });
    }
    if let Some(oc) = doc.get_in(&cat, b"OCProperties").as_dict() {
        let mut configs = vec![(*doc.get_in(oc, b"D")).clone()];
        configs.extend(
            doc.get_in(oc, b"Configs")
                .as_array()
                .unwrap_or_default()
                .iter()
                .map(|c| (*doc.resolve(c)).clone()),
        );
        for c in configs {
            if let Some(c) = c.as_dict() {
                is.check(
                    "20-001",
                    doc.get_in(c, b"Name")
                        .as_string()
                        .is_none_or(<[u8]>::is_empty),
                    None,
                    None,
                    || tr!("an optional content configuration has no name"),
                );
                is.check("20-003", c.contains(b"AS"), None, None, || {
                    tr!("an optional content configuration has an AS entry")
                });
            }
        }
    }
    // Pages and annotations. The structure element of an annotation may give the
    // alternative description instead of the annotation itself.
    let described = root
        .as_ref()
        .map(|r| described_parents(doc, r))
        .unwrap_or_default();
    let has_alt = |ad: &Dict| {
        doc.get_in(ad, b"StructParent")
            .as_int()
            .is_some_and(|k| described.contains(&k))
    };
    for (i, p) in pagebara_pages::PageTree::load(doc)
        .pages()
        .iter()
        .enumerate()
    {
        let Some(po) = doc.get(p.r) else { continue };
        let Some(pd) = po.as_dict() else { continue };
        let annots = doc.get_in(pd, b"Annots");
        let list = annots.as_array().unwrap_or_default();
        let visible: Vec<(ObjRef, Dict)> = list
            .iter()
            .filter_map(|a| Some((a.as_objref()?, doc.resolve(a).as_dict()?.clone())))
            .filter(|(_, d)| {
                d.name(b"Subtype") != Some(b"Popup")
                    && doc.get_in(d, b"F").as_int().unwrap_or(0) & 2 == 0
            })
            .collect();
        if !visible.is_empty() {
            is.check("28-009", pd.name(b"Tabs") != Some(b"S"), Some(i), Some(p.r), || tr!("the tab order of a page with annotations does not follow the structure (Tabs /S)"));
        }
        for (ar, ad) in &visible {
            let st = ad.name(b"Subtype").unwrap_or_default();
            if matches!(st, b"PrinterMark" | b"TrapNet") {
                is.check("28-001", true, Some(i), Some(*ar), || {
                    tr!("printer's marks and trap networks are not allowed")
                });
                continue;
            }
            if root.is_some() {
                is.check(
                    "28-002",
                    !ad.contains(b"StructParent"),
                    Some(i),
                    Some(*ar),
                    || tr!("a {} annotation is not tagged", String::from_utf8_lossy(st)),
                );
            }
            match st {
                b"Widget" => {
                    let tu = field_has_name(doc, ad) || has_alt(ad);
                    is.check("28-005", !tu, Some(i), Some(*ar), || {
                        tr!("a form field has no accessible name (TU)")
                    });
                }
                b"Link" => {}
                _ => {
                    let c = doc.get_in(ad, b"Contents");
                    is.check(
                        "28-004",
                        c.as_string().is_none_or(<[u8]>::is_empty) && !has_alt(ad),
                        Some(i),
                        Some(*ar),
                        || {
                            tr!(
                                "a {} annotation has no alternative description (Contents)",
                                String::from_utf8_lossy(st)
                            )
                        },
                    );
                }
            }
        }
    }
    for num in doc.object_numbers() {
        let Some(r) = doc.reference(num) else {
            continue;
        };
        let Some(o) = doc.get(r) else { continue };
        if let Some(s) = o.as_stream()
            && s.dict.name(b"Subtype") == Some(b"Form")
        {
            is.check("30-001", s.dict.contains(b"Ref"), None, Some(r), || {
                tr!("reference XObjects are not allowed")
            });
        }
    }
    // Fonts.
    for f in &facts.fonts {
        let sub = f.dict.name(b"Subtype").unwrap_or_default();
        if sub == b"Type3" {
            continue;
        }
        let name = String::from_utf8_lossy(f.dict.name(b"BaseFont").unwrap_or(b"?")).into_owned();
        let owner = if sub == b"Type0" {
            doc.get_in(&f.dict, b"DescendantFonts")
                .as_array()
                .and_then(|a| a.first().and_then(|d| doc.resolve(d).as_dict().cloned()))
                .unwrap_or_default()
        } else {
            f.dict.clone()
        };
        let embedded = pagebara_font::pdf::font_file_ref(doc, &owner).is_some();
        is.check("31-001", f.visible && !embedded, f.page, f.r, || {
            tr!("font {} is not embedded", name)
        });
        let mut s = String::new();
        let unmapped = f.codes.iter().any(|&c| {
            s.clear();
            !f.font.unicode(doc, c, &mut s) || !crate::real_unicode(&s)
        });
        is.check("31-027", unmapped, f.page, f.r, || {
            tr!("text in {} has characters without a Unicode mapping", name)
        });
    }
    let mut report = is.finish(Profile::PdfUa1);
    for i in &mut report.issues {
        i.fix = match i.rule.as_str() {
            "01-001" | "06-001" | "06-002" | "06-003" | "07-001" | "28-009" | "28-002"
            | "01-005" => Some(Fix::Automatic),
            "11-006" | "31-001" => Some(Fix::Automatic),
            "13-004" | "17-002" | "14-002" | "14-003" | "15-003" | "28-004" | "28-005" => {
                Some(Fix::Guided)
            }
            _ => None,
        };
    }
    report
}

fn field_has_name(doc: &Document, w: &Dict) -> bool {
    let mut cur = w.clone();
    for _ in 0..32 {
        let tu = doc.get_in(&cur, b"TU");
        if tu.as_string().is_some_and(|s| !s.is_empty()) {
            return true;
        }
        match doc.get_in(&cur, b"Parent").as_dict() {
            Some(p) => cur = p.clone(),
            None => return false,
        }
    }
    false
}

/// Choices for [`fix_safe`].
#[derive(Clone, Debug, Default)]
pub struct FixOptions {
    /// The language to set when the document has none (`en-US`).
    pub lang: Option<String>,
    /// The title to set when the document has none (the file name, say).
    pub title: Option<String>,
}

fn text_string(s: &str) -> Object {
    if s.is_ascii() {
        return Object::String(PdfString {
            bytes: s.as_bytes().to_vec(),
            hex: false,
        });
    }
    let mut b = vec![0xfe, 0xff];
    for u in s.encode_utf16() {
        b.extend_from_slice(&u.to_be_bytes());
    }
    Object::String(PdfString {
        bytes: b,
        hex: true,
    })
}

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

/// The fixes that need no judgement: tagged flag, language, title shown in the title bar,
/// PDF/UA identification, tab order, annotations in the structure tree, untagged paths
/// marked as artifacts (decoration), and installed substitutes embedded for fonts that are
/// not. Returns what was done.
pub fn fix_safe(doc: &mut Document, opts: &FixOptions) -> Vec<String> {
    let mut out = Vec::new();
    let Some(cat_ref) = doc.trailer().get(b"Root").and_then(Object::as_objref) else {
        return out;
    };
    let cat = doc
        .get(cat_ref)
        .and_then(|c| c.as_dict().cloned())
        .unwrap_or_default();
    let tagged = doc.get_in(&cat, b"StructTreeRoot").as_dict().is_some();
    let lang_missing = doc
        .get_in(&cat, b"Lang")
        .as_string()
        .is_none_or(<[u8]>::is_empty);
    edit(doc, cat_ref, |c| {
        if tagged {
            let mut mi = match c.get(b"MarkInfo") {
                Some(Object::Dict(d)) => d.clone(),
                _ => Dict::new(),
            };
            if mi.get(b"Marked").and_then(Object::as_bool) != Some(true) {
                mi.set(b"Marked", true);
                c.set(b"MarkInfo", mi);
                out.push(tr!("Marked the document as tagged."));
            }
        }
        if lang_missing && let Some(l) = &opts.lang {
            c.set(b"Lang", Object::string(l.as_bytes()));
            out.push(tr!("Set the document language to {}.", l));
        }
        let mut vp = match c.get(b"ViewerPreferences") {
            Some(Object::Dict(d)) => d.clone(),
            _ => Dict::new(),
        };
        if vp.get(b"DisplayDocTitle").and_then(Object::as_bool) != Some(true) {
            vp.set(b"DisplayDocTitle", true);
            c.set(b"ViewerPreferences", vp);
            out.push(tr!("Show the document title instead of the file name."));
        }
        true
    });
    // An indirect ViewerPreferences or MarkInfo dictionary is edited where it is.
    for (k, key, v) in [
        (&b"ViewerPreferences"[..], &b"DisplayDocTitle"[..], true),
        (b"MarkInfo", b"Marked", tagged),
    ] {
        if let Some(r) = cat.get(k).and_then(Object::as_objref)
            && v
        {
            edit(doc, r, |d| {
                d.set(key, true);
                true
            });
        }
    }
    // Title.
    let info = crate::pdfa::info_of_doc(doc);
    if info.title.is_none() {
        let title = opts.title.clone().filter(|t| !t.trim().is_empty());
        if let Some(t) = title {
            let info_ref = doc.trailer().get(b"Info").and_then(Object::as_objref);
            let mut d = info_ref
                .and_then(|r| doc.get(r))
                .and_then(|o| o.as_dict().cloned())
                .unwrap_or_default();
            d.set(b"Title", text_string(&t));
            match info_ref {
                Some(r) => doc.set(r, d),
                None => {
                    let r = doc.add(d);
                    let mut tr = doc.trailer().clone();
                    tr.set(b"Info", r);
                    doc.set_trailer(tr);
                }
            }
            out.push(tr!("Set the title to “{}”.", t));
        }
    }
    if tagged {
        let n = tag_annotations(doc);
        if n > 0 {
            out.push(trn!(
                "Added {n} annotation to the structure tree, with structure tab order.",
                "Added {n} annotations to the structure tree, with structure tab order.",
                n
            ));
        }
        let n = artifact_paths(doc);
        if n > 0 {
            out.push(trn!(
                "Marked untagged lines and shapes on {n} page as decoration (artifacts).",
                "Marked untagged lines and shapes on {n} pages as decoration (artifacts).",
                n
            ));
        }
    } else {
        let mut n = 0;
        for p in pagebara_pages::PageTree::load(doc).refs() {
            let Some(pd) = doc.get(p).and_then(|o| o.as_dict().cloned()) else {
                continue;
            };
            if doc
                .get_in(&pd, b"Annots")
                .as_array()
                .is_some_and(|a| !a.is_empty())
                && pd.name(b"Tabs") != Some(b"S")
            {
                let mut pd = pd;
                pd.set(b"Tabs", Object::name(b"S"));
                doc.set(p, pd);
                n += 1;
            }
        }
        if n > 0 {
            out.push(trn!(
                "Set structure tab order on {n} page.",
                "Set structure tab order on {n} pages.",
                n
            ));
        }
    }
    out.extend(crate::pdfa::embed_missing_fonts(doc));
    let x = crate::pdfa::metadata_of(doc).map(|m| xmp::parse(&m));
    let pdfa = x.as_ref().and_then(|x| {
        let part = x.pdfa_part.as_deref()?.parse::<u8>().ok()?;
        Some((
            part,
            x.pdfa_conformance.clone().unwrap_or_else(|| "B".into()),
        ))
    });
    crate::pdfa::sync_metadata(
        doc,
        pdfa.as_ref().map(|(p, c)| (*p, c.as_str())),
        true,
        &mut out,
    );
    out
}

/// Puts annotations that are not in the structure tree into it (as `Link`, `Form` or
/// `Annot` elements under the top element) and sets structure tab order on their pages.
fn tag_annotations(doc: &mut Document) -> usize {
    let Some(cat) = doc.catalog().and_then(|c| c.as_dict().cloned()) else {
        return 0;
    };
    let Some(root_ref) = cat.get(b"StructTreeRoot").and_then(Object::as_objref) else {
        return 0;
    };
    let Some(root) = doc.get(root_ref).and_then(|o| o.as_dict().cloned()) else {
        return 0;
    };
    // The element new ones go under: the root's first element child.
    let top = match &*doc.get_in(&root, b"K") {
        Object::Array(a) => a.iter().find_map(Object::as_objref),
        _ => root.get(b"K").and_then(Object::as_objref),
    };
    let parent = top.unwrap_or(root_ref);
    let mut next_key = doc
        .get_in(&root, b"ParentTreeNextKey")
        .as_int()
        .unwrap_or(0);
    let mut nums: Vec<Object> = Vec::new();
    let pt_ref = root.get(b"ParentTree").and_then(Object::as_objref);
    let pt = doc
        .get_in(&root, b"ParentTree")
        .as_dict()
        .cloned()
        .unwrap_or_default();
    if let Some(n) = doc.get_in(&pt, b"Nums").as_array() {
        nums = n.to_vec();
        for k in n.iter().step_by(2) {
            next_key = next_key.max(k.as_int().unwrap_or(0) + 1);
        }
    }
    let simple_tree = !pt.contains(b"Kids");
    let mut added = Vec::new();
    let mut count = 0;
    for p in pagebara_pages::PageTree::load(doc).refs() {
        let Some(pd) = doc.get(p).and_then(|o| o.as_dict().cloned()) else {
            continue;
        };
        let annots: Vec<Object> = doc
            .get_in(&pd, b"Annots")
            .as_array()
            .unwrap_or_default()
            .to_vec();
        let mut any = false;
        for a in annots {
            let Some(ar) = a.as_objref() else { continue };
            let Some(ad) = doc.get(ar).and_then(|o| o.as_dict().cloned()) else {
                continue;
            };
            let st = ad.name(b"Subtype").unwrap_or_default().to_vec();
            if st == b"Popup" {
                continue;
            }
            any = true;
            if ad.contains(b"StructParent") || !simple_tree {
                continue;
            }
            let tag: &[u8] = match st.as_slice() {
                b"Link" => b"Link",
                b"Widget" => b"Form",
                _ => b"Annot",
            };
            let mut objr = Dict::new();
            objr.set(b"Type", Object::name(b"OBJR"));
            objr.set(b"Obj", ar);
            objr.set(b"Pg", p);
            let mut el = Dict::new();
            el.set(b"Type", Object::name(b"StructElem"));
            el.set(b"S", Object::name(tag));
            el.set(b"P", parent);
            el.set(b"Pg", p);
            el.set(b"K", objr);
            if tag == b"Annot"
                && let Some(c) = ad.get(b"Contents")
            {
                el.set(b"Alt", c.clone());
            }
            let er = doc.add(el);
            let mut ad = ad.clone();
            ad.set(b"StructParent", next_key);
            doc.set(ar, ad);
            nums.push(Object::Int(next_key));
            nums.push(Object::Ref(er));
            next_key += 1;
            added.push(er);
            count += 1;
        }
        if any && pd.name(b"Tabs") != Some(b"S") {
            let mut pd = pd;
            pd.set(b"Tabs", Object::name(b"S"));
            doc.set(p, pd);
        }
    }
    if added.is_empty() {
        return count;
    }
    edit(doc, parent, |d| {
        let mut k: Vec<Object> = match d.get(b"K") {
            Some(Object::Array(a)) => a.clone(),
            Some(o) => vec![o.clone()],
            None => Vec::new(),
        };
        k.extend(added.iter().map(|r| Object::Ref(*r)));
        d.set(b"K", k);
        true
    });
    let mut pt = pt;
    pt.set(b"Nums", nums);
    match pt_ref {
        Some(r) => doc.set(r, pt),
        None => {
            let r = doc.add(pt);
            edit(doc, root_ref, |d| {
                d.set(b"ParentTree", r);
                true
            });
        }
    }
    edit(doc, root_ref, |d| {
        d.set(b"ParentTreeNextKey", next_key);
        true
    });
    count
}

/// Wraps path drawing outside marked content in `/Artifact BMC … EMC` on every page.
/// Returns the number of pages changed.
fn artifact_paths(doc: &mut Document) -> usize {
    let mut n = 0;
    for p in pagebara_pages::PageTree::load(doc).refs() {
        let Some(pd) = doc.get(p).and_then(|o| o.as_dict().cloned()) else {
            continue;
        };
        let data = pagebara_content::page_contents(doc, &pd);
        if let Some(new) = wrap_paths(&data) {
            let mut sd = Dict::new();
            sd.set(b"Filter", Object::name(b"FlateDecode"));
            let s = doc.add(Stream::new(sd, pagebara_filters::encode_flate(&new, 6)));
            let mut pd = pd;
            pd.set(b"Contents", s);
            doc.set(p, pd);
            n += 1;
        }
    }
    n
}

pub(crate) fn wrap_paths(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() + 64);
    let mut last = 0;
    let mut depth = 0i32;
    let mut in_text = false;
    let mut path_start: Option<usize> = None;
    let mut changed = false;
    let mut p = ContentParser::new(data);
    loop {
        let start = p.pos();
        let Some(i) = p.next() else { break };
        match i.op {
            Op::BeginMarked | Op::BeginMarkedProps => depth += 1,
            Op::EndMarked => depth -= 1,
            Op::BeginText => in_text = true,
            Op::EndText => in_text = false,
            Op::MoveTo | Op::Rect if depth == 0 && !in_text && path_start.is_none() => {
                path_start = Some(start)
            }
            Op::Stroke
            | Op::CloseStroke
            | Op::Fill
            | Op::FillCompat
            | Op::FillEvenOdd
            | Op::FillStroke
            | Op::FillStrokeEvenOdd
            | Op::CloseFillStroke
            | Op::CloseFillStrokeEvenOdd
            | Op::EndPath => {
                if let Some(s) = path_start.take() {
                    out.extend_from_slice(&data[last..s]);
                    out.extend_from_slice(b"\n/Artifact BMC\n");
                    out.extend_from_slice(&data[s..i.end]);
                    out.extend_from_slice(b"\nEMC\n");
                    last = i.end;
                    changed = true;
                }
            }
            Op::Save | Op::Restore | Op::Concat | Op::XObject | Op::Shade | Op::InlineImage => {
                path_start = None
            }
            _ => {}
        }
    }
    if !changed {
        return None;
    }
    out.extend_from_slice(&data[last..]);
    Some(out)
}

// Guided fixes.

/// Sets the alternative text of a structure element (empty removes it).
pub fn set_alt(doc: &mut Document, element: ObjRef, alt: &str) -> bool {
    edit(doc, element, |d| {
        if alt.trim().is_empty() {
            d.remove(b"Alt").is_some()
        } else {
            d.set(b"Alt", text_string(alt.trim()));
            true
        }
    })
}

/// Changes the structure type of an element (heading level, `TH`/`TD`, `Artifact`…).
pub fn set_tag(doc: &mut Document, element: ObjRef, tag: &str) -> bool {
    if tag.is_empty() || !tag.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return false;
    }
    edit(doc, element, |d| {
        d.set(b"S", Object::name(tag.as_bytes()));
        if tag == "TH" && !d.contains(b"A") {
            let mut a = Dict::new();
            a.set(b"O", Object::name(b"Table"));
            a.set(b"Scope", Object::name(b"Column"));
            d.set(b"A", a);
        }
        true
    })
}

/// Sets the description of an annotation (`/Contents`) or the accessible name of a form
/// field (`/TU`).
pub fn set_annotation_text(doc: &mut Document, annot: ObjRef, text: &str) -> bool {
    let Some(ad) = doc.get(annot).and_then(|o| o.as_dict().cloned()) else {
        return false;
    };
    let key: &[u8] = if ad.name(b"Subtype") == Some(b"Widget") {
        b"TU"
    } else {
        b"Contents"
    };
    let target = if key == b"TU" && !ad.contains(b"T") {
        ad.get(b"Parent")
            .and_then(Object::as_objref)
            .unwrap_or(annot)
    } else {
        annot
    };
    edit(doc, target, |d| {
        d.set(key, text_string(text));
        true
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untagged_paths_become_artifacts() {
        let src =
            b"/P <</MCID 0>> BDC BT (x) Tj ET EMC 0 0 m 10 10 l S q 1 0 0 1 0 0 cm 0 0 5 5 re f Q";
        let out = String::from_utf8(wrap_paths(src).unwrap()).unwrap();
        assert!(
            out.contains("/Artifact BMC\n 0 0 m 10 10 l S\nEMC"),
            "{out}"
        );
        assert_eq!(out.matches("/Artifact BMC").count(), 2, "{out}");
        assert!(wrap_paths(b"/P <</MCID 0>> BDC 0 0 m 1 1 l S EMC").is_none());
    }
}
