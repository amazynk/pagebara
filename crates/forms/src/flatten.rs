//! Flattening: widget appearances drawn into the page content, then the fields removed.

use std::collections::{BTreeMap, HashSet};

use pagebara_annot::Rect;
use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};

use crate::content::Content;
use crate::form::{Change, Form, acroform, update_acroform};
use crate::model::MAX_DEPTH;
use crate::{Error, Result};

/// A form XObject and the matrix placing it on the page.
type Draw = (ObjRef, [f64; 6]);

/// Our marker stream that opens a graphics state before the original content.
const OPEN: &[u8] = b"q\n";

impl Form {
    /// Draws the appearances of all fields, or of those named (a name also covers the
    /// fields below it), into their pages and removes the fields and widgets. When no
    /// field is left, the form itself is removed from the catalog.
    pub fn flatten(&mut self, doc: &mut Document, names: Option<&[&str]>) -> Result<Change> {
        self.check_editable(doc)?;
        let mut change = self.prepare(doc)?;
        let covers =
            |n: &str, l: &str| n == l || n.strip_prefix(l).is_some_and(|r| r.starts_with('.'));
        if let Some(list) = names
            && let Some(m) = list
                .iter()
                .find(|l| !self.fields.iter().any(|f| covers(&f.name, l)))
        {
            return Err(Error::NoSuchField((*m).to_owned()));
        }
        let chosen: Vec<usize> = (0..self.fields.len())
            .filter(|&i| names.is_none_or(|l| l.iter().any(|l| covers(&self.fields[i].name, l))))
            .collect();

        // Page object number to (page, forms to draw with their matrices).
        let mut draws: BTreeMap<u32, (ObjRef, Vec<Draw>)> = BTreeMap::new();
        let mut widgets: HashSet<ObjRef> = HashSet::new();
        let mut field_nodes: Vec<ObjRef> = Vec::new();
        for &i in &chosen {
            let f = &self.fields[i];
            field_nodes.push(f.id);
            for w in &f.widgets {
                widgets.insert(w.id);
                let Some(page) = w.page_ref else { continue };
                if w.is_hidden() {
                    continue;
                }
                let Some(d) = doc.get(w.id).and_then(|o| o.as_dict().cloned()) else {
                    continue;
                };
                if let Some(form) = normal_appearance(doc, &d) {
                    let m = form_matrix(doc, form, w.rect);
                    draws
                        .entry(page.num)
                        .or_insert((page, Vec::new()))
                        .1
                        .push((form, m));
                }
                change.widgets.push(w.id);
            }
            change.fields.push(f.name.clone());
        }
        for (page, forms) in draws.into_values() {
            draw_forms(doc, page, &forms);
        }
        // Widgets leave every page's /Annots.
        for p in &self.pages {
            if !p.annots.iter().any(|a| widgets.contains(a)) {
                continue;
            }
            remove_annots(doc, p.r, &widgets);
        }
        prune_fields(doc, &field_nodes);
        for &w in &widgets {
            doc.delete(w);
        }
        let left = acroform(doc).is_some_and(|af| {
            doc.get_in(&af, b"Fields")
                .as_array()
                .is_some_and(|a| !a.is_empty())
        });
        if !left {
            remove_form(doc);
        }
        let font = self.font.take();
        let generated = std::mem::take(&mut self.generated);
        *self = Form::load(doc);
        self.font = font;
        self.generated = generated;
        Ok(change)
    }
}

/// The `/AP /N` stream, choosing the `/AS` state when there are several.
fn normal_appearance(doc: &Document, d: &Dict) -> Option<ObjRef> {
    let ap = doc.get_in(d, b"AP");
    let n = ap.as_dict()?.get(b"N")?;
    if let Some(r) = n.as_objref()
        && doc.get(r)?.as_stream().is_some()
    {
        return Some(r);
    }
    let target = doc.resolve(n);
    let state = d.name(b"AS")?;
    let s = target.as_dict()?.get(state)?.as_objref()?;
    doc.get(s)?.as_stream().map(|_| s)
}

/// The matrix mapping a form's transformed bounding box onto `rect` (§12.5.5).
fn form_matrix(doc: &Document, form: ObjRef, rect: Rect) -> [f64; 6] {
    let identity = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let Some(f) = doc.get(form) else {
        return identity;
    };
    let d = f.as_dict().cloned().unwrap_or_default();
    let bbox = Rect::from_object(&doc.get_in(&d, b"BBox")).unwrap_or(rect);
    let m: Vec<f64> = doc
        .get_in(&d, b"Matrix")
        .as_array()
        .map(|a| a.iter().filter_map(Object::as_f64).collect())
        .filter(|m: &Vec<f64>| m.len() == 6)
        .unwrap_or(identity.to_vec());
    let tr = |x: f64, y: f64| {
        pagebara_annot::Point::new(m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
    };
    let b = Rect::bounding([
        tr(bbox.x0, bbox.y0),
        tr(bbox.x1, bbox.y0),
        tr(bbox.x0, bbox.y1),
        tr(bbox.x1, bbox.y1),
    ])
    .unwrap_or(rect);
    let sx = if b.width() > 0.0 {
        rect.width() / b.width()
    } else {
        1.0
    };
    let sy = if b.height() > 0.0 {
        rect.height() / b.height()
    } else {
        1.0
    };
    [sx, 0.0, 0.0, sy, rect.x0 - b.x0 * sx, rect.y0 - b.y0 * sy]
}

/// An attribute of the page or its nearest ancestor that has it.
fn inherited(doc: &Document, page: &Dict, key: &[u8]) -> Option<Object> {
    let mut cur = page.clone();
    for _ in 0..MAX_DEPTH {
        let v = doc.get_in(&cur, key);
        if !v.is_null() {
            return Some((*v).clone());
        }
        let parent = doc.get_in(&cur, b"Parent");
        cur = parent.as_dict()?.clone();
    }
    None
}

/// Paints `forms` on top of the page's content, which is wrapped in `q … Q` first.
fn draw_forms(doc: &mut Document, page: ObjRef, forms: &[Draw]) {
    let Some(mut d) = doc.get(page).and_then(|o| o.as_dict().cloned()) else {
        return;
    };
    let mut res = inherited(doc, &d, b"Resources")
        .and_then(|o| doc.resolve(&o).as_dict().cloned())
        .unwrap_or_default();
    let mut xobjects = doc
        .get_in(&res, b"XObject")
        .as_dict()
        .cloned()
        .unwrap_or_default();
    let mut c = Content::default();
    let mut n = 0;
    for (form, m) in forms {
        let name = loop {
            let name = format!("PagebaraFx{n}");
            n += 1;
            if !xobjects.contains(name.as_bytes()) {
                break name;
            }
        };
        xobjects.set(name.as_bytes(), *form);
        c.save().cm(*m).do_xobject(&name).restore();
    }
    res.set(b"XObject", xobjects);
    d.set(b"Resources", res);
    let mut contents: Vec<Object> = match d.get(b"Contents") {
        Some(Object::Array(a)) => a.clone(),
        Some(o @ Object::Ref(_)) => match doc.resolve(o).as_array() {
            Some(a) => a.to_vec(),
            None => vec![o.clone()],
        },
        _ => Vec::new(),
    };
    let wrapped = contents.first().is_some_and(|o| {
        let s = doc.resolve(o);
        s.as_stream()
            .is_some_and(|s| !s.dict.contains(b"Filter") && doc.raw(s) == OPEN)
    });
    let mut ops = Vec::new();
    if !wrapped && !contents.is_empty() {
        let open = doc.add(Stream::new(Dict::new(), OPEN));
        contents.insert(0, open.into());
        ops.extend_from_slice(b"Q\n");
    }
    ops.extend_from_slice(&c.0);
    let ours = doc.add(Stream::new(Dict::new(), ops));
    contents.push(ours.into());
    d.set(b"Contents", contents);
    doc.set(page, d);
}

fn remove_annots(doc: &mut Document, page: ObjRef, gone: &HashSet<ObjRef>) {
    let Some(mut d) = doc.get(page).and_then(|o| o.as_dict().cloned()) else {
        return;
    };
    let list: Vec<Object> = doc
        .get_in(&d, b"Annots")
        .as_array()
        .unwrap_or_default()
        .iter()
        .filter(|o| o.as_objref().is_none_or(|r| !gone.contains(&r)))
        .cloned()
        .collect();
    match d.get(b"Annots").and_then(Object::as_objref) {
        Some(r) if !list.is_empty() => doc.set(r, list),
        _ => {
            if list.is_empty() {
                d.remove(b"Annots");
            } else {
                d.set(b"Annots", list);
            }
            doc.set(page, d);
        }
    }
}

/// Removes field nodes from their parents' `/Kids` (or `/Fields`) and from `/CO`, and
/// parents left without kids with them.
fn prune_fields(doc: &mut Document, nodes: &[ObjRef]) {
    let mut queue: Vec<ObjRef> = nodes.to_vec();
    let mut gone: HashSet<ObjRef> = HashSet::new();
    while let Some(r) = queue.pop() {
        if !gone.insert(r) {
            continue;
        }
        let parent = doc
            .get(r)
            .and_then(|o| o.as_dict()?.get(b"Parent")?.as_objref());
        doc.delete(r);
        let Some(p) = parent else { continue };
        let Some(mut pd) = doc.get(p).and_then(|o| o.as_dict().cloned()) else {
            continue;
        };
        let kids: Vec<Object> = doc
            .get_in(&pd, b"Kids")
            .as_array()
            .unwrap_or_default()
            .iter()
            .filter(|k| k.as_objref().is_none_or(|k| k != r && !gone.contains(&k)))
            .cloned()
            .collect();
        if kids.is_empty() {
            queue.push(p);
        } else {
            pd.set(b"Kids", kids);
            doc.set(p, pd);
        }
    }
    update_acroform(doc, |doc, af| {
        for key in [&b"Fields"[..], b"CO"] {
            let Some(list) = doc.get_in(af, key).as_array().map(<[_]>::to_vec) else {
                continue;
            };
            let kept: Vec<Object> = list
                .into_iter()
                .filter(|o| o.as_objref().is_none_or(|r| !gone.contains(&r)))
                .collect();
            if kept.is_empty() && key == b"CO" {
                af.remove(key);
            } else {
                af.set(key, kept);
            }
        }
    });
}

/// Drops `/AcroForm` from the catalog.
fn remove_form(doc: &mut Document) {
    let Some(root) = doc.trailer().get(b"Root").and_then(Object::as_objref) else {
        return;
    };
    let Some(mut cat) = doc.get(root).and_then(|c| c.as_dict().cloned()) else {
        return;
    };
    if let Some(r) = cat.get(b"AcroForm").and_then(Object::as_objref) {
        doc.delete(r);
    }
    cat.remove(b"AcroForm");
    cat.remove(b"NeedsRendering");
    doc.set(root, cat);
}
