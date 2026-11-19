//! Helpers for the organize view: page ranges as typed, images as new pages, and crop
//! margins.

use std::ops::Range;

use pagebara_i18n::tr;
use pagebara_syntax::{Dict, Document, Object, Stream};

/// Parses "1-3, 5, 8-" (1-based, inclusive; an open end runs to the last page) into
/// 0-based ranges, in the order given.
pub fn parse_ranges(s: &str, count: usize) -> Result<Vec<Range<usize>>, String> {
    let mut out = Vec::new();
    for part in s.split([',', ';']).map(str::trim).filter(|p| !p.is_empty()) {
        let num = |t: &str| -> Result<usize, String> {
            let n: usize = t
                .trim()
                .parse()
                .map_err(|_| tr!("“{}” is not a page number", t))?;
            if n == 0 || n > count {
                return Err(tr!("There is no page {}; the document has {}.", n, count));
            }
            Ok(n)
        };
        let (a, b) = match part.split_once(['-', '–']) {
            Some((a, b)) => {
                let a = if a.trim().is_empty() { 1 } else { num(a)? };
                let b = if b.trim().is_empty() { count } else { num(b)? };
                (a, b)
            }
            None => {
                let n = num(part)?;
                (n, n)
            }
        };
        if a > b {
            // TRANSLATORS: a page range typed with the larger number first, like "5-2"
            return Err(tr!("“{}” runs backwards.", part));
        }
        out.push(a - 1..b);
    }
    if out.is_empty() {
        return Err(tr!("Enter pages, for example 1-3, 5."));
    }
    Ok(out)
}

/// Ranges of `n` pages each.
pub fn every(n: usize, count: usize) -> Vec<Range<usize>> {
    let n = n.max(1);
    (0..count)
        .step_by(n)
        .map(|s| s..(s + n).min(count))
        .collect()
}

/// A one-page document showing `img` (8-bit RGBA, straight alpha) at 96 dpi, scaled down
/// to fit an A4 page when larger.
pub fn image_document(width: u32, height: u32, rgba: &[u8]) -> Result<Document, String> {
    let mut doc = pagebara_pages::blank((1, 7));
    let image = pagebara_annot::embed_image(
        &mut doc,
        pagebara_annot::RgbaImage {
            width,
            height,
            pixels: rgba,
        },
    )
    .map_err(|e| e.to_string())?;
    let (mut w, mut h) = (f64::from(width) * 0.75, f64::from(height) * 0.75);
    let fit = (595.0 / w).min(842.0 / h).min(1.0);
    w *= fit;
    h *= fit;
    let content = format!("q {w:.3} 0 0 {h:.3} 0 0 cm /Im0 Do Q");
    let contents = doc.add(Stream::new(Dict::new(), content.into_bytes()));
    let root = doc
        .catalog()
        .and_then(|c| c.as_dict()?.get(b"Pages")?.as_objref())
        .ok_or("no page tree")?;
    let mut xobjects = Dict::new();
    xobjects.set(b"Im0", image);
    let mut res = Dict::new();
    res.set(b"XObject", xobjects);
    let mut page = Dict::new();
    page.set(b"Type", Object::name(b"Page"));
    page.set(b"Parent", root);
    page.set(
        b"MediaBox",
        vec![
            Object::Int(0),
            Object::Int(0),
            Object::Real(w),
            Object::Real(h),
        ],
    );
    page.set(b"Resources", res);
    page.set(b"Contents", contents);
    let page = doc.add(page);
    let mut pages = Dict::new();
    pages.set(b"Type", Object::name(b"Pages"));
    pages.set(b"Kids", vec![Object::Ref(page)]);
    pages.set(b"Count", 1);
    doc.set(root, pages);
    Ok(doc)
}

/// The crop box leaving `margins` (left, top, right, bottom, in points as the page is
/// shown) of a page with geometry `g`, in user space.
pub fn crop_rect(
    g: &crate::pages::PageGeom,
    [l, t, r, b]: [f64; 4],
) -> Option<pagebara_pages::Rect> {
    let (w, h) = g.size();
    if l + r >= w - 1.0 || t + b >= h - 1.0 {
        return None;
    }
    let [x0, y0, x1, y1] = g.rect_to_pdf([l, t, w - l - r, h - t - b]);
    Some(pagebara_pages::Rect::new(x0, y0, x1, y1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_ranges("1-3, 5, 8-", 10).unwrap(), [0..3, 4..5, 7..10]);
        assert_eq!(parse_ranges("-2", 10).unwrap(), vec![0..2]);
        assert!(parse_ranges("0", 10).is_err());
        assert!(parse_ranges("4-2", 10).is_err());
        assert!(parse_ranges("x", 10).is_err());
        assert!(parse_ranges("", 10).is_err());
        assert_eq!(every(4, 10), [0..4, 4..8, 8..10]);
    }

    #[test]
    fn image_page_inserts() {
        let px = vec![200u8; 40 * 20 * 4];
        let img = image_document(40, 20, &px).unwrap();
        let tree = pagebara_pages::PageTree::load(&img);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree.pages()[0].size(), (30.0, 15.0));
        let mut doc = pagebara_pages::blank((1, 7));
        pagebara_pages::insert(&mut doc, 0, &img, &[0]).unwrap();
        assert_eq!(pagebara_pages::PageTree::load(&doc).len(), 1);
    }

    #[test]
    fn crop_margins() {
        let g = crate::pages::PageGeom::default();
        let r = crop_rect(&g, [72.0, 36.0, 0.0, 0.0]).unwrap();
        assert_eq!((r.x0, r.y0, r.x1, r.y1), (72.0, 0.0, 612.0, 756.0));
        assert!(crop_rect(&g, [400.0, 0.0, 400.0, 0.0]).is_none());
    }
}
