//! The PDF/A checks. Rule ids follow veraPDF (`clause-test` of the part checked).

use std::collections::HashMap;

use pagebara_i18n::tr;
use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::content::{Device, Facts, Scanner};
use crate::{Issues, Profile, Report, icc, xmp};

/// Picks the id of a rule for the part: `a1` for PDF/A-1, `a2` for parts 2 and 3.
fn r(part: u8, a1: &'static str, a2: &'static str) -> &'static str {
    if part == 1 { a1 } else { a2 }
}

const STANDARD_ANNOTS: [&[u8]; 27] = [
    b"Text",
    b"Link",
    b"FreeText",
    b"Line",
    b"Square",
    b"Circle",
    b"Polygon",
    b"PolyLine",
    b"Highlight",
    b"Underline",
    b"Squiggly",
    b"StrikeOut",
    b"Stamp",
    b"Caret",
    b"Ink",
    b"Popup",
    b"FileAttachment",
    b"Sound",
    b"Movie",
    b"Widget",
    b"Screen",
    b"PrinterMark",
    b"TrapNet",
    b"Watermark",
    b"3D",
    b"Redact",
    b"Projection",
];

const STANDARD_BLEND: [&[u8]; 16] = [
    b"Normal",
    b"Compatible",
    b"Multiply",
    b"Screen",
    b"Overlay",
    b"Darken",
    b"Lighten",
    b"ColorDodge",
    b"ColorBurn",
    b"HardLight",
    b"SoftLight",
    b"Difference",
    b"Exclusion",
    b"Hue",
    b"Saturation",
    b"Color",
];

fn is_luminosity(n: &[u8]) -> bool {
    n == b"Luminosity"
}

/// Pages by object number, and the page each annotation is on.
fn page_maps(doc: &Document) -> (HashMap<u32, usize>, HashMap<u32, usize>) {
    let mut pages = HashMap::new();
    let mut annots = HashMap::new();
    for (i, p) in pagebara_pages::PageTree::load(doc)
        .pages()
        .iter()
        .enumerate()
    {
        pages.insert(p.r.num, i);
        if let Some(pd) = doc.get(p.r)
            && let Some(pd) = pd.as_dict()
        {
            for a in doc.get_in(pd, b"Annots").as_array().unwrap_or_default() {
                if let Some(ar) = a.as_objref() {
                    annots.insert(ar.num, i);
                }
            }
        }
    }
    (pages, annots)
}

struct Ctx<'a> {
    doc: &'a Document,
    part: u8,
    profile: Profile,
    is: Issues,
}

impl Ctx<'_> {
    fn check(
        &mut self,
        rule: &'static str,
        fail: bool,
        page: Option<usize>,
        obj: Option<ObjRef>,
        msg: impl FnOnce() -> String,
    ) {
        self.is.check(rule, fail, page, obj, msg);
    }

    /// Implementation limits, recursively through direct objects.
    fn limits(&mut self, o: &Object, at: ObjRef, depth: u32) {
        if depth > 64 {
            return;
        }
        let p = self.part;
        match o {
            Object::Int(i) => self.check(
                r(p, "6.1.12-1", "6.1.13-1"),
                *i > i64::from(i32::MAX) || *i < i64::from(i32::MIN),
                None,
                Some(at),
                || tr!("integer {} is out of range", i),
            ),
            Object::Real(v) => {
                if p == 1 {
                    self.check("6.1.12-2", v.abs() > 32767.0, None, Some(at), || {
                        tr!("real number {} is larger than 32767", v)
                    });
                }
            }
            Object::String(s) => {
                // PDF 1.4 (PDF/A-1) allows 65535 bytes, ISO 32000-1 32767.
                let max = if p == 1 { 65535 } else { 32767 };
                self.check(
                    r(p, "6.1.12-3", "6.1.13-3"),
                    s.bytes.len() > max,
                    None,
                    Some(at),
                    || tr!("a string is longer than {} bytes", max),
                )
            }
            Object::Name(n) => self.check(
                r(p, "6.1.12-4", "6.1.13-4"),
                n.as_bytes().len() > 127,
                None,
                Some(at),
                || tr!("a name is longer than 127 bytes"),
            ),
            Object::Array(a) => {
                if p == 1 {
                    self.check("6.1.12-5", a.len() > 8191, None, Some(at), || {
                        tr!("an array has more than 8191 elements")
                    });
                }
                for e in a {
                    self.limits(e, at, depth + 1);
                }
            }
            Object::Dict(d) => {
                if p == 1 {
                    self.check("6.1.12-6", d.len() > 4095, None, Some(at), || {
                        tr!("a dictionary has more than 4095 entries")
                    });
                }
                for (_, v) in d.iter() {
                    self.limits(v, at, depth + 1);
                }
            }
            Object::Stream(s) => self.limits(&Object::Dict(s.dict.clone()), at, depth + 1),
            _ => {}
        }
    }

    /// ICCBased colour spaces found anywhere in `o`.
    fn icc_spaces(&mut self, o: &Object, at: ObjRef, depth: u32) {
        if depth > 32 {
            return;
        }
        match o {
            Object::Array(a) => {
                if a.first().and_then(Object::as_name) == Some(b"ICCBased")
                    && let Some(s) = a.get(1)
                {
                    let so = self.doc.resolve(s);
                    let ok = so.as_stream().is_some_and(|st| {
                        let n = self.doc.get_in(&st.dict, b"N").as_int();
                        self.doc
                            .decode(st)
                            .ok()
                            .and_then(|d| icc::header(&d.data))
                            .is_some_and(|h| {
                                h.well_formed
                                    && h.components().map(i64::from) == n
                                    && (self.part > 1 || h.major < 4)
                            })
                    });
                    let rule = r(self.part, "6.2.3.2-1", "6.2.4.2-1");
                    self.check(rule, !ok, None, Some(at), || {
                        tr!("an ICC-based colour space has a broken or unsuitable profile")
                    });
                }
                for e in a {
                    self.icc_spaces(e, at, depth + 1);
                }
            }
            Object::Dict(d) => {
                for (_, v) in d.iter() {
                    self.icc_spaces(v, at, depth + 1);
                }
            }
            _ => {}
        }
    }

    fn stream_dict(&mut self, d: &Dict, at: ObjRef) {
        let p = self.part;
        let doc = self.doc;
        self.check(
            r(p, "6.1.7-2", "6.1.7.1-1"),
            d.contains(b"F") || d.contains(b"FFilter") || d.contains(b"FDecodeParms"),
            None,
            Some(at),
            || tr!("a stream refers to an external file"),
        );
        let names: Vec<Vec<u8>> = match &*doc.get_in(d, b"Filter") {
            Object::Name(n) => vec![n.as_bytes().to_vec()],
            Object::Array(a) => a
                .iter()
                .filter_map(|o| doc.resolve(o).as_name().map(<[u8]>::to_vec))
                .collect(),
            _ => Vec::new(),
        };
        self.check(
            r(p, "6.1.10-1", "6.1.7.2-1"),
            names.iter().any(|n| n == b"LZWDecode" || n == b"LZW"),
            None,
            Some(at),
            || tr!("LZW compression is not allowed"),
        );
        if p > 1 {
            self.check(
                "6.1.7.2-2",
                names.iter().any(|n| n == b"Crypt"),
                None,
                Some(at),
                || tr!("crypt filters are not allowed"),
            );
        }
    }

    fn object(
        &mut self,
        at: ObjRef,
        o: &Object,
        annot_page: &HashMap<u32, usize>,
        page_of: &HashMap<u32, usize>,
    ) {
        let doc = self.doc;
        let p = self.part;
        self.limits(o, at, 0);
        self.icc_spaces(o, at, 0);
        let d = match o {
            Object::Stream(s) => {
                self.stream_dict(&s.dict, at);
                &s.dict
            }
            Object::Dict(d) => d,
            _ => return,
        };
        let ty = d.name(b"Type");
        let sub = d.name(b"Subtype");
        // XObjects.
        if o.as_stream().is_some() {
            match sub {
                Some(b"Image") => {
                    self.check(
                        r(p, "6.2.4-1", "6.2.8-1"),
                        d.contains(b"Alternates"),
                        None,
                        Some(at),
                        || tr!("images shall not have alternates"),
                    );
                    self.check(
                        r(p, "6.2.4-2", "6.2.8-2"),
                        d.contains(b"OPI"),
                        None,
                        Some(at),
                        || tr!("images shall not have OPI"),
                    );
                    self.check(
                        r(p, "6.2.4-3", "6.2.8-3"),
                        doc.get_in(d, b"Interpolate").as_bool() == Some(true),
                        None,
                        Some(at),
                        || tr!("image interpolation is not allowed"),
                    );
                    if p == 1 {
                        self.check(
                            "6.4-2",
                            d.get(b"SMask").is_some_and(|m| {
                                !doc.resolve(m).is_null()
                                    && doc.resolve(m).as_name() != Some(b"None")
                            }),
                            None,
                            Some(at),
                            || tr!("soft masks (transparency) are not allowed in PDF/A-1"),
                        );
                    }
                    if let Some(i) = d.name(b"Intent") {
                        self.check(
                            r(p, "6.2.9-1", "6.2.6-1"),
                            !matches!(
                                i,
                                b"RelativeColorimetric"
                                    | b"AbsoluteColorimetric"
                                    | b"Perceptual"
                                    | b"Saturation"
                            ),
                            None,
                            Some(at),
                            || tr!("unknown rendering intent"),
                        );
                    }
                }
                Some(b"Form") => {
                    self.check(
                        r(p, "6.2.5-1", "6.2.9-1"),
                        d.contains(b"OPI")
                            || d.name(b"Subtype2") == Some(b"PS")
                            || d.contains(b"PS"),
                        None,
                        Some(at),
                        || tr!("forms shall not have OPI or PostScript"),
                    );
                    self.check(
                        r(p, "6.2.6-1", "6.2.9-2"),
                        d.contains(b"Ref"),
                        None,
                        Some(at),
                        || tr!("reference XObjects are not allowed"),
                    );
                    if p == 1 {
                        let g = doc.get_in(d, b"Group");
                        self.check(
                            "6.4-5",
                            g.as_dict()
                                .is_some_and(|g| g.name(b"S") == Some(b"Transparency")),
                            None,
                            Some(at),
                            || tr!("transparency groups are not allowed in PDF/A-1"),
                        );
                    }
                }
                Some(b"PS") => self.check(r(p, "6.2.7-1", "6.2.9-3"), true, None, Some(at), || {
                    tr!("PostScript XObjects are not allowed")
                }),
                _ => {}
            }
        }
        // Graphics states.
        if ty == Some(b"ExtGState")
            || (ty.is_none() && (d.contains(b"TR2") || d.contains(b"SMask") && d.contains(b"CA")))
        {
            self.check(
                r(p, "6.2.8-1", "6.2.5-1"),
                d.contains(b"TR"),
                None,
                Some(at),
                || tr!("transfer functions are not allowed"),
            );
            self.check(
                r(p, "6.2.8-2", "6.2.5-2"),
                d.get(b"TR2")
                    .is_some_and(|t| doc.resolve(t).as_name() != Some(b"Default")),
                None,
                Some(at),
                || tr!("TR2 other than Default is not allowed"),
            );
            self.check(
                r(p, "6.2.10-1", "6.2.5-3"),
                d.contains(b"HTP"),
                None,
                Some(at),
                || tr!("halftone phase (HTP) is not allowed"),
            );
            if let Some(ri) = d.name(b"RI") {
                self.check(
                    r(p, "6.2.9-1", "6.2.6-1"),
                    !matches!(
                        ri,
                        b"RelativeColorimetric"
                            | b"AbsoluteColorimetric"
                            | b"Perceptual"
                            | b"Saturation"
                    ),
                    None,
                    Some(at),
                    || tr!("unknown rendering intent"),
                );
            }
            let bm: Vec<Vec<u8>> = match &*doc.get_in(d, b"BM") {
                Object::Name(n) => vec![n.as_bytes().to_vec()],
                Object::Array(a) => a
                    .iter()
                    .filter_map(|o| o.as_name().map(<[u8]>::to_vec))
                    .collect(),
                _ => Vec::new(),
            };
            if p == 1 {
                let smask = doc.get_in(d, b"SMask");
                self.check(
                    "6.4-1",
                    !smask.is_null() && smask.as_name() != Some(b"None"),
                    None,
                    Some(at),
                    || tr!("soft masks (transparency) are not allowed in PDF/A-1"),
                );
                self.check(
                    "6.4-3",
                    bm.iter().any(|b| b != b"Normal" && b != b"Compatible"),
                    None,
                    Some(at),
                    || tr!("blend modes are not allowed in PDF/A-1"),
                );
                let alpha = |k: &[u8]| {
                    doc.get_in(d, k)
                        .as_f64()
                        .is_some_and(|v| (v - 1.0).abs() > 1e-6)
                };
                self.check(
                    "6.4-4",
                    alpha(b"CA") || alpha(b"ca"),
                    None,
                    Some(at),
                    || tr!("constant opacity below 1 is not allowed in PDF/A-1"),
                );
            } else {
                self.check(
                    "6.2.10-2",
                    bm.iter().any(|b| !STANDARD_BLEND.contains(&b.as_slice())),
                    None,
                    Some(at),
                    || tr!("unknown blend mode"),
                );
            }
            let _ = is_luminosity;
        }
        // Annotations.
        let is_annot = ty == Some(b"Annot")
            || (sub.is_some()
                && d.contains(b"Rect")
                && ty.is_none()
                && annot_page.contains_key(&at.num));
        if is_annot && let Some(st) = sub {
            let page = annot_page.get(&at.num).copied();
            let forbidden = if p == 1 {
                matches!(st, b"Sound" | b"Movie" | b"FileAttachment")
                    || !STANDARD_ANNOTS.contains(&st)
            } else {
                matches!(st, b"Sound" | b"Movie" | b"Screen" | b"3D" | b"RichMedia")
                    || !STANDARD_ANNOTS.contains(&st)
            };
            self.check(
                r(p, "6.5.2-1", "6.3.1-1"),
                forbidden,
                page,
                Some(at),
                || {
                    tr!(
                        "{} annotations are not allowed",
                        String::from_utf8_lossy(st)
                    )
                },
            );
            let f = doc.get_in(d, b"F").as_int().unwrap_or(0);
            if st != b"Popup" {
                self.check(
                    r(p, "6.5.3-1", "6.3.2-1"),
                    f & 4 == 0,
                    page,
                    Some(at),
                    || tr!("annotations shall be set to print"),
                );
            }
            let hidden = if p == 1 { 1 | 2 | 32 } else { 1 | 2 | 32 | 256 };
            self.check(
                r(p, "6.5.3-2", "6.3.2-2"),
                f & hidden != 0,
                page,
                Some(at),
                || tr!("annotations shall not be hidden or invisible"),
            );
            if p == 1 {
                self.check(
                    "6.5.3-3",
                    doc.get_in(d, b"CA")
                        .as_f64()
                        .is_some_and(|v| (v - 1.0).abs() > 1e-6),
                    page,
                    Some(at),
                    || tr!("annotation opacity below 1 is not allowed in PDF/A-1"),
                );
            }
            let ap = doc.get_in(d, b"AP");
            let rect = pagebara_content::Rect::from_object(&doc.get_in(d, b"Rect"));
            let zero = rect.is_some_and(|r| r.width() == 0.0 && r.height() == 0.0);
            if p > 1 && !matches!(st, b"Popup" | b"Link") && !zero {
                self.check("6.3.3-1", ap.as_dict().is_none(), page, Some(at), || {
                    tr!(
                        "the {} annotation has no appearance stream",
                        String::from_utf8_lossy(st)
                    )
                });
            }
            if let Some(ap) = ap.as_dict() {
                self.check(
                    r(p, "6.5.3-4", "6.3.3-2"),
                    ap.iter().any(|(k, _)| k.as_bytes() != b"N"),
                    page,
                    Some(at),
                    || tr!("appearance dictionaries shall only contain N"),
                );
                if p > 1 {
                    let n = doc.get_in(ap, b"N");
                    let btn =
                        st == b"Widget" && field_type(doc, d).as_deref() == Some(b"Btn".as_slice());
                    if btn {
                        self.check(
                            "6.3.3-3",
                            n.as_dict().is_none() && n.as_stream().is_none(),
                            page,
                            Some(at),
                            || {
                                "a button's normal appearance shall be a dictionary of states"
                                    .into()
                            },
                        );
                    } else {
                        self.check("6.3.3-4", n.as_stream().is_none(), page, Some(at), || {
                            tr!("the normal appearance shall be a stream")
                        });
                    }
                }
            }
            if st == b"Widget" {
                self.check(
                    r(p, "6.6.2-2", "6.4.1-1"),
                    d.contains(b"AA") || d.contains(b"A"),
                    page,
                    Some(at),
                    || tr!("form widgets shall not have actions"),
                );
            }
        }
        // Form fields.
        if d.contains(b"FT") && !is_annot {
            self.check(
                r(p, "6.6.2-2", "6.4.1-1"),
                d.contains(b"AA"),
                None,
                Some(at),
                || tr!("form fields shall not have additional actions"),
            );
        }
        // Actions, also those written inside other objects.
        self.actions(o, at, annot_page.get(&at.num).copied(), 0);
        // Pages.
        if ty == Some(b"Page") {
            let page = page_of.get(&at.num).copied();
            if p > 1 {
                self.check("6.5.2-2", d.contains(b"AA"), page, Some(at), || {
                    tr!("pages shall not have additional actions")
                });
                for k in [
                    &b"MediaBox"[..],
                    b"CropBox",
                    b"BleedBox",
                    b"TrimBox",
                    b"ArtBox",
                ] {
                    if let Some(b) = pagebara_content::Rect::from_object(&doc.get_in(d, k)) {
                        let bad = |v: f32| !(3.0..=14400.0).contains(&v);
                        self.check(
                            "6.1.13-10",
                            bad(b.width().abs()) || bad(b.height().abs()),
                            page,
                            Some(at),
                            || tr!("page boundaries shall be between 3 and 14400 units"),
                        );
                    }
                }
            } else {
                self.check("6.6.2-3", d.contains(b"AA"), page, Some(at), || {
                    tr!("pages shall not have additional actions")
                });
                let g = doc.get_in(d, b"Group");
                self.check(
                    "6.4-5",
                    g.as_dict()
                        .is_some_and(|g| g.name(b"S") == Some(b"Transparency")),
                    page,
                    Some(at),
                    || tr!("transparency groups are not allowed in PDF/A-1"),
                );
            }
        }
        // Attached files.
        if ty == Some(b"Filespec") || d.contains(b"EF") {
            let ef = doc.get_in(d, b"EF");
            if let Some(ef) = ef.as_dict() {
                match p {
                    1 => self.check("6.1.11-2", true, None, Some(at), || {
                        tr!("attached files are not allowed in PDF/A-1")
                    }),
                    2 => {
                        let pdf = ef.iter().all(|(_, v)| {
                            doc.resolve(v)
                                .as_stream()
                                .and_then(|s| doc.decode(s).ok())
                                .is_some_and(|d| d.data.starts_with(b"%PDF-"))
                        });
                        self.check("6.8-1", !pdf, None, Some(at), || {
                            tr!("attached files shall be PDF/A files")
                        });
                    }
                    _ => {
                        self.check(
                            "6.8-2",
                            d.name(b"AFRelationship").is_none(),
                            None,
                            Some(at),
                            || tr!("attached files need an AFRelationship"),
                        );
                        let mime = ef.iter().all(|(_, v)| {
                            doc.resolve(v)
                                .as_stream()
                                .is_some_and(|s| s.dict.name(b"Subtype").is_some())
                        });
                        self.check("6.8-3", !mime, None, Some(at), || {
                            tr!("attached files need a MIME type (Subtype)")
                        });
                    }
                }
            }
        }
        // Optional content configurations (parts 2 and 3).
        if p > 1 && d.contains(b"OCGs") && d.contains(b"D") {
            let mut configs = vec![(*doc.get_in(d, b"D")).clone()];
            if let Some(c) = doc.get_in(d, b"Configs").as_array() {
                configs.extend(c.iter().map(|o| (*doc.resolve(o)).clone()));
            }
            for c in configs {
                if let Some(c) = c.as_dict() {
                    self.check(
                        "6.1.11-1",
                        doc.get_in(c, b"Name").as_string().is_none(),
                        None,
                        Some(at),
                        || tr!("optional content configurations need a name"),
                    );
                    self.check("6.1.11-2", c.contains(b"AS"), None, Some(at), || {
                        tr!("optional content configurations shall not have AS")
                    });
                }
            }
        }
        let _ = self.profile;
    }
}

impl Ctx<'_> {
    fn actions(&mut self, o: &Object, at: ObjRef, page: Option<usize>, depth: u32) {
        if depth > 16 {
            return;
        }
        let doc = self.doc;
        let p = self.part;
        let d = match o {
            Object::Dict(d) => d,
            Object::Stream(s) => &s.dict,
            Object::Array(a) => {
                for e in a {
                    self.actions(e, at, page, depth + 1);
                }
                return;
            }
            _ => return,
        };
        let ty = d.name(b"Type");
        if let Some(s) = d.name(b"S").filter(|_| ty.is_none_or(|t| t == b"Action")) {
            let known = matches!(
                s,
                b"GoTo"
                    | b"GoToR"
                    | b"GoToE"
                    | b"Launch"
                    | b"Thread"
                    | b"URI"
                    | b"Sound"
                    | b"Movie"
                    | b"Hide"
                    | b"Named"
                    | b"SubmitForm"
                    | b"ResetForm"
                    | b"ImportData"
                    | b"JavaScript"
                    | b"SetOCGState"
                    | b"Rendition"
                    | b"Trans"
                    | b"GoTo3DView"
            );
            if known {
                let forbidden = matches!(
                    s,
                    b"Launch" | b"Sound" | b"Movie" | b"ResetForm" | b"ImportData" | b"JavaScript"
                ) || (p > 1
                    && matches!(
                        s,
                        b"Hide" | b"SetOCGState" | b"Rendition" | b"Trans" | b"GoTo3DView"
                    ));
                self.check(
                    r(p, "6.6.1-1", "6.5.1-1"),
                    forbidden,
                    page,
                    Some(at),
                    || tr!("{} actions are not allowed", String::from_utf8_lossy(s)),
                );
                if s == b"Named" {
                    let n = doc.get_in(d, b"N");
                    let ok = n.as_name().is_some_and(|n| {
                        matches!(n, b"NextPage" | b"PrevPage" | b"FirstPage" | b"LastPage")
                    });
                    self.check(r(p, "6.6.1-2", "6.5.1-2"), !ok, page, Some(at), || {
                        tr!("only page navigation named actions are allowed")
                    });
                }
            }
        }
        for (k, v) in d.iter() {
            if matches!(k.as_bytes(), b"A" | b"AA" | b"Next" | b"OpenAction" | b"PA")
                || matches!(v, Object::Array(_)) && k.as_bytes() == b"Next"
            {
                self.actions(v, at, page, depth + 1);
            } else if let Object::Dict(_) = v
                && ty == Some(b"Action")
            {
                self.actions(v, at, page, depth + 1);
            } else if let Object::Dict(_) = v
                && depth > 0
                && !matches!(k.as_bytes(), b"Parent" | b"P")
            {
                // Inside an additional-actions dictionary, every value is an action.
                self.actions(v, at, page, depth + 1);
            }
        }
    }
}

fn field_type(doc: &Document, d: &Dict) -> Option<Vec<u8>> {
    let mut cur = d.clone();
    for _ in 0..32 {
        if let Some(ft) = cur.name(b"FT") {
            return Some(ft.to_vec());
        }
        cur = doc.get_in(&cur, b"Parent").as_dict()?.clone();
    }
    None
}

fn info_string(doc: &Document, info: &Dict, k: &[u8]) -> Option<String> {
    let o = doc.get_in(info, k);
    o.as_string()?;
    Some(pagebara_content::extract::text_string(&o))
}

/// The document information as XMP would have it.
pub(crate) fn info_of(doc: &Document) -> xmp::Info {
    let info = doc
        .trailer()
        .get(b"Info")
        .and_then(|i| doc.resolve(i).as_dict().cloned())
        .unwrap_or_default();
    let s = |k: &[u8]| info_string(doc, &info, k).filter(|s| !s.is_empty());
    xmp::Info {
        title: s(b"Title"),
        author: s(b"Author"),
        subject: s(b"Subject"),
        keywords: s(b"Keywords"),
        creator: s(b"Creator"),
        producer: s(b"Producer"),
        created: s(b"CreationDate").and_then(|d| xmp::pdf_date_to_iso(&d)),
        modified: s(b"ModDate").and_then(|d| xmp::pdf_date_to_iso(&d)),
    }
}

/// The catalog's XMP packet and its stream dictionary.
pub(crate) fn metadata(doc: &Document) -> Option<(Dict, Vec<u8>)> {
    let cat = doc.catalog()?;
    let m = doc.get_in(cat.as_dict()?, b"Metadata");
    let s = m.as_stream()?;
    Some((s.dict.clone(), doc.decode(s).ok()?.data))
}

fn fonts(c: &mut Ctx<'_>, facts: &Facts) {
    let doc = c.doc;
    let p = c.part;
    for f in &facts.fonts {
        let at = f.r;
        let page = f.page;
        let d = &f.dict;
        let sub = d.name(b"Subtype").unwrap_or_default();
        if c.profile.unicode() {
            let mut s = String::new();
            let unmapped = f.codes.iter().find(|&&code| {
                s.clear();
                !f.font.unicode(doc, code, &mut s) || !crate::real_unicode(&s)
            });
            let name =
                String::from_utf8_lossy(d.name(b"BaseFont").unwrap_or(b"(unnamed)")).into_owned();
            c.check("6.2.11.7-1", unmapped.is_some(), page, at, || {
                tr!("text in {} has characters without a Unicode mapping", name)
            });
        }
        if sub == b"Type3" {
            continue;
        }
        let descendant = (sub == b"Type0")
            .then(|| {
                doc.get_in(d, b"DescendantFonts")
                    .as_array()
                    .and_then(|a| a.first().map(|x| (*doc.resolve(x)).clone()))
            })
            .flatten()
            .and_then(|o| o.as_dict().cloned());
        let fd_owner = descendant.as_ref().unwrap_or(d);
        let embedded = pagebara_font::pdf::font_file_ref(doc, fd_owner).is_some();
        let name =
            String::from_utf8_lossy(d.name(b"BaseFont").unwrap_or(b"(unnamed)")).into_owned();
        // Part 2 and 3 let text that is never shown (rendering mode 3) use any font.
        let needs = p == 1 || f.visible;
        c.check(
            r(p, "6.3.4-1", "6.2.11.4.1-1"),
            needs && !embedded,
            page,
            at,
            || tr!("font {} is not embedded", name),
        );
        let fd = pagebara_font::pdf::font_descriptor(doc, fd_owner);
        let subset = name.len() > 7 && name.as_bytes()[6] == b'+';
        if p == 1
            && subset
            && let Some(fd) = &fd
        {
            if sub == b"Type1" {
                c.check("6.3.5-2", !fd.contains(b"CharSet"), page, at, || {
                    tr!("Type 1 subset {} has no CharSet", name)
                });
            }
            if descendant.is_some() {
                c.check("6.3.5-3", !fd.contains(b"CIDSet"), page, at, || {
                    tr!("CID subset {} has no CIDSet", name)
                });
            }
        }
        if let Some(desc) = &descendant {
            let enc = doc.get_in(d, b"Encoding");
            let predefined = enc.as_name().is_some();
            if let Some(cmap) = enc.as_stream() {
                let _ = cmap;
            }
            c.check(
                r(p, "6.3.3.3-1", "6.2.11.3.3-1"),
                !predefined && enc.as_stream().is_none(),
                page,
                at,
                || tr!("the CMap of {} is neither predefined nor embedded", name),
            );
            if desc.name(b"Subtype") == Some(b"CIDFontType2") && embedded {
                let m = doc.get_in(desc, b"CIDToGIDMap");
                c.check(
                    r(p, "6.3.3.2-1", "6.2.11.3.2-1"),
                    m.as_name() != Some(b"Identity") && m.as_stream().is_none(),
                    page,
                    at,
                    || tr!("{} needs a CIDToGIDMap", name),
                );
            }
            if let (Some(cm), Some(csi)) = (
                enc.as_stream(),
                doc.get_in(desc, b"CIDSystemInfo").as_dict(),
            ) {
                let csi2 = doc.get_in(&cm.dict, b"CIDSystemInfo");
                if let Some(csi2) = csi2.as_dict() {
                    let s = |d: &Dict, k: &[u8]| doc.get_in(d, k).as_string().map(<[u8]>::to_vec);
                    let mismatch = s(csi, b"Registry") != s(csi2, b"Registry")
                        || s(csi, b"Ordering") != s(csi2, b"Ordering");
                    c.check(
                        r(p, "6.3.3.1-1", "6.2.11.3.1-1"),
                        mismatch,
                        page,
                        at,
                        || tr!("the CIDSystemInfo of {} does not match its CMap", name),
                    );
                }
            }
        }
        if sub == b"TrueType"
            && embedded
            && let Some(fd) = &fd
        {
            let flags = doc.get_in(fd, b"Flags").as_int().unwrap_or(0);
            let symbolic = flags & 4 != 0;
            let enc = doc.get_in(d, b"Encoding");
            let base_ok =
                |n: Option<&[u8]>| matches!(n, Some(b"WinAnsiEncoding" | b"MacRomanEncoding"));
            if symbolic {
                c.check(
                    r(p, "6.3.7-2", "6.2.11.6-3"),
                    !enc.is_null(),
                    page,
                    at,
                    || tr!("symbolic TrueType font {} shall not have an Encoding", name),
                );
            } else {
                let ok = base_ok(enc.as_name())
                    || enc.as_dict().is_some_and(|e| {
                        base_ok(e.name(b"BaseEncoding")) || !e.contains(b"BaseEncoding")
                    });
                c.check(r(p, "6.3.7-1", "6.2.11.6-1"), !ok, page, at, || {
                    tr!(
                        "non-symbolic TrueType font {} needs WinAnsi or MacRoman encoding",
                        name
                    )
                });
            }
        }
        // Widths as in the program, for simple embedded fonts whose text is drawn
        // (invisible text, mode 3, is exempt as in veraPDF).
        if embedded
            && f.visible
            && descendant.is_none()
            && let Some(widths) = doc.get_in(d, b"Widths").as_array()
        {
            let first = doc.get_in(d, b"FirstChar").as_int().unwrap_or(0);
            if let Some(pf) = pagebara_font::pdf::load(doc, d, None) {
                let bad = f.codes.iter().find(|&&code| {
                    let i = i64::from(code) - first;
                    let w = usize::try_from(i)
                        .ok()
                        .and_then(|i| widths.get(i))
                        .and_then(|w| doc.resolve(w).as_f64());
                    match (w, pf.advance(code)) {
                        (Some(w), Some(a)) => {
                            pf.glyph(code) != 0 && (w - f64::from(a) * 1000.0).abs() > 1.0
                        }
                        _ => false,
                    }
                });
                c.check(
                    r(p, "6.3.6-1", "6.2.11.5-1"),
                    bad.is_some(),
                    page,
                    at,
                    || tr!("the widths of {} differ from its font program", name),
                );
            }
        }
        if p > 1 && embedded {
            let notdef = f.codes.iter().any(|&code| f.font.glyph_id(code) == Some(0));
            c.check("6.2.11.8-1", notdef && f.visible, page, at, || {
                tr!("text in {} shows the .notdef glyph", name)
            });
        }
    }
}

/// Checks `doc` against a PDF/A profile.
pub fn validate(doc: &Document, profile: Profile) -> Report {
    let part = profile.part().unwrap_or(2);
    let standard = match part {
        1 => "ISO 19005-1:2005",
        2 => "ISO 19005-2:2011",
        _ => "ISO 19005-3:2012",
    };
    let mut c = Ctx {
        doc,
        part,
        profile,
        is: Issues::new(standard),
    };
    let p = part;
    let src = doc.source();
    // File structure.
    c.check(
        "6.1.2-1",
        !src.starts_with(b"%PDF-1.") && !src.starts_with(b"%PDF-2."),
        None,
        None,
        || tr!("the file does not start with a PDF header"),
    );
    let second = src.iter().position(|&b| b == b'\n' || b == b'\r').map(|i| {
        let rest = &src[i + 1..];
        let rest = rest.strip_prefix(b"\n").unwrap_or(rest);
        rest.iter()
            .take_while(|&&b| b != b'\n' && b != b'\r')
            .copied()
            .collect::<Vec<u8>>()
    });
    let binary = second.is_some_and(|l| {
        l.first() == Some(&b'%') && l.iter().skip(1).take(4).filter(|&&b| b > 127).count() >= 4
    });
    c.check("6.1.2-2", !binary, None, None, || {
        tr!("the header is not followed by a comment with binary characters")
    });
    let t = doc.trailer();
    c.check(
        "6.1.3-1",
        !t.get(b"ID")
            .and_then(Object::as_array)
            .is_some_and(|a| a.len() == 2),
        None,
        None,
        || tr!("the trailer has no file identifier"),
    );
    c.check(
        "6.1.3-2",
        doc.is_encrypted() || t.contains(b"Encrypt"),
        None,
        None,
        || tr!("the file is encrypted"),
    );
    let tail = src.trim_ascii_end();
    c.check("6.1.3-3", !tail.ends_with(b"%%EOF"), None, None, || {
        tr!("data follows the last end-of-file marker")
    });
    let cat = doc
        .catalog()
        .and_then(|c| c.as_dict().cloned())
        .unwrap_or_default();
    let (page_of, annot_page) = page_maps(doc);
    let mut has_objstm = false;
    let nums = doc.object_numbers();
    if p == 1 {
        c.check(
            "6.1.12-7",
            nums.last().is_some_and(|&n| n > 8_388_607),
            None,
            None,
            || tr!("too many indirect objects"),
        );
    } else {
        c.check(
            "6.1.13-7",
            nums.last().is_some_and(|&n| n > 8_388_607),
            None,
            None,
            || tr!("too many indirect objects"),
        );
    }
    for num in nums {
        let Some(rf) = doc.reference(num) else {
            continue;
        };
        let Some(o) = doc.get(rf) else { continue };
        if let Some(s) = o.as_stream()
            && matches!(s.dict.name(b"Type"), Some(b"ObjStm" | b"XRef"))
        {
            has_objstm = true;
            continue;
        }
        c.object(rf, &o, &annot_page, &page_of);
    }
    if p == 1 {
        c.check("6.1.4-1", has_objstm, None, None, || {
            tr!("PDF/A-1 does not allow object or cross-reference streams")
        });
    }
    // Catalog.
    if p == 1 {
        c.check(
            "6.1.13-1",
            cat.contains(b"OCProperties"),
            None,
            None,
            || tr!("optional content is not allowed in PDF/A-1"),
        );
        let names = doc.get_in(&cat, b"Names");
        c.check(
            "6.1.11-1",
            names
                .as_dict()
                .is_some_and(|n| n.contains(b"EmbeddedFiles")),
            None,
            None,
            || tr!("attached files are not allowed in PDF/A-1"),
        );
        c.check("6.6.2-1", cat.contains(b"AA"), None, None, || {
            tr!("the catalog shall not have additional actions")
        });
    } else {
        c.check("6.5.2-1", cat.contains(b"AA"), None, None, || {
            tr!("the catalog shall not have additional actions")
        });
        if let Some(perms) = doc.get_in(&cat, b"Perms").as_dict() {
            c.check(
                "6.1.12-1",
                perms
                    .iter()
                    .any(|(k, _)| k.as_bytes() != b"UR3" && k.as_bytes() != b"DocMDP"),
                None,
                None,
                || tr!("only UR3 and DocMDP permissions are allowed"),
            );
        }
    }
    let names = doc.get_in(&cat, b"Names");
    c.check(
        r(p, "6.6.1-3", "6.5.1-3"),
        names.as_dict().is_some_and(|n| n.contains(b"JavaScript")),
        None,
        None,
        || tr!("scripts are not allowed"),
    );
    let form = doc.get_in(&cat, b"AcroForm");
    if let Some(form) = form.as_dict() {
        c.check(
            r(p, "6.9-1", "6.4.1-2"),
            doc.get_in(form, b"NeedAppearances").as_bool() == Some(true),
            None,
            None,
            || tr!("NeedAppearances shall not be true"),
        );
        if p > 1 {
            c.check("6.4.2-1", form.contains(b"XFA"), None, None, || {
                tr!("XFA forms are not allowed")
            });
        }
    }
    // Output intents.
    let mut oi_space: Option<u8> = None;
    let mut profiles = Vec::new();
    for oi in doc
        .get_in(&cat, b"OutputIntents")
        .as_array()
        .unwrap_or_default()
    {
        let oi = doc.resolve(oi);
        let Some(oi) = oi.as_dict() else { continue };
        if oi.name(b"S") != Some(b"GTS_PDFA1") {
            continue;
        }
        let prof = oi.get(b"DestOutputProfile");
        let data = prof.and_then(|pr| {
            doc.resolve(pr)
                .as_stream()
                .and_then(|s| doc.decode(s).ok())
                .map(|d| d.data)
        });
        let h = data.as_deref().and_then(icc::header);
        let ok = h.is_some_and(|h| {
            h.well_formed
                && matches!(&h.class, b"mntr" | b"prtr")
                && (p > 1 || h.major < 4)
                && h.components().is_some()
        });
        c.check(r(p, "6.2.2-1", "6.2.3-1"), !ok, None, None, || {
            tr!("the output intent has no valid output profile")
        });
        if let Some(h) = h {
            oi_space = h.components();
        }
        profiles.push(prof.and_then(Object::as_objref));
    }
    profiles.dedup();
    c.check(
        r(p, "6.2.2-2", "6.2.3-2"),
        profiles.len() > 1,
        None,
        None,
        || tr!("output intents with different profiles"),
    );
    // Content.
    let facts = Scanner::run(doc);
    for (&dev, &page) in &facts.device {
        let (rule, fail, what) = match dev {
            Device::Rgb => (
                r(p, "6.2.3.3-1", "6.2.4.3-2"),
                oi_space != Some(3),
                "DeviceRGB",
            ),
            Device::Cmyk => (
                r(p, "6.2.3.3-2", "6.2.4.3-3"),
                oi_space != Some(4),
                "DeviceCMYK",
            ),
            Device::Gray => (
                r(p, "6.2.3.3-3", "6.2.4.3-4"),
                oi_space.is_none(),
                "DeviceGray",
            ),
        };
        c.check(rule, fail, page, None, || {
            tr!(
                "{} is used without a matching output intent or default colour space",
                what
            )
        });
    }
    c.check(
        r(p, "6.1.12-8", "6.1.13-8"),
        facts.q_depth > 28,
        None,
        None,
        || tr!("graphics states are nested deeper than 28"),
    );
    fonts(&mut c, &facts);
    // Metadata.
    let meta = metadata(doc);
    c.check(
        r(p, "6.7.2-1", "6.6.2.1-1"),
        meta.is_none(),
        None,
        None,
        || tr!("the catalog has no XMP metadata"),
    );
    if let Some((md, data)) = &meta {
        if p == 1 {
            c.check("6.7.2-2", md.contains(b"Filter"), None, None, || {
                tr!("the metadata stream shall not be compressed in PDF/A-1")
            });
        }
        let x = xmp::parse(data);
        c.check(
            r(p, "6.7.5-1", "6.6.2.1-2"),
            x.bad_header,
            None,
            None,
            || tr!("the XMP packet header has bytes or encoding attributes"),
        );
        c.check(
            r(p, "6.7.11-1", "6.6.4-1"),
            x.pdfa_part.as_deref() != Some(&p.to_string()),
            None,
            None,
            || tr!("the metadata does not claim PDF/A-{}", p),
        );
        let conf = profile.conformance();
        let claimed = x.pdfa_conformance.as_deref().unwrap_or("");
        let ok = claimed == conf
            || (conf == "B" && matches!(claimed, "U" | "A"))
            || (conf == "U" && claimed == "A");
        c.check(r(p, "6.7.11-2", "6.6.4-2"), !ok, None, None, || {
            tr!("the metadata does not claim conformance level {}", conf)
        });
        // Only PDF/A-1 requires the document information to agree with the metadata;
        // later parts leave the dictionary to PDF itself (veraPDF passes a PDF/A-2 file
        // whose title differs).
        let info = if p == 1 {
            info_of(doc)
        } else {
            Default::default()
        };
        let pairs = [
            ("Title", &info.title, &x.info.title),
            ("Author", &info.author, &x.info.author),
            ("Subject", &info.subject, &x.info.subject),
            ("Keywords", &info.keywords, &x.info.keywords),
            ("Creator", &info.creator, &x.info.creator),
            ("Producer", &info.producer, &x.info.producer),
        ];
        for (k, a, b) in pairs {
            if let Some(a) = a {
                c.check(
                    r(p, "6.7.3-1", "6.6.3-1"),
                    b.as_deref() != Some(a.as_str()),
                    None,
                    None,
                    || tr!("document information {} differs from the XMP metadata", k),
                );
            }
        }
        for (k, a, b) in [
            ("CreationDate", &info.created, &x.info.created),
            ("ModDate", &info.modified, &x.info.modified),
        ] {
            if let Some(a) = a {
                c.check(
                    r(p, "6.7.3-1", "6.6.3-1"),
                    !b.as_deref().is_some_and(|b| xmp::same_date(a, b)),
                    None,
                    None,
                    || tr!("document information {} differs from the XMP metadata", k),
                );
            }
        }
    }
    c.is.finish(profile)
}
