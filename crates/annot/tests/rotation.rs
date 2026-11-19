//! Appearances on pages with `/Rotate`: text and stamps upright as the page is shown,
//! geometry in page space.

mod common;

use common::*;
use pagebara_annot::*;
use pagebara_syntax::{Document, ObjRef};

const PAGE: ObjRef = ObjRef::new(3, 0);
const W: f64 = 300.0;
const H: f64 = 400.0;

/// A page rectangle centred on `(cx, cy)` that the viewer sees as `vw` x `vh`.
fn shown_as(cx: f64, cy: f64, vw: f64, vh: f64, rot: u16) -> Rect {
    let (w, h) = if rot % 180 == 90 { (vh, vw) } else { (vw, vh) };
    Rect::from_size(cx - w / 2.0, cy - h / 2.0, w, h)
}

fn edges(r: Rect) -> (f64, f64, f64, f64) {
    (r.x0, r.y0, r.x1, r.y1)
}

/// Red over blue, 40 x 40.
fn two_tone() -> Vec<u8> {
    (0..40 * 40)
        .flat_map(|i| {
            if i / 40 < 20 {
                [220, 20, 20, 255]
            } else {
                [20, 20, 220, 255]
            }
        })
        .collect()
}

#[test]
fn upright_on_rotated_pages() {
    for rot in [0u16, 90, 180, 270] {
        let mut doc = Document::open(page_with(300, 400, "", &format!("/Rotate {rot}"))).unwrap();
        assert_eq!(
            page_rotation(&doc, doc.get(PAGE).unwrap().as_dict().unwrap()),
            rot
        );

        let text_rect = Rect::new(30.0, 240.0, 150.0, 360.0);
        let mut ft = Annotation::new(
            Kind::FreeText {
                font_size: 24.0,
                text_color: Color::BLACK,
                align: Align::Left,
            },
            text_rect,
        );
        ft.contents = Some("MM".into());
        let ft = add(&mut doc, PAGE, &ft).unwrap();

        let stamp_rect = shown_as(220.0, 120.0, 150.0, 50.0, rot);
        let stamp = Annotation::new(
            Kind::Stamp {
                icon: Some("Approved".into()),
            },
            stamp_rect,
        );
        add(&mut doc, PAGE, &stamp).unwrap();

        let px = two_tone();
        let img_rect = shown_as(90.0, 120.0, 100.0, 60.0, rot);
        let img = RgbaImage {
            width: 40,
            height: 40,
            pixels: &px,
        };
        add_image_stamp(&mut doc, PAGE, img, img_rect, &StampOptions::default()).unwrap();

        let mut ink = Annotation::new(
            Kind::Ink {
                strokes: vec![vec![Point::new(200.0, 250.0), Point::new(280.0, 330.0)]],
            },
            Rect::default(),
        );
        ink.border.width = 4.0;
        add(&mut doc, PAGE, &ink).unwrap();

        let note = Annotation::new(
            Kind::Text {
                icon: "Note".into(),
                open: false,
            },
            Rect::new(160.0, 180.0, 184.0, 204.0),
        );
        add(&mut doc, PAGE, &note).unwrap();

        let name = format!("rotate-{rot}");
        let bytes = doc.save_incremental().unwrap();
        qpdf_check(&bytes, &name);
        let doc = Document::open(bytes.clone()).unwrap();
        let got = pagebara_annot::read(&doc, ft).unwrap();
        assert_eq!(got.rotate, (rot != 0).then_some(rot));
        assert_eq!(got.rect, text_rect);

        for (renderer, img) in render_all(&bytes, &name) {
            let ctx = format!("{renderer}, /Rotate {rot}");
            // Free text: one short line at the top left of the box as the viewer sees it.
            let (x0, y0, x1, y1) = image_box(edges(text_rect), W, H, rot);
            let (mx, my) = ((x0 + x1) / 2, (y0 + y1) / 2);
            let dark = |b| img.count_px(b, is_dark).0;
            assert!(dark((x0, y0, mx, my)) > 50, "{ctx}: text at top left");
            assert_eq!(dark((mx, y0, x1, my)), 0, "{ctx}: top right clear");
            assert_eq!(dark((x0, my, x1, y1)), 0, "{ctx}: bottom clear");

            // Stamp: its label reads horizontally.
            let (x0, y0, x1, y1) = image_box(edges(stamp_rect.inset(8.0)), W, H, rot);
            let (n, b) = img.count_px((x0, y0, x1, y1), is_red);
            let (bx0, by0, bx1, by1) = b.unwrap_or_else(|| panic!("{ctx}: no stamp label"));
            assert!(n > 100 && bx1 - bx0 > 3 * (by1 - by0), "{ctx}: label {b:?}");

            // Image: red on top, blue below.
            let (x0, y0, x1, y1) = image_box(edges(img_rect), W, H, rot);
            let q = (y1 - y0) / 4;
            let top = (x0 + 2, y0 + 2, x1 - 2, y0 + q);
            let bottom = (x0 + 2, y1 - q, x1 - 2, y1 - 2);
            assert!(img.count_px(top, is_red).0 > 50, "{ctx}: image top red");
            assert_eq!(img.count_px(top, is_blue).0, 0, "{ctx}: image top");
            assert!(img.count_px(bottom, is_blue).0 > 50, "{ctx}: image bottom");
            assert_eq!(img.count_px(bottom, is_red).0, 0, "{ctx}: image bottom");

            // Ink stays where it was drawn on the page.
            let (x, y) = page_to_image(240.0, 290.0, W, H, rot);
            let (x, y) = (x as usize, y as usize);
            assert!(
                img.count_px((x - 3, y - 3, x + 3, y + 3), is_blue).0 > 0,
                "{ctx}: ink"
            );
            // Sticky note icon: NoRotate keeps the rectangle's upper-left corner (x0, y1)
            // in place and draws it unturned from there. mutool 1.27 pins text icons to the
            // device origin on rotated pages whatever the flags say.
            if renderer != "mutool" || rot == 0 {
                let (ux, uy) = page_to_image(160.0, 204.0, W, H, rot);
                let b = (ux as usize, uy as usize, ux as usize + 24, uy as usize + 24);
                assert!(img.count_px(b, is_yellow).0 > 50, "{ctx}: note icon");
            }
        }
    }
}

#[test]
fn flattened_free_text_stays_upright() {
    let mut doc = Document::open(page_with(300, 400, "", "/Rotate 90")).unwrap();
    let rect = Rect::new(30.0, 240.0, 150.0, 360.0);
    let mut ft = Annotation::new(
        Kind::FreeText {
            font_size: 24.0,
            text_color: Color::BLACK,
            align: Align::Left,
        },
        rect,
    );
    ft.contents = Some("MM".into());
    let r = add(&mut doc, PAGE, &ft).unwrap();
    flatten(&mut doc, PAGE, r).unwrap();
    let bytes = doc.save_incremental().unwrap();
    qpdf_check(&bytes, "rotate-flattened");
    for (renderer, img) in render_all(&bytes, "rotate-flattened") {
        let (x0, y0, x1, y1) = image_box(edges(rect), W, H, 90);
        let (mx, my) = ((x0 + x1) / 2, (y0 + y1) / 2);
        assert!(img.count_px((x0, y0, mx, my), is_dark).0 > 50, "{renderer}");
        assert_eq!(img.count_px((x0, my, x1, y1), is_dark).0, 0, "{renderer}");
    }
}
