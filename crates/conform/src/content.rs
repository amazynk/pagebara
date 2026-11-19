//! What the content streams of a document use, for the checks that depend on it: device
//! colour, fonts with the codes they show, invisible text, nesting depth, and content
//! outside marked-content sequences (PDF/UA).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use pagebara_content::font::{Font, ProgramLoader, embedded_programs};
use pagebara_content::{ContentParser, Op};
use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};

const MAX_DEPTH: u32 = 24;

/// Device colour families.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Device {
    Gray,
    Rgb,
    Cmyk,
}

impl Device {
    pub fn default_key(self) -> &'static [u8] {
        match self {
            Device::Gray => b"DefaultGray",
            Device::Rgb => b"DefaultRGB",
            Device::Cmyk => b"DefaultCMYK",
        }
    }
}

/// A font as the content uses it.
pub(crate) struct FontUse {
    pub r: Option<ObjRef>,
    pub dict: Dict,
    pub font: Arc<Font>,
    pub codes: BTreeSet<u32>,
    /// Drawn with a text rendering mode other than 3 (invisible).
    pub visible: bool,
    pub page: Option<usize>,
}

#[derive(Default)]
pub(crate) struct Facts {
    /// Device colour used without a default colour space, and the first page.
    pub device: HashMap<Device, Option<usize>>,
    pub fonts: Vec<FontUse>,
    /// The deepest `q` nesting.
    pub q_depth: u32,
    /// Pages with painting outside marked content (neither tagged nor artifact).
    pub untagged: BTreeSet<usize>,
    /// Marked-content ids used on each page.
    pub mcids: HashMap<usize, HashSet<i64>>,
}

/// The device family a colour space object falls back on, if any.
pub(crate) fn device_of(doc: &Document, cs: &Object, depth: u32) -> Option<Device> {
    if depth > 8 {
        return None;
    }
    match &*doc.resolve(cs) {
        Object::Name(n) => match n.as_bytes() {
            b"DeviceGray" | b"G" => Some(Device::Gray),
            b"DeviceRGB" | b"RGB" => Some(Device::Rgb),
            b"DeviceCMYK" | b"CMYK" => Some(Device::Cmyk),
            _ => None,
        },
        Object::Array(a) => match a.first().and_then(Object::as_name) {
            Some(b"Indexed" | b"I") => device_of(doc, a.get(1)?, depth + 1),
            Some(b"Pattern") => device_of(doc, a.get(1)?, depth + 1),
            Some(b"Separation") => device_of(doc, a.get(2)?, depth + 1),
            Some(b"DeviceN") => device_of(doc, a.get(2)?, depth + 1),
            _ => None,
        },
        _ => None,
    }
}

struct Res {
    dict: Dict,
}

impl Res {
    fn sub(&self, doc: &Document, cat: &[u8], name: &[u8]) -> Option<Object> {
        doc.get_in(&self.dict, cat).as_dict()?.get(name).cloned()
    }

    fn has_default(&self, doc: &Document, d: Device) -> bool {
        doc.get_in(&self.dict, b"ColorSpace")
            .as_dict()
            .is_some_and(|c| c.contains(d.default_key()))
    }
}

pub(crate) struct Scanner<'d> {
    doc: &'d Document,
    loader: Arc<ProgramLoader>,
    font_index: HashMap<u32, usize>,
    visited: HashSet<(u32, bool)>,
    pub facts: Facts,
}

impl<'d> Scanner<'d> {
    pub fn run(doc: &'d Document) -> Facts {
        let mut s = Scanner {
            doc,
            loader: embedded_programs(),
            font_index: HashMap::new(),
            visited: HashSet::new(),
            facts: Facts::default(),
        };
        let tree = pagebara_pages::PageTree::load(doc);
        for (i, page) in tree.pages().iter().enumerate() {
            let Some(po) = doc.get(page.r) else { continue };
            let Some(pd) = po.as_dict() else { continue };
            let res = Res {
                dict: inherited_resources(doc, page.r),
            };
            if let Some(g) = doc.get_in(pd, b"Group").as_dict()
                && let Some(cs) = g.get(b"CS")
            {
                s.device(device_of(doc, cs, 0), &res, Some(i));
            }
            let data = pagebara_content::page_contents(doc, pd);
            s.stream(&data, &res, Some(i), false, true, 0);
            let annots = doc.get_in(pd, b"Annots");
            for a in annots.as_array().unwrap_or_default() {
                let ao = doc.resolve(a);
                let Some(ad) = ao.as_dict() else { continue };
                let ap = doc.get_in(ad, b"AP");
                let Some(ap) = ap.as_dict() else { continue };
                for (_, v) in ap.iter() {
                    let streams: Vec<Object> = match &*doc.resolve(v) {
                        Object::Stream(_) => vec![v.clone()],
                        Object::Dict(d) => d.iter().map(|(_, s)| s.clone()).collect(),
                        _ => Vec::new(),
                    };
                    for st in streams {
                        let so = doc.resolve(&st);
                        if let (Some(r), Some(f)) = (st.as_objref(), so.as_stream()) {
                            let f = f.clone();
                            s.form(r.num, &f, &res, Some(i), true, false, 0);
                        }
                    }
                }
            }
        }
        s.facts
    }

    fn device(&mut self, d: Option<Device>, res: &Res, page: Option<usize>) {
        if let Some(d) = d
            && !res.has_default(self.doc, d)
        {
            self.facts.device.entry(d).or_insert(page);
        }
    }

    fn font(&mut self, o: &Object, page: Option<usize>) -> Option<usize> {
        let r = o.as_objref();
        if let Some(r) = r
            && let Some(&i) = self.font_index.get(&r.num)
        {
            return Some(i);
        }
        let doc = self.doc;
        let d = doc.resolve(o).as_dict()?.clone();
        let font = Arc::new(Font::load_with(doc, &d, Some(&*self.loader)));
        self.facts.fonts.push(FontUse {
            r,
            dict: d,
            font,
            codes: BTreeSet::new(),
            visible: false,
            page,
        });
        let i = self.facts.fonts.len() - 1;
        if let Some(r) = r {
            self.font_index.insert(r.num, i);
        }
        Some(i)
    }

    #[allow(clippy::too_many_arguments)]
    fn form(
        &mut self,
        num: u32,
        s: &Stream,
        parent: &Res,
        page: Option<usize>,
        covered: bool,
        tagged_check: bool,
        depth: u32,
    ) {
        if !self.visited.insert((num, covered)) {
            return;
        }
        let doc = self.doc;
        let res = match doc.get_in(&s.dict, b"Resources").as_dict() {
            Some(d) => Res { dict: d.clone() },
            None => Res {
                dict: parent.dict.clone(),
            },
        };
        if let Some(g) = doc.get_in(&s.dict, b"Group").as_dict()
            && let Some(cs) = g.get(b"CS")
        {
            self.device(device_of(doc, cs, 0), &res, page);
        }
        if let Ok(d) = doc.decode(s) {
            self.stream(&d.data, &res, page, covered, tagged_check, depth + 1);
        }
    }

    fn stream(
        &mut self,
        data: &[u8],
        res: &Res,
        page: Option<usize>,
        covered: bool,
        tagged_check: bool,
        depth: u32,
    ) {
        if depth > MAX_DEPTH {
            return;
        }
        let doc = self.doc;
        let mut q = 0u32;
        let mut mode = 0i64;
        let mut font: Option<usize> = None;
        // Marked content: whether each open sequence tags (MCID) or marks an artifact.
        let mut marks: Vec<bool> = Vec::new();
        let mut fill_space: Option<Device> = Some(Device::Gray);
        let mut stroke_space: Option<Device> = Some(Device::Gray);
        let mut buf = Vec::new();
        let mut p = ContentParser::new(data);
        while let Some(i) = p.next() {
            let tagged = covered || marks.iter().any(|&m| m);
            let paints = matches!(
                i.op,
                Op::Stroke
                    | Op::CloseStroke
                    | Op::Fill
                    | Op::FillCompat
                    | Op::FillEvenOdd
                    | Op::FillStroke
                    | Op::FillStrokeEvenOdd
                    | Op::CloseFillStroke
                    | Op::CloseFillStrokeEvenOdd
                    | Op::ShowText
                    | Op::ShowTextArray
                    | Op::NextLineShow
                    | Op::NextLineSpacingShow
                    | Op::Shade
                    | Op::InlineImage
            );
            if paints && tagged_check && !tagged {
                self.facts.untagged.extend(page);
            }
            match i.op {
                Op::Save => {
                    q += 1;
                    self.facts.q_depth = self.facts.q_depth.max(q);
                }
                Op::Restore => q = q.saturating_sub(1),
                Op::BeginMarked | Op::BeginMarkedProps => {
                    let tag = i.args.first().and_then(|a| a.name());
                    let artifact = tag.as_deref() == Some(b"Artifact");
                    let mcid = i.args.get(1).and_then(|a| {
                        let props = match i.dict(a) {
                            Some(d) => Some(d.clone()),
                            None => a
                                .name()
                                .and_then(|n| res.sub(doc, b"Properties", &n))
                                .and_then(|o| doc.resolve(&o).as_dict().cloned()),
                        };
                        props.and_then(|p| doc.get_in(&p, b"MCID").as_int())
                    });
                    if let (Some(m), Some(pg)) = (mcid, page) {
                        self.facts.mcids.entry(pg).or_default().insert(m);
                    }
                    marks.push(artifact || mcid.is_some());
                }
                Op::EndMarked => {
                    marks.pop();
                }
                Op::FillGray => self.device(Some(Device::Gray), res, page),
                Op::StrokeGray => self.device(Some(Device::Gray), res, page),
                Op::FillRgb | Op::StrokeRgb => self.device(Some(Device::Rgb), res, page),
                Op::FillCmyk | Op::StrokeCmyk => self.device(Some(Device::Cmyk), res, page),
                Op::FillSpace | Op::StrokeSpace => {
                    let d = i.args.last().and_then(|a| a.name()).and_then(|n| {
                        let o = res
                            .sub(doc, b"ColorSpace", &n)
                            .unwrap_or_else(|| Object::name(&n));
                        device_of(doc, &o, 0)
                    });
                    if i.op == Op::FillSpace {
                        fill_space = d;
                    } else {
                        stroke_space = d;
                    }
                }
                Op::FillColor | Op::FillColorN => self.device(fill_space, res, page),
                Op::StrokeColor | Op::StrokeColorN => self.device(stroke_space, res, page),
                Op::Shade => {
                    if let Some(sh) = i
                        .args
                        .last()
                        .and_then(|a| a.name())
                        .and_then(|n| res.sub(doc, b"Shading", &n))
                    {
                        let sh = doc.resolve(&sh);
                        let d = match &*sh {
                            Object::Dict(d) => d.get(b"ColorSpace").cloned(),
                            Object::Stream(s) => s.dict.get(b"ColorSpace").cloned(),
                            _ => None,
                        };
                        if let Some(cs) = d {
                            self.device(device_of(doc, &cs, 0), res, page);
                        }
                    }
                }
                Op::InlineImage => {
                    let cs = i.args.chunks(2).find_map(|kv| match kv {
                        [k, v] if matches!(k.name().as_deref(), Some(b"CS" | b"ColorSpace")) => {
                            Some(i.object(v))
                        }
                        _ => None,
                    });
                    if let Some(cs) = cs {
                        let cs = match cs.as_name().and_then(|n| res.sub(doc, b"ColorSpace", n)) {
                            Some(o) => o,
                            None => cs,
                        };
                        self.device(device_of(doc, &cs, 0), res, page);
                    }
                }
                Op::TextRender => mode = i.number().unwrap_or(0.0) as i64,
                Op::Font => {
                    font = i
                        .args
                        .first()
                        .and_then(|a| a.name())
                        .and_then(|n| res.sub(doc, b"Font", &n))
                        .and_then(|o| self.font(&o, page));
                }
                Op::ShowText | Op::ShowTextArray | Op::NextLineShow | Op::NextLineSpacingShow => {
                    let Some(fi) = font else { continue };
                    let items = match (i.op, i.args.last()) {
                        (Op::ShowTextArray, Some(a)) => i.array(a),
                        (_, Some(s)) => std::slice::from_ref(s),
                        _ => continue,
                    };
                    let f = self.facts.fonts[fi].font.clone();
                    for it in items {
                        buf.clear();
                        if !it.string_into(&mut buf) {
                            continue;
                        }
                        let mut s = &buf[..];
                        while !s.is_empty() {
                            let (code, len) = f.next_code(s);
                            s = &s[len.clamp(1, s.len())..];
                            self.facts.fonts[fi].codes.insert(code);
                        }
                    }
                    if mode != 3 {
                        self.facts.fonts[fi].visible = true;
                    }
                }
                Op::XObject => {
                    let Some(o) = i
                        .args
                        .last()
                        .and_then(|a| a.name())
                        .and_then(|n| res.sub(doc, b"XObject", &n))
                    else {
                        continue;
                    };
                    let so = doc.resolve(&o);
                    let Some(s) = so.as_stream() else { continue };
                    match s.dict.name(b"Subtype") {
                        Some(b"Form") => {
                            if let Some(r) = o.as_objref() {
                                let s = s.clone();
                                self.form(r.num, &s, res, page, tagged, tagged_check, depth);
                            }
                        }
                        Some(b"Image") => {
                            if tagged_check && !tagged {
                                self.facts.untagged.extend(page);
                            }
                            if doc.get_in(&s.dict, b"ImageMask").as_bool() != Some(true)
                                && let Some(cs) = s.dict.get(b"ColorSpace")
                            {
                                let d = device_of(doc, cs, 0);
                                self.device(d, res, page);
                            } else if doc.get_in(&s.dict, b"ImageMask").as_bool() == Some(true) {
                                self.device(fill_space, res, page);
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
}

/// A page's resources, inherited from the page tree when it has none.
pub(crate) fn inherited_resources(doc: &Document, page: ObjRef) -> Dict {
    let mut cur = Some(page);
    for _ in 0..64 {
        let Some(r) = cur else { break };
        let Some(o) = doc.get(r) else { break };
        let Some(d) = o.as_dict() else { break };
        if let Some(res) = doc.get_in(d, b"Resources").as_dict() {
            return res.clone();
        }
        cur = d.get(b"Parent").and_then(Object::as_objref);
    }
    Dict::new()
}
