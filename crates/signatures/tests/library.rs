mod common;
#[path = "../../annot/tests/common/mod.rs"]
mod pdf;

use std::path::PathBuf;

use common::*;
use pagebara_signatures::*;
use pagebara_syntax::{Document, ObjRef};

fn fresh_dir(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("signature-libraries")
        .join(name);
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn png_file(img: &Rgba) -> Vec<u8> {
    pagebara_png::encode(
        img.width,
        img.height,
        pagebara_png::ColorType::Rgb,
        &rgb(img),
    )
    .unwrap()
}

/// A JPEG whose Exif says "rotate 90° clockwise to display".
fn rotated_jpeg(img: &Rgba) -> Vec<u8> {
    let mut exif = b"MM\0\x2a\0\0\0\x08\0\x01".to_vec();
    exif.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0, 0, 0, 0, 0]);
    let opts = pagebara_jpeg::encode::EncodeOptions {
        quality: 92,
        exif: Some(&exif),
        ..Default::default()
    };
    let rgb = rgb(img);
    pagebara_jpeg::encode::encode(
        img.width,
        img.height,
        pagebara_jpeg::encode::Samples::Rgb(&rgb),
        &opts,
    )
    .unwrap()
}

fn drawing() -> Drawing {
    Drawing {
        strokes: vec![
            (0..40)
                .map(|i| StrokePoint::new(i as f32 * 3.0, (i as f32 / 4.0).cos() * 15.0))
                .collect(),
        ],
        pen_width: 2.5,
    }
}

#[test]
fn library_round_trip() {
    let dir = fresh_dir("round-trip");
    let s = scan(900, 600);
    let mut lib = Library::open(&dir).unwrap();
    assert!(lib.signatures().is_empty());
    let a = lib.import(&png_file(&s.img), "Scan PNG").unwrap();
    let b = lib.import(&rotated_jpeg(&s.img), "Photo JPEG").unwrap();
    let c = lib.add_drawn(&drawing(), "Drawn", [0, 0, 0x80]).unwrap();
    assert!(
        a.width > 500 && a.width > a.height * 2,
        "{}x{}",
        a.width,
        a.height
    );
    // Upright after the Exif rotation: taller than wide.
    assert!(b.height > b.width * 2, "{}x{}", b.width, b.height);
    assert!(matches!(a.source, Source::Imported { ref file } if file == "original.png"));
    assert_eq!(c.source, Source::Drawn);

    lib.rename(&a.id, "Mine").unwrap();
    lib.set_default_width(&a.id, 42.5).unwrap();
    lib.move_to(&c.id, 0).unwrap();
    let settings = Settings {
        threshold: 70,
        ink: Ink::BLACK,
        ..Settings::default()
    };
    lib.set_settings(&a.id, settings).unwrap();
    assert!(lib.set_default_width(&a.id, -1.0).is_err());
    let before = lib.signatures().to_vec();
    let img = lib.image(&a.id).unwrap();
    assert!(
        img.pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] > 0)
            .all(|p| p[..3] == [0x10; 3])
    );

    let lib = Library::open(&dir).unwrap();
    assert_eq!(lib.signatures(), &before[..]);
    let ids: Vec<&str> = lib.signatures().iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, [&c.id, &a.id, &b.id]);
    let a2 = lib.get(&a.id).unwrap();
    assert_eq!(
        (a2.name.as_str(), a2.default_width_mm, a2.settings),
        ("Mine", 42.5, settings)
    );
    assert_eq!(lib.image(&a.id).unwrap(), img);
    assert_eq!(lib.drawing(&c.id).unwrap(), drawing());
    // The original is kept for re-processing.
    assert_eq!(lib.original(&a.id).unwrap().width, 900);

    let mut lib = lib;
    lib.delete(&b.id).unwrap();
    assert!(!dir.join(&b.id).exists());
    assert_eq!(Library::open(&dir).unwrap().signatures().len(), 2);
}

#[test]
fn corrupted_index_is_rebuilt() {
    let dir = fresh_dir("corrupted");
    let s = scan(600, 400);
    let mut lib = Library::open(&dir).unwrap();
    let a = lib.import(&png_file(&s.img), "First").unwrap();
    let b = lib.add_drawn(&drawing(), "Second", [0; 3]).unwrap();
    let c = lib.import(&png_file(&s.img), "Third").unwrap();
    lib.set_default_width(&b.id, 30.0).unwrap();
    drop(lib);

    std::fs::write(dir.join("index.json"), b"{ not json").unwrap();
    let lib = Library::open(&dir).unwrap();
    assert_eq!(lib.signatures().len(), 3);
    assert_eq!(lib.get(&b.id).unwrap().default_width_mm, 30.0);
    assert_eq!(lib.get(&a.id).unwrap().name, "First");
    // And the index is good again.
    let text = std::fs::read_to_string(dir.join("index.json")).unwrap();
    assert!(text.contains("\"version\": 1"));

    // Without index or meta: recovered with defaults. A folder without its image is skipped.
    std::fs::remove_file(dir.join("index.json")).unwrap();
    std::fs::remove_file(dir.join(&c.id).join("meta.json")).unwrap();
    std::fs::remove_file(dir.join(&a.id).join("signature.png")).unwrap();
    std::fs::create_dir_all(dir.join("not-a-signature")).unwrap();
    let lib = Library::open(&dir).unwrap();
    assert_eq!(lib.signatures().len(), 2);
    let c2 = lib.get(&c.id).unwrap();
    assert_eq!(
        c2.source,
        Source::Imported {
            file: "original.png".into()
        }
    );
    assert_eq!((c2.width, c2.height), (c.width, c.height));
    assert!(lib.image(&c.id).is_ok());
    assert!(lib.get(&a.id).is_none());
}

#[test]
fn placed_signature_renders_everywhere() {
    let dir = fresh_dir("placement");
    let s = scan(900, 600);
    let mut lib = Library::open(&dir).unwrap();
    let sig = lib.import(&png_file(&s.img), "Me").unwrap();
    let page = ObjRef::new(3, 0);
    let mut doc = Document::open(pdf::one_page(400, 400, "0 0 0 rg 0 0 10 10 re f")).unwrap();
    let r = lib
        .place(
            &mut doc,
            page,
            &sig.id,
            Target::At {
                center: pagebara_annot::Point::new(200.0, 300.0),
                width: None,
            },
            Placement::Annotation,
        )
        .unwrap()
        .unwrap();
    lib.place(
        &mut doc,
        page,
        &sig.id,
        Target::Rect(pagebara_annot::Rect::new(50.0, 50.0, 350.0, 150.0)),
        Placement::Flatten,
    )
    .unwrap();
    let bytes = doc.save_incremental().unwrap();
    pdf::qpdf_check(&bytes, "signature-placed");
    let doc = Document::open(bytes.clone()).unwrap();
    let a = pagebara_annot::read(&doc, r).unwrap();
    assert_eq!(a.subtype(), "Stamp");
    let w = DEFAULT_WIDTH_MM * POINTS_PER_MM;
    assert!((a.rect.width() - w).abs() < 0.01);
    assert!((a.rect.height() - sig.height_for(w)).abs() < 0.01);
    assert_eq!(
        pagebara_annot::annotations(&doc, doc.get(page).unwrap().as_dict().unwrap()).len(),
        1
    );

    let Some(img) = pdf::render(&bytes, "signature-placed") else {
        return;
    };
    let inky = |p: [u8; 3]| u16::from(p[2]) > u16::from(p[0]) + 20 && p[0] < 120;
    let r = a.rect;
    assert!(
        img.any(r.x0, r.y0, r.x1, r.y1, inky),
        "annotation ink visible"
    );
    // The paper was removed: page white shows at the stamp's corners.
    assert!(pdf::is_white(img.at(r.x0 + 1.0, r.y0 + 1.0)));
    assert!(pdf::is_white(img.at(r.x1 - 1.0, r.y1 - 1.0)));
    assert!(
        img.any(50.0, 50.0, 350.0, 150.0, inky),
        "flattened ink visible"
    );
    assert!(pdf::is_white(img.at(51.0, 149.0)));
}

/// Image XObjects in `doc`: those carrying a signature key, and all of them.
fn images(doc: &Document) -> (usize, usize) {
    let mut keyed = 0;
    let mut all = 0;
    for n in doc.object_numbers() {
        let Some(o) = doc.get(ObjRef::new(n, 0)) else {
            continue;
        };
        let Some(s) = o.as_stream() else { continue };
        if s.dict.name(b"Subtype") == Some(b"Image") {
            all += 1;
            keyed += usize::from(s.dict.contains(b"PagebaraSignature"));
        }
    }
    (keyed, all)
}

#[test]
fn placements_share_one_image() {
    let dir = fresh_dir("shared-image");
    let s = scan(600, 400);
    let mut lib = Library::open(&dir).unwrap();
    let sig = lib.import(&png_file(&s.img), "Me").unwrap();
    let page = ObjRef::new(3, 0);
    let mut doc = Document::open(pdf::one_page(400, 400, "")).unwrap();
    let at = |x, y| Target::At {
        center: pagebara_annot::Point::new(x, y),
        width: Some(100.0),
    };
    lib.place(
        &mut doc,
        page,
        &sig.id,
        at(100.0, 300.0),
        Placement::Annotation,
    )
    .unwrap();
    lib.place(
        &mut doc,
        page,
        &sig.id,
        at(300.0, 300.0),
        Placement::Annotation,
    )
    .unwrap();
    lib.place(
        &mut doc,
        page,
        &sig.id,
        at(200.0, 100.0),
        Placement::Flatten,
    )
    .unwrap();
    // Opaque image plus its soft mask, once.
    assert_eq!(images(&doc), (1, 2));

    // Still shared after saving and reopening.
    let bytes = doc.save_incremental().unwrap();
    pdf::qpdf_check(&bytes, "shared-image");
    let mut doc = Document::open(bytes).unwrap();
    lib.place(
        &mut doc,
        page,
        &sig.id,
        at(200.0, 200.0),
        Placement::Annotation,
    )
    .unwrap();
    assert_eq!(images(&doc), (1, 2));

    // Changed settings give a different image, embedded next to the first.
    lib.set_settings(
        &sig.id,
        Settings {
            ink: Ink::BLACK,
            ..Settings::default()
        },
    )
    .unwrap();
    lib.place(
        &mut doc,
        page,
        &sig.id,
        at(200.0, 50.0),
        Placement::Annotation,
    )
    .unwrap();
    assert_eq!(images(&doc), (2, 4));
    let bytes = doc.save_full().unwrap();
    pdf::qpdf_check(&bytes, "shared-image-full");
    let doc = Document::open(bytes.clone()).unwrap();
    assert_eq!(images(&doc), (2, 4));
    assert_eq!(
        pagebara_annot::annotations(&doc, doc.get(page).unwrap().as_dict().unwrap()).len(),
        4
    );
    for (renderer, img) in pdf::render_all(&bytes, "shared-image") {
        let inky = |p: [u8; 3]| p.iter().any(|&c| c < 160);
        for (x, y) in [
            (100.0, 300.0),
            (300.0, 300.0),
            (200.0, 100.0),
            (200.0, 200.0),
        ] {
            assert!(
                img.any(x - 50.0, y - 15.0, x + 50.0, y + 15.0, inky),
                "{renderer}: signature at {x},{y}"
            );
        }
    }
}

#[test]
fn placed_upright_on_rotated_pages() {
    let dir = fresh_dir("rotated");
    let mut lib = Library::open(&dir).unwrap();
    let sig = lib.add_drawn(&drawing(), "Drawn", [0, 0, 0x80]).unwrap();
    let page = ObjRef::new(3, 0);
    for rot in [0u16, 90, 180, 270] {
        let mut doc =
            Document::open(pdf::page_with(300, 400, "", &format!("/Rotate {rot}"))).unwrap();
        let r = lib
            .place(
                &mut doc,
                page,
                &sig.id,
                Target::At {
                    center: pagebara_annot::Point::new(150.0, 200.0),
                    width: Some(200.0),
                },
                Placement::Annotation,
            )
            .unwrap()
            .unwrap();
        let a = pagebara_annot::read(&doc, r).unwrap();
        let shown = if rot % 180 == 90 {
            (a.rect.height(), a.rect.width())
        } else {
            (a.rect.width(), a.rect.height())
        };
        assert!((shown.0 - 200.0).abs() < 1e-3, "/Rotate {rot}: {shown:?}");
        assert!((shown.1 - sig.height_for(200.0)).abs() < 1e-3);

        let name = format!("signature-rotate-{rot}");
        let bytes = doc.save_incremental().unwrap();
        pdf::qpdf_check(&bytes, &name);
        for (renderer, img) in pdf::render_all(&bytes, &name) {
            // The drawn stroke is a wave along the signature's width: as shown, its ink
            // spans far more horizontally than vertically.
            let inky = |p: [u8; 3]| u16::from(p[2]) > u16::from(p[0]) + 40;
            let (n, b) = img.count_px((0, 0, 400, 400), inky);
            let (x0, y0, x1, y1) = b.unwrap_or_else(|| panic!("{renderer} /Rotate {rot}: no ink"));
            assert!(
                n > 100 && x1 - x0 > 2 * (y1 - y0),
                "{renderer} /Rotate {rot}: {x0},{y0} {x1},{y1}"
            );
        }
    }
}
