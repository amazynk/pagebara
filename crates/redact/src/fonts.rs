//! Fonts lose the shapes of removed text: a glyph that redaction removed and that nothing
//! in the document still shows is blanked in its embedded font program (an empty TrueType
//! glyph, a CFF or Type 1 `endchar`, a Type 3 glyph procedure removed), its name goes from
//! the program, `/Differences` and `/CharSet`, its `/ToUnicode` entry and its bit in
//! `/CIDSet` too.
//!
//! What still shows a glyph is found by reading every content stream the document can draw:
//! pages, forms, annotation appearances, tiling patterns, soft masks and Type 3 glyphs, then
//! any form or pattern reachable some other way. A font is left alone when a stream that
//! may use it cannot be read with certainty.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use pagebara_content::font::{Font, FontKind, ProgramLoader, embedded_programs};
use pagebara_content::{ContentParser, Op};
use pagebara_font::subset::blank::{blank, blank_type1};
use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream, write_object};

/// Codes removed per font dictionary, with the font that reads them.
pub(crate) type Removed = HashMap<ObjRef, (Arc<Font>, BTreeSet<u32>)>;

/// Adds the codes of `more` to `into`.
pub(crate) fn merge(into: &mut Removed, more: Removed) {
    for (font, (f, codes)) in more {
        into.entry(font)
            .or_insert_with(|| (f, BTreeSet::new()))
            .1
            .extend(codes);
    }
}

/// Streams nested deeper than this are not followed (and make the result uncertain).
const MAX_DEPTH: u32 = 32;

/// What draws glyphs: an embedded program (by the object holding it), or a Type 3 font.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Key {
    Program(ObjRef),
    Type3(ObjRef),
}

#[derive(Default)]
struct Glyphs {
    gids: BTreeSet<u16>,
    names: BTreeSet<Vec<u8>>,
}

/// What the fonts lost.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pruned {
    pub glyphs: usize,
    pub fonts: usize,
}

/// Blanks the glyphs of `removed` that the document no longer shows.
pub(crate) fn prune(doc: &mut Document, removed: &Removed, warnings: &mut Vec<String>) -> Pruned {
    let mut want: HashMap<Key, Glyphs> = HashMap::new();
    for (&r, (font, codes)) in removed {
        let Some(key) = key_of(doc, r, font) else {
            continue;
        };
        let g = want.entry(key).or_default();
        for &c in codes {
            match key {
                Key::Type3(_) => {
                    if let Some(n) = font.glyph_name(c) {
                        g.names.insert(n.to_vec());
                    }
                }
                Key::Program(_) => {
                    if let Some(id) = font.glyph_id(c).and_then(|g| u16::try_from(g).ok()) {
                        g.gids.insert(id);
                    }
                }
            }
        }
    }
    want.retain(|_, g| !g.gids.is_empty() || !g.names.is_empty());
    if want.is_empty() {
        return Pruned::default();
    }
    let mut scan = Scan::new(want.keys().copied().collect());
    scan.document(doc);
    if scan.uncertain {
        warnings.push(
            "fonts keep the shapes of removed glyphs: not every use of them could be read".into(),
        );
        return Pruned::default();
    }
    let mut out = Pruned::default();
    let mut gone: HashMap<Key, Glyphs> = HashMap::new();
    for (key, g) in want {
        let used = scan.used.remove(&key).unwrap_or_default();
        let g = Glyphs {
            gids: g.gids.difference(&used.gids).copied().collect(),
            names: g.names.difference(&used.names).cloned().collect(),
        };
        let done = match key {
            Key::Program(r) if !scan.keep.contains(&r) => Glyphs {
                gids: blank_program(doc, r, &g.gids).into_iter().collect(),
                names: BTreeSet::new(),
            },
            Key::Program(_) => Glyphs::default(),
            Key::Type3(r) => Glyphs {
                gids: BTreeSet::new(),
                names: blank_type3(doc, r, &g.names).into_iter().collect(),
            },
        };
        let n = done.gids.len() + done.names.len();
        if n == 0 {
            continue;
        }
        out.fonts += 1;
        out.glyphs += n;
        gone.insert(key, done);
    }
    // Tables that list the glyphs a font has.
    for (&r, (font, codes)) in removed {
        let Some(key) = key_of(doc, r, font) else {
            continue;
        };
        let Some(g) = gone.get(&key) else { continue };
        let lost: BTreeSet<u32> = codes
            .iter()
            .copied()
            .filter(|&c| match key {
                Key::Type3(_) => font.glyph_name(c).is_some_and(|n| g.names.contains(n)),
                Key::Program(_) => font
                    .glyph_id(c)
                    .and_then(|i| u16::try_from(i).ok())
                    .is_some_and(|i| g.gids.contains(&i)),
            })
            .collect();
        forget_codes(doc, r, font, &lost);
    }
    out
}

fn key_of(doc: &Document, r: ObjRef, font: &Font) -> Option<Key> {
    if font.kind == FontKind::Type3 {
        return Some(Key::Type3(r));
    }
    pagebara_font::pdf::font_file_ref(doc, &font.dict).map(Key::Program)
}

/// Rewrites a font program without `gids`. Returns the glyphs blanked.
fn blank_program(doc: &mut Document, r: ObjRef, gids: &BTreeSet<u16>) -> Vec<u16> {
    if gids.is_empty() {
        return Vec::new();
    }
    let Some(o) = doc.get(r) else {
        return Vec::new();
    };
    let Some(s) = o.as_stream() else {
        return Vec::new();
    };
    let Ok(data) = doc.decode(s) else {
        return Vec::new();
    };
    let len = |k: &[u8]| {
        doc.get_in(&s.dict, k)
            .as_int()
            .and_then(|v| usize::try_from(v).ok())
    };
    let b = match (len(b"Length1"), len(b"Length2")) {
        (Some(a), Some(b)) => blank_type1(&data.data, gids, Some([a, b])),
        _ => None,
    };
    let Some(b) = b.or_else(|| blank(&data.data, gids)) else {
        return Vec::new();
    };
    if b.glyphs.is_empty() {
        return b.glyphs;
    }
    let mut d = Dict::new();
    for (k, v) in s.dict.iter() {
        if !matches!(
            k.as_bytes(),
            b"Filter" | b"DecodeParms" | b"Length" | b"Length1" | b"Length2" | b"Length3"
        ) {
            d.set(k.as_bytes(), v.clone());
        }
    }
    match b.lengths {
        Some(l) => {
            for (k, v) in [&b"Length1"[..], b"Length2", b"Length3"].into_iter().zip(l) {
                d.set(k, Object::Int(v as i64));
            }
        }
        None if s.dict.contains(b"Length1") => {
            d.set(b"Length1", Object::Int(b.data.len() as i64));
        }
        None => {}
    }
    d.set(b"Filter", Object::name(b"FlateDecode"));
    doc.set(
        r,
        Stream::new(d, pagebara_filters::encode_flate(&b.data, 6)),
    );
    b.glyphs
}

/// Removes the glyph procedures `names` from a Type 3 font (`/Differences` loses their
/// names too, see [`forget_codes`]). Returns the names removed.
fn blank_type3(doc: &mut Document, font: ObjRef, names: &BTreeSet<Vec<u8>>) -> Vec<Vec<u8>> {
    let Some(fd) = doc.get(font).and_then(|o| o.as_dict().cloned()) else {
        return Vec::new();
    };
    let Some(mut procs) = doc.get_in(&fd, b"CharProcs").as_dict().cloned() else {
        return Vec::new();
    };
    let done: Vec<Vec<u8>> = names
        .iter()
        .filter(|n| procs.remove(n).is_some())
        .cloned()
        .collect();
    if done.is_empty() {
        return done;
    }
    match fd.get(b"CharProcs") {
        // Another font with the same glyph procedures keeps them.
        Some(Object::Ref(pr)) if !shared_procs(doc, font, *pr) => {
            let pr = *pr;
            doc.set(pr, procs);
        }
        _ => {
            let mut fd = fd;
            fd.set(b"CharProcs", procs);
            doc.set(font, fd);
        }
    }
    done
}

/// Whether a font other than `font` has `/CharProcs` `procs`.
fn shared_procs(doc: &Document, font: ObjRef, procs: ObjRef) -> bool {
    reachable(doc).into_iter().any(|r| {
        r != font
            && doc.get(r).is_some_and(|o| {
                o.as_dict()
                    .and_then(|d| d.get(b"CharProcs"))
                    .is_some_and(|p| p.as_objref() == Some(procs))
            })
    })
}

/// Drops codes whose glyphs are gone from the font's `/ToUnicode`, and the glyphs from the
/// descriptor's `/CIDSet` and `/CharSet`.
fn forget_codes(doc: &mut Document, font_ref: ObjRef, font: &Font, codes: &BTreeSet<u32>) {
    if codes.is_empty() {
        return;
    }
    let mut fd = doc
        .get(font_ref)
        .and_then(|o| o.as_dict().cloned())
        .unwrap_or_else(|| font.dict.clone());
    let mut fd_changed = false;
    if let Some(Object::Ref(tu)) = fd.get(b"ToUnicode")
        && let Some(s) = doc.get(*tu).and_then(|o| o.as_stream().cloned())
        && let Ok(d) = doc.decode(&s)
        && let Some(new) = crate::tounicode::without(&d.data, codes)
    {
        let mut sd = Dict::new();
        sd.set(b"Filter", Object::name(b"FlateDecode"));
        let r = doc.add(Stream::new(sd, pagebara_filters::encode_flate(&new, 6)));
        fd.set(b"ToUnicode", Object::Ref(r));
        fd_changed = true;
    }
    // Glyph names of the codes in `/Differences`.
    if !font.is_composite()
        && let Some(enc) = doc.get_in(&fd, b"Encoding").as_dict().cloned()
        && let Some(diffs) = doc
            .get_in(&enc, b"Differences")
            .as_array()
            .map(<[_]>::to_vec)
    {
        let mut code = 0i64;
        let mut hit = false;
        let diffs: Vec<Object> = diffs
            .into_iter()
            .map(|o| match o {
                Object::Int(c) => {
                    code = c;
                    o
                }
                Object::Name(_) => {
                    let lost = u32::try_from(code).is_ok_and(|c| codes.contains(&c));
                    code += 1;
                    hit |= lost;
                    if lost { Object::name(b".notdef") } else { o }
                }
                o => o,
            })
            .collect();
        if hit {
            let mut enc = enc;
            enc.set(b"Differences", Object::Array(diffs));
            fd.set(b"Encoding", Object::Dict(enc));
            fd_changed = true;
        }
    }
    if fd_changed {
        doc.set(font_ref, fd);
    }
    // The descriptor: of the descendant for composite fonts.
    let holder = match font.dict.get(b"DescendantFonts") {
        Some(df) => doc
            .resolve(df)
            .as_array()
            .and_then(|a| a.first()?.as_objref()),
        None => Some(font_ref),
    };
    let Some(desc_ref) = holder
        .and_then(|h| doc.get(h))
        .and_then(|o| o.as_dict()?.get(b"FontDescriptor")?.as_objref())
    else {
        return;
    };
    let Some(mut desc) = doc.get(desc_ref).and_then(|o| o.as_dict().cloned()) else {
        return;
    };
    let mut changed = false;
    if font.is_composite()
        && let Some(Object::Ref(cs)) = desc.get(b"CIDSet").cloned()
        && let Some(s) = doc.get(cs).and_then(|o| o.as_stream().cloned())
        && let Ok(d) = doc.decode(&s)
    {
        let mut bits = d.data.clone();
        for &c in codes {
            let cid = font.cid(c) as usize;
            if let Some(b) = bits.get_mut(cid / 8) {
                *b &= !(0x80 >> (cid % 8));
            }
        }
        let mut sd = Dict::new();
        sd.set(b"Filter", Object::name(b"FlateDecode"));
        let r = doc.add(Stream::new(sd, pagebara_filters::encode_flate(&bits, 6)));
        desc.set(b"CIDSet", Object::Ref(r));
        changed = true;
    }
    if !font.is_composite()
        && let Some(cs) = doc
            .get_in(&desc, b"CharSet")
            .as_string()
            .map(<[u8]>::to_vec)
    {
        let names: HashSet<&[u8]> = codes.iter().filter_map(|&c| font.glyph_name(c)).collect();
        let kept: Vec<&[u8]> = cs
            .split(|&b| b == b'/')
            .map(|n| n.trim_ascii())
            .filter(|n| !n.is_empty() && !names.contains(n))
            .collect();
        let mut s = Vec::new();
        for n in kept {
            s.push(b'/');
            s.extend_from_slice(n);
        }
        desc.set(b"CharSet", Object::string(&s));
        changed = true;
    }
    if changed {
        doc.set(desc_ref, desc);
    }
}

/// Reads every content stream for the glyphs shown with the fonts of interest.
struct Scan {
    want: HashSet<Key>,
    loader: Arc<ProgramLoader>,
    /// Font dictionaries read so far, with their key (when of interest).
    fonts: HashMap<ObjRef, Option<(Key, Arc<Font>)>>,
    used: HashMap<Key, Glyphs>,
    /// Programs some other part of the document may need whole (fonts of the interactive
    /// form's default resources that are not subsets).
    keep: HashSet<ObjRef>,
    visited: HashSet<(u32, Vec<u8>)>,
    walked: HashSet<u32>,
    uncertain: bool,
}

impl Scan {
    fn new(want: HashSet<Key>) -> Scan {
        Scan {
            want,
            loader: embedded_programs(),
            fonts: HashMap::new(),
            used: HashMap::new(),
            keep: HashSet::new(),
            visited: HashSet::new(),
            walked: HashSet::new(),
            uncertain: false,
        }
    }

    fn document(&mut self, doc: &Document) {
        let tree = pagebara_pages::PageTree::load(doc);
        let form_res = doc
            .catalog()
            .and_then(|c| c.as_dict()?.get(b"AcroForm").cloned())
            .and_then(|f| {
                doc.get_in(doc.resolve(&f).as_dict()?, b"DR")
                    .as_dict()
                    .cloned()
            });
        if let Some(dr) = &form_res {
            self.form_fonts(doc, dr);
        }
        for page in tree.pages() {
            let Some(po) = doc.get(page.r) else { continue };
            let Some(pd) = po.as_dict() else { continue };
            let res = crate::apply::inherited(doc, pd, b"Resources")
                .and_then(|o| doc.resolve(&o).as_dict().cloned())
                .unwrap_or_default();
            let data = pagebara_content::page_contents(doc, pd);
            self.stream(doc, &data, &res, false, 0);
            for a in crate::mark::annots(doc, pd) {
                let Some(ao) = doc.get(a) else { continue };
                let Some(ad) = ao.as_dict() else { continue };
                let fallback = form_res.as_ref().unwrap_or(&res);
                self.appearances(doc, ad, fallback);
            }
        }
        // Anything else that draws: forms and patterns reachable some other way.
        for r in reachable(doc) {
            if self.walked.contains(&r.num) {
                continue;
            }
            let Some(o) = doc.get(r) else { continue };
            match &*o {
                Object::Stream(s)
                    if s.dict.name(b"Subtype") == Some(b"Form")
                        || doc.get_in(&s.dict, b"PatternType").as_int() == Some(1) =>
                {
                    let res = doc
                        .get_in(&s.dict, b"Resources")
                        .as_dict()
                        .cloned()
                        .unwrap_or_default();
                    let inherits = !s.dict.contains(b"Resources");
                    self.form(doc, r.num, s, &res, inherits, 0);
                }
                Object::Dict(d) if d.name(b"Subtype") == Some(b"Type3") => {
                    self.type3(doc, r, d, &Dict::new(), true, 0);
                }
                _ => {}
            }
        }
    }

    /// Notes the programs of the form's default fonts that may draw any glyph later.
    fn form_fonts(&mut self, doc: &Document, dr: &Dict) {
        let Some(fonts) = doc.get_in(dr, b"Font").as_dict().cloned() else {
            return;
        };
        for (_, v) in fonts.iter() {
            let Some(d) = doc.resolve(v).as_dict().cloned() else {
                continue;
            };
            let subset = d
                .name(b"BaseFont")
                .is_some_and(|n| n.len() > 7 && n[6] == b'+');
            if !subset && let Some(p) = pagebara_font::pdf::font_file_ref(doc, &d) {
                self.keep.insert(p);
            }
        }
    }

    fn appearances(&mut self, doc: &Document, ad: &Dict, fallback: &Dict) {
        let ap = doc.get_in(ad, b"AP");
        let Some(ap) = ap.as_dict() else { return };
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
                self.form(doc, r.num, st, fallback, false, 0);
            }
        }
    }

    /// A form or tiling pattern drawn with `parent` resources when it has none.
    fn form(
        &mut self,
        doc: &Document,
        num: u32,
        s: &Stream,
        parent: &Dict,
        inherits: bool,
        depth: u32,
    ) {
        let res = doc
            .get_in(&s.dict, b"Resources")
            .as_dict()
            .cloned()
            .unwrap_or_else(|| parent.clone());
        let mut fp = Vec::new();
        if let Some(f) = res.get(b"Font") {
            write_object(&mut fp, f);
        }
        if !self.visited.insert((num, fp)) {
            return;
        }
        self.walked.insert(num);
        match doc.decode(s) {
            Ok(d) => self.stream(doc, &d.data, &res, inherits, depth + 1),
            Err(_) => self.uncertain = true,
        }
    }

    fn type3(
        &mut self,
        doc: &Document,
        r: ObjRef,
        d: &Dict,
        parent: &Dict,
        inherits: bool,
        depth: u32,
    ) {
        if !self.walked.insert(r.num) {
            return;
        }
        let (res, inherits) = match doc.get_in(d, b"Resources").as_dict() {
            Some(res) => (res.clone(), false),
            None => (parent.clone(), inherits),
        };
        let Some(procs) = doc.get_in(d, b"CharProcs").as_dict().cloned() else {
            return;
        };
        for (_, p) in procs.iter() {
            let po = doc.resolve(p);
            let Some(s) = po.as_stream() else { continue };
            match doc.decode(s) {
                Ok(data) => self.stream(doc, &data.data, &res, inherits, depth + 1),
                Err(_) => self.uncertain = true,
            }
        }
    }

    fn font(&mut self, doc: &Document, o: &Object) -> Option<(Key, Arc<Font>)> {
        let r = o.as_objref();
        if let Some(r) = r
            && let Some(f) = self.fonts.get(&r)
        {
            return f.clone();
        }
        let d = doc.resolve(o).as_dict()?.clone();
        let font = Arc::new(Font::load_with(doc, &d, Some(&*self.loader)));
        let key = match (font.kind, r) {
            (FontKind::Type3, Some(r)) => Some(Key::Type3(r)),
            (FontKind::Type3, None) => None,
            _ => pagebara_font::pdf::font_file_ref(doc, &d).map(Key::Program),
        };
        let v = key.filter(|k| self.want.contains(k)).map(|k| (k, font));
        if let Some(r) = r {
            self.fonts.insert(r, v.clone());
        }
        v
    }

    /// Reads a content stream drawn with `res`; `inherits` when its real resources may be
    /// another stream's (unknown here).
    fn stream(&mut self, doc: &Document, data: &[u8], res: &Dict, inherits: bool, depth: u32) {
        if depth > MAX_DEPTH {
            self.uncertain = true;
            return;
        }
        let sub = |cat: &[u8], name: &[u8]| -> Option<Object> {
            doc.get_in(res, cat).as_dict()?.get(name).cloned()
        };
        // Fonts this stream's resources hold: Type 3 glyphs and the patterns and soft
        // masks they list are read whether or not this stream uses them.
        if let Some(fonts) = doc.get_in(res, b"Font").as_dict().cloned() {
            for (_, v) in fonts.iter() {
                if let Some(r) = v.as_objref()
                    && let Some(d) = doc.resolve(v).as_dict()
                    && d.name(b"Subtype") == Some(b"Type3")
                {
                    let d = d.clone();
                    self.type3(doc, r, &d, res, inherits, depth);
                }
            }
        }
        if let Some(pats) = doc.get_in(res, b"Pattern").as_dict().cloned() {
            for (_, v) in pats.iter() {
                let Some(r) = v.as_objref() else { continue };
                let po = doc.resolve(v);
                if let Some(s) = po.as_stream() {
                    self.form(doc, r.num, s, res, inherits, depth);
                }
            }
        }
        if let Some(gss) = doc.get_in(res, b"ExtGState").as_dict().cloned() {
            for (_, v) in gss.iter() {
                let Some(gs) = doc.resolve(v).as_dict().cloned() else {
                    continue;
                };
                if let Some(sm) = doc.get_in(&gs, b"SMask").as_dict().cloned()
                    && let Some(g) = sm.get(b"G")
                    && let Some(r) = g.as_objref()
                    && let Some(s) = doc.resolve(g).as_stream()
                {
                    self.form(doc, r.num, s, res, inherits, depth);
                }
            }
        }
        let mut font: Option<(Key, Arc<Font>)> = None;
        let mut buf = Vec::new();
        let mut p = ContentParser::new(data);
        while let Some(i) = p.next() {
            match i.op {
                Op::Font => {
                    let name = i.args.first().and_then(|a| a.name());
                    font = match name.and_then(|n| sub(b"Font", &n)) {
                        Some(o) => self.font(doc, &o),
                        None => {
                            if inherits {
                                self.uncertain = true;
                            }
                            None
                        }
                    };
                }
                Op::ExtGState => {
                    let gs = i
                        .args
                        .last()
                        .and_then(|a| a.name())
                        .and_then(|n| sub(b"ExtGState", &n));
                    if let Some(gs) = gs
                        && let Some(gs) = doc.resolve(&gs).as_dict()
                        && let Some(a) = doc.get_in(gs, b"Font").as_array()
                        && let Some(f) = a.first()
                    {
                        font = self.font(doc, f);
                    }
                }
                Op::ShowText | Op::ShowTextArray | Op::NextLineShow | Op::NextLineSpacingShow => {
                    let Some((key, f)) = &font else { continue };
                    let items = match (i.op, i.args.last()) {
                        (Op::ShowTextArray, Some(a)) => i.array(a),
                        (_, Some(s)) => std::slice::from_ref(s),
                        _ => continue,
                    };
                    let g = self.used.entry(*key).or_default();
                    for it in items {
                        buf.clear();
                        if !it.string_into(&mut buf) {
                            continue;
                        }
                        let mut s = &buf[..];
                        while !s.is_empty() {
                            let (code, len) = f.next_code(s);
                            s = &s[len.clamp(1, s.len())..];
                            match key {
                                Key::Type3(_) => {
                                    if let Some(n) = f.glyph_name(code) {
                                        g.names.insert(n.to_vec());
                                    }
                                }
                                Key::Program(_) => {
                                    if let Some(id) = f.glyph_id(code) {
                                        g.gids.insert(u16::try_from(id).unwrap_or(0));
                                    }
                                }
                            }
                        }
                    }
                }
                Op::XObject => {
                    let Some(o) = i
                        .args
                        .last()
                        .and_then(|a| a.name())
                        .and_then(|n| sub(b"XObject", &n))
                    else {
                        if inherits {
                            self.uncertain = true;
                        }
                        continue;
                    };
                    let Some(r) = o.as_objref() else { continue };
                    let so = doc.resolve(&o);
                    if let Some(s) = so.as_stream()
                        && s.dict.name(b"Subtype") == Some(b"Form")
                    {
                        self.form(doc, r.num, s, res, inherits, depth);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Objects reachable from the trailer.
fn reachable(doc: &Document) -> Vec<ObjRef> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut stack: Vec<Object> = doc.trailer().iter().map(|(_, v)| v.clone()).collect();
    while let Some(o) = stack.pop() {
        match o {
            Object::Ref(r) => {
                if seen.insert(r.num)
                    && let Some(o) = doc.get(r)
                {
                    out.push(r);
                    stack.push((*o).clone());
                }
            }
            Object::Array(a) => stack.extend(a),
            Object::Dict(d) => stack.extend(d.0.into_iter().map(|(_, v)| v)),
            Object::Stream(s) => stack.extend(s.dict.0.into_iter().map(|(_, v)| v)),
            _ => {}
        }
    }
    out
}
