//! Page operations. Each edits the document in place; take a snapshot (clone) first to undo.
//! Page numbers are 0-based indices into [`PageTree::load`]'s list.
//!
//! Operations check the document's permissions ([`permitted`](crate::permitted)) and fail
//! with [`Error::NotPermitted`] when its security settings forbid them.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use pagebara_syntax::xref::NumMap;
use pagebara_syntax::{Document, ObjRef, Object};

use crate::assemble::{self, Carry, Outline};
use crate::rebuild::{push_down, rebuild};
use crate::tree::{INHERITABLE, Page, normalize_rotation};
use crate::{Access, Error, PageTree, Rect, Result, check, fixup, labels};

/// The distinct indices of `pages`, ascending, checked against `count`.
fn selection(pages: &[usize], count: usize) -> Result<Vec<usize>> {
    if let Some(&index) = pages.iter().find(|&&i| i >= count) {
        return Err(Error::OutOfRange { index, count });
    }
    let mut v = pages.to_vec();
    v.sort_unstable();
    v.dedup();
    Ok(v)
}

/// Removes pages, along with the bookmarks' and links' destinations, named destinations
/// and form fields that pointed at them.
pub fn delete(doc: &mut Document, pages: &[usize]) -> Result<()> {
    check(doc, Access::Assemble)?;
    let before = doc.clone();
    let tree = PageTree::load(doc);
    let sel = selection(pages, tree.len())?;
    if sel.is_empty() {
        return Ok(());
    }
    if sel.len() == tree.len() {
        return Err(Error::WouldBeEmpty);
    }
    let gone: HashSet<usize> = sel.into_iter().collect();
    let labels = labels::read(doc, tree.len());
    let keep = |i: &usize| !gone.contains(i);
    let order: Vec<ObjRef> = (0..tree.len())
        .filter(keep)
        .map(|i| tree.pages()[i].r)
        .collect();
    rebuild(doc, &tree, &order);
    let gone_nums = gone.iter().map(|&i| tree.pages()[i].r.num).collect();
    fixup::forget_pages(doc, &before, &gone_nums, &order);
    if let Some(l) = labels {
        let l: Vec<_> = l
            .into_iter()
            .enumerate()
            .filter(|(i, _)| keep(i))
            .map(|x| x.1)
            .collect();
        labels::write(doc, &l);
    }
    Ok(())
}

/// Writes a proper page tree when the pages could only be found by scanning every object
/// (the tree was missing or broken), so that rewriting the file keeps them; returns
/// whether it did.
pub fn repair_tree(doc: &mut Document) -> bool {
    let tree = PageTree::load(doc);
    if !tree.repaired() || tree.is_empty() {
        return false;
    }
    let refs = tree.refs();
    // What each page inherits through its old parents goes onto the page itself.
    for &r in &refs {
        let Some(page) = doc.get(r).and_then(|o| o.as_dict().cloned()) else {
            continue;
        };
        let mut own = page.clone();
        for key in INHERITABLE {
            if own.contains(key) {
                continue;
            }
            let mut node = doc.get_in(&page, b"Parent").as_dict().cloned();
            for _ in 0..64 {
                let Some(n) = node else { break };
                if let Some(v) = n.get(key) {
                    own.set(key, v.clone());
                    break;
                }
                node = doc.get_in(&n, b"Parent").as_dict().cloned();
            }
        }
        if own != page {
            doc.set(r, own);
        }
    }
    let tree = PageTree::load(doc);
    rebuild(doc, &tree, &refs);
    true
}

/// Puts the pages in a new order: `order[i]` is the current index of the page that
/// becomes page `i`.
pub fn reorder(doc: &mut Document, order: &[usize]) -> Result<()> {
    check(doc, Access::Assemble)?;
    let tree = PageTree::load(doc);
    let n = tree.len();
    let mut seen = vec![false; n];
    if order.len() != n
        || !order
            .iter()
            .all(|&i| i < n && !std::mem::replace(&mut seen[i], true))
    {
        return Err(Error::NotPermutation);
    }
    if order.iter().enumerate().all(|(i, &j)| i == j) {
        return Ok(());
    }
    let labels = labels::read(doc, n);
    let refs: Vec<ObjRef> = order.iter().map(|&i| tree.pages()[i].r).collect();
    rebuild(doc, &tree, &refs);
    if let Some(l) = labels {
        labels::write(
            doc,
            &order.iter().map(|&i| l[i].clone()).collect::<Vec<_>>(),
        );
    }
    Ok(())
}

/// Moves `pages` (kept in their order) so they come just before current page `to`, or at
/// the end when `to` is the page count.
pub fn move_pages(doc: &mut Document, pages: &[usize], to: usize) -> Result<()> {
    check(doc, Access::Assemble)?;
    let n = PageTree::load(doc).len();
    let sel = selection(pages, n)?;
    if to > n {
        return Err(Error::OutOfRange {
            index: to,
            count: n,
        });
    }
    let moved: HashSet<usize> = sel.iter().copied().collect();
    let mut order = Vec::with_capacity(n);
    for i in 0..=n {
        if i == to {
            order.extend(&sel);
        }
        if i < n && !moved.contains(&i) {
            order.push(i);
        }
    }
    reorder(doc, &order)
}

/// Turns pages clockwise by `quarter_turns` × 90° (negative turns counter-clockwise).
pub fn rotate(doc: &mut Document, pages: &[usize], quarter_turns: i32) -> Result<()> {
    check(doc, Access::Assemble)?;
    let tree = PageTree::load(doc);
    for i in selection(pages, tree.len())? {
        let p = &tree.pages()[i];
        let r = normalize_rotation(i64::from(p.rotate) + 90 * i64::from(quarter_turns));
        set_page_key(doc, p.r, b"Rotate", Object::Int(r.into()));
    }
    Ok(())
}

/// Sets the visible region of pages (`None` shows the whole media box again). The
/// rectangle is in default user space, before rotation, and is clipped to the media box.
pub fn crop(doc: &mut Document, pages: &[usize], rect: Option<Rect>) -> Result<()> {
    check(doc, Access::Assemble)?;
    let tree = PageTree::load(doc);
    for i in selection(pages, tree.len())? {
        let p = &tree.pages()[i];
        let r = rect
            .and_then(|r| r.intersect(&p.media_box))
            .unwrap_or(p.media_box);
        set_page_key(doc, p.r, b"CropBox", r.to_object());
    }
    Ok(())
}

fn set_page_key(doc: &mut Document, r: ObjRef, key: &[u8], v: Object) {
    if let Some(mut d) = doc.get(r).and_then(|o| o.as_dict().cloned()) {
        d.set(key, v);
        doc.set(r, d);
    }
}

/// Inserts a copy of each of `pages` right after the original. The copies share content
/// and resources with their originals; their annotations are copied, except form widgets.
pub fn duplicate(doc: &mut Document, pages: &[usize]) -> Result<()> {
    check(doc, Access::Assemble)?;
    let tree = PageTree::load(doc);
    let sel: HashSet<usize> = selection(pages, tree.len())?.into_iter().collect();
    if sel.is_empty() {
        return Ok(());
    }
    let labels = labels::read(doc, tree.len());
    let mut order = Vec::with_capacity(tree.len() + sel.len());
    let mut new_labels = Vec::new();
    let mut shared = HashMap::new();
    for (i, p) in tree.pages().iter().enumerate() {
        order.push(p.r);
        new_labels.extend(labels.as_ref().map(|l| l[i].clone()));
        if sel.contains(&i) {
            order.push(copy_page_from(doc, p, &mut shared));
            new_labels.extend(labels.as_ref().map(|l| l[i].clone()));
        }
    }
    rebuild(doc, &tree, &order);
    if labels.is_some() {
        labels::write(doc, &new_labels);
    }
    Ok(())
}

fn copy_page_from(doc: &mut Document, p: &Page, shared: &mut HashMap<usize, Object>) -> ObjRef {
    let r = copy_page(doc, p.r);
    if let Some(mut d) = doc.get(r).and_then(|o| o.as_dict().cloned())
        && push_down(doc, &mut d, p, None, shared)
    {
        doc.set(r, d);
    }
    r
}

/// A new page object with the dictionary of `r` and copies of its annotations (form
/// widgets are left out: they belong to a single field and page). The copy is not part of
/// the structure tree or any article thread: those belong to the original.
pub(crate) fn copy_page(doc: &mut Document, r: ObjRef) -> ObjRef {
    let mut d = doc
        .get(r)
        .and_then(|o| o.as_dict().cloned())
        .unwrap_or_default();
    for key in [&b"Parent"[..], b"B", b"StructParents"] {
        d.remove(key);
    }
    let new = doc.add(Object::Null);
    let annots: Vec<Object> = doc
        .get_in(&d, b"Annots")
        .as_array()
        .unwrap_or_default()
        .to_vec();
    if !annots.is_empty() {
        let mut map: NumMap<ObjRef> = NumMap::default();
        map.insert(r.num, new);
        let mut copies = Vec::new();
        for a in &annots {
            let o = doc.resolve(a);
            let Some(ad) = o.as_dict() else { continue };
            if ad.name(b"Subtype") == Some(b"Widget") {
                continue;
            }
            let mut ad = ad.clone();
            ad.remove(b"StructParent");
            let c = doc.add(Object::Null);
            if let Some(old) = a.as_objref() {
                map.insert(old.num, c);
            }
            copies.push((c, ad));
        }
        let relink = |o: &mut Object| {
            if let Some(&n) = o.as_objref().and_then(|r| map.get(&r.num)) {
                *o = n.into();
            }
        };
        for (c, mut ad) in copies.iter().cloned() {
            for key in [&b"P"[..], b"Popup", b"Parent", b"IRT"] {
                if let Some(v) = ad.get_mut(key) {
                    relink(v);
                }
            }
            doc.set(c, ad);
        }
        d.set(
            b"Annots",
            copies.iter().map(|c| c.0.into()).collect::<Vec<Object>>(),
        );
    }
    doc.set(new, d);
    new
}

/// Inserts copies of `pages` of `src` (in that order; repeats allowed) before page `at`
/// of `doc`, or at the end when `at` is the page count. Shared resources are copied once.
/// Form fields, the pages' part of the structure tree, article threads, optional content
/// groups and named destinations come along.
pub fn insert(doc: &mut Document, at: usize, src: &Document, pages: &[usize]) -> Result<()> {
    check(doc, Access::Assemble)?;
    check(src, Access::Extract)?;
    let tree = PageTree::load(doc);
    let n = tree.len();
    if at > n {
        return Err(Error::OutOfRange {
            index: at,
            count: n,
        });
    }
    let src_tree = PageTree::load(src);
    check_pages(&src_tree, pages)?;
    if pages.is_empty() {
        return Ok(());
    }
    let dst_labels = labels::read(doc, n);
    let src_labels = labels::read(src, src_tree.len());
    let carry = Carry {
        all_pages: covers_all(&src_tree, pages),
        ..Carry::default()
    };
    let new = assemble::add(doc, src, &src_tree, pages, &carry);
    let mut order = tree.refs();
    order.splice(at..at, new);
    rebuild(doc, &tree, &order);
    if dst_labels.is_some() || src_labels.is_some() {
        // Pages keep the labels they had, which for unlabelled documents is their number.
        let mut l = dst_labels.unwrap_or_else(|| labels::default(n));
        let src_labels = src_labels.unwrap_or_else(|| labels::default(src_tree.len()));
        l.splice(at..at, pages.iter().map(|&i| src_labels[i].clone()));
        labels::write(doc, &l);
    }
    Ok(())
}

fn check_pages(tree: &PageTree, pages: &[usize]) -> Result<()> {
    match pages.iter().find(|&&i| i >= tree.len()) {
        Some(&index) => Err(Error::OutOfRange {
            index,
            count: tree.len(),
        }),
        None => Ok(()),
    }
}

fn covers_all(tree: &PageTree, pages: &[usize]) -> bool {
    let mut seen = vec![false; tree.len()];
    for &i in pages {
        seen[i] = true;
    }
    seen.into_iter().all(|s| s)
}

/// A new, empty document (no pages yet) of the given PDF version.
pub fn blank(version: (u8, u8)) -> Document {
    let (major, minor) = version;
    let objs = [
        "<</Type/Catalog/Pages 2 0 R>>",
        "<</Type/Pages/Kids[]/Count 0>>",
    ];
    let mut out = format!("%PDF-{major}.{minor}\n").into_bytes();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f\r\n", objs.len() + 1).bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n\r\n").bytes());
    }
    out.extend(
        format!(
            "trailer\n<</Size {} /Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .bytes(),
    );
    Document::open(out).expect("the blank document parses")
}

/// A new document holding copies of `pages` of `src`, in that order, with the outline
/// items leading to them, the document information, language and viewer settings, and
/// what [`insert`] brings along.
pub fn extract(src: &Document, pages: &[usize]) -> Result<Document> {
    check(src, Access::Extract)?;
    let tree = PageTree::load(src);
    check_pages(&tree, pages)?;
    Ok(assemble_new(src.version(), &[(src, &tree, pages)], |_| {
        Carry {
            document: true,
            outline: Outline::Pruned,
            ..Carry::default()
        }
    }))
}

/// One new document per range of pages, each as [`extract`] makes it.
pub fn split(src: &Document, ranges: &[Range<usize>]) -> Result<Vec<Document>> {
    check(src, Access::Extract)?;
    let tree = PageTree::load(src);
    let sels: Vec<Vec<usize>> = ranges.iter().map(|r| r.clone().collect()).collect();
    for s in &sels {
        check_pages(&tree, s)?;
    }
    Ok(sels
        .iter()
        .map(|sel| {
            assemble_new(src.version(), &[(src, &tree, sel)], |_| Carry {
                document: true,
                outline: Outline::Pruned,
                ..Carry::default()
            })
        })
        .collect())
}

/// A new document with all pages of `docs`, one after the other. The first document
/// provides the document information and viewer settings; each gets a top-level outline
/// item (its title, or `Document N`) holding its own outline. Form fields whose names are
/// taken are renamed (`name#2`), and attachments, scripts, optional content, structure
/// and threads of all documents are kept.
pub fn merge(docs: &[&Document]) -> Result<Document> {
    for d in docs {
        check(d, Access::Extract)?;
    }
    let version = docs.iter().map(|d| d.version()).max().unwrap_or((1, 7));
    let trees: Vec<PageTree> = docs.iter().map(|d| PageTree::load(d)).collect();
    let sels: Vec<Vec<usize>> = trees.iter().map(|t| (0..t.len()).collect()).collect();
    let sources: Vec<_> = docs
        .iter()
        .zip(&trees)
        .zip(&sels)
        .map(|((d, t), s)| (*d, t, s.as_slice()))
        .collect();
    Ok(assemble_new(version, &sources, |i| Carry {
        document: i == 0,
        outline: Outline::Under(format!("Document {}", i + 1)),
        attachments: true,
        all_pages: true,
    }))
}

/// A new document of the given pages of several sources.
fn assemble_new(
    version: (u8, u8),
    sources: &[(&Document, &PageTree, &[usize])],
    carry: impl Fn(usize) -> Carry,
) -> Document {
    let mut out = blank(version);
    let tree = PageTree::load(&out);
    let mut order = Vec::new();
    let mut l = Vec::new();
    let mut any_labels = false;
    for (i, (src, src_tree, sel)) in sources.iter().enumerate() {
        let mut c = carry(i);
        c.all_pages |= covers_all(src_tree, sel);
        order.extend(assemble::add(&mut out, src, src_tree, sel, &c));
        let src_labels = labels::read(src, src_tree.len());
        any_labels |= src_labels.is_some();
        let src_labels = src_labels.unwrap_or_else(|| labels::default(src_tree.len()));
        l.extend(sel.iter().map(|&i| src_labels[i].clone()));
    }
    rebuild(&mut out, &tree, &order);
    if any_labels {
        labels::write(&mut out, &l);
    }
    out
}
