//! Flattening keeps what pages look like while annotations, fields, layers and
//! transparency become plain content.

use std::process::Command;

use pagebara_annot::{Annotation, Color, Kind, MarkupKind, Point, Quad, Rect};
use pagebara_flatten::{Layers, Options};
use pagebara_render::{Annotations, PixelRect, RenderRequest, Renderer};
use pagebara_syntax::{Document, ObjRef};

type Image = (u32, u32, Vec<u8>);

fn render(doc: &Document, page: usize, scale: f32) -> Image {
    let r = Renderer::new(doc.clone());
    let (w, h) = r.page_size(page).unwrap();
    let rect = PixelRect {
        x: 0,
        y: 0,
        w: (w * scale).round() as u32,
        h: (h * scale).round() as u32,
    };
    let mut req = RenderRequest::new(scale, rect);
    req.annotations = Annotations::View;
    let img = r.render(page, &req).unwrap();
    (img.width, img.height, img.data)
}

/// Share of pixels differing by more than `tol` in some channel.
fn differing(a: &Image, b: &Image, tol: u8) -> f64 {
    assert_eq!((a.0, a.1), (b.0, b.1));
    let n =
        a.2.as_chunks::<4>()
            .0
            .iter()
            .zip(b.2.as_chunks::<4>().0)
            .filter(|(p, q)| (0..3).any(|c| p[c].abs_diff(q[c]) > tol))
            .count();
    n as f64 / (a.0 * a.1) as f64
}

fn reopen(doc: &Document) -> Document {
    Document::open(doc.save_full().unwrap()).unwrap()
}

fn page_ref(doc: &Document, i: usize) -> ObjRef {
    pagebara_pages::PageTree::load(doc).page(i).unwrap().r
}

fn annot_count(doc: &Document, i: usize) -> usize {
    let p = doc.get(page_ref(doc, i)).unwrap();
    doc.get_in(p.as_dict().unwrap(), b"Annots")
        .as_array()
        .map_or(0, |a| a.len())
}

fn text(doc: &Document, i: usize) -> String {
    let p = doc.get(page_ref(doc, i)).unwrap();
    let d = p.as_dict().unwrap();
    let res = doc
        .get_in(d, b"Resources")
        .as_dict()
        .cloned()
        .unwrap_or_default();
    let list = pagebara_content::Interpreter::new(doc).page(d, &res);
    pagebara_content::extract_text(doc, &list).text
}

fn qpdf_ok(doc: &Document, name: &str) {
    let dir = std::env::temp_dir().join("pagebara-flatten-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, doc.save_full().unwrap()).unwrap();
    let Ok(out) = Command::new("qpdf").arg("--check").arg(&path).output() else {
        return;
    };
    let t = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && !t.contains("WARNING"),
        "{name}: {t}"
    );
}

fn annotated() -> Document {
    let mut doc = Document::open(pagebara_samples::lease()).unwrap();
    let page = page_ref(&doc, 0);
    let mut square = Annotation::new(
        Kind::Square {
            interior: Some(Color::Rgb(0.2, 0.6, 1.0)),
        },
        Rect::new(300.0, 500.0, 420.0, 560.0),
    );
    square.color = Some(Color::Rgb(0.0, 0.0, 0.8));
    let mut hl = Annotation::new(
        Kind::Markup {
            kind: MarkupKind::Highlight,
            quads: vec![Quad::from_rect(Rect::new(72.0, 700.0, 250.0, 716.0))],
        },
        Rect::new(72.0, 700.0, 250.0, 716.0),
    );
    hl.color = Some(Color::Rgb(1.0, 0.9, 0.0));
    let mut ink = Annotation::new(
        Kind::Ink {
            strokes: vec![vec![
                Point::new(100.0, 200.0),
                Point::new(150.0, 260.0),
                Point::new(200.0, 210.0),
            ]],
        },
        Rect::new(90.0, 190.0, 210.0, 270.0),
    );
    ink.color = Some(Color::Rgb(0.8, 0.0, 0.0));
    let mut ft = Annotation::new(
        Kind::FreeText {
            font_size: 14.0,
            text_color: Color::BLACK,
            align: pagebara_annot::Align::Left,
        },
        Rect::new(300.0, 300.0, 500.0, 340.0),
    );
    ft.contents = Some("Reviewed by Sam".into());
    for a in [square, hl, ink, ft] {
        pagebara_annot::add(&mut doc, page, &a).unwrap();
    }
    doc
}

#[test]
fn annotations_into_content_look_the_same() {
    let doc = annotated();
    let links = annot_count(&doc, 0) - 4;
    let before = render(&doc, 0, 1.5);
    let mut flat = doc.clone();
    let r = pagebara_flatten::flatten(&mut flat, &Options::all()).unwrap();
    assert_eq!(r.annotations, 4);
    let flat = reopen(&flat);
    // Links stay; the drawn annotations are content now.
    assert_eq!(annot_count(&flat, 0), links);
    let after = render(&flat, 0, 1.5);
    let d = differing(&before, &after, 24);
    assert!(d < 0.002, "{d}");
    assert!(text(&flat, 0).contains("Reviewed by Sam"));
    qpdf_ok(&flat, "annots.pdf");
    // Links too, when asked.
    let mut all = doc.clone();
    let opts = Options {
        links: true,
        ..Options::all()
    };
    pagebara_flatten::flatten(&mut all, &opts).unwrap();
    assert_eq!(annot_count(&all, 0), 0);
}

#[test]
fn form_fields_into_content_keep_values() {
    let mut doc = Document::open(pagebara_samples::form()).unwrap();
    let mut form = pagebara_forms::Form::load(&doc);
    let name = form
        .fields
        .iter()
        .find(|f| f.kind == pagebara_forms::FieldKind::Text)
        .unwrap()
        .name
        .clone();
    form.set_text(&mut doc, &name, "Ada Lovelace").unwrap();
    let doc = reopen(&doc);
    let before = render(&doc, 0, 1.5);
    let c = pagebara_flatten::contents(&doc);
    assert!(c.fields > 0);
    let mut flat = doc.clone();
    let r = pagebara_flatten::flatten(&mut flat, &Options::all()).unwrap();
    assert_eq!(r.fields, c.fields);
    let flat = reopen(&flat);
    assert!(!pagebara_forms::Form::load(&flat).has_form());
    let cat = flat.catalog().unwrap();
    assert!(!cat.as_dict().unwrap().contains(b"AcroForm"));
    assert!(text(&flat, 0).contains("Ada Lovelace"));
    let after = render(&flat, 0, 1.5);
    let d = differing(&before, &after, 24);
    assert!(d < 0.002, "{d}");
    qpdf_ok(&flat, "form.pdf");
}

/// One page with text in a visible layer, a hidden layer and a hidden form XObject.
fn layered() -> Document {
    let content = b"BT /F1 18 Tf 50 150 Td (Always here) Tj ET\n\
/OC /On BDC BT /F1 18 Tf 50 110 Td (Shown layer) Tj ET EMC\n\
/OC /Off BDC q BT /F1 18 Tf 50 70 Td (Hidden layer) Tj ET EMC\n\
Q q 1 0 0 1 200 20 cm /Fm Do Q\n";
    let form = b"0 0 1 rg 0 0 60 40 re f";
    let objs: Vec<Vec<u8>> = vec![
        b"<</Type /Catalog /Pages 2 0 R /OCProperties <</OCGs [5 0 R 6 0 R] /D <</ON [5 0 R] /OFF [6 0 R] /Order [5 0 R 6 0 R]>>>>>>".to_vec(),
        b"<</Type /Pages /Kids [3 0 R] /Count 1>>".to_vec(),
        b"<</Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources <</Font <</F1 <</Type /Font /Subtype /Type1 /BaseFont /Helvetica>>>> /Properties <</On 5 0 R /Off 6 0 R>> /XObject <</Fm 7 0 R>>>> /Contents 4 0 R>>".to_vec(),
        [format!("<</Length {}>>stream\n", content.len()).as_bytes(), content, b"\nendstream"].concat(),
        b"<</Type /OCG /Name (Shown)>>".to_vec(),
        b"<</Type /OCG /Name (Hidden)>>".to_vec(),
        [format!("<</Type /XObject /Subtype /Form /BBox [0 0 60 40] /OC 6 0 R /Length {}>>stream\n", form.len()).as_bytes(), form, b"\nendstream"].concat(),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).bytes());
        out.extend(o);
        out.extend(b"\nendobj\n");
    }
    let x = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
    for o in offs {
        out.extend(format!("{o:010} 00000 n \n").bytes());
    }
    out.extend(
        format!(
            "trailer\n<</Size {} /Root 1 0 R>>\nstartxref\n{x}\n%%EOF\n",
            objs.len() + 1
        )
        .bytes(),
    );
    Document::open(out).unwrap()
}

#[test]
fn layers_applied_or_merged() {
    let doc = layered();
    assert_eq!(pagebara_flatten::contents(&doc).layers, 2);
    let before = render(&doc, 0, 2.0);
    let opts = Options {
        layers: Layers::Apply,
        ..Options::nothing()
    };
    let mut applied = doc.clone();
    let r = pagebara_flatten::flatten(&mut applied, &opts).unwrap();
    assert_eq!(r.layers, 2);
    assert_eq!(r.hidden_removed, 2);
    let applied = reopen(&applied);
    assert!(
        !applied
            .catalog()
            .unwrap()
            .as_dict()
            .unwrap()
            .contains(b"OCProperties")
    );
    let t = text(&applied, 0);
    assert!(t.contains("Always here") && t.contains("Shown layer"));
    assert!(!t.contains("Hidden layer"));
    assert_eq!(differing(&before, &render(&applied, 0, 2.0), 2), 0.0);
    let raw = pagebara_content::page_contents(
        &applied,
        applied
            .get(page_ref(&applied, 0))
            .unwrap()
            .as_dict()
            .unwrap(),
    );
    assert!(!String::from_utf8_lossy(&raw).contains("/OC"));
    qpdf_ok(&applied, "layers-applied.pdf");

    let mut merged = doc.clone();
    let opts = Options {
        layers: Layers::Merge,
        ..Options::nothing()
    };
    pagebara_flatten::flatten(&mut merged, &opts).unwrap();
    let merged = reopen(&merged);
    assert!(text(&merged, 0).contains("Hidden layer"));
    // The hidden blue box shows now.
    let img = render(&merged, 0, 1.0);
    let blue = img
        .2
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[2] > 200 && p[0] < 50)
        .count();
    assert!(blue > 2000, "{blue}");
    qpdf_ok(&merged, "layers-merged.pdf");
}

#[test]
fn transparency_rendered_in_place() {
    let content = b"BT /F1 24 Tf 30 100 Td (Under glass) Tj ET\n\
q /Half gs 1 0 0 rg 20 80 200 60 re f Q\n\
BT /F1 12 Tf 30 20 Td (Plain text stays text) Tj ET\n";
    let mut doc = layered();
    let page = page_ref(&doc, 0);
    let mut d = doc.get(page).unwrap().as_dict().unwrap().clone();
    let s = doc.add(pagebara_syntax::Stream::new(
        Default::default(),
        &content[..],
    ));
    d.set(b"Contents", s);
    let mut res = doc.get_in(&d, b"Resources").as_dict().unwrap().clone();
    let mut gs = pagebara_syntax::Dict::new();
    let mut half = pagebara_syntax::Dict::new();
    half.set(b"ca", 0.5);
    gs.set(b"Half", half);
    res.set(b"ExtGState", gs);
    d.set(b"Resources", res);
    doc.set(page, d);
    let doc = reopen(&doc);
    let before = render(&doc, 0, 2.0);
    let mut flat = doc.clone();
    let opts = Options {
        transparency: Some(144.0),
        ..Options::nothing()
    };
    let r = pagebara_flatten::flatten(&mut flat, &opts).unwrap();
    assert_eq!(r.transparent_areas, 1);
    let flat = reopen(&flat);
    let objs = pagebara_edit::parse(&flat, 0).unwrap();
    for o in objs.top_level() {
        if let pagebara_edit::ObjectData::Path(p) = &o.data {
            assert_eq!(p.fill_alpha, 1.0);
        }
    }
    assert!(text(&flat, 0).contains("Plain text stays text"));
    let d = differing(&before, &render(&flat, 0, 2.0), 40);
    assert!(d < 0.01, "{d}");
    qpdf_ok(&flat, "transparency.pdf");
}

#[test]
fn signed_documents_warn_and_keep_signatures() {
    let pki = pagebara_samples::pki();
    let mut doc = Document::open(pagebara_samples::signed(&pki)).unwrap();
    assert_eq!(pagebara_flatten::signed_fields(&doc).len(), 1);
    let r = pagebara_flatten::flatten(&mut doc, &Options::all()).unwrap();
    assert_eq!(r.warnings.len(), 1);
    assert!(r.warnings[0].contains("signed"));
    let doc = reopen(&doc);
    assert_eq!(pagebara_flatten::signed_fields(&doc).len(), 1);
}
