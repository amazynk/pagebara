use pagebara_pages::PageTree;
use pagebara_syntax::{Document, ObjRef};

use super::*;
use crate::test_pdf;

#[test]
fn tree_sizes_and_lookup() {
    let doc = Document::open(test_pdf::sample()).unwrap();
    let tree = PageTree::open(&doc);
    assert_eq!(tree.len(), 3);
    assert_eq!(
        sizes(&tree),
        [(595.0, 842.0), (400.0, 300.0), (792.0, 612.0)]
    );
    assert_eq!(PageSource::index_of(&tree, ObjRef::new(6, 0)), Some(2));
}

#[test]
fn geometry_round_trips_every_rotation() {
    for rotate in [0, 90, 180, 270] {
        let g = PageGeom {
            crop: [10.0, 20.0, 310.0, 420.0],
            rotate,
            unit: 1.0,
        };
        for (x, y) in [(10.0, 20.0), (310.0, 420.0), (100.0, 250.0)] {
            let (u, v) = g.to_view(x, y);
            let (w, h) = g.size();
            assert!((-1e-9..=w + 1e-9).contains(&u) && (-1e-9..=h + 1e-9).contains(&v));
            let (x2, y2) = g.to_pdf(u, v);
            assert!((x - x2).abs() < 1e-9 && (y - y2).abs() < 1e-9, "{rotate}");
        }
    }
    let g = PageGeom {
        crop: [0.0, 0.0, 200.0, 100.0],
        rotate: 90,
        unit: 1.0,
    };
    // The top-left corner of the unrotated page turns to the top right.
    assert_eq!(g.to_view(0.0, 100.0), (100.0, 0.0));
    assert_eq!(g.size(), (100.0, 200.0));
}
