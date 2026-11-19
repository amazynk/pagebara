//! Keeping the text of unchanged pages when the page list changes (pages rotated, cropped,
//! moved, deleted or inserted), so search stays instant after organising pages.
//!
//! A page's text depends on its content streams and the fonts and forms of its resources,
//! not on its rotation, boxes or place in the tree. A page keeps its text when it is the
//! same page object showing the same objects: unchanged objects are shared between
//! revisions of a [`Document`], so that is a pointer comparison.

use std::collections::HashMap;
use std::sync::Arc;

use pagebara_syntax::{Dict, Document, ObjRef, Object};

use super::TextIndex;

/// Forms inside forms followed this deep; deeper ones count as changed.
const MAX_DEPTH: u32 = 8;

impl TextIndex {
    /// Takes over what `old` knows of pages whose text cannot have changed since: their
    /// folded text and kept text. Returns how many pages' folded text it took.
    pub(super) fn carry_over(&self, old: &TextIndex) -> usize {
        let mut was: HashMap<ObjRef, usize> = HashMap::with_capacity(old.refs.len());
        for (j, &r) in old.refs.iter().enumerate() {
            was.entry(r).or_insert(j);
        }
        let kept: HashMap<usize, Arc<pagebara_content::TextPage>> = old
            .pages
            .lock()
            .map
            .iter()
            .map(|(&j, (t, _, _))| (j, t.clone()))
            .collect();
        let mut carried = 0;
        for (i, &r) in self.refs.iter().enumerate() {
            let Some(&j) = was.get(&r) else { continue };
            let (folded, page) = (old.folded(j), kept.get(&j));
            if (folded.is_none() && page.is_none()) || !same_text(&old.doc, &self.doc, r) {
                continue;
            }
            if let Some(f) = folded {
                self.set_folded(i, f.to_owned());
                carried += 1;
            }
            if let Some(t) = page {
                self.keep(i, t.clone());
            }
        }
        carried
    }
}

/// Whether page `r` shows the same text in `a` as in `b`.
fn same_text(a: &Document, b: &Document, r: ObjRef) -> bool {
    let (Some(pa), Some(pb)) = (a.get(r), b.get(r)) else {
        return false;
    };
    let (Some(da), Some(db)) = (pa.as_dict(), pb.as_dict()) else {
        return false;
    };
    let c = Compare { a, b };
    c.entry(da.get(b"Contents"), db.get(b"Contents"))
        && c.contents(da)
        && c.resources(resources_entry(a, da), resources_entry(b, db), 0)
}

/// `/Resources` of a page dictionary, inherited from its ancestors if need be.
fn resources_entry(doc: &Document, page: &Dict) -> Option<Object> {
    let mut node = page.clone();
    for _ in 0..64 {
        if let Some(r) = node.get(b"Resources") {
            return Some(r.clone());
        }
        node = doc.get_in(&node, b"Parent").as_dict()?.clone();
    }
    None
}

struct Compare<'a> {
    a: &'a Document,
    b: &'a Document,
}

impl Compare<'_> {
    /// Equal values whose references (at the top or in an array) name the very same
    /// objects in both documents.
    fn entry(&self, x: Option<&Object>, y: Option<&Object>) -> bool {
        if x != y {
            return false;
        }
        match x {
            Some(Object::Ref(r)) => self.same_object(*r),
            Some(Object::Array(items)) => items.iter().all(|o| self.refs_same(o)),
            _ => true,
        }
    }

    fn refs_same(&self, o: &Object) -> bool {
        o.as_objref().is_none_or(|r| self.same_object(r))
    }

    fn same_object(&self, r: ObjRef) -> bool {
        match (self.a.get(r), self.b.get(r)) {
            (Some(x), Some(y)) => Arc::ptr_eq(&x, &y),
            (None, None) => true,
            _ => false,
        }
    }

    /// The streams of a `/Contents` that refers to an array of them (the array itself is
    /// the same object by now).
    fn contents(&self, page: &Dict) -> bool {
        match &*self.a.get_in(page, b"Contents") {
            Object::Array(items) => items.iter().all(|o| self.refs_same(o)),
            _ => true,
        }
    }

    /// Resources (`x` and `y` unresolved): the same objects, with the same fonts, graphics
    /// states and forms in them, and the same resources in those forms.
    fn resources(&self, x: Option<Object>, y: Option<Object>, depth: u32) -> bool {
        if depth > MAX_DEPTH || !self.entry(x.as_ref(), y.as_ref()) {
            return false;
        }
        let Some(x) = x else { return true };
        let (ra, rb) = (self.a.resolve(&x), self.b.resolve(&x));
        let (Some(ra), Some(rb)) = (ra.as_dict(), rb.as_dict()) else {
            return true;
        };
        for key in [&b"Font"[..], b"XObject", b"ExtGState"] {
            if !self.entry(ra.get(key), rb.get(key)) {
                return false;
            }
            let (ca, cb) = (self.a.get_in(ra, key), self.b.get_in(rb, key));
            let (Some(ca), Some(cb)) = (ca.as_dict(), cb.as_dict()) else {
                continue;
            };
            for (name, va) in ca.iter() {
                if !self.entry(Some(va), cb.get(name.as_bytes())) {
                    return false;
                }
                if key == b"XObject" && !self.form(va, depth) {
                    return false;
                }
            }
        }
        true
    }

    /// The resources of a form XObject (the same object in both documents by now).
    fn form(&self, xobject: &Object, depth: u32) -> bool {
        let (fa, fb) = (self.a.resolve(xobject), self.b.resolve(xobject));
        let is_form = |o: &Object| {
            o.as_stream()
                .and_then(|s| s.dict.get(b"Subtype")?.as_name())
                == Some(b"Form")
        };
        if !is_form(&fa) {
            return true;
        }
        let res = |o: &Object| o.as_dict().and_then(|d| d.get(b"Resources").cloned());
        self.resources(res(&fa), res(&fb), depth + 1)
    }
}
