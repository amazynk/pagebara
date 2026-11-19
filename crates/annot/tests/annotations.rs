mod common;

use common::*;
use pagebara_annot::*;
use pagebara_syntax::{Document, ObjRef, Object};

const PAGE: ObjRef = ObjRef::new(3, 0);
const TEXT: &str = "BT /F1 24 Tf 20 360 Td (Hello world) Tj ET";

fn page_doc() -> Document {
    Document::open(one_page(400, 400, TEXT)).unwrap()
}

fn page_annots(doc: &Document) -> Vec<Annotation> {
    let page = doc.get(PAGE).unwrap();
    annotations(doc, page.as_dict().unwrap())
}

fn reopen(doc: &Document, name: &str) -> (Document, Vec<u8>) {
    let bytes = doc.save_incremental().unwrap();
    qpdf_check(&bytes, name);
    (Document::open(bytes.clone()).unwrap(), bytes)
}

fn with<F: FnOnce(&mut Annotation)>(kind: Kind, rect: Rect, f: F) -> Annotation {
    let mut a = Annotation::new(kind, rect);
    f(&mut a);
    a
}

fn every_kind() -> Vec<Annotation> {
    let p = Point::new;
    vec![
        // Over "Hello" (baseline 360, 24 pt).
        with(
            Kind::Markup {
                kind: MarkupKind::Highlight,
                quads: vec![Quad::from_rect(Rect::new(18.0, 354.0, 90.0, 380.0))],
            },
            Rect::default(),
            |a| a.author = Some("Ann".into()),
        ),
        with(
            Kind::Markup {
                kind: MarkupKind::Underline,
                quads: vec![Quad::from_rect(Rect::new(200.0, 330.0, 280.0, 346.0))],
            },
            Rect::default(),
            |a| a.color = Some(Color::BLACK),
        ),
        with(
            Kind::Markup {
                kind: MarkupKind::StrikeOut,
                quads: vec![Quad::from_rect(Rect::new(200.0, 300.0, 280.0, 316.0))],
            },
            Rect::default(),
            |a| a.color = Some(Color::BLACK),
        ),
        with(
            Kind::Markup {
                kind: MarkupKind::Squiggly,
                quads: vec![Quad::from_rect(Rect::new(200.0, 270.0, 280.0, 286.0))],
            },
            Rect::default(),
            |a| a.color = Some(Color::BLACK),
        ),
        with(
            Kind::Ink {
                strokes: vec![
                    (0..=40)
                        .map(|i| p(20.0 + i as f64 * 2.0, 300.0 + (i as f64 / 4.0).sin() * 10.0))
                        .collect(),
                    vec![p(30.0, 280.0)],
                ],
            },
            Rect::default(),
            |a| {
                a.color = Some(Color::RED);
                a.border.width = 4.0;
            },
        ),
        with(
            Kind::Square {
                interior: Some(Color::Rgb(0.0, 0.0, 1.0)),
            },
            Rect::new(20.0, 200.0, 80.0, 250.0),
            |a| a.border.width = 2.0,
        ),
        with(
            Kind::Circle {
                interior: Some(Color::Rgb(0.0, 0.0, 1.0)),
            },
            Rect::new(100.0, 200.0, 160.0, 250.0),
            |_| {},
        ),
        with(
            Kind::Line {
                start: p(20.0, 170.0),
                end: p(160.0, 170.0),
                endings: [LineEnding::Circle, LineEnding::ClosedArrow],
                interior: Some(Color::RED),
            },
            Rect::default(),
            |a| a.border.width = 3.0,
        ),
        with(
            Kind::Polygon {
                vertices: vec![p(200.0, 200.0), p(260.0, 200.0), p(230.0, 250.0)],
                interior: Some(Color::Rgb(0.0, 0.0, 1.0)),
            },
            Rect::default(),
            |_| {},
        ),
        with(
            Kind::PolyLine {
                vertices: vec![p(280.0, 200.0), p(320.0, 250.0), p(360.0, 200.0)],
                endings: [LineEnding::None, LineEnding::OpenArrow],
                interior: None,
            },
            Rect::default(),
            |a| a.border.width = 2.0,
        ),
        with(
            Kind::FreeText {
                font_size: 14.0,
                text_color: Color::BLACK,
                align: Align::Left,
            },
            Rect::new(20.0, 60.0, 180.0, 140.0),
            |a| {
                a.contents = Some("Grüße — a free text note that wraps".into());
                a.border.width = 1.0;
            },
        ),
        with(
            Kind::Text {
                icon: "Comment".into(),
                open: false,
            },
            Rect::new(200.0, 100.0, 224.0, 124.0),
            |a| a.contents = Some("A sticky note".into()),
        ),
        with(
            Kind::Stamp {
                icon: Some("Approved".into()),
            },
            Rect::new(250.0, 40.0, 390.0, 90.0),
            |_| {},
        ),
        with(
            Kind::Link {
                action: Some(LinkAction::Uri("https://example.com/".into())),
                quads: vec![],
            },
            Rect::new(20.0, 10.0, 120.0, 30.0),
            |_| {},
        ),
    ]
}

#[test]
fn create_every_kind_and_read_back() {
    let mut doc = page_doc();
    let wanted = every_kind();
    let refs: Vec<ObjRef> = wanted
        .iter()
        .map(|a| add(&mut doc, PAGE, a).unwrap())
        .collect();
    let (doc, bytes) = reopen(&doc, "every-kind");
    let got = page_annots(&doc);
    assert_eq!(got.len(), wanted.len());
    for ((g, w), r) in got.iter().zip(&wanted).zip(&refs) {
        assert_eq!(g.id, Some(*r));
        assert_eq!(g.subtype(), w.subtype());
        assert_eq!(g.contents, w.contents);
        assert_eq!(g.author, w.author);
        assert_eq!(g.color, w.color, "{}", w.subtype());
        assert!(g.name.is_some() && g.created.is_some() && g.modified.is_some());
        assert!((g.border.width - w.border.width).abs() < 1e-6);
        match (&g.kind, &w.kind) {
            (Kind::Ink { strokes: a }, Kind::Ink { strokes: b }) => {
                assert_eq!(a.len(), b.len());
                assert!((a[0][3].y - b[0][3].y).abs() < 1e-3);
            }
            (a, b) => assert_eq!(a, b),
        }
        let d = doc.get(*r).unwrap();
        let ap = doc.get_in(d.as_dict().unwrap(), b"AP");
        assert!(ap.as_dict().unwrap().get(b"N").is_some(), "{}", g.subtype());
    }
    let ink = got.iter().find(|a| a.subtype() == "Ink").unwrap();
    assert!(ink.rect.x0 < 20.0 && ink.rect.x1 > 100.0);

    let Some(img) = render(&bytes, "every-kind") else {
        return;
    };
    // Highlight: yellow on paper, text stays dark (multiply).
    assert!(is_yellow(img.at(19.0, 377.0)), "{:?}", img.at(19.0, 377.0));
    assert!(img.any(20.0, 356.0, 90.0, 378.0, is_dark));
    assert!(is_white(img.at(19.0, 390.0)));
    // Underline near the bottom, strike-out through the middle, squiggle along the bottom.
    assert!(img.any(240.0, 330.0, 241.0, 334.0, is_dark));
    assert!(img.any(240.0, 305.0, 241.0, 311.0, is_dark));
    assert!(img.any(200.0, 266.0, 280.0, 274.0, is_dark));
    assert!(is_white(img.at(240.0, 340.0)));
    // Ink.
    assert!(is_red(img.at(20.0, 300.0)) || is_red(img.at(21.0, 300.0)));
    assert!(img.any(28.0, 278.0, 32.0, 282.0, is_red));
    // Shapes: blue fill, red border.
    assert!(is_blue(img.at(50.0, 225.0)));
    assert!(is_red(img.at(20.5, 225.0)));
    assert!(is_blue(img.at(130.0, 225.0)));
    assert!(is_white(img.at(101.0, 201.0)));
    assert!(is_blue(img.at(230.0, 215.0)));
    // Line and its arrow.
    assert!(is_red(img.at(90.0, 170.0)));
    assert!(img.any(150.0, 174.0, 160.0, 180.0, |p| !is_white(p)));
    assert!(img.any(300.0, 220.0, 302.0, 230.0, is_red));
    // Free text inside its box, sticky note icon, stamp.
    assert!(img.any(24.0, 115.0, 120.0, 138.0, is_dark));
    assert!(img.any(200.0, 100.0, 224.0, 124.0, is_yellow));
    assert!(img.any(260.0, 50.0, 380.0, 80.0, is_red));
}

#[test]
fn update_keeps_unknown_keys_and_redraws() {
    let mut doc = page_doc();
    let mut a = Annotation::new(
        Kind::Square { interior: None },
        Rect::new(50.0, 50.0, 150.0, 150.0),
    );
    a.border.width = 6.0;
    let r = add(&mut doc, PAGE, &a).unwrap();
    let mut d = doc.get(r).unwrap().as_dict().unwrap().clone();
    d.set(b"PagebaraTest", 7);
    doc.set(r, d);

    let mut b = pagebara_annot::read(&doc, r).unwrap();
    b.color = Some(Color::Rgb(0.0, 0.0, 1.0));
    b.rect = Rect::new(200.0, 200.0, 300.0, 300.0);
    b.contents = Some("moved".into());
    update(&mut doc, r, &b).unwrap();

    let (doc, bytes) = reopen(&doc, "update");
    let got = pagebara_annot::read(&doc, r).unwrap();
    assert_eq!(got.color, b.color);
    assert_eq!(got.rect, b.rect);
    assert_eq!(got.created, b.created);
    let d = doc.get(r).unwrap();
    assert_eq!(d.as_dict().unwrap().int(b"PagebaraTest"), Some(7));
    if let Some(img) = render(&bytes, "update") {
        assert!(is_blue(img.at(202.0, 250.0)));
        assert!(is_white(img.at(52.0, 100.0)));
    }
}

#[test]
fn replies_and_delete() {
    let mut doc = page_doc();
    let mut note = Annotation::new(
        Kind::Text {
            icon: "Note".into(),
            open: false,
        },
        Rect::new(10.0, 10.0, 30.0, 30.0),
    );
    note.contents = Some("Question?".into());
    let n = add(&mut doc, PAGE, &note).unwrap();
    let popup = add_popup(&mut doc, PAGE, n, Rect::new(40.0, 10.0, 200.0, 80.0), false).unwrap();
    let r1 = reply(&mut doc, PAGE, n, "Bob", "Answer").unwrap();
    let r2 = reply(&mut doc, PAGE, r1, "Ann", "Thanks").unwrap();
    let other = add(&mut doc, PAGE, &note).unwrap();

    let (mut doc, _) = reopen(&doc, "replies");
    let all = page_annots(&doc);
    assert_eq!(all.len(), 5);
    let thread = replies(&all, n);
    assert_eq!(thread.len(), 1);
    assert_eq!(thread[0].author.as_deref(), Some("Bob"));
    assert_eq!(thread[0].contents.as_deref(), Some("Answer"));
    assert_eq!(replies(&all, r1)[0].id, Some(r2));
    assert_eq!(pagebara_annot::read(&doc, n).unwrap().popup, Some(popup));

    let mut gone = remove(&mut doc, PAGE, n).unwrap();
    gone.sort();
    let mut want = vec![n, popup, r1, r2];
    want.sort();
    assert_eq!(gone, want);
    let (doc, _) = reopen(&doc, "replies-deleted");
    let all = page_annots(&doc);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, Some(other));
    assert!(doc.get(n).is_none());
}

/// A red disc on a transparent background with a soft edge.
fn disc(size: u32) -> Vec<u8> {
    let c = size as f64 / 2.0;
    let mut px = Vec::new();
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f64 + 0.5 - c).hypot(y as f64 + 0.5 - c) - c * 0.6).clamp(0.0, 1.0);
            px.extend_from_slice(&[220, 20, 20, ((1.0 - d) * 255.0) as u8]);
        }
    }
    px
}

#[test]
fn image_stamp_and_flatten() {
    let mut doc = page_doc();
    let px = disc(64);
    let img = RgbaImage {
        width: 64,
        height: 64,
        pixels: &px,
    };
    let opts = StampOptions {
        author: Some("Ann".into()),
        subject: Some("Signature".into()),
        ..Default::default()
    };
    let r = add_image_stamp(
        &mut doc,
        PAGE,
        img,
        Rect::new(100.0, 100.0, 200.0, 200.0),
        &opts,
    )
    .unwrap()
    .unwrap();
    let flat = StampOptions {
        placement: Placement::Flatten,
        ..Default::default()
    };
    assert!(
        add_image_stamp(
            &mut doc,
            PAGE,
            img,
            Rect::new(250.0, 100.0, 350.0, 200.0),
            &flat
        )
        .unwrap()
        .is_none()
    );
    let (mut doc, bytes) = reopen(&doc, "image-stamp");
    let all = page_annots(&doc);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].subtype(), "Stamp");
    assert_eq!(all[0].subject.as_deref(), Some("Signature"));
    if let Some(img) = render(&bytes, "image-stamp") {
        assert!(is_red(img.at(150.0, 150.0)));
        assert!(is_white(img.at(103.0, 103.0)));
        assert!(is_red(img.at(300.0, 150.0)));
        assert!(is_white(img.at(253.0, 103.0)));
    }

    // Resizing keeps the image appearance, scaled.
    let mut a = all[0].clone();
    a.rect = Rect::new(100.0, 100.0, 140.0, 140.0);
    update(&mut doc, r, &a).unwrap();
    let (mut doc, bytes) = reopen(&doc, "image-stamp-resized");
    if let Some(img) = render(&bytes, "image-stamp-resized") {
        assert!(is_red(img.at(120.0, 120.0)));
        assert!(is_white(img.at(150.0, 150.0)));
    }

    // Flattening the annotation keeps its look and removes it.
    flatten(&mut doc, PAGE, r).unwrap();
    let (doc, bytes) = reopen(&doc, "image-stamp-flattened");
    assert!(page_annots(&doc).is_empty());
    if let Some(img) = render(&bytes, "image-stamp-flattened") {
        assert!(is_red(img.at(120.0, 120.0)));
        assert!(is_red(img.at(300.0, 150.0)));
        assert!(
            img.any(20.0, 356.0, 90.0, 378.0, is_dark),
            "original content kept"
        );
    }
}

#[test]
fn flattened_shapes_and_unbalanced_content() {
    // Content that leaves a transformed, unbalanced graphics state behind.
    let mut doc = Document::open(one_page(300, 300, "q 0.5 0 0 0.5 0 0 cm 0 0 1 rg")).unwrap();
    let a = Annotation::new(
        Kind::Square {
            interior: Some(Color::Rgb(1.0, 0.0, 0.0)),
        },
        Rect::new(100.0, 100.0, 200.0, 200.0),
    );
    add_flattened(&mut doc, PAGE, &a).unwrap();
    add_flattened(&mut doc, PAGE, &a).unwrap();
    let (doc, bytes) = reopen(&doc, "flattened-shapes");
    assert!(page_annots(&doc).is_empty());
    let page = doc.get(PAGE).unwrap();
    let contents = doc.get_in(page.as_dict().unwrap(), b"Contents");
    assert_eq!(contents.as_array().map(<[Object]>::len), Some(4));
    if let Some(img) = render(&bytes, "flattened-shapes") {
        assert!(is_red(img.at(150.0, 150.0)), "{:?}", img.at(150.0, 150.0));
        assert!(is_white(img.at(60.0, 60.0)));
    }
}

#[test]
fn reads_foreign_annotations() {
    let mut doc = page_doc();
    let mut d = pagebara_syntax::Dict::new();
    d.set(b"Type", Object::name(b"Annot"));
    d.set(b"Subtype", Object::name(b"FreeText"));
    d.set(b"Rect", Rect::new(0.0, 0.0, 10.0, 10.0).to_object());
    d.set(b"DA", Object::string(b"/Helv 9 Tf 0 0 1 rg"));
    d.set(b"Q", 2);
    d.set(b"Border", Object::Array(vec![0.into(), 0.into(), 3.into()]));
    d.set(b"Contents", Object::string(b"\xFE\xFF\x00h\x00i"));
    let mut w = pagebara_syntax::Dict::new();
    w.set(b"Type", Object::name(b"Annot"));
    w.set(b"Subtype", Object::name(b"Widget"));
    w.set(b"FT", Object::name(b"Tx"));
    w.set(b"T", Object::string(b"name"));
    let fr = doc.add(d);
    let wr = doc.add(w);
    let mut page = doc.get(PAGE).unwrap().as_dict().unwrap().clone();
    page.set(
        b"Annots",
        Object::Array(vec![fr.into(), wr.into(), Object::Int(5)]),
    );
    doc.set(PAGE, page);

    let all = page_annots(&doc);
    assert_eq!(all.len(), 2);
    assert_eq!(
        all[0].kind,
        Kind::FreeText {
            font_size: 9.0,
            text_color: Color::Rgb(0.0, 0.0, 1.0),
            align: Align::Right,
        }
    );
    assert_eq!(all[0].border.width, 3.0);
    assert_eq!(all[0].contents.as_deref(), Some("hi"));
    assert_eq!(
        all[1].kind,
        Kind::Widget {
            field_type: Some("Tx".into()),
            field_name: Some("name".into()),
        }
    );
    assert!(matches!(
        update(&mut doc, wr, &all[1]),
        Err(Error::ReadOnly)
    ));
}

#[test]
fn page_box_is_inherited_and_cropped() {
    let mut doc = page_doc();
    let mut pages = doc
        .get(ObjRef::new(2, 0))
        .unwrap()
        .as_dict()
        .unwrap()
        .clone();
    pages.set(b"CropBox", Rect::new(10.0, 10.0, 200.0, 900.0).to_object());
    doc.set(ObjRef::new(2, 0), pages);
    let page = doc.get(PAGE).unwrap();
    assert_eq!(
        page_box(&doc, page.as_dict().unwrap()),
        Rect::new(10.0, 10.0, 200.0, 400.0)
    );
}
