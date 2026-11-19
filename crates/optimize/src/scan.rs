//! Reading every content stream the document draws for what it uses: the glyphs of each
//! embedded font program, the largest resolution each image is drawn at, and the resource
//! names each resource dictionary is asked for.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use pagebara_content::font::{Font, ProgramLoader, embedded_programs};
use pagebara_content::lex::Operand;
use pagebara_content::{ContentParser, Matrix, Op, Rect};
use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};
use rayon::prelude::*;

/// Streams nested deeper than this are not followed (and make the result uncertain).
const MAX_DEPTH: u32 = 32;

/// Where a resource dictionary is: an object of its own, or directly in its holder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ResLoc {
    Obj(u32),
    In(u32),
}

/// A resource category dictionary (`/Font`, `/XObject`, …): an object of its own, or
/// directly in a resource dictionary.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CatKey {
    Obj(u32),
    In(ResLoc, &'static [u8]),
}

/// The categories whose unused entries may go.
pub(crate) const CATEGORIES: [&[u8]; 7] = [
    b"Font",
    b"XObject",
    b"ExtGState",
    b"ColorSpace",
    b"Pattern",
    b"Shading",
    b"Properties",
];

/// Resources as a stream sees them, with where they are when known.
#[derive(Clone)]
struct Res {
    dict: Dict,
    loc: Option<ResLoc>,
}

#[derive(Default)]
pub(crate) struct Usage {
    /// Glyph ids shown, per font program object.
    pub glyphs: HashMap<u32, BTreeSet<u16>>,
    /// Font programs that must stay whole (fonts of the interactive form).
    pub keep_programs: HashSet<u32>,
    /// The highest pixels per inch each image is drawn at (infinite when not known).
    pub images: HashMap<u32, f32>,
    /// Names asked for, per category dictionary.
    pub names: HashMap<CatKey, HashSet<Vec<u8>>>,
    /// Objects holding `/Resources` whose streams were read.
    pub holders: HashSet<u32>,
    /// Some stream could not be read, or used resources it does not have.
    pub uncertain: bool,
}

impl Usage {
    fn merge(&mut self, o: Usage) {
        for (k, v) in o.glyphs {
            self.glyphs.entry(k).or_default().extend(v);
        }
        self.keep_programs.extend(o.keep_programs);
        for (k, v) in o.images {
            let e = self.images.entry(k).or_insert(0.0);
            *e = e.max(v);
        }
        for (k, v) in o.names {
            self.names.entry(k).or_default().extend(v);
        }
        self.holders.extend(o.holders);
        self.uncertain |= o.uncertain;
    }
}

pub(crate) struct Scan<'d> {
    doc: &'d Document,
    loader: Arc<ProgramLoader>,
    fonts: HashMap<u32, Option<(u32, Arc<Font>)>>,
    visited: HashSet<(u32, Option<ResLoc>, i32)>,
    pub u: Usage,
}

/// The resources of a holder (page, form, pattern, Type 3 font).
fn own_res(doc: &Document, holder: u32, d: &Dict) -> Option<Res> {
    let o = d.get(b"Resources")?;
    let loc = match o.as_objref() {
        Some(r) => ResLoc::Obj(r.num),
        None => ResLoc::In(holder),
    };
    Some(Res {
        dict: doc.resolve(o).as_dict()?.clone(),
        loc: Some(loc),
    })
}

/// A page's resources, inherited from the page tree when it has none, and the object
/// holding them.
fn page_res(doc: &Document, page: ObjRef) -> (Res, Option<u32>) {
    let mut cur = Some(page);
    for _ in 0..64 {
        let Some(r) = cur else { break };
        let Some(o) = doc.get(r) else { break };
        let Some(d) = o.as_dict() else { break };
        if let Some(res) = own_res(doc, r.num, d) {
            return (res, Some(r.num));
        }
        cur = d.get(b"Parent").and_then(Object::as_objref);
    }
    let none = Res {
        dict: Dict::new(),
        loc: None,
    };
    (none, None)
}

impl<'d> Scan<'d> {
    /// Reads the whole document, pages in parallel.
    pub fn run(doc: &'d Document) -> Usage {
        let loader = embedded_programs();
        let new = || Scan {
            doc,
            loader: loader.clone(),
            fonts: HashMap::new(),
            visited: HashSet::new(),
            u: Usage::default(),
        };
        let mut first = new();
        first.form_fonts();
        let tree = pagebara_pages::PageTree::load(doc);
        let parts: Vec<Usage> = tree
            .pages()
            .par_chunks(8)
            .map(|pages| {
                let mut s = new();
                for page in pages {
                    s.page(page);
                }
                s.u
            })
            .collect();
        let mut u = first.u;
        for p in parts {
            u.merge(p);
        }
        u
    }

    fn form_fonts(&mut self) {
        let doc = self.doc;
        let form = doc
            .catalog()
            .and_then(|c| doc.get_in(c.as_dict()?, b"AcroForm").as_dict().cloned());
        if let Some(dr) = form
            .as_ref()
            .and_then(|f| doc.get_in(f, b"DR").as_dict().cloned())
            && let Some(fonts) = doc.get_in(&dr, b"Font").as_dict()
        {
            for (_, v) in fonts.iter() {
                if let Some(d) = doc.resolve(v).as_dict()
                    && let Some(p) = pagebara_font::pdf::font_file_ref(doc, d)
                {
                    self.u.keep_programs.insert(p.num);
                }
            }
        }
    }

    fn page(&mut self, page: &pagebara_pages::Page) {
        let doc = self.doc;
        let Some(po) = doc.get(page.r) else { return };
        let Some(pd) = po.as_dict() else { return };
        let (res, holder) = page_res(doc, page.r);
        let unit = page.user_unit as f32;
        let ctm = Matrix::from_array([unit, 0.0, 0.0, unit, 0.0, 0.0]);
        self.u.holders.extend(holder);
        let data = pagebara_content::page_contents(doc, pd);
        self.stream(&data, &res, Some(ctm), 0);
        let annots = doc.get_in(pd, b"Annots");
        for a in annots.as_array().unwrap_or_default() {
            let ao = doc.resolve(a);
            let Some(ad) = ao.as_dict() else { continue };
            self.appearances(ad, &res, ctm);
        }
    }

    fn appearances(&mut self, ad: &Dict, page_res: &Res, page_ctm: Matrix) {
        let doc = self.doc;
        let ap = doc.get_in(ad, b"AP");
        let Some(ap) = ap.as_dict() else { return };
        let rect = doc.get_in(ad, b"Rect");
        let rect = Rect::from_object(&rect);
        for (_, v) in ap.iter() {
            let streams: Vec<Object> = match &*doc.resolve(v) {
                Object::Stream(_) => vec![v.clone()],
                Object::Dict(d) => d.iter().map(|(_, s)| s.clone()).collect(),
                _ => Vec::new(),
            };
            for s in streams {
                let Some(r) = s.as_objref() else { continue };
                let so = doc.resolve(&s);
                let Some(st) = so.as_stream() else { continue };
                let ctm = rect.and_then(|rect| appearance_matrix(doc, st, rect));
                // Appearances are drawn in default page space, so no form matrix again.
                let ctm = ctm.map(|m| m.then(&page_ctm));
                self.form(r.num, st, page_res, ctm, true, 0);
            }
        }
    }

    /// Reads a form XObject or tiling pattern drawn at `ctm` (`None`: unknown). With
    /// `mapped`, `ctm` already includes the form's own matrix.
    fn form(
        &mut self,
        num: u32,
        s: &Stream,
        parent: &Res,
        ctm: Option<Matrix>,
        mapped: bool,
        depth: u32,
    ) {
        let doc = self.doc;
        let res = own_res(doc, num, &s.dict);
        if res.is_some() {
            self.u.holders.insert(num);
        }
        let res = res.unwrap_or_else(|| parent.clone());
        let ctm = ctm.map(|c| {
            if mapped {
                c
            } else {
                let m = doc
                    .get_in(&s.dict, b"Matrix")
                    .as_array()
                    .and_then(|_| Matrix::from_object(&doc.get_in(&s.dict, b"Matrix")))
                    .unwrap_or_default();
                m.then(&c)
            }
        });
        let bucket = ctm.map_or(i32::MIN, |m| (scale_of(&m).max(1e-6).log2() * 4.0) as i32);
        if !self.visited.insert((num, res.loc, bucket)) {
            return;
        }
        match doc.decode(s) {
            Ok(d) => self.stream(&d.data, &res, ctm, depth + 1),
            Err(_) => self.u.uncertain = true,
        }
    }

    fn type3(&mut self, r: ObjRef, d: &Dict, parent: &Res, depth: u32) {
        let doc = self.doc;
        let res = own_res(doc, r.num, d);
        if res.is_some() {
            self.u.holders.insert(r.num);
        }
        let res = res.unwrap_or_else(|| parent.clone());
        if !self.visited.insert((r.num, res.loc, i32::MIN)) {
            return;
        }
        let Some(procs) = doc.get_in(d, b"CharProcs").as_dict().cloned() else {
            return;
        };
        for (_, p) in procs.iter() {
            let po = doc.resolve(p);
            let Some(s) = po.as_stream() else { continue };
            match doc.decode(s) {
                Ok(data) => self.stream(&data.data, &res, None, depth + 1),
                Err(_) => self.u.uncertain = true,
            }
        }
    }

    fn font(&mut self, o: &Object) -> Option<(u32, Arc<Font>)> {
        let r = o.as_objref();
        if let Some(r) = r
            && let Some(f) = self.fonts.get(&r.num)
        {
            return f.clone();
        }
        let doc = self.doc;
        let d = doc.resolve(o).as_dict()?.clone();
        let v = pagebara_font::pdf::font_file_ref(doc, &d).map(|p| {
            let font = Arc::new(Font::load_with(doc, &d, Some(&*self.loader)));
            (p.num, font)
        });
        if let Some(r) = r {
            self.fonts.insert(r.num, v.clone());
        }
        v
    }

    /// Notes that `name` of category `cat` is asked for in `res`.
    fn use_name(&mut self, res: &Res, cat: &'static [u8], name: &[u8]) -> Option<Object> {
        let doc = self.doc;
        let entry = res.dict.get(cat);
        let found = entry.and_then(|c| doc.resolve(c).as_dict()?.get(name).cloned());
        if let Some(loc) = res.loc {
            let key = match entry.and_then(Object::as_objref) {
                Some(r) => CatKey::Obj(r.num),
                None => CatKey::In(loc, cat),
            };
            self.u.names.entry(key).or_default().insert(name.to_vec());
        }
        if found.is_none() {
            self.u.uncertain = true;
        }
        found
    }

    fn image(&mut self, o: &Object, ctm: Option<Matrix>) {
        let doc = self.doc;
        let Some(r) = o.as_objref() else { return };
        let so = doc.resolve(o);
        let Some(s) = so.as_stream() else { return };
        let ppi = match ctm {
            Some(m) => {
                let w = doc.get_in(&s.dict, b"Width").as_f64().unwrap_or(0.0) as f32;
                let h = doc.get_in(&s.dict, b"Height").as_f64().unwrap_or(0.0) as f32;
                let (sx, sy) = (m.a.hypot(m.b) / 72.0, m.c.hypot(m.d) / 72.0);
                if sx > 1e-4 && sy > 1e-4 {
                    (w / sx).max(h / sy)
                } else {
                    f32::INFINITY
                }
            }
            None => f32::INFINITY,
        };
        let e = self.u.images.entry(r.num).or_insert(0.0);
        *e = e.max(ppi);
        // Masks are drawn over the same square.
        for k in [&b"SMask"[..], b"Mask"] {
            if let Some(Object::Ref(m)) = s.dict.get(k) {
                let mo = doc.get(*m);
                if let Some(ms) = mo.as_deref().and_then(Object::as_stream) {
                    let mw = doc.get_in(&ms.dict, b"Width").as_f64().unwrap_or(0.0) as f32;
                    let w = doc.get_in(&s.dict, b"Width").as_f64().unwrap_or(1.0) as f32;
                    let mppi = if w > 0.0 { ppi * mw / w } else { f32::INFINITY };
                    let e = self.u.images.entry(m.num).or_insert(0.0);
                    *e = e.max(mppi);
                }
            }
        }
    }

    /// Reads a content stream drawn with `res` at `ctm` (`None`: unknown scale).
    fn stream(&mut self, data: &[u8], res: &Res, ctm: Option<Matrix>, depth: u32) {
        if depth > MAX_DEPTH {
            self.u.uncertain = true;
            return;
        }
        let doc = self.doc;
        // Type 3 fonts and patterns are read whether or not the stream uses them, as their
        // own drawing may need glyphs and images.
        if let Some(fonts) = doc.get_in(&res.dict, b"Font").as_dict().cloned() {
            for (_, v) in fonts.iter() {
                if let Some(r) = v.as_objref()
                    && let Some(d) = doc.resolve(v).as_dict()
                    && d.name(b"Subtype") == Some(b"Type3")
                {
                    let d = d.clone();
                    self.type3(r, &d, res, depth);
                }
            }
        }
        if let Some(pats) = doc.get_in(&res.dict, b"Pattern").as_dict().cloned() {
            for (_, v) in pats.iter() {
                let Some(r) = v.as_objref() else { continue };
                let po = doc.resolve(v);
                if let Some(s) = po.as_stream() {
                    self.form(r.num, s, res, None, true, depth);
                }
            }
        }
        if let Some(gss) = doc.get_in(&res.dict, b"ExtGState").as_dict().cloned() {
            for (_, v) in gss.iter() {
                let Some(gs) = doc.resolve(v).as_dict().cloned() else {
                    continue;
                };
                if let Some(sm) = doc.get_in(&gs, b"SMask").as_dict().cloned()
                    && let Some(g) = sm.get(b"G")
                    && let Some(r) = g.as_objref()
                    && let Some(s) = doc.resolve(g).as_stream()
                {
                    self.form(r.num, s, res, None, true, depth);
                }
            }
        }
        let mut ctm = ctm;
        let mut stack: Vec<Option<Matrix>> = Vec::new();
        let mut font: Option<(u32, Arc<Font>)> = None;
        let mut buf = Vec::new();
        let mut p = ContentParser::new(data);
        while let Some(i) = p.next() {
            match i.op {
                Op::Save => stack.push(ctm),
                Op::Restore => {
                    if let Some(c) = stack.pop() {
                        ctm = c;
                    }
                }
                Op::Concat => {
                    if let Some(n) = i.numbers::<6>() {
                        ctm = ctm.map(|c| Matrix::from_array(n).then(&c));
                    }
                }
                Op::Font => {
                    let name = i.args.first().and_then(|a| a.name());
                    font = name
                        .and_then(|n| self.use_name(res, b"Font", &n))
                        .and_then(|o| self.font(&o));
                }
                Op::ExtGState => {
                    let gs = i
                        .args
                        .last()
                        .and_then(|a| a.name())
                        .and_then(|n| self.use_name(res, b"ExtGState", &n));
                    if let Some(gs) = gs
                        && let Some(gs) = doc.resolve(&gs).as_dict()
                        && let Some(a) = doc.get_in(gs, b"Font").as_array()
                        && let Some(f) = a.first()
                    {
                        font = self.font(f);
                    }
                }
                Op::FillSpace | Op::StrokeSpace => {
                    if let Some(n) = i.args.last().and_then(|a| a.name())
                        && !is_device_space(&n)
                    {
                        self.use_name(res, b"ColorSpace", &n);
                    }
                }
                Op::FillColorN | Op::StrokeColorN => {
                    if let Some(n) = i.args.last().and_then(|a| a.name()) {
                        self.use_name(res, b"Pattern", &n);
                    }
                }
                Op::Shade => {
                    if let Some(n) = i.args.last().and_then(|a| a.name()) {
                        self.use_name(res, b"Shading", &n);
                    }
                }
                Op::BeginMarkedProps | Op::MarkPointProps => {
                    if let Some(n) = i.args.last().and_then(|a| a.name()) {
                        self.use_name(res, b"Properties", &n);
                    }
                }
                Op::InlineImage => {
                    // Every colour space key counts (readers differ in which of duplicate
                    // `/CS` and `/ColorSpace` entries they take), and so does the base of
                    // an indexed space given as an array.
                    let mut names: Vec<Vec<u8>> = Vec::new();
                    for kv in i.args.chunks(2) {
                        let [k, v] = kv else { continue };
                        if !matches!(k.name().as_deref(), Some(b"CS" | b"ColorSpace")) {
                            continue;
                        }
                        let parts = match v {
                            Operand::Array { .. } => i.array(v),
                            v => std::slice::from_ref(v),
                        };
                        names.extend(
                            parts
                                .iter()
                                .filter_map(|p| p.name().map(|n| n.into_owned())),
                        );
                    }
                    for n in names {
                        if !is_device_space(&n)
                            && !matches!(&n[..], b"G" | b"RGB" | b"CMYK" | b"I" | b"Indexed")
                        {
                            self.use_name(res, b"ColorSpace", &n);
                        }
                    }
                }
                Op::ShowText | Op::ShowTextArray | Op::NextLineShow | Op::NextLineSpacingShow => {
                    let Some((prog, f)) = &font else { continue };
                    let items = match (i.op, i.args.last()) {
                        (Op::ShowTextArray, Some(a)) => i.array(a),
                        (_, Some(s)) => std::slice::from_ref(s),
                        _ => continue,
                    };
                    let g = self.u.glyphs.entry(*prog).or_default();
                    for it in items {
                        buf.clear();
                        if !it.string_into(&mut buf) {
                            continue;
                        }
                        let mut s = &buf[..];
                        while !s.is_empty() {
                            let (code, len) = f.next_code(s);
                            s = &s[len.clamp(1, s.len())..];
                            match f.glyph_id(code) {
                                Some(id) => {
                                    g.insert(u16::try_from(id).unwrap_or(0));
                                }
                                None => {
                                    self.u.keep_programs.insert(*prog);
                                }
                            }
                        }
                    }
                }
                Op::XObject => {
                    let Some(n) = i.args.last().and_then(|a| a.name()) else {
                        continue;
                    };
                    let Some(o) = self.use_name(res, b"XObject", &n) else {
                        continue;
                    };
                    let so = doc.resolve(&o);
                    let Some(s) = so.as_stream() else { continue };
                    match s.dict.name(b"Subtype") {
                        Some(b"Form") => {
                            if let Some(r) = o.as_objref() {
                                let s = s.clone();
                                self.form(r.num, &s, res, ctm, false, depth);
                            }
                        }
                        Some(b"Image") => self.image(&o, ctm),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
}

fn is_device_space(n: &[u8]) -> bool {
    matches!(n, b"DeviceGray" | b"DeviceRGB" | b"DeviceCMYK" | b"Pattern")
}

/// How much a matrix enlarges (the geometric mean of its axis scales).
fn scale_of(m: &Matrix) -> f32 {
    (m.a.hypot(m.b) * m.c.hypot(m.d)).sqrt()
}

/// The matrix an annotation appearance is drawn with (§12.5.5): its form matrix, then the
/// map of its transformed bounding box onto the annotation rectangle.
fn appearance_matrix(doc: &Document, s: &Stream, rect: Rect) -> Option<Matrix> {
    let bbox = Rect::from_object(&doc.get_in(&s.dict, b"BBox"))?;
    let m = Matrix::from_object(&doc.get_in(&s.dict, b"Matrix")).unwrap_or_default();
    let b = bbox.transform(&m);
    if b.width() <= 0.0 || b.height() <= 0.0 {
        return None;
    }
    let sx = rect.width() / b.width();
    let sy = rect.height() / b.height();
    let a = Matrix::from_array([sx, 0.0, 0.0, sy, rect.x0 - b.x0 * sx, rect.y0 - b.y0 * sy]);
    Some(m.then(&a))
}
