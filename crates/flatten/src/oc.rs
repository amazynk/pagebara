//! Optional content into plain content: marked sequences and XObjects in hidden groups
//! deleted (when applying) and every `/OC` mark removed, then the layers themselves.

use std::collections::HashMap;

use pagebara_content::ContentParser;
use pagebara_content::oc::OptionalContent;
use pagebara_content::ops::Op;
use pagebara_pages::PageTree;
use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};

use crate::{Layers, Report};

/// Forms nested deeper than this are left as they are.
const MAX_DEPTH: usize = 12;

pub(crate) fn group_count(doc: &Document) -> usize {
    doc.catalog()
        .and_then(|c| {
            let props = doc.get_in(c.as_dict()?, b"OCProperties");
            let n = doc.get_in(props.as_dict()?, b"OCGs").as_array()?.len();
            Some(n)
        })
        .unwrap_or(0)
}

/// Annotations in hidden groups go; the others lose their `/OC`.
pub(crate) fn annotations(doc: &mut Document, mode: Layers, report: &mut Report) {
    let oc = OptionalContent::new(doc);
    for p in PageTree::load(doc).pages() {
        let Some(mut pd) = doc.get(p.r).and_then(|o| o.as_dict().cloned()) else {
            continue;
        };
        let list = doc
            .get_in(&pd, b"Annots")
            .as_array()
            .map(<[Object]>::to_vec)
            .unwrap_or_default();
        let mut kept = Vec::with_capacity(list.len());
        let mut changed = false;
        for a in list {
            let Some(r) = a.as_objref() else {
                kept.push(a);
                continue;
            };
            let Some(mut d) = doc.get(r).and_then(|o| o.as_dict().cloned()) else {
                kept.push(a);
                continue;
            };
            let Some(m) = d.get(b"OC").cloned() else {
                kept.push(a);
                continue;
            };
            changed = true;
            if mode == Layers::Apply && !oc.visible(doc, &m) {
                report.hidden_removed += 1;
                doc.delete(r);
                continue;
            }
            d.remove(b"OC");
            doc.set(r, d);
            kept.push(a);
        }
        if changed {
            pd.set(b"Annots", kept);
            doc.set(p.r, pd);
        }
    }
}

struct Rewriter<'a> {
    doc: &'a Document,
    oc: OptionalContent,
    mode: Layers,
    /// Forms rewritten: old object to new stream (or `None` when unchanged).
    forms: HashMap<ObjRef, Option<Stream>>,
    removed: usize,
}

/// The effective resources of a page.
fn page_resources(doc: &Document, page: &Dict) -> Dict {
    let mut cur = page.clone();
    for _ in 0..64 {
        let r = doc.get_in(&cur, b"Resources");
        if let Some(d) = r.as_dict() {
            return d.clone();
        }
        let parent = doc.get_in(&cur, b"Parent");
        match parent.as_dict() {
            Some(p) => cur = p.clone(),
            None => break,
        }
    }
    Dict::new()
}

impl Rewriter<'_> {
    /// The rewritten content, or `None` when nothing changes.
    fn content(&mut self, src: &[u8], res: &Dict, depth: usize) -> Option<Vec<u8>> {
        let props = self.doc.get_in(res, b"Properties").as_dict().cloned();
        let xobjects = self.doc.get_in(res, b"XObject").as_dict().cloned();
        let mut out = Vec::with_capacity(src.len());
        let mut changed = false;
        // Per open sequence: whether its BDC was dropped (so its EMC goes too).
        let mut stack: Vec<bool> = Vec::new();
        // Inside a hidden sequence: its nesting and the q/Q balance skipped.
        let mut skip = 0usize;
        let mut skipped_q = 0i64;
        let mut p = ContentParser::new(src);
        let mut start = 0;
        while let Some(i) = p.next() {
            let end = i.end;
            let range = start..end;
            start = end;
            if skip > 0 {
                match i.op {
                    Op::BeginMarked | Op::BeginMarkedProps => skip += 1,
                    Op::EndMarked => skip -= 1,
                    Op::Save => skipped_q += 1,
                    Op::Restore => skipped_q -= 1,
                    _ => {}
                }
                if skip == 0 {
                    // Keep the graphics state stack as the hidden content left it.
                    let op: &[u8] = if skipped_q > 0 { b"q\n" } else { b"Q\n" };
                    for _ in 0..skipped_q.unsigned_abs() {
                        out.extend_from_slice(op);
                    }
                    skipped_q = 0;
                }
                continue;
            }
            match i.op {
                Op::BeginMarkedProps
                    if i.args.first().and_then(|a| a.raw_name()) == Some(b"OC") =>
                {
                    let target = i.args.get(1).and_then(|o| match o.raw_name() {
                        Some(n) => props.as_ref().and_then(|p| p.get(&decode_name(n)).cloned()),
                        None => i.dict(o).cloned().map(Object::Dict),
                    });
                    changed = true;
                    let visible = target.is_none_or(|t| self.oc.visible(self.doc, &t));
                    if self.mode == Layers::Apply && !visible {
                        self.removed += 1;
                        skip = 1;
                    } else {
                        stack.push(true);
                    }
                    out.push(b'\n');
                    continue;
                }
                Op::BeginMarked | Op::BeginMarkedProps => stack.push(false),
                Op::EndMarked => {
                    if stack.pop() == Some(true) {
                        out.push(b'\n');
                        continue;
                    }
                }
                Op::XObject => {
                    if let Some(n) = i.raw_name()
                        && let Some(r) = xobjects
                            .as_ref()
                            .and_then(|x| x.get(&decode_name(n)))
                            .and_then(Object::as_objref)
                        && let Some(o) = self.doc.get(r)
                        && let Some(s) = o.as_stream()
                    {
                        let is_form = s.dict.name(b"Subtype") == Some(b"Form");
                        if let Some(m) = s.dict.get(b"OC") {
                            changed = true;
                            if self.mode == Layers::Apply && !self.oc.visible(self.doc, m) {
                                self.removed += 1;
                                out.push(b'\n');
                                continue;
                            }
                            if !is_form {
                                let mut d = s.dict.clone();
                                d.remove(b"OC");
                                let data = s.data.clone();
                                self.forms.insert(r, Some(Stream { dict: d, data }));
                            }
                        }
                        if is_form && depth < MAX_DEPTH {
                            self.form(r, s, res, depth + 1);
                        }
                    }
                }
                _ => {}
            }
            out.extend_from_slice(&src[range]);
        }
        changed.then_some(out)
    }

    fn form(&mut self, r: ObjRef, s: &Stream, parent_res: &Dict, depth: usize) {
        if self.forms.get(&r).is_some_and(Option::is_some) {
            return;
        }
        // Mark it seen before descending (cycles).
        self.forms.entry(r).or_insert(None);
        let Ok(data) = self.doc.decode(s) else { return };
        let res = self
            .doc
            .get_in(&s.dict, b"Resources")
            .as_dict()
            .cloned()
            .unwrap_or_else(|| parent_res.clone());
        let new = self.content(&data.data, &res, depth);
        let has_oc = s.dict.contains(b"OC");
        if new.is_none() && !has_oc {
            return;
        }
        let mut d = s.dict.clone();
        d.remove(b"OC");
        let stream = match new {
            Some(c) => encode(d, &c),
            None => Stream {
                dict: d,
                data: s.data.clone(),
            },
        };
        self.forms.insert(r, Some(stream));
    }
}

fn decode_name(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'#'
            && i + 2 < raw.len()
            && let Ok(v) =
                u8::from_str_radix(std::str::from_utf8(&raw[i + 1..i + 3]).unwrap_or(""), 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(raw[i]);
        i += 1;
    }
    out
}

fn encode(mut dict: Dict, data: &[u8]) -> Stream {
    dict.remove(b"DecodeParms");
    dict.remove(b"Length");
    dict.set(b"Filter", Object::name(b"FlateDecode"));
    Stream::new(dict, pagebara_filters::encode_flate(data, 6))
}

/// Page content and forms without optional content; then the layers go.
pub(crate) fn content(doc: &mut Document, mode: Layers, report: &mut Report) {
    let groups = group_count(doc);
    let pages: Vec<ObjRef> = PageTree::load(doc).pages().iter().map(|p| p.r).collect();
    let snapshot = doc.clone();
    let mut rw = Rewriter {
        doc: &snapshot,
        oc: OptionalContent::new(&snapshot),
        mode,
        forms: HashMap::new(),
        removed: 0,
    };
    let mut new_pages = Vec::new();
    for page in pages {
        let Some(pd) = snapshot.get(page).and_then(|o| o.as_dict().cloned()) else {
            continue;
        };
        let res = page_resources(&snapshot, &pd);
        let src = pagebara_content::page_contents(&snapshot, &pd);
        if let Some(c) = rw.content(&src, &res, 0) {
            new_pages.push((page, pd, c));
        }
    }
    report.hidden_removed += rw.removed;
    let forms = std::mem::take(&mut rw.forms);
    drop(rw);
    for (page, mut pd, c) in new_pages {
        let s = doc.add(encode(Dict::new(), &c));
        pd.set(b"Contents", s);
        doc.set(page, pd);
    }
    for (r, s) in forms {
        if let Some(s) = s {
            doc.set(r, s);
        }
    }
    // The layers themselves.
    if let Some(cat_ref) = doc.trailer().get(b"Root").and_then(Object::as_objref)
        && let Some(mut cat) = doc.get(cat_ref).and_then(|o| o.as_dict().cloned())
        && cat.remove(b"OCProperties").is_some()
    {
        doc.set(cat_ref, cat);
        report.layers += groups;
    }
}
