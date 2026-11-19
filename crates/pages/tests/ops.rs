mod common;

use common::*;
use pagebara_pages::{Error, PageTree, Rect, page_labels};
use pagebara_syntax::{Document, Object};

fn sizes(doc: &Document) -> Vec<(f64, f64)> {
    PageTree::load(doc)
        .pages()
        .iter()
        .map(|p| p.size())
        .collect()
}

fn labels(doc: &Document) -> Vec<String> {
    page_labels(doc, PageTree::load(doc).len()).unwrap_or_default()
}

/// Named destinations from both the `/Dests` dictionary and the name tree.
fn names(doc: &Document) -> Vec<String> {
    let cat = catalog(doc);
    let mut v: Vec<String> = doc
        .get_in(&cat, b"Dests")
        .as_dict()
        .map(|d| {
            d.iter()
                .map(|(k, _)| String::from_utf8_lossy(k.as_bytes()).into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.extend(name_keys(doc, b"Dests"));
    v.sort();
    v
}

fn annots(doc: &Document, page: usize) -> Vec<pagebara_syntax::Dict> {
    let p = PageTree::load(doc).pages()[page].r;
    let d = dict(doc, p);
    let a = doc.get_in(&d, b"Annots");
    a.as_array()
        .unwrap_or_default()
        .iter()
        .map(|o| doc.resolve(o).as_dict().unwrap().clone())
        .collect()
}

fn field_names(doc: &Document) -> Vec<String> {
    let cat = catalog(doc);
    let af = doc.get_in(&cat, b"AcroForm");
    let Some(af) = af.as_dict() else {
        return vec![];
    };
    let f = doc.get_in(af, b"Fields");
    f.as_array()
        .unwrap()
        .iter()
        .map(|o| {
            let d = doc.resolve(o);
            let d = d.as_dict().unwrap();
            let kids = doc.get_in(d, b"Kids").as_array().map_or(0, |a| a.len());
            format!(
                "{}:{kids}",
                String::from_utf8_lossy(d.get(b"T").unwrap().as_string().unwrap())
            )
        })
        .collect()
}

fn outline_has_dest(doc: &Document) -> Vec<bool> {
    let cat = catalog(doc);
    let o = doc.get_in(&cat, b"Outlines");
    let mut item = o
        .as_dict()
        .unwrap()
        .get(b"First")
        .and_then(Object::as_objref);
    let mut v = Vec::new();
    while let Some(r) = item {
        let d = dict(doc, r);
        v.push(d.contains(b"Dest") || d.contains(b"A"));
        item = d.get(b"Next").and_then(Object::as_objref);
    }
    v
}

#[test]
fn delete_keeps_document_consistent() {
    let mut doc = open(rich());
    let before = doc.clone();
    pagebara_pages::delete(&mut doc, &[1, 3]).unwrap();
    // The snapshot is untouched: undo is keeping the clone.
    assert_eq!(texts(&before).len(), 6);
    for d in std::iter::once(doc.clone()).chain(saved(&doc, "")) {
        assert_eq!(check_tree(&d), 4);
        assert_eq!(texts(&d), ["Page 1", "Page 3", "Page 5", "Page 6"]);
        // Attributes from the removed interior nodes were pushed down.
        assert_eq!(
            sizes(&d),
            [
                (792.0, 612.0),
                (690.0, 490.0),
                (300.0, 400.0),
                (300.0, 400.0)
            ]
        );
        assert_eq!(labels(&d), ["i", "1", "3", "4"]);
        assert_eq!(names(&d), ["chap1"]);
        assert_eq!(outline_has_dest(&d), [true, false]);
        // The named link to the deleted page is gone, the one to page 5 stays.
        let a = annots(&d, 0);
        assert_eq!(a.len(), 1);
        assert!(a[0].contains(b"Dest"));
        // Field "name" lost the widget on page 2, "check" lived on page 4 only.
        assert_eq!(field_names(&d), ["name:1"]);
        assert!(catalog(&d).contains(b"OpenAction"));
        assert!(d.get(pagebara_syntax::ObjRef::new(6, 0)).is_none());
        assert!(d.get(pagebara_syntax::ObjRef::new(44, 0)).is_none());
    }
}

#[test]
fn delete_first_page_drops_open_action() {
    let mut doc = open(rich());
    pagebara_pages::delete(&mut doc, &[0]).unwrap();
    assert!(!catalog(&doc).contains(b"OpenAction"));
    assert_eq!(outline_has_dest(&doc), [false, true]);
    assert_eq!(labels(&doc), ["ii", "1", "2", "3", "4"]);
    assert!(matches!(
        pagebara_pages::delete(&mut doc, &[0, 1, 2, 3, 4]),
        Err(Error::WouldBeEmpty)
    ));
    assert!(matches!(
        pagebara_pages::delete(&mut doc, &[9]),
        Err(Error::OutOfRange { index: 9, count: 5 })
    ));
}

#[test]
fn reorder_and_move() {
    let mut doc = open(rich());
    let s = sizes(&doc);
    pagebara_pages::reorder(&mut doc, &[5, 4, 3, 2, 1, 0]).unwrap();
    for d in std::iter::once(doc.clone()).chain(saved(&doc, "")) {
        check_tree(&d);
        assert_eq!(
            texts(&d),
            ["Page 6", "Page 5", "Page 4", "Page 3", "Page 2", "Page 1"]
        );
        assert_eq!(sizes(&d), s.iter().rev().copied().collect::<Vec<_>>());
        assert_eq!(labels(&d), ["4", "3", "2", "1", "ii", "i"]);
        // Destinations follow the page objects, so nothing else changes.
        assert_eq!(names(&d), ["chap1", "chap4", "d2"]);
        assert_eq!(field_names(&d), ["name:2", "check:0"]);
    }
    assert!(matches!(
        pagebara_pages::reorder(&mut doc, &[0, 0, 1, 2, 3, 4]),
        Err(Error::NotPermutation)
    ));
    assert!(matches!(
        pagebara_pages::reorder(&mut doc, &[0, 1]),
        Err(Error::NotPermutation)
    ));

    let mut doc = open(rich());
    pagebara_pages::move_pages(&mut doc, &[4, 0], 2).unwrap();
    assert_eq!(
        texts(&doc),
        ["Page 2", "Page 1", "Page 5", "Page 3", "Page 4", "Page 6"]
    );
    pagebara_pages::move_pages(&mut doc, &[0], 6).unwrap();
    assert_eq!(
        texts(&doc),
        ["Page 1", "Page 5", "Page 3", "Page 4", "Page 6", "Page 2"]
    );
    check_tree(&doc);
}

#[test]
fn rotate_relative_to_inherited() {
    let mut doc = open(rich());
    pagebara_pages::rotate(&mut doc, &[0, 3], -1).unwrap();
    pagebara_pages::rotate(&mut doc, &[1], 6).unwrap();
    for d in std::iter::once(doc.clone()).chain(saved(&doc, "")) {
        let r: Vec<u16> = PageTree::load(&d)
            .pages()
            .iter()
            .map(|p| p.rotate)
            .collect();
        assert_eq!(r, [0, 270, 90, 270, 0, 0]);
    }
}

#[test]
fn crop_and_uncrop() {
    let mut doc = open(rich());
    pagebara_pages::crop(&mut doc, &[3], Some(Rect::new(-50.0, 100.0, 200.5, 1000.0))).unwrap();
    pagebara_pages::crop(&mut doc, &[2], None).unwrap();
    for d in std::iter::once(doc.clone()).chain(saved(&doc, "")) {
        let t = PageTree::load(&d);
        assert_eq!(t.pages()[3].crop_box, Rect::new(0.0, 100.0, 200.5, 400.0));
        assert_eq!(t.pages()[2].crop_box, Rect::new(0.0, 0.0, 612.0, 792.0));
    }
}

#[test]
fn duplicate_pages() {
    let mut doc = open(rich());
    pagebara_pages::duplicate(&mut doc, &[0, 3]).unwrap();
    for d in std::iter::once(doc.clone()).chain(saved(&doc, "")) {
        assert_eq!(check_tree(&d), 8);
        assert_eq!(
            texts(&d),
            [
                "Page 1", "Page 1", "Page 2", "Page 3", "Page 4", "Page 4", "Page 5", "Page 6"
            ]
        );
        assert_eq!(sizes(&d)[1], (792.0, 612.0));
        assert_eq!(labels(&d), ["i", "i", "ii", "1", "2", "2", "3", "4"]);
        // Links are copied and point at the copy; the widget is not.
        let t = PageTree::load(&d);
        let a = annots(&d, 1);
        assert_eq!(a.len(), 2);
        assert_eq!(
            a[0].get(b"P").and_then(Object::as_objref),
            Some(t.pages()[1].r)
        );
        assert!(annots(&d, 5).is_empty());
    }
}

#[test]
fn extract_pages() {
    let src = open(rich());
    let doc = pagebara_pages::extract(&src, &[0, 3, 3]).unwrap();
    for d in std::iter::once(doc.clone()).chain(saved(&doc, "")) {
        assert_eq!(check_tree(&d), 3);
        assert_eq!(texts(&d), ["Page 1", "Page 4", "Page 4"]);
        assert_eq!(sizes(&d), [(792.0, 612.0), (300.0, 400.0), (300.0, 400.0)]);
        assert_eq!(labels(&d), ["i", "2", "2"]);
        // The link to page 5 was dropped; the named link to page 4 became explicit.
        let a = annots(&d, 0);
        assert_eq!(a.len(), 1);
        let dest = a[0].get(b"Dest").unwrap().as_array().unwrap();
        assert_eq!(dest[0].as_objref(), Some(PageTree::load(&d).pages()[1].r));
        assert_eq!(field_names(&d), ["check:0"]);
        // Bookmarks and named destinations leading to the copied pages come along.
        assert_eq!(outline(&d), ["One -> 0", "Four -> 1"]);
        assert_eq!(names(&d), ["chap1", "chap4"]);
    }
    // Only what the pages use was copied: no other page contents.
    let n = doc.object_numbers().len();
    assert!(n < 25, "{n} objects");
}

#[test]
fn extract_keeps_partial_fields() {
    let src = open(rich());
    let doc = pagebara_pages::extract(&src, &[4]).unwrap();
    assert_eq!(field_names(&doc), ["name:1"]);
    let a = annots(&doc, 0);
    assert!(a[0].get(b"Parent").and_then(Object::as_objref).is_some());
}

#[test]
fn insert_from_other_document() {
    let mut doc = open(flat(3));
    let src = open(rich());
    pagebara_pages::insert(&mut doc, 1, &src, &[2, 4, 5]).unwrap();
    for d in std::iter::once(doc.clone()).chain(saved(&doc, "")) {
        assert_eq!(check_tree(&d), 6);
        assert_eq!(
            texts(&d),
            ["Page 1", "Page 3", "Page 5", "Page 6", "Page 2", "Page 3"]
        );
        assert_eq!(sizes(&d)[1], (690.0, 490.0));
        assert_eq!(sizes(&d)[0], (200.0, 300.0));
        // The source labels come along; the destination had none and counts 1, 2, 3.
        assert_eq!(labels(&d), ["1", "1", "3", "4", "2", "3"]);
        assert_eq!(field_names(&d), ["name:1"]);
    }
    // Pages 5 and 6 of the source share one font and one resource dictionary.
    let fonts = doc
        .object_numbers()
        .into_iter()
        .filter(|&n| {
            doc.get(pagebara_syntax::ObjRef::new(n, 0))
                .is_some_and(|o| {
                    o.as_dict().is_some_and(|d| {
                        d.get(b"BaseFont").and_then(Object::as_name) == Some(b"Helvetica")
                    })
                })
        })
        .count();
    assert_eq!(fonts, 1);
}

#[test]
fn insert_into_tree_with_inherited_rotation() {
    // The destination root lends /Rotate to its pages; inserted pages must not take it.
    let mut doc = open(rich());
    let src = open(flat(2));
    pagebara_pages::rotate(&mut doc, &[0], 0).unwrap();
    let mut d = dict(&doc, pagebara_syntax::ObjRef::new(2, 0));
    d.set(b"Rotate", 180);
    doc.set(pagebara_syntax::ObjRef::new(2, 0), d);
    let before: Vec<u16> = PageTree::load(&doc)
        .pages()
        .iter()
        .map(|p| p.rotate)
        .collect();
    pagebara_pages::insert(&mut doc, 6, &src, &[0, 1]).unwrap();
    let after: Vec<u16> = PageTree::load(&doc)
        .pages()
        .iter()
        .map(|p| p.rotate)
        .collect();
    assert_eq!(after[..6], before[..]);
    assert_eq!(after[6..], [0, 0]);
}

#[test]
fn split_and_merge() {
    let src = open(rich());
    let parts = pagebara_pages::split(&src, &[0..2, 2..6]).unwrap();
    assert_eq!(texts(&parts[0]), ["Page 1", "Page 2"]);
    assert_eq!(texts(&parts[1]), ["Page 3", "Page 4", "Page 5", "Page 6"]);
    let other = open(flat(40));
    let merged = pagebara_pages::merge(&[&parts[1], &parts[0], &other]).unwrap();
    for d in std::iter::once(merged.clone()).chain(saved(&merged, "")) {
        assert_eq!(check_tree(&d), 46);
        let t = texts(&d);
        assert_eq!(
            t[..7],
            [
                "Page 3", "Page 4", "Page 5", "Page 6", "Page 1", "Page 2", "Page 1"
            ]
        );
        assert_eq!(t[45], "Page 40");
        let l = labels(&d);
        assert_eq!(l[..8], ["1", "2", "3", "4", "i", "ii", "1", "2"]);
        assert_eq!(l[45], "40");
    }
}

#[test]
fn large_documents_get_balanced_trees() {
    let mut doc = open(flat(5000));
    let t0 = std::time::Instant::now();
    pagebara_pages::reorder(&mut doc, &(0..5000).rev().collect::<Vec<_>>()).unwrap();
    eprintln!("reordered 5000 pages in {:?}", t0.elapsed());
    assert_eq!(check_tree(&doc), 5000);
    pagebara_pages::delete(&mut doc, &(0..4990).collect::<Vec<_>>()).unwrap();
    assert_eq!(check_tree(&doc), 10);
    assert_eq!(texts(&doc)[0], "Page 10");
    // Interior nodes no longer needed were deleted with the pages.
    assert!(pagebara_pages::remove_unreachable(&mut doc) >= 4990);
    let full = doc.save_full().unwrap();
    qpdf_check(&full, "");
    let re = open(full);
    assert_eq!(texts(&re).len(), 10);
    assert!(re.object_numbers().len() < 30);
}

#[test]
fn snapshots_undo_operations() {
    let doc = open(rich());
    let mut edited = doc.clone();
    pagebara_pages::delete(&mut edited, &[0]).unwrap();
    pagebara_pages::rotate(&mut edited, &[0], 1).unwrap();
    assert_eq!(texts(&doc).len(), 6);
    assert_eq!(PageTree::load(&doc).pages()[1].rotate, 90);
    assert_eq!(texts(&edited).len(), 5);
}
