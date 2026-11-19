//! Visible signatures: upright on rotated pages, and text in any script with embedded font
//! subsets. Checked by rendering the signed pages.

mod common;

use std::path::Path;
use std::sync::Arc;

use common::*;
use pagebara_annot::{EmbeddedFont, FontDb, TextFont};
use pagebara_render::{PixelRect, RenderRequest, Renderer};
use pagebara_sign::*;
use pagebara_syntax::{Document, Object};

const SIDE: f64 = 400.0;

/// Four square pages rotated by 0, 90, 180 and 270 degrees.
fn rotated_pdf() -> Vec<u8> {
    let mut objs = vec![
        "<</Type/Catalog/Pages 2 0 R>>".to_string(),
        "<</Type/Pages/Kids[3 0 R 4 0 R 5 0 R 6 0 R]/Count 4>>".to_string(),
    ];
    for rot in [0, 90, 180, 270] {
        objs.push(format!(
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 {SIDE} {SIDE}]/Resources<<>>/Rotate {rot}>>"
        ));
    }
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
    }
    let x = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f\r\n", objs.len() + 1).bytes());
    for o in offs {
        out.extend(format!("{o:010} 00000 n\r\n").bytes());
    }
    out.extend(
        format!(
            "trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{x}\n%%EOF\n",
            objs.len() + 1
        )
        .bytes(),
    );
    out
}

/// A rectangle as the viewer sees it (`[left, top, right, bottom]`, y down) on a square
/// page rotated clockwise by `rot`, in page space.
fn shown_to_page(r: [f64; 4], rot: u16) -> [f64; 4] {
    let map = |x: f64, y: f64| match rot {
        90 => (y, x),
        180 => (SIDE - x, y),
        270 => (SIDE - y, SIDE - x),
        _ => (x, SIDE - y),
    };
    let (a, b) = (map(r[0], r[1]), map(r[2], r[3]));
    [a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)]
}

/// Dark pixels of `page` as shown, inside `area` (`[left, top, right, bottom]`).
fn ink(doc: &Document, page: usize, area: [f64; 4]) -> Vec<(u32, u32)> {
    let r = Renderer::new(doc.clone());
    let side = SIDE as u32;
    let img = r
        .render(
            page,
            &RenderRequest::new(
                1.0,
                PixelRect {
                    x: 0,
                    y: 0,
                    w: side,
                    h: side,
                },
            ),
        )
        .unwrap();
    let mut out = Vec::new();
    for y in area[1] as u32..area[3] as u32 {
        for x in area[0] as u32..area[2] as u32 {
            let p = &img.data[((y * img.width + x) * 4) as usize..][..3];
            if p.iter().map(|&c| u32::from(c)).sum::<u32>() < 300 {
                out.push((x, y));
            }
        }
    }
    out
}

/// The signature's widget and its normal appearance stream dictionary.
fn widget(doc: &Document, field: &str) -> (pagebara_syntax::Dict, pagebara_syntax::Dict) {
    let form = pagebara_forms::Form::load(doc);
    let w = form.field(field).unwrap().widgets[0].id;
    let wd = doc.get(w).unwrap().as_dict().unwrap().clone();
    let ap = doc.get_in(&wd, b"AP");
    let n = doc.get_in(ap.as_dict().unwrap(), b"N");
    let n = n.as_stream().unwrap().dict.clone();
    (wd, n)
}

#[test]
fn upright_on_rotated_pages() {
    let pki = pki();
    let mut bytes = rotated_pdf();
    // The first line is much longer than the second ("Date: …"): upright text has its
    // ink reaching further right in the upper half of the box.
    let name = "Wolfeschlegelsteinhausenbergerdorff Senior";
    let shown = [60.0, 100.0, 340.0, 160.0];
    for (page, rot) in [0u16, 90, 180, 270].into_iter().enumerate() {
        let app = Appearance {
            page,
            rect: shown_to_page(shown, rot),
            image: None,
            text: true,
        };
        let field = format!("Sig{page}");
        let opts = SignOptions {
            field: Some(&field),
            appearance: Some(&app),
            name: Some(name),
            ..Default::default()
        };
        let doc = Document::open(bytes).unwrap();
        bytes = sign(&doc, &pki.ec_signer, &opts).unwrap();
        let doc = Document::open(bytes.clone()).unwrap();

        let (wd, n) = widget(&doc, &field);
        let mk_r = doc
            .get_in(&wd, b"MK")
            .as_dict()
            .and_then(|mk| mk.int(b"R"))
            .unwrap_or(0);
        assert_eq!(mk_r, i64::from(rot), "page {page}");
        assert_eq!(n.get(b"Matrix").is_some(), rot != 0, "page {page}");

        let dots = ink(&doc, page, shown);
        assert!(dots.len() > 200, "page {page}: {} dark pixels", dots.len());
        let (x0, x1) = (
            dots.iter().map(|d| d.0).min().unwrap(),
            dots.iter().map(|d| d.0).max().unwrap(),
        );
        let (y0, y1) = (
            dots.iter().map(|d| d.1).min().unwrap(),
            dots.iter().map(|d| d.1).max().unwrap(),
        );
        assert!(
            x1 - x0 > 3 * (y1 - y0),
            "page {page}: text is not horizontal"
        );
        let mid = (y0 + y1) / 2;
        let right = |upper: bool| {
            dots.iter()
                .filter(|d| (d.1 < mid) == upper)
                .map(|d| d.0)
                .max()
                .unwrap_or(0)
        };
        assert!(
            right(true) > right(false) + 40,
            "page {page}: text is upside down ({} vs {})",
            right(true),
            right(false)
        );
        // Left-aligned at the box's left edge as seen.
        assert!(
            f64::from(x0) < shown[0] + 12.0,
            "page {page}: starts at {x0}"
        );
    }
    let reports = verify_all(&bytes, &trust());
    assert_eq!(reports.len(), 4);
    for r in &reports {
        assert_eq!(r.status, Status::ValidTrusted, "{r:#?}");
    }
    qpdf_check(&bytes, "rotated");
}

/// An existing field's own `/MK /R` wins over the page's rotation.
#[test]
fn existing_field_keeps_its_rotation() {
    let pki = pki();
    let doc = Document::open(sample_pdf()).unwrap();
    let mut doc = doc;
    let w = pagebara_forms::Form::load(&doc)
        .field("approval")
        .unwrap()
        .widgets[0]
        .id;
    let mut wd = doc.get(w).unwrap().as_dict().unwrap().clone();
    let mut mk = pagebara_syntax::Dict::new();
    mk.set(b"R", 90);
    wd.set(b"MK", mk);
    doc.set(w, wd);
    let doc = Document::open(doc.save_incremental().unwrap()).unwrap();
    let app = Appearance {
        page: 0,
        rect: [0.0; 4],
        image: None,
        text: true,
    };
    let opts = SignOptions {
        field: Some("approval"),
        appearance: Some(&app),
        ..Default::default()
    };
    let signed = sign(&doc, &pki.rsa_signer, &opts).unwrap();
    let doc = Document::open(signed.clone()).unwrap();
    let (wd, n) = widget(&doc, "approval");
    let mk = doc.get_in(&wd, b"MK");
    assert_eq!(mk.as_dict().and_then(|m| m.int(b"R")), Some(90));
    // The 200 × 60 widget turned a quarter: a 60 × 200 box.
    let bbox: Vec<f64> = n
        .get(b"BBox")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o.as_f64().unwrap())
        .collect();
    assert_eq!(bbox, [0.0, 0.0, 60.0, 200.0]);
    assert_eq!(
        verify_all(&signed, &trust())[0].status,
        Status::ValidTrusted
    );
}

/// The font bundled with the app, plus the system's fonts for other scripts.
fn inter() -> Option<EmbeddedFont> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/desktop/ui/fonts/InterVariable.ttf");
    let data: Arc<[u8]> = std::fs::read(&path).ok()?.into();
    let mut db = FontDb::system();
    db.add_data(Path::new("bundled/InterVariable.ttf"), data);
    let face = db
        .faces()
        .iter()
        .find(|f| f.path == Path::new("bundled/InterVariable.ttf"))?
        .clone();
    EmbeddedFont::with_primary(Arc::new(db), &face)
}

/// The fonts of an appearance stream: base font names and `/ToUnicode` text.
fn fonts(doc: &Document, n: &pagebara_syntax::Dict) -> Vec<(String, String)> {
    let res = doc.get_in(n, b"Resources");
    let fonts = doc.get_in(res.as_dict().unwrap(), b"Font");
    let mut out = Vec::new();
    for (_, f) in fonts.as_dict().unwrap().iter() {
        let f = doc.resolve(f);
        let f = f.as_dict().unwrap();
        let base = f
            .name(b"BaseFont")
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default();
        let tu = doc.get_in(f, b"ToUnicode");
        let tu = tu
            .as_stream()
            .and_then(|s| doc.decode(s).ok())
            .map(|d| String::from_utf8_lossy(&d.data).into_owned())
            .unwrap_or_default();
        out.push((base, tu));
    }
    out
}

#[test]
fn unicode_text() {
    let pki = pki();
    let name = "Zoë Łukasiewicz – 東京";
    let app = Appearance {
        page: 0,
        rect: [72.0, 300.0, 400.0, 380.0],
        image: None,
        text: true,
    };
    let font = inter();
    let opts = SignOptions {
        field: Some("Unicode"),
        appearance: Some(&app),
        name: Some(name),
        reason: Some("Approuvé – 承認"),
        font: font.as_ref().map(|f| f as &dyn TextFont),
        ..Default::default()
    };
    let doc = Document::open(sample_pdf()).unwrap();
    let signed = sign(&doc, &pki.rsa_signer, &opts).unwrap();
    let doc = Document::open(signed.clone()).unwrap();
    let r = &verify_all(&signed, &trust())[0];
    assert_eq!(r.status, Status::ValidTrusted, "{r:#?}");
    assert_eq!(r.info.signer.as_deref(), Some(name));
    let (_, n) = widget(&doc, &r.info.field);
    let fonts = fonts(&doc, &n);
    // Inter shows the Latin text, ToUnicode maps it back.
    let all: String = fonts.iter().map(|f| f.1.as_str()).collect();
    if font.is_some() {
        assert!(fonts[0].0.contains("Inter"), "{fonts:?}");
    }
    assert!(all.contains("<0141>"), "Ł is mapped: {all}");
    assert!(all.contains("<00EB>"), "ë is mapped: {all}");
    assert!(all.contains("<2013>"), "the dash is mapped: {all}");
    // The CJK characters come from a system font when there is one; without, the
    // appearance shows the primary face's fallback glyph for them.
    let cjk = pagebara_annot::FontDb::system()
        .faces()
        .iter()
        .any(|f| f.family.contains("CJK"));
    if cjk {
        assert!(fonts.len() >= 2, "{fonts:?}");
        assert!(
            all.contains("<6771>") && all.contains("<4EAC>"),
            "東京: {all}"
        );
    } else {
        eprintln!("no CJK font on this system: 東京 is shown as fallback glyphs");
    }
    // Something is drawn there.
    let r = Renderer::new(doc.clone());
    let img = r
        .render(
            0,
            &RenderRequest::new(
                1.0,
                PixelRect {
                    x: 72,
                    y: 792 - 380,
                    w: 328,
                    h: 80,
                },
            ),
        )
        .unwrap();
    let dark = img
        .data
        .chunks(4)
        .filter(|p| u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2]) < 300)
        .count();
    assert!(dark > 300, "{dark}");
    qpdf_check(&signed, "unicode");
    if let Some(out) = pdfsig(&signed, "unicode") {
        assert!(out.contains("Signature is Valid"), "{out}");
    }
}

/// Text Helvetica can show stays in Helvetica, without embedding anything.
#[test]
fn latin_text_uses_helvetica() {
    let pki = pki();
    let app = Appearance {
        page: 0,
        rect: [72.0, 300.0, 400.0, 380.0],
        image: None,
        text: true,
    };
    let opts = SignOptions {
        appearance: Some(&app),
        name: Some("Zoë Müller"),
        ..Default::default()
    };
    let doc = Document::open(sample_pdf()).unwrap();
    let signed = sign(&doc, &pki.rsa_signer, &opts).unwrap();
    let doc = Document::open(signed.clone()).unwrap();
    let r = &verify_all(&signed, &trust())[0];
    let (_, n) = widget(&doc, &r.info.field);
    let fonts = fonts(&doc, &n);
    assert_eq!(fonts.len(), 1);
    assert_eq!(fonts[0].0, "Helvetica");
    let _ = Object::Null;
}
