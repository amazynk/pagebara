mod common;

use common::*;
use pagebara_pages::{PageTree, Rect};

#[test]
fn inherited_attributes_and_sizes() {
    let doc = open(rich());
    let t = PageTree::load(&doc);
    assert!(!t.repaired());
    assert_eq!(t.len(), 6);
    assert_eq!(
        t.pages().iter().map(|p| p.r.num).collect::<Vec<_>>(),
        [5, 6, 7, 8, 9, 10]
    );
    assert_eq!(t.pages()[0].rotate, 90);
    assert_eq!(t.pages()[0].size(), (792.0, 612.0));
    assert_eq!(t.pages()[2].crop_box, Rect::new(10.0, 10.0, 500.0, 700.0));
    assert_eq!(t.pages()[2].size(), (690.0, 490.0));
    assert_eq!(t.pages()[3].rotate, 0);
    assert_eq!(t.pages()[3].size(), (300.0, 400.0));
    assert_eq!(t.pages()[5].size(), (300.0, 400.0));
    assert_eq!(
        pagebara_pages::page_labels(&doc, t.len()).unwrap(),
        ["i", "ii", "1", "2", "3", "4"]
    );
}

fn tree_with(pages_root: &str, extra: &[(u32, &str)]) -> pagebara_syntax::Document {
    let mut o = vec![
        (1, "<</Type/Catalog/Pages 2 0 R>>".to_string()),
        (2, pages_root.to_string()),
    ];
    o.extend(extra.iter().map(|(n, s)| (*n, s.to_string())));
    o.sort_by_key(|x| x.0);
    open(pdf(&o, 1))
}

#[test]
fn damaged_trees() {
    // A cycle back to the root, a repeated kid, a wrong /Count, a non-reference kid and a
    // missing object.
    let doc = tree_with(
        "<</Type/Pages/Kids[3 0 R 4 0 R 3 0 R 5 0 R (junk) 99 0 R]/Count 42>>",
        &[
            (3, "<</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]>>"),
            (4, "<</Type/Pages/Kids[2 0 R 4 0 R 6 0 R]/Count 1>>"),
            (5, "<</Type/Page/MediaBox[0 0 50 60]/Rotate -90>>"),
            (6, "<</Type/Page/MediaBox[0 0 1 1]/Rotate 45>>"),
        ],
    );
    let t = PageTree::load(&doc);
    assert_eq!(
        t.pages().iter().map(|p| p.r.num).collect::<Vec<_>>(),
        [3, 6, 5]
    );
    assert_eq!(t.pages()[1].rotate, 0);
    assert_eq!(t.pages()[2].rotate, 270);
    assert_eq!(t.pages()[2].size(), (60.0, 50.0));
}

#[test]
fn kids_without_types_and_broken_boxes() {
    let doc = tree_with(
        "<</Kids[3 0 R]/MediaBox[0 0 0 0]>>",
        &[
            (3, "<</Kids[4 0 R]/CropBox[1000 1000 2000 2000]>>"),
            (4, "<</Contents 5 0 R/UserUnit 2>>"),
        ],
    );
    let t = PageTree::load(&doc);
    assert_eq!(t.len(), 1);
    // Empty media box falls back to Letter; a crop box outside it is ignored.
    assert_eq!(t.pages()[0].media_box, Rect::LETTER);
    assert_eq!(t.pages()[0].crop_box, Rect::LETTER);
    assert_eq!(t.pages()[0].size(), (1224.0, 1584.0));
}

#[test]
fn catalog_pointing_at_a_page() {
    let doc = tree_with("<</Type/Page/MediaBox[0 0 10 20]>>", &[]);
    let t = PageTree::load(&doc);
    assert_eq!(t.len(), 1);
    assert!(t.root().is_none());
    let mut doc = doc;
    pagebara_pages::duplicate(&mut doc, &[0]).unwrap();
    assert_eq!(check_tree(&doc), 2);
    for d in saved(&doc, "") {
        assert_eq!(PageTree::load(&d).len(), 2);
    }
}

#[test]
fn missing_tree_is_recovered_by_scanning() {
    let o = vec![
        (1, "<</Type/Catalog/Pages 9 0 R>>".to_string()),
        (
            3,
            "<</Type/Page/MediaBox[0 0 10 10]/Contents 4 0 R>>".to_string(),
        ),
        (4, content("Page 1")),
        (
            5,
            "<</Type/Page/MediaBox[0 0 10 10]/Contents 6 0 R>>".to_string(),
        ),
        (6, content("Page 2")),
    ];
    let mut doc = open(pdf(&o, 1));
    let t = PageTree::load(&doc);
    assert!(t.repaired());
    assert_eq!(t.len(), 2);
    // Any structural edit writes a proper tree.
    pagebara_pages::move_pages(&mut doc, &[1], 0).unwrap();
    assert_eq!(check_tree(&doc), 2);
    assert!(!PageTree::load(&doc).repaired());
    assert_eq!(texts(&doc), ["Page 2", "Page 1"]);
}

#[test]
fn empty_document() {
    let doc = pagebara_pages::blank((1, 7));
    let t = PageTree::load(&doc);
    assert!(t.is_empty() && !t.repaired());
}

#[test]
fn large_flat_tree_is_fast() {
    let doc = open(flat(20_000));
    let t0 = std::time::Instant::now();
    let t = PageTree::load(&doc);
    let dt = t0.elapsed();
    assert_eq!(t.len(), 20_000);
    assert_eq!(t.pages()[19_999].size(), (200.0, 300.0));
    eprintln!("20k pages walked in {dt:?}");
}

#[test]
fn lazy_lookup_matches_full_walk() {
    for (n, fanout) in [(1, 4), (7, 3), (100, 10), (1000, 32), (50, 50)] {
        let doc = open(balanced(n, fanout));
        let lazy = PageTree::open(&doc);
        assert_eq!(lazy.len(), n as usize);
        let full = PageTree::load(&doc);
        for i in 0..n as usize {
            let p = lazy.page(i).unwrap();
            let q = &full.pages()[i];
            assert_eq!(
                (p.r, p.media_box, p.rotate),
                (q.r, q.media_box, q.rotate),
                "{n}/{i}"
            );
            assert_eq!(lazy.index_of(p.r), Some(i));
        }
        assert!(lazy.page(n as usize).is_none());
        assert!(!lazy.repaired());
    }
    let doc = open(rich());
    let lazy = PageTree::open(&doc);
    let full = PageTree::load(&doc);
    for (i, q) in full.pages().iter().enumerate() {
        let p = lazy.page(i).unwrap();
        assert_eq!((p.r, p.size()), (q.r, q.size()));
    }
}

#[test]
fn lazy_lookup_reads_only_its_path() {
    // Node 4 claims two pages but one of its kids is missing. Page 0 is found without
    // looking inside node 4; page 2 then needs it, and the walk corrects the count.
    let doc = tree_with(
        "<</Type/Pages/Kids[3 0 R 4 0 R]/Count 3>>",
        &[
            (3, "<</Type/Page/Parent 2 0 R>>"),
            (4, "<</Type/Pages/Parent 2 0 R/Kids[5 0 R 99 0 R]/Count 2>>"),
            (5, "<</Type/Page/Parent 4 0 R>>"),
        ],
    );
    let t = PageTree::open(&doc);
    assert_eq!(t.len(), 3);
    assert_eq!(t.page(0).unwrap().r.num, 3);
    assert_eq!(t.page(1).unwrap().r.num, 5);
    assert_eq!(t.len(), 3);
    assert!(t.page(2).is_none());
    assert_eq!(t.len(), 2);
    assert_eq!(t.index_of(pagebara_syntax::ObjRef::new(5, 0)), Some(1));
}

#[test]
fn wrong_counts_fall_back_to_the_walk() {
    // The root's kids do not add up: walked on opening.
    let doc = tree_with(
        "<</Type/Pages/Kids[3 0 R 4 0 R]/Count 5>>",
        &[
            (3, "<</Type/Page/Parent 2 0 R>>"),
            (4, "<</Type/Page/Parent 2 0 R>>"),
        ],
    );
    assert_eq!(PageTree::open(&doc).len(), 2);
    // An interior node's do not: found on the way to page 1.
    let doc = tree_with(
        "<</Type/Pages/Kids[3 0 R 4 0 R]/Count 3>>",
        &[
            (3, "<</Type/Page/Parent 2 0 R>>"),
            (4, "<</Type/Pages/Parent 2 0 R/Kids[5 0 R]/Count 2>>"),
            (5, "<</Type/Page/Parent 4 0 R/Rotate 90>>"),
        ],
    );
    let t = PageTree::open(&doc);
    assert_eq!(t.len(), 3);
    assert_eq!(t.page(1).unwrap().rotate, 90);
    assert_eq!(t.len(), 2);
    // A cycle back to the root.
    let doc = tree_with(
        "<</Type/Pages/Kids[3 0 R 4 0 R]/Count 2>>",
        &[
            (3, "<</Type/Page/Parent 2 0 R>>"),
            (4, "<</Type/Pages/Parent 2 0 R/Kids[2 0 R]/Count 1>>"),
        ],
    );
    let t = PageTree::open(&doc);
    assert_eq!(t.page(0).unwrap().r.num, 3);
    assert!(t.page(1).is_none());
    assert_eq!(t.len(), 1);
}

/// Prints timings for 100 000 pages; run in release for meaningful numbers:
/// `cargo test --release -p pagebara-pages --test tree hundred_k -- --nocapture`.
#[test]
fn hundred_k_pages() {
    use std::time::Instant;
    const N: u32 = 100_000;
    for (name, bytes) in [("balanced", balanced(N, 10)), ("flat", flat(N))] {
        let len = bytes.len();
        let t0 = Instant::now();
        let doc = open(bytes);
        let opened = t0.elapsed();
        let t1 = Instant::now();
        let t = PageTree::open(&doc);
        let first = t.page(0).unwrap();
        let first_page = t1.elapsed();
        let t2 = Instant::now();
        let last = t.page(N as usize - 1).unwrap();
        let mid = t.index_of(t.page(N as usize / 2).unwrap().r);
        let lookups = t2.elapsed();
        assert_eq!(t.len(), N as usize);
        assert_eq!(mid, Some(N as usize / 2));
        let t3 = Instant::now();
        let full = PageTree::load(&doc);
        let walk = t3.elapsed();
        assert_eq!(full.len(), N as usize);
        assert_eq!(full.pages()[0].r, first.r);
        assert_eq!(full.pages()[N as usize - 1].r, last.r);
        eprintln!(
            "{name} 100k ({} MB): open {opened:?}, first page {first_page:?}, \
             last page + index_of {lookups:?}, full walk {walk:?}",
            len >> 20
        );
        assert!(first_page < walk, "lazy access should not walk");
    }
}
