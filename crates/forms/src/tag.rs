//! Tagged documents: new widgets get a `/Form` structure element (with an object
//! reference to the widget) so assistive technology finds them; removed widgets lose it.

use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::props::edit_dict;

/// The structure tree root, when the document is tagged.
fn struct_root(doc: &Document) -> Option<(ObjRef, Dict)> {
    let cat = doc.catalog()?;
    let r = cat.as_dict()?.get(b"StructTreeRoot")?.as_objref()?;
    let d = doc.get(r)?.as_dict()?.clone();
    Some((r, d))
}

/// The element new content goes under: the root's only `/Document` child, else the root.
fn container(doc: &Document, root_ref: ObjRef, root: &Dict) -> ObjRef {
    let kids: Vec<Object> = match root.get(b"K") {
        Some(Object::Array(a)) => a.clone(),
        Some(o) => vec![o.clone()],
        None => Vec::new(),
    };
    if let [Object::Ref(r)] = kids.as_slice()
        && doc
            .get(*r)
            .and_then(|o| o.as_dict().map(|d| d.name(b"S") == Some(b"Document")))
            .unwrap_or(false)
    {
        return *r;
    }
    root_ref
}

/// The parent tree's `/Nums` (only a flat tree is edited) and its reference.
fn parent_tree(doc: &Document, root: &Dict) -> Option<(Option<ObjRef>, Dict)> {
    let r = root.get(b"ParentTree").and_then(Object::as_objref);
    let pt = doc
        .get_in(root, b"ParentTree")
        .as_dict()
        .cloned()
        .unwrap_or_default();
    (!pt.contains(b"Kids")).then_some((r, pt))
}

/// Adds a `/Form` element for `widget` on `page` (no-op for untagged documents or widgets
/// that already have one).
pub(crate) fn tag_widget(doc: &mut Document, page: ObjRef, widget: ObjRef, alt: &str) {
    let Some((root_ref, root)) = struct_root(doc) else {
        return;
    };
    if doc
        .get(widget)
        .and_then(|o| o.as_dict().map(|d| d.contains(b"StructParent")))
        .unwrap_or(true)
    {
        return;
    }
    let Some((pt_ref, mut pt)) = parent_tree(doc, &root) else {
        return;
    };
    let mut nums: Vec<Object> = doc
        .get_in(&pt, b"Nums")
        .as_array()
        .unwrap_or_default()
        .to_vec();
    let mut key = root.int(b"ParentTreeNextKey").unwrap_or(0);
    for k in nums.iter().step_by(2) {
        key = key.max(k.as_int().unwrap_or(0) + 1);
    }
    let parent = container(doc, root_ref, &root);
    let mut objr = Dict::new();
    objr.set(b"Type", Object::name(b"OBJR"));
    objr.set(b"Obj", widget);
    objr.set(b"Pg", page);
    let mut el = Dict::new();
    el.set(b"Type", Object::name(b"StructElem"));
    el.set(b"S", Object::name(b"Form"));
    el.set(b"P", parent);
    el.set(b"Pg", page);
    el.set(b"K", objr);
    if !alt.is_empty() {
        el.set(b"Alt", crate::text::encode(alt));
    }
    let er = doc.add(el);
    edit_dict(doc, widget, |_, d| d.set(b"StructParent", Object::Int(key)));
    edit_dict(doc, parent, |_, d| {
        let mut k: Vec<Object> = match d.get(b"K") {
            Some(Object::Array(a)) => a.clone(),
            Some(o) => vec![o.clone()],
            None => Vec::new(),
        };
        k.push(Object::Ref(er));
        d.set(b"K", k);
    });
    nums.push(Object::Int(key));
    nums.push(Object::Ref(er));
    pt.set(b"Nums", nums);
    let pt_ref = match pt_ref {
        Some(r) => {
            doc.set(r, pt);
            r
        }
        None => doc.add(pt),
    };
    edit_dict(doc, root_ref, |_, d| {
        d.set(b"ParentTree", pt_ref);
        d.set(b"ParentTreeNextKey", Object::Int(key + 1));
    });
    edit_dict(doc, page, |_, d| {
        if !d.contains(b"Tabs") {
            d.set(b"Tabs", Object::name(b"S"));
        }
    });
}

/// The structure element that `/StructParent` `key` names.
fn element_of(doc: &Document, root: &Dict, key: i64) -> Option<ObjRef> {
    let (_, pt) = parent_tree(doc, root)?;
    let nums = doc.get_in(&pt, b"Nums");
    let nums = nums.as_array()?;
    nums.chunks(2)
        .find(|c| c[0].as_int() == Some(key))
        .and_then(|c| c.get(1)?.as_objref())
}

/// Points the structure element of a widget that moved to a new object at `new`.
pub(crate) fn retarget(doc: &mut Document, key: i64, new: ObjRef) {
    let Some((_, root)) = struct_root(doc) else {
        return;
    };
    let Some(el) = element_of(doc, &root, key) else {
        return;
    };
    edit_dict(doc, el, |_, d| {
        let fix = |o: &mut Object| {
            if let Object::Dict(k) = o
                && k.name(b"Type") == Some(b"OBJR")
            {
                k.set(b"Obj", new);
            }
        };
        match d.get_mut(b"K") {
            Some(Object::Array(a)) => a.iter_mut().for_each(fix),
            Some(o) => fix(o),
            None => {}
        }
    });
}

/// Removes the structure element of a widget with `/StructParent` `key`, if it holds
/// nothing but the widget.
pub(crate) fn untag(doc: &mut Document, key: i64) {
    let Some((root_ref, root)) = struct_root(doc) else {
        return;
    };
    let Some(el) = element_of(doc, &root, key) else {
        return;
    };
    let only_objr = doc
        .get(el)
        .and_then(|o| {
            let d = o.as_dict()?;
            Some(matches!(d.get(b"K"), Some(Object::Dict(k)) if k.name(b"Type") == Some(b"OBJR")))
        })
        .unwrap_or(false);
    if let Some((pt_ref, mut pt)) = parent_tree(doc, &root) {
        let nums: Vec<Object> = doc
            .get_in(&pt, b"Nums")
            .as_array()
            .unwrap_or_default()
            .chunks(2)
            .filter(|c| c[0].as_int() != Some(key))
            .flatten()
            .cloned()
            .collect();
        pt.set(b"Nums", nums);
        match pt_ref {
            Some(r) => doc.set(r, pt),
            None => edit_dict(doc, root_ref, |_, d| d.set(b"ParentTree", pt)),
        }
    }
    if !only_objr {
        return;
    }
    let parent = doc
        .get(el)
        .and_then(|o| o.as_dict()?.get(b"P")?.as_objref());
    if let Some(p) = parent {
        edit_dict(doc, p, |_, d| {
            let k = match d.get(b"K") {
                Some(Object::Array(a)) => a
                    .iter()
                    .filter(|o| o.as_objref() != Some(el))
                    .cloned()
                    .collect(),
                Some(Object::Ref(r)) if *r == el => Vec::new(),
                Some(o) => vec![o.clone()],
                None => Vec::new(),
            };
            d.set(b"K", k);
        });
    }
    doc.delete(el);
}
