//! Tiling patterns painted where regions are keep nothing of what lay under them.

mod common;

use common::*;
use pagebara_redact::{ApplyOptions, Mark, Rect, Report, Sanitize, apply_marks, sanitize, save};
use pagebara_syntax::Document;

struct Out {
    before: Document,
    after: Document,
    bytes: Vec<u8>,
    report: Report,
}

/// A page painting `paint` with pattern `/P1` (cell `cell`: BBox, XStep, YStep, content).
fn pattern_page(cell: (&str, f64, f64, &str), paint: &str) -> Vec<u8> {
    let mut p = Pdf::new();
    let (bbox, xs, ys, content) = cell;
    let pat = p.stream(
        &format!(
            "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [{bbox}] \
             /XStep {xs} /YStep {ys} /Resources << {HELV} >>"
        ),
        content.as_bytes(),
    );
    p.page(
        &format!("/Pattern << /P1 {pat} 0 R >> {HELV}"),
        &format!("/Pattern cs /P1 scn {paint}"),
        "",
    );
    p.finish()
}

fn run(file: Vec<u8>, area: Rect) -> Out {
    let before = Document::open(file).unwrap();
    let mut doc = before.clone();
    let report = apply_marks(&mut doc, &[(0, Mark::rect(area))], &ApplyOptions::default()).unwrap();
    sanitize(&mut doc, &Sanitize::default());
    let bytes = save(&doc).unwrap();
    Out {
        before,
        after: Document::open(bytes.clone()).unwrap(),
        bytes,
        report,
    }
}

/// Whether the file holds `s`, as a literal or hex string.
fn holds(file: &[u8], s: &str) -> bool {
    let all = all_bytes(file);
    contains_ci(&all, s) || contains_ci(&all, &hex(s.as_bytes()))
}

/// Pixels differing outside the region (by more than anti-aliasing), and non-white pixels
/// inside it before.
fn compare(o: &Out, area: &Rect) -> (usize, usize) {
    let a = render(&o.before, 0);
    let b = render(&o.after, 0);
    let bx = device_box(area, 0, 2);
    (diff_outside(&a, &b, &[bx], 2), non_white_inside(&a, bx))
}

/// A pattern used as a picture (cells as large as the painted area): the cell meeting the
/// region is replaced by a copy without the text under it; the rest draws the same.
#[test]
fn large_cells_lose_content_under_regions() {
    let cell = (
        "0 0 260 140",
        260.0,
        140.0,
        "0 0 1 rg 0 0 260 140 re f 0 g BT /F1 14 Tf 40 100 Td (PATSECRET) Tj ET \
         BT /F1 14 Tf 40 40 Td (KEEPME) Tj ET 1 0 0 rg 4 w 160 0 m 160 140 l S",
    );
    let file = pattern_page(cell, "20 20 260 160 re f");
    let area = Rect::new(35.0, 95.0, 135.0, 118.0);
    let o = run(file, area);
    assert_eq!(o.report.pages[0].stats.patterns_rewritten, 1);
    assert!(!holds(&o.bytes, "PATSECRET"), "the pattern keeps the text");
    assert!(holds(&o.bytes, "KEEPME"));
    let (diff, inside) = compare(&o, &area);
    assert!(inside > 0);
    assert_eq!(diff, 0);
    let v = pagebara_redact::verify(&o.bytes, None, &o.report.regions(), &["PATSECRET"]);
    assert!(v.is_clean(), "{v:#?}");
    external_checks(&o.bytes, &["PATSECRET"]);
}

/// A repeating pattern whose whole cell is also shown outside the region stays as it is:
/// what it holds is shown there anyway.
#[test]
fn repeating_cells_stay() {
    let cell = (
        "0 0 20 20",
        20.0,
        20.0,
        "0 0.5 0 rg 2 2 8 8 re f 0 g BT /F1 6 Tf 10 12 Td (ab) Tj ET",
    );
    let file = pattern_page(cell, "10 10 280 180 re f");
    let area = Rect::new(100.0, 80.0, 160.0, 120.0);
    let o = run(file, area);
    assert_eq!(o.report.pages[0].stats.patterns_rewritten, 0);
    let (diff, inside) = compare(&o, &area);
    assert!(inside > 0);
    assert_eq!(diff, 0);
}

/// A pattern painted only inside the region keeps nothing.
#[test]
fn pattern_only_under_region_is_emptied() {
    let cell = (
        "0 0 300 200",
        300.0,
        200.0,
        "BT /F1 12 Tf 55 70 Td (ONLYSECRET) Tj ET 0 0 1 rg 50 50 10 10 re f",
    );
    let file = pattern_page(cell, "50 50 100 40 re f");
    let area = Rect::new(40.0, 40.0, 160.0, 100.0);
    let o = run(file, area);
    assert_eq!(o.report.pages[0].stats.patterns_rewritten, 1);
    assert!(!holds(&o.bytes, "ONLYSECRET"));
    let (diff, inside) = compare(&o, &area);
    assert!(inside > 0);
    assert_eq!(diff, 0);
}

/// Text filled with a large-cell pattern counts as painting with it too.
#[test]
fn text_painted_with_a_pattern() {
    let cell = (
        "0 0 300 200",
        300.0,
        200.0,
        "0 0 1 rg 0 0 300 200 re f 1 1 0 rg 20 95 250 10 re f BT /F1 8 Tf 90 98 Td (HIDDENWORD) Tj ET",
    );
    let file = pattern_page(cell, "BT /F1 40 Tf 15 90 Td (MMMMMMM) Tj ET");
    // The third and fourth `M` (33.32 pt each).
    let area = Rect::new(81.0, 70.0, 149.0, 140.0);
    let o = run(file, area);
    assert_eq!(o.report.pages[0].stats.patterns_rewritten, 1);
    assert!(!holds(&o.bytes, "HIDDENWORD"));
    assert!(!holds(&o.bytes, "HIDD") && !holds(&o.bytes, "WORD"));
    let (diff, inside) = compare(&o, &area);
    assert!(inside > 0);
    assert_eq!(diff, 0);
}

/// [`pattern_page`] with more page resources.
fn pattern_page_with(cell: (&str, f64, f64, &str), paint: &str, res: &str, p: &mut Pdf) {
    let (bbox, xs, ys, content) = cell;
    let pat = p.stream(
        &format!(
            "/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [{bbox}] \
             /XStep {xs} /YStep {ys} /Resources << {HELV} >>"
        ),
        content.as_bytes(),
    );
    p.page(
        &format!("/Pattern << /P1 {pat} 0 R >> {HELV} {res}"),
        &format!("/Pattern cs /P1 scn {paint}"),
        "",
    );
}

/// A cell the painted path's box holds but the path (or the clip) does not show whole: the
/// pattern is replaced, and copies of cells keep only what lies in the painted part.
#[test]
fn cells_in_the_box_but_not_the_shape_are_not_shown_whole() {
    let cell = (
        "0 0 100 100",
        100.0,
        100.0,
        "0 0 1 rg 0 0 100 100 re f 0 g BT /F1 9 Tf 5 60 Td (TILESECRET) Tj ET",
    );
    // A triangle under y = 100 - x / 2: the text shows in the first cell only.
    for paint in [
        "0 0 m 200 0 l 0 100 l h f",
        "0 0 m 200 0 l 0 100 l h W n 0 0 300 200 re f",
    ] {
        let file = pattern_page(cell, paint);
        let area = Rect::new(2.0, 55.0, 75.0, 75.0);
        let o = run(file, area);
        assert_eq!(o.report.pages[0].stats.patterns_rewritten, 1, "{paint}");
        assert!(!holds(&o.bytes, "TILESECRET"), "{paint}");
        let (diff, inside) = compare(&o, &area);
        assert!(inside > 0);
        assert_eq!(diff, 0, "{paint}");
    }
}

/// Text paints with the colours its rendering mode uses: filled text does not paint with
/// a stroke pattern, invisible text with neither.
#[test]
fn text_rendering_mode_picks_the_pattern() {
    let cell = (
        "0 0 300 200",
        300.0,
        200.0,
        "0 0 1 rg 0 0 300 200 re f 0 g BT /F1 8 Tf 90 98 Td (MODESECRET) Tj ET",
    );
    let area = Rect::new(81.0, 70.0, 149.0, 140.0);
    let text = "BT /F1 40 Tf 15 90 Td (MMMMMMM) Tj ET";
    for (setup, rewritten) in [
        ("0 g /Pattern CS /P1 SCN 0 Tr", 0),
        ("0 g /Pattern CS /P1 SCN 3 Tr", 0),
        ("/Pattern cs /P1 scn 3 Tr", 0),
        ("0 g /Pattern CS /P1 SCN 1 Tr", 1),
        ("/Pattern cs /P1 scn 4 Tr", 1),
    ] {
        let mut p = Pdf::new();
        pattern_page_with(cell, &format!("{setup} {text}"), "", &mut p);
        let o = run(p.finish(), area);
        let s = &o.report.pages[0].stats;
        assert_eq!(s.patterns_rewritten, rewritten, "{setup}");
        assert_eq!(holds(&o.bytes, "MODESECRET"), rewritten == 0, "{setup}");
        assert_eq!(compare(&o, &area).0, 0, "{setup}");
    }
}

/// Stencil masks painted with a pattern (as an image XObject or inline) and forms painting
/// with the pattern they inherit are uses of it.
#[test]
fn stencil_masks_and_forms_paint_with_the_pattern() {
    let cell = (
        "0 0 300 200",
        300.0,
        200.0,
        "0 0 1 rg 0 0 300 200 re f 0 g BT /F1 12 Tf 60 70 Td (USESECRET) Tj ET",
    );
    let area = Rect::new(50.0, 60.0, 150.0, 90.0);
    // A mask of one pixel per unit, so pixels under the region are wholly inside it.
    let mask = "q 120 0 0 60 40 45 cm";
    let hex = "0".repeat(1800);
    for kind in ["image", "inline", "form"] {
        let mut p = Pdf::new();
        let (paint, res) = match kind {
            "image" => {
                let im = p.stream(
                    "/Type /XObject /Subtype /Image /Width 120 /Height 60 /ImageMask true \
                     /BitsPerComponent 1",
                    &[0u8; 900],
                );
                (
                    format!("{mask} /Im0 Do Q"),
                    format!("/XObject << /Im0 {im} 0 R >>"),
                )
            }
            "inline" => (
                format!("{mask} BI /W 120 /H 60 /IM true /BPC 1 /F /AHx ID {hex}> EI Q"),
                String::new(),
            ),
            _ => {
                let fm = p.stream(
                    "/Type /XObject /Subtype /Form /BBox [0 0 300 200]",
                    b"30 40 150 70 re f",
                );
                ("/Fm0 Do".into(), format!("/XObject << /Fm0 {fm} 0 R >>"))
            }
        };
        pattern_page_with(cell, &paint, &res, &mut p);
        let o = run(p.finish(), area);
        assert_eq!(o.report.pages[0].stats.patterns_rewritten, 1, "{kind}");
        assert!(!holds(&o.bytes, "USESECRET"), "{kind}");
        let (diff, inside) = compare(&o, &area);
        assert!(inside > 0, "{kind}");
        assert_eq!(diff, 0, "{kind}");
    }
}
