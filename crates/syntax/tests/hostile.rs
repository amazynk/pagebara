//! Damaged files found by corpus runs: each opens, and saves to a file that reads back
//! the same, in bounded memory.

use pagebara_syntax::{Document, ObjRef, SaveOptions};

/// Objects written one after another with no cross-reference, for repair to find.
fn bare(objs: &[(u32, &str)], trailer: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    for (n, o) in objs {
        out.extend(format!("{n} 0 obj\n{o}\nendobj\n").bytes());
    }
    out.extend(trailer.bytes());
    out
}

const PAGES: [(u32, &str); 3] = [
    (1, "<</Type/Catalog/Pages 2 0 R>>"),
    (2, "<</Type/Pages/Kids[3 0 R]/Count 1>>"),
    (3, "<</Type/Page/Parent 2 0 R/MediaBox[0 0 10 10]>>"),
];

fn pages(doc: &Document) -> i64 {
    let root = doc.catalog().unwrap();
    let pages = doc.get_in(root.as_dict().unwrap(), b"Pages");
    pages.as_dict().and_then(|d| d.int(b"Count")).unwrap_or(0)
}

/// An object numbered 2147483647 (pdf.js bug 1980958) made the full save write a
/// cross-reference table of two billion rows.
#[test]
fn huge_object_number_saves_small() {
    let mut objs = PAGES.to_vec();
    objs.push((2_147_483_647, "<</Root 1 0 R>>"));
    let doc = Document::open(bare(&objs, "")).unwrap();
    for out in [
        doc.save_full().unwrap(),
        doc.save_full_with(&SaveOptions::compact()).unwrap(),
    ] {
        assert!(out.len() < 4096, "{} bytes", out.len());
        assert_eq!(pages(&Document::open(out).unwrap()), 1);
    }
}

/// Sparse object numbers below the limit still give a small table.
#[test]
fn sparse_object_numbers_save_small() {
    let mut objs = PAGES.to_vec();
    objs.push((8_000_000, "(far)"));
    let doc = Document::open(bare(&objs, "")).unwrap();
    let out = doc.save_full().unwrap();
    assert!(out.len() < 4096, "{} bytes", out.len());
    let back = Document::open(out).unwrap();
    assert!(back.get(ObjRef::new(8_000_000, 0)).is_some());
}

/// An `/Encrypt` entry naming a damaged object (PDFBOX-4352): the file reads as
/// unprotected, as in other readers, instead of failing as unsupported encryption.
#[test]
fn broken_encrypt_dictionary_is_ignored() {
    let mut objs = PAGES.to_vec();
    objs.push((4, "E< /Filter /Standard /V 5 /R 6 >>"));
    let doc = Document::open(bare(
        &objs,
        "trailer\n<</Root 1 0 R/Size 5/Encrypt 4 0 R>>\n",
    ))
    .unwrap();
    assert!(!doc.is_encrypted());
    assert_eq!(pages(&doc), 1);
    let saved = Document::open(doc.save_full().unwrap()).unwrap();
    assert!(!saved.is_encrypted());
}

/// A cross-reference that lists only some objects (a truncated xref stream in
/// PDFBOX-3148): the rest are found by scanning, and a full save must keep them.
#[test]
fn full_save_keeps_objects_missing_from_the_table() {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (n, o) in PAGES {
        offsets.push(out.len());
        out.extend(format!("{n} 0 obj\n{o}\nendobj\n").bytes());
    }
    let x = out.len();
    // Lists objects 1 and 2 only; the page, object 3, is missing.
    out.extend(
        format!(
            "xref\n0 3\n0000000000 65535 f\r\n{:010} 00000 n\r\n{:010} 00000 n\r\ntrailer\n<</Size 3/Root 1 0 R>>\nstartxref\n{x}\n%%EOF\n",
            offsets[0], offsets[1]
        )
        .bytes(),
    );
    let doc = Document::open(out).unwrap();
    for saved in [
        doc.save_full().unwrap(),
        doc.save_full_with(&SaveOptions::compact()).unwrap(),
    ] {
        let back = Document::open(saved).unwrap();
        let page = back.get(ObjRef::new(3, 0)).expect("the page survives");
        assert_eq!(page.as_dict().unwrap().name(b"Type"), Some(&b"Page"[..]));
    }
}

/// A fuzzer find: a catalog numbered 0, found by repair, was left out of full saves
/// (object 0 cannot be written), so the saved file had no pages.
#[test]
fn catalog_numbered_zero_survives_saving() {
    let mut objs = PAGES.to_vec();
    objs[0].0 = 0;
    let mut doc = Document::open(bare(&objs, "")).unwrap();
    assert_eq!(pages(&doc), 1);
    for out in [
        doc.save_full().unwrap(),
        doc.save_full_with(&SaveOptions::compact()).unwrap(),
    ] {
        assert_eq!(pages(&Document::open(out).unwrap()), 1);
    }
    assert!(doc.move_root_off_zero());
    assert!(!doc.move_root_off_zero());
}

/// A fuzzer find: the catalog numbered 0 names a page tree that is missing, under the
/// number the next new object would get. Moved to that number, the catalog was its own
/// page tree, and the saved file had a page the first never had.
#[test]
fn catalog_numbered_zero_does_not_take_a_number_something_refers_to() {
    let objs = [
        (1, "<</Kind/Other/Next 3 0 R>>"),
        (0, "<</Type/Catalog/Pages 2 0 R>>"),
    ];
    let doc = Document::open(bare(&objs, "")).unwrap();
    let tree = |d: &Document| {
        let root = d.catalog().unwrap();
        d.get_in(root.as_dict().unwrap(), b"Pages")
            .as_dict()
            .cloned()
    };
    assert!(tree(&doc).is_none());
    for out in [
        doc.save_full().unwrap(),
        doc.save_full_with(&SaveOptions::compact()).unwrap(),
    ] {
        let back = Document::open(out).unwrap();
        assert!(back.catalog().is_some());
        assert!(tree(&back).is_none());
    }
    let mut moved = doc.clone();
    assert!(moved.move_root_off_zero());
    let root = moved
        .trailer()
        .get(b"Root")
        .and_then(|o| o.as_objref())
        .unwrap();
    assert_eq!(root.num, 4);
}

/// A fuzzer find: a trailer that names object 0 as the catalog in a file with no object 0.
/// The full save moves such a catalog to a number of its own and starts again; with no
/// catalog to move it started again without end.
#[test]
fn root_zero_without_a_catalog_saves() {
    let doc = Document::open(bare(&PAGES[1..], "trailer\n<</Root 0 0 R>>\n")).unwrap();
    assert!(doc.catalog().is_none());
    doc.save_full().unwrap();
    doc.save_full_with(&SaveOptions::compact()).unwrap();
}
