//! Writing a balanced page tree for a new page order.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use pagebara_syntax::xref::NumMap;
use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::edit::update_catalog;
use crate::rect::Rect;
use crate::tree::{INHERITABLE, Page, PageTree};

/// Kids per interior node.
const FANOUT: usize = 32;

/// Replaces the tree with a balanced one holding `order`. The root keeps its inheritable
/// attributes; pages of `tree` get those they inherited from interior nodes written onto
/// themselves, and other pages get explicit defaults where the root would otherwise lend
/// them its values. Old interior nodes are reused, so pages whose parent stays the same
/// are not rewritten; unused ones are deleted.
pub(crate) fn rebuild(doc: &mut Document, tree: &PageTree, order: &[ObjRef]) {
    let root = tree.root().unwrap_or_else(|| doc.add(Object::Null));
    let mut root_dict = tree
        .root()
        .and_then(|r| doc.get(r)?.as_dict().cloned())
        .unwrap_or_default();
    root_dict.remove(b"Parent");

    let mut pool: VecDeque<ObjRef> = tree.nodes().iter().copied().collect();
    let mut parent_of: NumMap<ObjRef> = NumMap::default();
    let mut interior: Vec<(ObjRef, Vec<Object>, usize)> = Vec::new();
    let mut level: Vec<(ObjRef, usize)> = order.iter().map(|&r| (r, 1)).collect();
    while level.len() > FANOUT {
        let groups = level.len().div_ceil(FANOUT);
        let (size, extra) = (level.len() / groups, level.len() % groups);
        let mut it = level.into_iter();
        level = (0..groups)
            .map(|g| {
                let node = pool.pop_front().unwrap_or_else(|| doc.add(Object::Null));
                let kids: Vec<_> = it.by_ref().take(size + usize::from(g < extra)).collect();
                let count = kids.iter().map(|k| k.1).sum();
                for k in &kids {
                    parent_of.insert(k.0.num, node);
                }
                interior.push((node, kids.iter().map(|k| k.0.into()).collect(), count));
                (node, count)
            })
            .collect();
    }
    for k in &level {
        parent_of.insert(k.0.num, root);
    }

    for (node, kids, count) in interior {
        let mut d = Dict::new();
        d.set(b"Type", Object::name(b"Pages"));
        d.set(b"Parent", parent_of[&node.num]);
        d.set(b"Kids", kids);
        d.set(b"Count", count as i64);
        doc.set(node, d);
    }
    root_dict.set(b"Type", Object::name(b"Pages"));
    root_dict.set(
        b"Kids",
        level.iter().map(|k| k.0.into()).collect::<Vec<_>>(),
    );
    root_dict.set(b"Count", order.len() as i64);
    doc.set(root, root_dict.clone());

    let by_num = tree.index();
    let mut shared = HashMap::new();
    for &r in order {
        let page = by_num.get(&r.num).map(|&i| &tree.pages()[i]);
        relink(doc, r, parent_of[&r.num], page, &root_dict, &mut shared);
    }
    for n in pool {
        doc.delete(n);
    }
    update_catalog(doc, |_, cat| {
        let stale = cat.get(b"Pages") != Some(&Object::Ref(root));
        if stale {
            cat.set(b"Pages", root);
        }
        stale
    });
}

/// Sets a page's `/Parent` and pushes down what it inherited, writing it only on change.
fn relink(
    doc: &mut Document,
    r: ObjRef,
    parent: ObjRef,
    page: Option<&Page>,
    root: &Dict,
    shared: &mut HashMap<usize, Object>,
) {
    let Some(mut d) = doc.get(r).and_then(|o| o.as_dict().cloned()) else {
        return;
    };
    let mut changed = match page {
        Some(p) => push_down(doc, &mut d, p, Some(root), shared),
        None => shield(doc, &mut d, root),
    };
    if d.name(b"Type") != Some(b"Page") {
        d.set(b"Type", Object::name(b"Page"));
        changed = true;
    }
    // Required attributes that nothing provides.
    for key in [&b"Resources"[..], b"MediaBox"] {
        if !d.contains(key) && !root.contains(key) {
            let v = match page {
                _ if key == b"Resources" => Dict::new().into(),
                Some(p) => p.media_box.to_object(),
                None => Rect::LETTER.to_object(),
            };
            d.set(key, v);
            changed = true;
        }
    }
    if d.get(b"Parent") != Some(&Object::Ref(parent)) {
        d.set(b"Parent", parent);
        changed = true;
    }
    if changed {
        doc.set(r, d);
    }
}

/// Gives a page from elsewhere explicit values for what the root would lend it.
fn shield(doc: &Document, d: &mut Dict, root: &Dict) -> bool {
    let mut changed = false;
    for key in INHERITABLE {
        if d.contains(key) || !root.contains(key) {
            continue;
        }
        let v: Object = match key {
            b"Resources" => Dict::new().into(),
            b"MediaBox" => Rect::LETTER.to_object(),
            b"CropBox" => match d.get(b"MediaBox") {
                Some(m) => (*doc.resolve(m)).clone(),
                None => Rect::LETTER.to_object(),
            },
            _ => Object::Int(0),
        };
        d.set(key, v);
        changed = true;
    }
    changed
}

/// Copies inherited attributes onto the page dictionary `d`. A direct `/Resources`
/// dictionary is made indirect once, so the pages sharing it keep sharing it.
pub(crate) fn push_down(
    doc: &mut Document,
    d: &mut Dict,
    page: &Page,
    root: Option<&Dict>,
    shared: &mut HashMap<usize, Object>,
) -> bool {
    let mut changed = false;
    for (key, v) in INHERITABLE.into_iter().zip(&page.inherited.0) {
        let Some(v) = v else { continue };
        if d.contains(key) || root.is_some_and(|r| r.get(key) == Some(v)) {
            continue;
        }
        let v = if key == b"Resources" && !matches!(v, Object::Ref(_)) {
            shared
                .entry(Arc::as_ptr(&page.inherited) as usize)
                .or_insert_with(|| doc.add(v.clone()).into())
                .clone()
        } else {
            v.clone()
        };
        d.set(key, v);
        changed = true;
    }
    changed
}
