//! Each operation: applied, saved and read back, it draws the expected change, leaves
//! everything else pixel for pixel, passes qpdf and mutool and extracts as expected.

mod common;

use common::*;
use pagebara_content::{Matrix, Point, Rect};
use pagebara_edit::{
    Color, Editor, FormEdit, ImageFit, Kind, NewImage, NewText, ObjectId, PageObjects,
    SimpleEncoder, TextStyle, ZOrder, hit,
};
use pagebara_syntax::Document;

const H: f32 = 200.0;

fn find(objs: &PageObjects, f: impl Fn(&pagebara_edit::PageObject) -> bool) -> ObjectId {
    objs.objects.iter().find(|o| f(o)).expect("object").id
}

fn text_block(objs: &PageObjects, s: &str) -> ObjectId {
    find(objs, |o| o.text().is_some_and(|t| t.text.contains(s)))
}

fn run(objs: &PageObjects, s: &str) -> pagebara_edit::RunId {
    objs.runs.iter().find(|r| r.text == s).expect("run").id
}

/// Saves, checks the file with external tools and returns it read back.
fn finish(doc: &Document, texts: &[&str]) -> Document {
    let (d, file) = reopen(doc);
    external_checks(&file, texts);
    d
}

#[test]
fn delete_text_keeps_the_rest() {
    let doc = open(docs::basic());
    let before = render(&doc, 0);
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let id = text_block(&objs, "Hello");
    let bbox = objs.get(id).unwrap().bbox;
    let mut d = doc.clone();
    pagebara_edit::delete(&mut d, 0, &[id.into()]).unwrap();
    let d = finish(&d, &["Second line"]);
    let after = render(&d, 0);
    let b = device_box(&bbox, H, 2);
    assert_eq!(diff_outside(&before, &after, &[b], 0), 0);
    assert_eq!(ink_inside(&after, b), 0);
    let t = page_text(&d, 0);
    assert!(!t.contains("Hello") && t.contains("Third line here"), "{t}");
}

#[test]
fn delete_run_keeps_following_text_in_place() {
    let doc = open(docs::basic());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let second = run(&objs, "Second line");
    let third = objs
        .runs
        .iter()
        .find(|r| r.text == "Third line here")
        .unwrap();
    let pos = third.glyphs[0].origin;
    let mut d = doc.clone();
    pagebara_edit::delete(&mut d, 0, &[second.into()]).unwrap();
    let d = finish(&d, &["Third line here"]);
    let objs = pagebara_edit::parse(&d, 0).unwrap();
    let third = objs
        .runs
        .iter()
        .find(|r| r.text == "Third line here")
        .unwrap();
    let p = third.glyphs[0].origin;
    assert!(
        (p.x - pos.x).abs() < 1e-3 && (p.y - pos.y).abs() < 1e-3,
        "{p:?} {pos:?}"
    );
    assert!(!page_text(&d, 0).contains("Second"));
}

#[test]
fn delete_image_and_path() {
    let doc = open(docs::basic());
    let before = render(&doc, 0);
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let img = find(&objs, |o| o.kind == Kind::Image);
    let path = find(&objs, |o| o.kind == Kind::Path);
    let boxes: Vec<_> = [img, path]
        .iter()
        .map(|&i| device_box(&objs.get(i).unwrap().bbox, H, 2))
        .collect();
    let mut d = doc.clone();
    pagebara_edit::delete(&mut d, 0, &[img.into(), path.into()]).unwrap();
    let d = finish(&d, &["Hello World"]);
    let after = render(&d, 0);
    assert_eq!(diff_outside(&before, &after, &boxes, 0), 0);
    for b in boxes {
        assert_eq!(ink_inside(&after, b), 0);
    }
    // The image left the resources.
    let objs = pagebara_edit::parse(&d, 0).unwrap();
    assert!(objs.objects.iter().all(|o| o.kind != Kind::Image));
    let pd = d.get(objs.page_ref).unwrap();
    let res = d.get_in(pd.as_dict().unwrap(), b"Resources");
    let xo = d.get_in(res.as_dict().unwrap(), b"XObject");
    assert!(xo.as_dict().is_none_or(|x| x.is_empty()));
}

#[test]
fn move_image_and_text() {
    let doc = open(docs::basic());
    let before = render(&doc, 0);
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let img = find(&objs, |o| o.kind == Kind::Image);
    let hello = text_block(&objs, "Hello");
    let second = run(&objs, "Second line");
    let third_pos = objs
        .runs
        .iter()
        .find(|r| r.text == "Third line here")
        .unwrap()
        .glyphs[0]
        .origin;
    let ib = objs.get(img).unwrap().bbox;
    let hb = objs.get(hello).unwrap().bbox;
    let sb = objs.run(second).unwrap().bbox;
    let mut d = doc.clone();
    let mut e = Editor::open(&mut d, 0).unwrap();
    e.transform(&[img.into()], Matrix::translate(-10.0, 100.0))
        .unwrap();
    e.transform(
        &[hello.into(), second.into()],
        Matrix::translate(100.0, -10.0),
    )
    .unwrap();
    e.commit().unwrap();
    let d = finish(&d, &["Hello World", "Second line"]);
    let after = render(&d, 0);
    let moved = |r: Rect, dx: f32, dy: f32| Rect::new(r.x0 + dx, r.y0 + dy, r.x1 + dx, r.y1 + dy);
    let boxes = [
        device_box(&ib, H, 2),
        device_box(&moved(ib, -10.0, 100.0), H, 2),
        device_box(&hb, H, 2),
        device_box(&moved(hb, 100.0, -10.0), H, 2),
        device_box(&sb, H, 2),
        device_box(&moved(sb, 100.0, -10.0), H, 2),
    ];
    assert_eq!(diff_outside(&before, &after, &boxes, 0), 0);
    assert_eq!(ink_inside(&after, device_box(&ib, H, -1)), 0);
    assert_eq!(pixel(&after, 230.0, 150.0, H)[..3], [255, 0, 0]);
    let objs = pagebara_edit::parse(&d, 0).unwrap();
    let h = objs.runs.iter().find(|r| r.text == "Hello World").unwrap();
    assert!((h.bbox.x0 - (hb.x0 + 100.0)).abs() < 0.01);
    // Text after the moved run stays.
    let t = objs
        .runs
        .iter()
        .find(|r| r.text == "Third line here")
        .unwrap();
    let p = t.glyphs[0].origin;
    assert!((p.x - third_pos.x).abs() < 1e-3 && (p.y - third_pos.y).abs() < 1e-3);
    let ni = objs.objects.iter().find(|o| o.kind == Kind::Image).unwrap();
    assert!(
        (ni.bbox.x0 - (ib.x0 - 10.0)).abs() < 0.01 && (ni.bbox.y0 - (ib.y0 + 100.0)).abs() < 0.01
    );
}

#[test]
fn scale_and_rotate() {
    let doc = open(docs::basic());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let rect = find(&objs, |o| o.kind == Kind::Path);
    let b = objs.get(rect).unwrap().bbox;
    let mut d = doc.clone();
    let m = pagebara_edit::geom::scale_about(Point::new(b.x0, b.y0), 2.0, 0.5);
    pagebara_edit::transform(&mut d, 0, &[rect.into()], m).unwrap();
    let d = finish(&d, &[]);
    let objs = pagebara_edit::parse(&d, 0).unwrap();
    let nb = objs.get(rect).unwrap().bbox;
    assert!(
        (nb.width() - 2.0 * b.width()).abs() < 0.01
            && (nb.height() - 0.5 * b.height()).abs() < 0.01
    );
    let mut d2 = d.clone();
    let c = Point::new((nb.x0 + nb.x1) / 2.0, (nb.y0 + nb.y1) / 2.0);
    pagebara_edit::transform(
        &mut d2,
        0,
        &[rect.into()],
        pagebara_edit::geom::rotate_about(c, 90.0),
    )
    .unwrap();
    let d2 = finish(&d2, &[]);
    let rb = pagebara_edit::parse(&d2, 0)
        .unwrap()
        .get(rect)
        .unwrap()
        .bbox;
    assert!((rb.width() - nb.height()).abs() < 0.01 && (rb.height() - nb.width()).abs() < 0.01);
    let img = render(&d2, 0);
    assert_eq!(pixel(&img, c.x, c.y, H)[..3], [0, 0, 255]);
}

#[test]
fn colours_and_opacity() {
    let doc = open(docs::basic());
    let before = render(&doc, 0);
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let rect = find(&objs, |o| o.kind == Kind::Path);
    let line = ObjectId(rect.0 + 1);
    let hello = text_block(&objs, "Hello");
    let mut d = doc.clone();
    let mut e = Editor::open(&mut d, 0).unwrap();
    e.set_fill(&[rect.into()], Color::Rgb(0.0, 1.0, 0.0))
        .unwrap();
    e.set_stroke(&[line.into()], Color::Gray(0.0)).unwrap();
    e.set_fill(&[hello.into()], Color::Rgb(1.0, 0.0, 0.0))
        .unwrap();
    e.commit().unwrap();
    let d = finish(&d, &["Hello World"]);
    let after = render(&d, 0);
    let boxes: Vec<_> = [rect, line, hello]
        .iter()
        .map(|&i| device_box(&objs.get(i).unwrap().bbox, H, 2))
        .collect();
    assert_eq!(diff_outside(&before, &after, &boxes, 0), 0);
    assert_eq!(pixel(&after, 50.0, 40.0, H)[..3], [0, 255, 0]);
    assert_eq!(pixel(&after, 140.0, 30.0, H)[..3], [0, 0, 0]);
    let objs = pagebara_edit::parse(&d, 0).unwrap();
    let r = objs.runs.iter().find(|r| r.text == "Hello World").unwrap();
    assert_eq!(r.fill.rgb(), Some([1.0, 0.0, 0.0]));
    // Text after keeps its colour.
    let s = objs.runs.iter().find(|r| r.text == "Second line").unwrap();
    assert_eq!(s.fill.rgb(), Some([0.0, 0.0, 0.0]));

    let mut d2 = d.clone();
    pagebara_edit::set_opacity(&mut d2, 0, &[rect.into()], Some(0.5), None).unwrap();
    let d2 = finish(&d2, &[]);
    let p = pixel(&render(&d2, 0), 50.0, 40.0, H);
    assert!((120..=135).contains(&p[0]) && p[1] == 255, "{p:?}");
    let o = pagebara_edit::parse(&d2, 0).unwrap();
    let pagebara_edit::ObjectData::Path(info) = &o.get(rect).unwrap().data else {
        unreachable!()
    };
    assert_eq!(info.fill_alpha, 0.5);
}

/// Two overlapping squares, red under blue.
fn squares() -> Document {
    let mut pdf = Pdf::new();
    pdf.page(
        "",
        "1 0 0 rg 20 20 80 80 re f 0 0 1 rg 60 60 80 80 re f 0 1 0 rg 200 20 50 50 re f",
        "",
    );
    open(pdf.finish())
}

#[test]
fn painting_order() {
    let doc = squares();
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    assert_eq!(objs.objects.len(), 3);
    let (red, blue) = (ObjectId(0), ObjectId(1));
    let at = |d: &Document| pixel(&render(d, 0), 80.0, 80.0, H);
    for (id, z, want) in [
        (red, ZOrder::Front, [255, 0, 0]),
        (red, ZOrder::Forward, [255, 0, 0]),
        (blue, ZOrder::Back, [255, 0, 0]),
        (blue, ZOrder::Backward, [255, 0, 0]),
        (red, ZOrder::Back, [0, 0, 255]),
        (blue, ZOrder::Front, [0, 0, 255]),
    ] {
        let mut d = doc.clone();
        pagebara_edit::reorder(&mut d, 0, &[id.into()], z).unwrap();
        let d = finish(&d, &[]);
        assert_eq!(at(&d)[..3], want, "{z:?}");
        assert_eq!(
            diff(&render(&doc, 0), &render(&d, 0), 0) > 0,
            want != [0, 0, 255]
        );
        assert_eq!(pagebara_edit::parse(&d, 0).unwrap().objects.len(), 3);
    }
}

#[test]
fn z_order_in_tagged_content() {
    let doc = open(docs::marked());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let path = find(&objs, |o| o.kind == Kind::Path);
    let mut d = doc.clone();
    pagebara_edit::reorder(&mut d, 0, &[path.into()], ZOrder::Front).unwrap();
    let d = finish(&d, &["Tagged text"]);
    let objs = pagebara_edit::parse(&d, 0).unwrap();
    let last = objs.objects.last().unwrap();
    assert_eq!(last.kind, Kind::Path);
    // The optional content membership went along.
    assert!(
        objs.objects
            .iter()
            .any(|o| o.kind == Kind::Path && o.marked.iter().any(|m| m.optional_content.is_some()))
    );
    assert_eq!(diff(&render(&doc, 0), &render(&d, 0), 0), 0);
}

fn green(n: u32) -> Vec<u8> {
    [0u8, 200, 0, 255].repeat((n * n) as usize)
}

#[test]
fn replace_add_extract_and_crop_images() {
    let doc = open(docs::basic());
    let before = render(&doc, 0);
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let img = find(&objs, |o| o.kind == Kind::Image);
    let ib = objs.get(img).unwrap().bbox;
    let px = green(4);
    let mut d = doc.clone();
    pagebara_edit::replace_image(
        &mut d,
        0,
        img.into(),
        &NewImage::Rgba {
            width: 4,
            height: 4,
            pixels: &px,
        },
        ImageFit::Contain,
    )
    .unwrap();
    let d = finish(&d, &[]);
    let after = render(&d, 0);
    assert_eq!(
        diff_outside(&before, &after, &[device_box(&ib, H, 2)], 0),
        0
    );
    // A square image in a 60 × 40 frame: centred, white at the sides.
    assert_eq!(pixel(&after, 250.0, 40.0, H)[..3], [0, 200, 0]);
    assert_eq!(pixel(&after, 222.0, 40.0, H)[..3], [255, 255, 255]);
    let objs = pagebara_edit::parse(&d, 0).unwrap();
    let i = objs.objects.iter().find(|o| o.kind == Kind::Image).unwrap();
    assert_eq!(i.image().unwrap().width, 4);
    // The old image went from the resources.
    let pd = d.get(objs.page_ref).unwrap();
    let res = d.get_in(pd.as_dict().unwrap(), b"Resources");
    let xo = d.get_in(res.as_dict().unwrap(), b"XObject");
    assert_eq!(xo.as_dict().unwrap().len(), 1);

    // Extracting gives the pixels back.
    let png = pagebara_edit::extract_image(&d, 0, i.id).unwrap();
    let im = pagebara_png::decode(&png, &Default::default()).unwrap();
    assert_eq!((im.width, im.height), (4, 4));
    assert_eq!(&im.to_rgba8()[..4], &[0, 200, 0, 255]);
    let png = pagebara_edit::extract_image(&doc, 0, img).unwrap();
    let im = pagebara_png::decode(&png, &Default::default()).unwrap();
    assert_eq!(&im.to_rgba8()[..4], &[255, 0, 0, 255]);

    // Crop to the left half, then add a picture.
    let mut d2 = d.clone();
    let i = objs.objects.iter().find(|o| o.kind == Kind::Image).unwrap();
    pagebara_edit::crop(
        &mut d2,
        0,
        &[i.id.into()],
        Rect::new(220.0, 20.0, 250.0, 60.0),
    )
    .unwrap();
    let place = pagebara_edit::placement_in(&Rect::new(150.0, 150.0, 190.0, 190.0), 4, 4);
    pagebara_edit::add_image(
        &mut d2,
        0,
        &NewImage::Rgba {
            width: 4,
            height: 4,
            pixels: &px,
        },
        place,
    )
    .unwrap();
    let d2 = finish(&d2, &["Hello World"]);
    let r = render(&d2, 0);
    assert_eq!(pixel(&r, 245.0, 40.0, H)[..3], [0, 200, 0]);
    assert_eq!(pixel(&r, 255.0, 40.0, H)[..3], [255, 255, 255]);
    assert_eq!(pixel(&r, 170.0, 170.0, H)[..3], [0, 200, 0]);
    assert_eq!(
        diff_outside(
            &after,
            &r,
            &[
                device_box(&ib, H, 2),
                device_box(&Rect::new(150.0, 150.0, 190.0, 190.0), H, 2)
            ],
            0
        ),
        0
    );
    let objs = pagebara_edit::parse(&d2, 0).unwrap();
    assert_eq!(
        objs.objects
            .iter()
            .filter(|o| o.kind == Kind::Image)
            .count(),
        2
    );
}

#[test]
fn added_png_keeps_its_proportions() {
    let doc = open(docs::basic());
    let px = [0u8, 200, 0].repeat(8 * 2);
    let png = pagebara_png::encode(8, 2, pagebara_png::ColorType::Rgb, &px).unwrap();
    let mut d = doc.clone();
    let mut e = Editor::open(&mut d, 0).unwrap();
    e.add_image_in(&NewImage::Png(&png), Rect::new(100.0, 100.0, 180.0, 180.0))
        .unwrap();
    e.commit().unwrap();
    let d = finish(&d, &["Hello World"]);
    let objs = pagebara_edit::parse(&d, 0).unwrap();
    let b = objs
        .objects
        .iter()
        .rfind(|o| o.kind == Kind::Image)
        .unwrap()
        .bbox;
    // 8 × 2 pixels in an 80 × 80 frame: 80 × 20, centred.
    assert!(
        (b.width() - 80.0).abs() < 0.01 && (b.height() - 20.0).abs() < 0.01,
        "{b:?}"
    );
    assert!((b.y0 - 130.0).abs() < 0.01, "{b:?}");
    assert_eq!(
        pagebara_edit::image_size(&NewImage::Png(&png)).unwrap(),
        (8, 2)
    );
}

#[test]
fn line_width_and_dash() {
    let doc = open(docs::basic());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let line = find(&objs, |o| {
        o.path()
            .is_some_and(|p| p.stroke.is_some() && p.fill.is_none())
    });
    let mut d = doc.clone();
    pagebara_edit::set_line(&mut d, 0, &[line.into()], Some(10.0), Some(&[10.0, 10.0])).unwrap();
    let d = finish(&d, &[]);
    let r = render(&d, 0);
    // 10 wide around y = 30, dashed from x = 100: on to 110, off to 120.
    assert_eq!(pixel(&r, 105.0, 34.0, H)[..3], [255, 0, 0]);
    assert_eq!(pixel(&r, 115.0, 30.0, H)[..3], [255, 255, 255]);
    let o = pagebara_edit::parse(&d, 0).unwrap();
    let p = o
        .objects
        .iter()
        .find_map(|o| o.path().filter(|p| p.stroke.is_some() && p.fill.is_none()))
        .unwrap();
    assert!((p.line_width - 10.0).abs() < 1e-3 && p.dashed);
}

#[test]
fn inline_images() {
    let doc = open(docs::inline_and_clip());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let img = find(&objs, |o| o.kind == Kind::Image);
    let png = pagebara_edit::extract_image(&doc, 0, img).unwrap();
    let im = pagebara_png::decode(&png, &Default::default())
        .unwrap()
        .to_rgba8();
    assert_eq!(&im[..4], &[0, 0, 255, 255]);
    let px = green(2);
    let mut d = doc.clone();
    pagebara_edit::replace_image(
        &mut d,
        0,
        img.into(),
        &NewImage::Rgba {
            width: 2,
            height: 2,
            pixels: &px,
        },
        ImageFit::Stretch,
    )
    .unwrap();
    let d = finish(&d, &[]);
    assert_eq!(pixel(&render(&d, 0), 30.0, 30.0, H)[..3], [0, 200, 0]);
    let b = objs.get(img).unwrap().bbox;
    assert_eq!(
        diff_outside(&render(&doc, 0), &render(&d, 0), &[device_box(&b, H, 2)], 0),
        0
    );
}

#[test]
fn clipping_paths_survive_edits() {
    let doc = open(docs::inline_and_clip());
    let before = render(&doc, 0);
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    // The red triangle also clips the gray square after it.
    let tri = find(
        &objs,
        |o| matches!(&o.data, pagebara_edit::ObjectData::Path(p) if p.clips),
    );
    let mut d = doc.clone();
    pagebara_edit::delete(&mut d, 0, &[tri.into()]).unwrap();
    let d = finish(&d, &[]);
    let after = render(&d, 0);
    // The square is still clipped to the triangle; only the red is gone.
    let p = pixel(&after, 240.0, 40.0, H);
    assert_eq!(p[..3], [0, 0, 0]);
    assert_eq!(pixel(&after, 195.0, 100.0, H)[..3], [255, 255, 255]);
    let tb = objs.get(tri).unwrap().bbox;
    assert_eq!(
        diff_outside(&before, &after, &[device_box(&tb, H, 2)], 0),
        0
    );

    let mut d = doc.clone();
    pagebara_edit::set_fill(&mut d, 0, &[tri.into()], Color::Rgb(0.0, 1.0, 0.0)).unwrap();
    let d = finish(&d, &[]);
    let after = render(&d, 0);
    assert_eq!(pixel(&after, 195.0, 100.0, H)[..3], [255, 255, 255]);
    assert_eq!(
        diff_outside(&before, &after, &[device_box(&tb, H, 2)], 0),
        0
    );
}

#[test]
fn duplicate_objects() {
    let doc = open(docs::basic());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let rect = find(&objs, |o| o.kind == Kind::Path);
    let hello = text_block(&objs, "Hello");
    let mut d = doc.clone();
    pagebara_edit::duplicate(
        &mut d,
        0,
        &[rect.into(), hello.into()],
        Matrix::translate(0.0, 100.0),
    )
    .unwrap();
    let d = finish(&d, &[]);
    let r = render(&d, 0);
    assert_eq!(pixel(&r, 50.0, 140.0, H)[..3], [0, 0, 255]);
    assert_eq!(pixel(&r, 50.0, 40.0, H)[..3], [0, 0, 255]);
    let t = page_text(&d, 0);
    assert_eq!(t.matches("Hello World").count(), 2, "{t}");
    let objs = pagebara_edit::parse(&d, 0).unwrap();
    let third = objs
        .runs
        .iter()
        .find(|r| r.text == "Third line here")
        .unwrap();
    let before = pagebara_edit::parse(&doc, 0).unwrap();
    let was = before
        .runs
        .iter()
        .find(|r| r.text == "Third line here")
        .unwrap();
    assert_eq!(third.glyphs[0].origin, was.glyphs[0].origin);
}

#[test]
fn text_style() {
    let doc = open(docs::basic());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let second = run(&objs, "Second line");
    let third_was = objs
        .runs
        .iter()
        .find(|r| r.text == "Third line here")
        .unwrap()
        .glyphs[0]
        .origin;
    let width = objs.run(second).unwrap().bbox.width();
    let mut d = doc.clone();
    pagebara_edit::set_text_style(
        &mut d,
        0,
        &[second.into()],
        TextStyle {
            size: Some(24.0),
            fill: Some(Color::Rgb(0.0, 0.0, 1.0)),
            ..Default::default()
        },
    )
    .unwrap();
    let d = finish(&d, &["Second line"]);
    let objs = pagebara_edit::parse(&d, 0).unwrap();
    let s = objs.runs.iter().find(|r| r.text == "Second line").unwrap();
    assert_eq!(s.size, 24.0);
    assert!((s.bbox.width() - 2.0 * width).abs() < 0.1);
    assert_eq!(s.fill.rgb(), Some([0.0, 0.0, 1.0]));
    let t = objs
        .runs
        .iter()
        .find(|r| r.text == "Third line here")
        .unwrap();
    assert_eq!(t.size, 12.0);
    assert_eq!(t.fill.rgb(), Some([0.0, 0.0, 0.0]));
    assert!((t.glyphs[0].origin.x - third_was.x).abs() < 1e-3);
}

#[test]
fn replace_text_in_existing_fonts() {
    let doc = open(docs::basic());
    let before = render(&doc, 0);
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let hello = run(&objs, "Hello World");
    let hb = objs.run(hello).unwrap().bbox;
    let mut d = doc.clone();
    pagebara_edit::replace_text(&mut d, 0, hello.into(), "Hello There (1)", &SimpleEncoder)
        .unwrap();
    let d = finish(&d, &["Hello There (1)"]);
    let t = page_text(&d, 0);
    assert!(t.contains("Hello There (1)") && !t.contains("World"), "{t}");
    let after = render(&d, 0);
    let wide = Rect::new(hb.x0, hb.y0, hb.x1 + 60.0, hb.y1);
    assert_eq!(
        diff_outside(&before, &after, &[device_box(&wide, H, 2)], 0),
        0
    );

    // Characters the font cannot show.
    let mut d2 = d.clone();
    let objs = pagebara_edit::parse(&d2, 0).unwrap();
    let r = run(&objs, "Hello There (1)");
    let e = pagebara_edit::replace_text(&mut d2, 0, r.into(), "漢字", &SimpleEncoder);
    assert!(matches!(
        e,
        Err(pagebara_edit::Error::Encode(
            pagebara_edit::EncodeError::Missing(_)
        ))
    ));

    // A whole line of two runs.
    let doc = open(docs::basic());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let block = text_block(&objs, "Second line");
    let mut d = doc.clone();
    pagebara_edit::replace_line(&mut d, 0, block, 1, "A new third line", &SimpleEncoder).unwrap();
    let d = finish(&d, &["A new third line"]);
    let t = page_text(&d, 0);
    assert!(t.contains("Second line\nA new third line"), "{t}");
}

#[test]
fn replace_text_in_composite_and_type3_fonts() {
    let doc = open(docs::cid());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    assert_eq!(objs.runs[0].text, "CID text");
    let mut d = doc.clone();
    pagebara_edit::replace_text(&mut d, 0, objs.runs[0].id.into(), "New CID", &SimpleEncoder)
        .unwrap();
    let d = finish(&d, &[]);
    assert!(page_text(&d, 0).contains("New CID"));

    let doc = open(docs::type3());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    assert_eq!(objs.runs[0].text, "abba");
    let mut d = doc.clone();
    pagebara_edit::replace_text(&mut d, 0, objs.runs[0].id.into(), "bab", &SimpleEncoder).unwrap();
    let d = finish(&d, &[]);
    assert!(page_text(&d, 0).contains("bab"));
}

#[test]
fn actual_text_follows_the_text() {
    let doc = open(docs::marked());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let fi = objs.runs.iter().find(|r| r.text == "fi").unwrap().id;
    let mut d = doc.clone();
    pagebara_edit::replace_text(&mut d, 0, fi.into(), "f l", &SimpleEncoder).unwrap();
    let d = finish(&d, &["Tagged text"]);
    let t = page_text(&d, 0);
    assert!(t.contains("f l") && !t.contains("fi"), "{t}");
    let objs = pagebara_edit::parse(&d, 0).unwrap();
    let r = objs.runs.iter().find(|r| r.text == "f l").unwrap();
    assert_eq!(r.marked.last().and_then(|m| m.mcid), Some(1));
    let blk = objs.get(r.block).unwrap();
    assert_eq!(blk.marked.last().and_then(|m| m.mcid), Some(0));
}

#[test]
fn add_text() {
    let doc = open(docs::basic());
    let before = render(&doc, 0);
    let mut d = doc.clone();
    pagebara_edit::add_text(
        &mut d,
        0,
        &NewText {
            text: "Added text\nSecond added",
            origin: Point::new(150.0, 150.0),
            size: 10.0,
            color: Color::Rgb(1.0, 0.0, 0.0),
            angle: 0.0,
            leading: None,
        },
        &SimpleEncoder,
    )
    .unwrap();
    let d = finish(&d, &["Added text", "Second added"]);
    let t = page_text(&d, 0);
    assert!(t.contains("Added text\nSecond added"), "{t}");
    let area = Rect::new(148.0, 130.0, 260.0, 162.0);
    assert_eq!(
        diff_outside(&before, &render(&d, 0), &[device_box(&area, H, 2)], 0),
        0
    );
    let objs = pagebara_edit::parse(&d, 0).unwrap();
    let r = objs.runs.iter().find(|r| r.text == "Added text").unwrap();
    assert_eq!(r.fill.rgb(), Some([1.0, 0.0, 0.0]));
    assert!((r.glyphs[0].origin.x - 150.0).abs() < 1e-3);
}

#[test]
fn shared_forms_are_copied_for_one_use() {
    let doc = open(docs::shared_form());
    let p0 = render(&doc, 0);
    let p1 = render(&doc, 1);
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let form = find(&objs, |o| o.kind == Kind::Form);
    let pagebara_edit::ObjectData::Form(f) = &objs.get(form).unwrap().data else {
        unreachable!()
    };
    let inner = f.children[0];
    let fb = objs.get(form).unwrap().bbox;
    let mut d = doc.clone();
    pagebara_edit::set_fill(&mut d, 0, &[inner.into()], Color::Rgb(1.0, 0.0, 0.0)).unwrap();
    let d = finish(&d, &["In form"]);
    let a = render(&d, 0);
    assert_eq!(pixel(&a, 25.0, 25.0, H)[..3], [255, 0, 0]);
    assert_eq!(diff_outside(&p0, &a, &[device_box(&fb, H, 2)], 0), 0);
    assert_eq!(diff(&p1, &render(&d, 1), 0), 0);

    // Every use.
    let mut d = doc.clone();
    let mut e = Editor::open(&mut d, 0).unwrap();
    e.forms = FormEdit::AllUses;
    e.set_fill(&[inner.into()], Color::Rgb(1.0, 0.0, 0.0))
        .unwrap();
    e.commit().unwrap();
    let d = finish(&d, &[]);
    assert_eq!(pixel(&render(&d, 1), 105.0, 75.0, H)[..3], [255, 0, 0]);
    assert_eq!(pixel(&render(&d, 0), 155.0, 125.0, H)[..3], [255, 0, 0]);

    // Moving the form itself and editing inside it at once.
    let mut d = doc.clone();
    let mut e = Editor::open(&mut d, 0).unwrap();
    e.set_fill(&[inner.into()], Color::Gray(0.5)).unwrap();
    e.transform(&[form.into()], Matrix::translate(0.0, 50.0))
        .unwrap();
    e.commit().unwrap();
    let d = finish(&d, &[]);
    assert_eq!(pixel(&render(&d, 0), 25.0, 75.0, H)[..3], [128, 128, 128]);
    assert_eq!(diff(&p1, &render(&d, 1), 0), 0);
}

#[test]
fn rotated_and_cropped_pages() {
    let doc = open(docs::rotated_cropped());
    for page in 0..4 {
        let objs = pagebara_edit::parse(&doc, page).unwrap();
        let rect = find(&objs, |o| o.kind == Kind::Path);
        let mut d = doc.clone();
        pagebara_edit::transform(&mut d, page, &[rect.into()], Matrix::translate(5.0, 5.0))
            .unwrap();
        let d = finish(&d, &[]);
        let nb = pagebara_edit::parse(&d, page)
            .unwrap()
            .get(rect)
            .unwrap()
            .bbox;
        let ob = objs.get(rect).unwrap().bbox;
        assert!((nb.x0 - ob.x0 - 5.0).abs() < 1e-3, "page {page}");
        // The view helpers map the crop box to the view.
        let f = objs.frame;
        let v = f.view_rect(f.crop_box);
        assert!(v.x0.abs() < 1e-3 && v.y0.abs() < 1e-3);
    }
    // Contents in two streams: only the one changed is rewritten.
    let objs = pagebara_edit::parse(&doc, 3).unwrap();
    let text = text_block(&objs, "Cropped");
    let mut d = doc.clone();
    pagebara_edit::delete(&mut d, 3, &[text.into()]).unwrap();
    let pd = d.get(objs.page_ref).unwrap();
    let c = pd.as_dict().unwrap().get(b"Contents").unwrap().clone();
    let old = doc.get(objs.page_ref).unwrap();
    let oc = old.as_dict().unwrap().get(b"Contents").unwrap().clone();
    let (c, oc) = (
        c.as_array().unwrap().to_vec(),
        oc.as_array().unwrap().to_vec(),
    );
    assert_eq!(c[0], oc[0]);
    assert_ne!(c[1], oc[1]);
    let d = finish(&d, &[]);
    assert!(!page_text(&d, 3).contains("Cropped"));
}

#[test]
fn hit_testing() {
    let doc = open(docs::basic());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let o = hit::HitOptions::default();
    let at = |x, y| hit::object_at(&objs, Point::new(x, y), &o).map(|i| objs.get(i).unwrap().kind);
    assert_eq!(at(50.0, 40.0), Some(Kind::Path));
    assert_eq!(at(140.0, 31.0), Some(Kind::Path));
    assert_eq!(at(250.0, 40.0), Some(Kind::Image));
    assert_eq!(at(30.0, 155.0), Some(Kind::Text));
    assert_eq!(at(280.0, 180.0), None);
    let all = hit::objects_in(&objs, Rect::new(0.0, 0.0, 300.0, 100.0), true, &o);
    assert_eq!(all.len(), 3);
    let hello = objs.runs.iter().find(|r| r.text == "Hello World").unwrap();
    let g = &hello.glyphs[1];
    let p = Point::new((g.quad[0].x + g.quad[1].x) / 2.0 - 0.5, 152.0);
    let pos = hit::text_pos_at(&objs, p, 5.0).unwrap();
    assert_eq!((pos.run, pos.glyph), (hello.id, 1));
    let c = hit::caret(&objs, pos).unwrap();
    assert!((c.bottom.x - g.quad[0].x).abs() < 1e-3 && c.top.y > c.bottom.y);
    let end = hit::caret(
        &objs,
        hit::TextPos {
            run: hello.id,
            glyph: hello.glyphs.len(),
        },
    )
    .unwrap();
    assert!((end.bottom.x - hello.bbox.x1).abs() < 0.01);
    let h = hit::handles(objs.objects.last().unwrap());
    assert_eq!(h[0], Point::new(220.0, 20.0));
}
