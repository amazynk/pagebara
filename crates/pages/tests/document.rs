//! What travels with pages: structure, threads, outlines, document settings, forms,
//! optional content, attachments; plus permissions and compaction.

mod common;

use common::*;
use pagebara_pages::{Access, Error, PageTree};
use pagebara_syntax::{Document, Object, Permissions};

fn structure(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|s| s.to_string()).collect()
}

fn fields(doc: &Document) -> Vec<String> {
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
            let r = o.as_objref().unwrap();
            let d = dict(doc, r);
            let kids = doc.get_in(&d, b"Kids");
            let kids = kids.as_array().unwrap_or_default();
            for k in kids {
                let k = dict(doc, k.as_objref().unwrap());
                assert_eq!(k.get(b"Parent").and_then(Object::as_objref), Some(r));
                assert!(k.contains(b"AP"), "widget keeps its appearance");
            }
            format!(
                "{}:{}",
                String::from_utf8_lossy(d.get(b"T").unwrap().as_string().unwrap()),
                kids.len()
            )
        })
        .collect()
}

fn ocgs(doc: &Document, key: &[u8]) -> usize {
    let cat = catalog(doc);
    let oc = doc.get_in(&cat, b"OCProperties");
    let oc = oc.as_dict().unwrap();
    let d = doc.get_in(oc, b"D");
    let holder = if key == b"OCGs" {
        oc
    } else {
        d.as_dict().unwrap()
    };
    doc.get_in(holder, key)
        .as_array()
        .map_or(0, <[Object]>::len)
}

fn info_title(doc: &Document) -> String {
    let info = doc.get_in(doc.trailer(), b"Info");
    let t = doc.get_in(info.as_dict().unwrap(), b"Title");
    String::from_utf8_lossy(t.as_string().unwrap()).into_owned()
}

#[test]
fn source_is_consistent() {
    let doc = open(tagged());
    check_all(&doc);
    assert_eq!(
        check_structure(&doc),
        structure(&[
            "Document[]",
            "  P[0]",
            "  Span[0, 1]",
            "  Link[1]",
            "  Sect[]",
            "    Para[2]",
            "    Span[2]"
        ])
    );
    assert_eq!(check_threads(&doc), [vec![0, 1, 2], vec![1]]);
}

#[test]
fn delete_prunes_structure_and_threads() {
    let mut doc = open(tagged());
    pagebara_pages::delete(&mut doc, &[1]).unwrap();
    for d in std::iter::once(doc.clone()).chain(saved(&doc, "")) {
        check_all(&d);
        assert_eq!(texts(&d), ["Page 1", "Page 3"]);
        assert_eq!(
            check_structure(&d),
            structure(&[
                "Document[]",
                "  P[0]",
                "  Span[0]",
                "  Sect[]",
                "    Para[1]",
                "    Span[1]"
            ])
        );
        // Thread B had its only bead on the deleted page.
        assert_eq!(check_threads(&d), [vec![0, 1]]);
        assert_eq!(
            outline(&d),
            [
                "Chapter 1 -> 0",
                "  Sec 1.1 -> -",
                "  Sec 1.2 -> (n3)",
                "Chapter 2 -> -+",
                "  Sec 2.1 -> 1",
                "Web -> -"
            ]
        );
        assert_eq!(fields(&d), ["choice:2"]);
    }

    let mut doc = open(tagged());
    pagebara_pages::delete(&mut doc, &[0, 2]).unwrap();
    for d in std::iter::once(doc.clone()).chain(saved(&doc, "")) {
        check_all(&d);
        assert_eq!(
            check_structure(&d),
            structure(&["Document[]", "  Span[0]", "  Link[0]"])
        );
        assert_eq!(check_threads(&d), [vec![0], vec![0]]);
        // The bookmark's structure element went with page 1.
        let cat = catalog(&d);
        let o = d.get_in(&cat, b"Outlines");
        let first = dict(
            &d,
            o.as_dict()
                .unwrap()
                .get(b"First")
                .unwrap()
                .as_objref()
                .unwrap(),
        );
        assert!(!first.contains(b"SE"));
    }
}

#[test]
fn delete_in_steps() {
    let mut doc = open(tagged());
    pagebara_pages::delete(&mut doc, &[1]).unwrap();
    pagebara_pages::delete(&mut doc, &[0]).unwrap();
    check_all(&doc);
    assert_eq!(
        check_structure(&doc),
        structure(&["Document[]", "  Sect[]", "    Para[0]", "    Span[0]"])
    );
}

#[test]
fn extract_carries_the_document_along() {
    let src = open(tagged());
    let doc = pagebara_pages::extract(&src, &[2, 0]).unwrap();
    for d in std::iter::once(doc.clone()).chain(saved(&doc, "")) {
        check_all(&d);
        assert_eq!(texts(&d), ["Page 3", "Page 1"]);
        assert_eq!(
            outline(&d),
            [
                "Chapter 1 -> 1",
                "  Sec 1.2 -> 0",
                "Chapter 2 -> -+",
                "  Sec 2.1 -> 0"
            ]
        );
        assert_eq!(
            check_structure(&d),
            structure(&[
                "Document[]",
                "  P[1]",
                "  Span[1]",
                "  Sect[]",
                "    Para[0]",
                "    Span[0]"
            ])
        );
        assert_eq!(check_threads(&d), [vec![1, 0]]);
        assert_eq!(info_title(&d), "Tagged");
        let cat = catalog(&d);
        for key in [
            &b"Lang"[..],
            b"PageMode",
            b"PageLayout",
            b"ViewerPreferences",
            b"MarkInfo",
        ] {
            assert!(cat.contains(key), "{}", String::from_utf8_lossy(key));
        }
        assert_eq!(name_keys(&d, b"Dests"), ["n3"]);
        assert!(name_keys(&d, b"EmbeddedFiles").is_empty());
        assert_eq!(ocgs(&d, b"OCGs"), 1);
        assert_eq!(fields(&d), ["choice:2"]);
    }
}

#[test]
fn dropped_links_leave_the_structure() {
    // Page 2's link leads to page 3, which is not copied: the link, its object reference
    // and its parent tree entry all go.
    let src = open(tagged());
    let doc = pagebara_pages::extract(&src, &[1]).unwrap();
    for d in std::iter::once(doc.clone()).chain(saved(&doc, "")) {
        check_all(&d);
        assert_eq!(
            check_structure(&d),
            structure(&["Document[]", "  Span[0]", "  Link[0]"])
        );
        let s = check_structure(&d);
        assert_eq!(s.len(), 3);
        let p = PageTree::load(&d).pages()[0].r;
        let annots = d.get_in(&dict(&d, p), b"Annots").as_array().unwrap().len();
        assert_eq!(annots, 1);
    }
}

#[test]
fn split_carries_each_part() {
    let src = open(tagged());
    let parts = pagebara_pages::split(&src, &[0..1, 1..3]).unwrap();
    for p in &parts {
        check_all(p);
        assert_eq!(info_title(p), "Tagged");
    }
    assert_eq!(outline(&parts[0]), ["Chapter 1 -> 0"]);
    assert_eq!(
        outline(&parts[1]),
        [
            "Chapter 1 -> -",
            "  Sec 1.1 -> 0",
            "  Sec 1.2 -> 1",
            "Chapter 2 -> -+",
            "  Sec 2.1 -> 1"
        ]
    );
    assert_eq!(check_threads(&parts[0]), [vec![0]]);
    assert_eq!(check_threads(&parts[1]), [vec![0, 1], vec![0]]);
    assert_eq!(
        check_structure(&parts[1]),
        structure(&[
            "Document[]",
            "  Span[0]",
            "  Link[0]",
            "  Sect[]",
            "    Para[1]",
            "    Span[1]"
        ])
    );
}

#[test]
fn merge_keeps_both_documents_apart() {
    let a = open(tagged());
    let b = open(tagged());
    let doc = pagebara_pages::merge(&[&a, &b]).unwrap();
    // Left for manual inspection (mutool draw, qpdf --check).
    std::fs::write(tmp("merged-tagged.pdf"), doc.save_full().unwrap()).unwrap();
    for d in std::iter::once(doc.clone()).chain(saved(&doc, "")) {
        check_all(&d);
        assert_eq!(check_tree(&d), 6);
        assert_eq!(
            outline(&d),
            [
                "Tagged -> 0+",
                "  Chapter 1 -> 0",
                "    Sec 1.1 -> 1",
                "    Sec 1.2 -> 2",
                "  Chapter 2 -> -+",
                "    Sec 2.1 -> 2",
                "Tagged -> 3+",
                "  Chapter 1 -> 3",
                "    Sec 1.1 -> 4",
                "    Sec 1.2 -> 5",
                "  Chapter 2 -> -+",
                "    Sec 2.1 -> 5"
            ]
        );
        // Colliding fields are renamed; the radio groups stay whole.
        assert_eq!(fields(&d), ["choice:2", "name:0", "choice#2:2", "name#2:0"]);
        assert_eq!(name_keys(&d, b"Dests"), ["n3", "n3#2"]);
        assert_eq!(name_keys(&d, b"EmbeddedFiles"), ["a.txt", "a.txt#2"]);
        assert_eq!(name_keys(&d, b"JavaScript"), ["init", "init#2"]);
        assert_eq!(ocgs(&d, b"OCGs"), 2);
        assert_eq!(ocgs(&d, b"ON"), 2);
        assert_eq!(ocgs(&d, b"Order"), 2);
        assert_eq!(
            check_threads(&d),
            [vec![0, 1, 2], vec![1], vec![3, 4, 5], vec![4]]
        );
        let s = check_structure(&d);
        assert_eq!(s.len(), 14);
        assert_eq!(s[7], "Document[]");
        assert_eq!(s[9], "  Span[3, 4]");
        assert_eq!(info_title(&d), "Tagged");
    }
}

#[test]
fn merge_titles_untitled_documents_by_index() {
    let doc = pagebara_pages::merge(&[&open(rich()), &open(flat(2))]).unwrap();
    assert_eq!(
        outline(&doc),
        [
            "Document 1 -> 0+",
            "  One -> 0",
            "  Four -> 3",
            "Document 2 -> 6"
        ]
    );
    check_all(&doc);
}

#[test]
fn merge_keeps_layer_states() {
    // Base state OFF: the layer is hidden unless listed in /ON.
    let o = vec![
        (1, "<</Type/Catalog/Pages 2 0 R/OCProperties<</OCGs[5 0 R]/D<</BaseState/OFF/Order[5 0 R]>>>>>>".to_string()),
        (2, "<</Type/Pages/Kids[3 0 R]/Count 1/MediaBox[0 0 100 100]>>".to_string()),
        (3, "<</Type/Page/Parent 2 0 R/Contents 4 0 R/Resources<</Properties<</l 5 0 R>>>>>>".to_string()),
        (4, stream("/OC /l BDC EMC")),
        (5, "<</Type/OCG/Name(Hidden)>>".to_string()),
    ];
    let hidden = open(pdf(&o, 1));
    let doc = pagebara_pages::merge(&[&open(tagged()), &hidden]).unwrap();
    assert_eq!(ocgs(&doc, b"OCGs"), 2);
    assert_eq!(ocgs(&doc, b"ON"), 1);
    assert_eq!(ocgs(&doc, b"OFF"), 1);
    assert_eq!(ocgs(&doc, b"Order"), 2);
    check_all(&doc);
}

#[test]
fn insert_joins_structure_trees() {
    let mut doc = open(tagged());
    let src = open(tagged());
    pagebara_pages::insert(&mut doc, 1, &src, &[1, 2]).unwrap();
    for d in std::iter::once(doc.clone()).chain(saved(&doc, "")) {
        check_all(&d);
        assert_eq!(
            texts(&d),
            ["Page 1", "Page 2", "Page 3", "Page 2", "Page 3"]
        );
        let s = check_structure(&d);
        assert_eq!(
            s[7..],
            [
                "Document[]",
                "  Span[1]",
                "  Link[1]",
                "  Sect[]",
                "    Para[2]",
                "    Span[2]"
            ]
        );
        assert_eq!(
            check_threads(&d),
            [vec![0, 3, 4], vec![3], vec![1, 2], vec![1]]
        );
        assert_eq!(fields(&d), ["choice:2", "name:0", "name#2:0"]);
    }
}

#[test]
fn duplicates_stay_out_of_structure_and_threads() {
    let mut doc = open(tagged());
    pagebara_pages::duplicate(&mut doc, &[0, 1]).unwrap();
    for d in std::iter::once(doc.clone()).chain(saved(&doc, "")) {
        check_all(&d);
        assert_eq!(check_threads(&d), [vec![0, 2, 4], vec![2]]);
    }
}

#[test]
fn permissions_are_enforced() {
    let mut locked = open(rich());
    let p = Permissions {
        assemble: false,
        modify: false,
        copy: false,
        ..Permissions::ALL
    };
    locked.set_password("", "owner", p).unwrap();
    let mut doc = open(locked.save_full().unwrap());
    assert!(!pagebara_pages::permitted(&doc, Access::Assemble));
    assert!(!pagebara_pages::permitted(&doc, Access::Extract));
    let refused = |r: pagebara_pages::Result<()>, a| {
        assert!(matches!(r, Err(Error::NotPermitted(x)) if x == a), "{r:?}");
    };
    refused(pagebara_pages::delete(&mut doc, &[0]), Access::Assemble);
    refused(pagebara_pages::rotate(&mut doc, &[0], 1), Access::Assemble);
    refused(
        pagebara_pages::reorder(&mut doc, &[1, 0, 2, 3, 4, 5]),
        Access::Assemble,
    );
    refused(
        pagebara_pages::extract(&doc, &[0]).map(drop),
        Access::Extract,
    );
    refused(pagebara_pages::merge(&[&doc]).map(drop), Access::Extract);
    let mut other = open(flat(1));
    refused(
        pagebara_pages::insert(&mut other, 0, &doc, &[0]),
        Access::Extract,
    );
    assert!(!doc.is_modified());

    // The owner password lifts the restrictions.
    assert!(doc.unlock_owner("owner"));
    pagebara_pages::delete(&mut doc, &[0]).unwrap();
    assert_eq!(PageTree::load(&doc).len(), 5);
    pagebara_pages::extract(&doc, &[0]).unwrap();
}

#[test]
fn compact_drops_deleted_pages() {
    let mut doc = open(flat(50));
    pagebara_pages::delete(&mut doc, &(0..45).collect::<Vec<_>>()).unwrap();
    let full = doc.save_full().unwrap();
    let compact = pagebara_pages::save_compact(&doc).unwrap();
    qpdf_check(&compact, "");
    assert!(
        compact.len() * 2 < full.len(),
        "{} vs {}",
        compact.len(),
        full.len()
    );
    let re = open(compact);
    assert_eq!(
        texts(&re),
        ["Page 46", "Page 47", "Page 48", "Page 49", "Page 50"]
    );
    assert!(re.object_numbers().len() < 20);
    // Incremental saves append to the original, which keeps every old object.
    let inc = doc.save_incremental().unwrap();
    assert!(inc.len() > doc.source().len());
    let mut c = doc.clone();
    assert!(pagebara_pages::compact(&mut c) >= 45);
}
