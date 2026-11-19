//! The document outline (bookmarks, §12.3.3), flattened in reading order.

use std::collections::HashSet;

use pagebara_syntax::{Dict, Document, Object};

use crate::pages::PageSource;
use crate::text;

const MAX_ITEMS: usize = 10_000;
const MAX_DEPTH: usize = 32;

#[derive(Clone, Debug, PartialEq)]
pub struct OutlineItem {
    pub title: String,
    pub depth: usize,
    /// The target page, when the destination resolves to one in this document.
    pub page: Option<usize>,
}

pub fn load(doc: &Document, pages: &dyn PageSource) -> Vec<OutlineItem> {
    let mut out = Vec::new();
    let Some(cat) = doc.catalog() else {
        return out;
    };
    let Some(cat) = cat.as_dict() else {
        return out;
    };
    let root = doc.get_in(cat, b"Outlines");
    let Some(first) = root.as_dict().and_then(|d| d.get(b"First")) else {
        return out;
    };
    let mut walk = Walk {
        doc,
        pages,
        cat,
        seen: HashSet::new(),
        out: &mut out,
    };
    walk.siblings(first, 0);
    out
}

struct Walk<'a> {
    doc: &'a Document,
    pages: &'a dyn PageSource,
    cat: &'a Dict,
    seen: HashSet<u32>,
    out: &'a mut Vec<OutlineItem>,
}

impl Walk<'_> {
    fn siblings(&mut self, first: &Object, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        let mut next = first.clone();
        while let Some(r) = next.as_objref()
            && self.seen.insert(r.num)
            && self.out.len() < MAX_ITEMS
        {
            let Some(item) = self.doc.get(r) else { break };
            let Some(d) = item.as_dict() else { break };
            let title = self.doc.get_in(d, b"Title");
            self.out.push(OutlineItem {
                title: text::decode(title.as_string().unwrap_or_default()),
                depth,
                page: self.target(d),
            });
            if let Some(first) = d.get(b"First") {
                self.siblings(first, depth + 1);
            }
            next = d.get(b"Next").cloned().unwrap_or_default();
        }
    }

    fn target(&self, item: &Dict) -> Option<usize> {
        let dest = self.doc.get_in(item, b"Dest");
        if !dest.is_null() {
            return self.dest(&dest, 0);
        }
        let action = self.doc.get_in(item, b"A");
        let action = action.as_dict()?;
        (action.name(b"S") == Some(b"GoTo"))
            .then(|| self.dest(&self.doc.get_in(action, b"D"), 0))
            .flatten()
    }

    /// A destination: an explicit array, or a name looked up in `/Dests` or the name tree.
    fn dest(&self, d: &Object, depth: usize) -> Option<usize> {
        if depth > 2 {
            return None;
        }
        match d {
            Object::Array(a) => match a.first()? {
                Object::Ref(r) => self.pages.index_of(*r),
                // Destinations into other files use page numbers; not ours.
                _ => None,
            },
            Object::Dict(d) => self.dest(&self.doc.get_in(d, b"D"), depth + 1),
            Object::Name(n) => {
                let dests = self.doc.get_in(self.cat, b"Dests");
                let found = self.doc.get_in(dests.as_dict()?, n.as_bytes());
                self.dest(&found, depth + 1)
            }
            Object::String(s) => {
                let names = self.doc.get_in(self.cat, b"Names");
                let tree = self.doc.get_in(names.as_dict()?, b"Dests");
                let found = name_tree_get(self.doc, tree.as_dict()?, &s.bytes, 0)?;
                self.dest(&found, depth + 1)
            }
            _ => None,
        }
    }
}

/// Looks `key` up in a name tree (§7.9.6), using `/Limits` to skip subtrees.
fn name_tree_get(doc: &Document, node: &Dict, key: &[u8], depth: usize) -> Option<Object> {
    if depth > MAX_DEPTH {
        return None;
    }
    if let Some(names) = doc.get_in(node, b"Names").as_array() {
        for [name, value] in names.as_chunks::<2>().0 {
            if doc.resolve(name).as_string() == Some(key) {
                return Some((*doc.resolve(value)).clone());
            }
        }
    }
    let kids = doc.get_in(node, b"Kids");
    for kid in kids.as_array()? {
        let kid = doc.resolve(kid);
        let Some(kid) = kid.as_dict() else { continue };
        let limits = doc.get_in(kid, b"Limits");
        if let Some([lo, hi]) = limits.as_array()
            && let (Some(lo), Some(hi)) = (lo.as_string(), hi.as_string())
            && (key < lo || key > hi)
        {
            continue;
        }
        if let Some(found) = name_tree_get(doc, kid, key, depth + 1) {
            return Some(found);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use pagebara_syntax::Document;

    use super::*;
    use crate::test_pdf;
    use pagebara_pages::PageTree;

    #[test]
    fn flattens_outline_and_resolves_destinations() {
        let doc = Document::open(test_pdf::sample()).unwrap();
        let pages = PageTree::load(&doc);
        let items = load(&doc, &pages);
        let got: Vec<_> = items
            .iter()
            .map(|i| (i.title.as_str(), i.depth, i.page))
            .collect();
        assert_eq!(
            got,
            [
                ("Intro", 0, Some(0)),
                ("Été", 0, Some(2)),
                ("Named", 1, Some(1))
            ]
        );
    }
}
