//! Generated appearances, checked in the stream and in renderings by mutool and poppler.

mod common;

use std::sync::Arc;

use common::*;
use pagebara_annot::{Glyph, TextFont};
use pagebara_forms::*;
use pagebara_syntax::{Document, ObjRef, Object};

fn normal_stream(doc: &Document, w: ObjRef) -> (pagebara_syntax::Dict, String) {
    let d = doc.get(w).unwrap();
    let ap = doc.get_in(d.as_dict().unwrap(), b"AP");
    let n = doc.get_in(ap.as_dict().unwrap(), b"N");
    let s = match n.as_stream() {
        Some(s) => s.clone(),
        None => {
            let state = d.as_dict().unwrap().name(b"AS").unwrap().to_vec();
            let r = n.as_dict().unwrap().get(&state).unwrap().clone();
            doc.resolve(&r).as_stream().unwrap().clone()
        }
    };
    let data = doc.decode(&s).unwrap().data;
    (s.dict.clone(), String::from_utf8_lossy(&data).into_owned())
}

fn filled() -> Document {
    let mut doc = sample_form("");
    let mut form = Form::load(&doc);
    form.set_text(&mut doc, "name", "Ann Example").unwrap();
    form.set_text(
        &mut doc,
        "notes",
        "A long note that has to wrap over several lines inside the box.\nNew paragraph.",
    )
    .unwrap();
    form.set_text(&mut doc, "zip", "12345").unwrap();
    form.set_text(&mut doc, "pin", "secret").unwrap();
    form.set_checked(&mut doc, "agree", true).unwrap();
    form.select_choice(&mut doc, "country", "Italy").unwrap();
    form.set_text(&mut doc, "city", "Berlin").unwrap();
    form.select_choices(&mut doc, "langs", &["French", "Spanish"])
        .unwrap();
    form.set_text(&mut doc, "addr.street", "1 Main Street")
        .unwrap();
    form.set_text(&mut doc, "rot", "Turned").unwrap();
    form.generate_appearances(&mut doc, true).unwrap();
    // Regenerated radio buttons: a ZapfDingbats dot in the selected one.
    form.select_radio(&mut doc, "color", Some("Green")).unwrap();
    let bytes = doc.save_incremental().unwrap();
    qpdf_check(&bytes, "appearance-filled");
    Document::open(bytes).unwrap()
}

#[test]
fn stream_contents() {
    let doc = filled();
    let form = Form::load(&doc);
    let w = |n: &str| form.field(n).unwrap().widgets[0].id;

    let (d, s) = normal_stream(&doc, w("name"));
    assert!(s.starts_with("1 1 0.8 rg"), "background first: {s}");
    assert!(s.contains("/Tx BMC") && s.contains("EMC"));
    assert!(s.contains("/Helv 12 Tf") && s.contains("0 0 1 rg"), "{s}");
    assert!(s.contains("(Ann Example) Tj"));
    assert!(s.contains("W n"), "clipped: {s}");
    // The form's own Helvetica from /DR is used by reference.
    let res = d.get(b"Resources").unwrap().as_dict().unwrap();
    let font = res
        .get(b"Font")
        .unwrap()
        .as_dict()
        .unwrap()
        .get(b"Helv")
        .unwrap();
    assert!(font.as_objref().is_some());

    // Auto-sized multiline text wraps into several lines.
    let (_, s) = normal_stream(&doc, w("notes"));
    assert_eq!(s.matches(" Tj").count(), 3, "{s}");
    assert!(s.contains("(New paragraph.) Tj"));

    // Comb: one show per character, centred in five cells with dividers.
    let (_, s) = normal_stream(&doc, w("zip"));
    for c in ["(1) Tj", "(2) Tj", "(5) Tj"] {
        assert!(s.contains(c), "{s}");
    }
    assert_eq!(s.matches(" l\n").count(), 4, "cell dividers: {s}");

    // Password: bullets (WinAnsi 0x95).
    let (_, s) = normal_stream(&doc, w("pin"));
    // (0x95 is not UTF-8: each byte reads back as a replacement character.)
    assert!(
        s.contains(&format!("({}) Tj", "\u{FFFD}".repeat(6))),
        "{s:?}"
    );
    assert!(!s.contains("secret"));

    // Check box: ZapfDingbats cross ('8') in red, and an Off state.
    let (d, s) = normal_stream(&doc, w("agree"));
    assert!(
        s.contains("/ZaDb") && s.contains("(8) Tj") && s.contains("1 0 0 rg"),
        "{s}"
    );
    assert!(d.get(b"Resources").is_some());
    let wd = doc.get(w("agree")).unwrap();
    let ap = doc.get_in(wd.as_dict().unwrap(), b"AP");
    let n = doc.get_in(ap.as_dict().unwrap(), b"N");
    assert!(n.as_dict().unwrap().contains(b"Off"));
    assert!(n.as_dict().unwrap().contains(b"Yes"));

    // Combo box: the displayed text of the selected export value.
    let (_, s) = normal_stream(&doc, w("country"));
    assert!(s.contains("(Italy) Tj"), "{s}");
    // List box: highlight and the visible items from /TI.
    let (_, s) = normal_stream(&doc, w("langs"));
    assert!(s.contains("0.6 0.757 0.855 rg"), "{s}");
    assert!(!s.contains("(English)"), "TI 1 scrolls past the first: {s}");
    assert!(s.contains("(Spanish) Tj"));
    // Push button: caption with a beveled border.
    let (_, s) = normal_stream(&doc, w("submit"));
    assert!(s.contains("(Submit) Tj"));
    assert!(s.contains("1 g\n"), "bevel light: {s}");
    // Inherited DA and right quadding (Q from the parent is absent: left).
    let (_, s) = normal_stream(&doc, w("addr.street"));
    assert!(s.contains("/Helv 9 Tf"), "{s}");
    // Rotated widget: the form is turned by its matrix.
    let (d, _) = normal_stream(&doc, w("rot"));
    let m = d.get(b"Matrix").unwrap().as_array().unwrap().to_vec();
    assert_eq!(
        m[..4],
        [
            Object::Int(0),
            Object::Int(1),
            Object::Int(-1),
            Object::Int(0)
        ]
    );
    let bbox = d.get(b"BBox").unwrap().as_array().unwrap().to_vec();
    assert_eq!(bbox[2], Object::Int(200));
}

#[test]
fn renderings() {
    let doc = filled();
    let src = doc.source().to_vec();
    std::fs::write(scratch("appearance-view.pdf"), &src).unwrap();
    for (tool, r) in render_all(&src, "appearance-render") {
        // Text in each kind of field.
        assert!(r.any(52.0, 722.0, 150.0, 742.0, is_blue), "{tool}: name");
        assert!(
            r.any(52.0, 640.0, 298.0, 698.0, is_dark),
            "{tool}: notes top"
        );
        // Comb digits: one per fifth of 50..150.
        for cell in 0..5 {
            let x0 = 50.0 + 20.0 * cell as f64;
            assert!(
                r.any(x0 + 5.0, 563.0, x0 + 15.0, 580.0, is_dark),
                "{tool}: comb cell {cell}"
            );
        }
        assert!(
            r.any(202.0, 563.0, 260.0, 580.0, is_dark),
            "{tool}: bullets"
        );
        assert!(r.any(52.0, 522.0, 64.0, 534.0, is_red), "{tool}: check");
        assert!(r.any(52.0, 443.0, 120.0, 457.0, is_dark), "{tool}: combo");
        assert!(r.any(85.0, 485.0, 89.0, 489.0, is_dark), "{tool}: Green on");
        assert!(!r.any(55.0, 485.0, 59.0, 489.0, is_dark), "{tool}: Red off");
        // List box: the highlighted rows are light blue.
        let blue_hl = |p: [u8; 3]| p[2] > 200 && p[0] > 130 && p[0] < 175;
        assert!(
            r.any(52.0, 345.0, 198.0, 418.0, blue_hl),
            "{tool}: highlight"
        );
        // Push button: grey background.
        assert!(
            r.any(405.0, 724.0, 495.0, 740.0, |p| p
                .iter()
                .all(|&c| (170..210).contains(&c))),
            "{tool}: button background"
        );
        // The rotated field's text runs vertically inside 550..580.
        assert!(
            r.any(552.0, 410.0, 578.0, 590.0, is_dark),
            "{tool}: rotated"
        );
    }
}

/// A stand-in for an embedded Unicode font: shows Cyrillic as two-byte codes.
#[derive(Debug)]
struct Cyrillic;

impl TextFont for Cyrillic {
    fn resource_name(&self) -> &str {
        "Cyr"
    }
    fn family(&self) -> &str {
        "Cyr"
    }
    fn glyph(&self, c: char) -> Option<Glyph> {
        Some(Glyph {
            code: c as u32,
            len: 2,
            width: 0.6,
            face: 0,
        })
    }
    fn fallback(&self) -> Glyph {
        self.glyph('?').unwrap()
    }
    fn ascent(&self) -> f64 {
        0.8
    }
    fn resource(&self, _: &mut Document, used: &[(char, Glyph)]) -> Object {
        assert!(used.iter().any(|(c, _)| *c == 'Ж'));
        Object::name(b"FakeType0")
    }
}

#[test]
fn pluggable_font() {
    let mut doc = sample_form("");
    let mut form = Form::load(&doc);
    form.set_font(Arc::new(Cyrillic));
    // Latin text keeps the form's font; other scripts use the plugged-in one.
    form.set_text(&mut doc, "name", "Ann").unwrap();
    let w = form.field("name").unwrap().widgets[0].id;
    assert!(normal_stream(&doc, w).1.contains("/Helv 12 Tf"));
    let c = form.set_text(&mut doc, "name", "Жанна").unwrap();
    assert!(c.missing.is_empty());
    let (d, s) = normal_stream(&doc, w);
    assert!(s.contains("/Cyr 12 Tf"), "{s}");
    let res = d.get(b"Resources").unwrap().as_dict().unwrap();
    assert!(
        res.get(b"Font")
            .unwrap()
            .as_dict()
            .unwrap()
            .contains(b"Cyr")
    );
}

#[test]
fn widths_from_dr_font() {
    // A TrueType font with its own /Widths and a PDFDocEncoding-style /Differences.
    let mut doc = Document::open(one_page(300, 200, "")).unwrap();
    let enc = add(
        &mut doc,
        "<</Type/Encoding/Differences[39/quotesingle 128/bullet]>>",
    );
    let font = add(
        &mut doc,
        &format!(
            "<</Type/Font/Subtype/TrueType/BaseFont/ABCDEF+Arial/FirstChar 32/LastChar 128\
             /Widths[{}]/Encoding {} 0 R>>",
            ["500"; 97].join(" "),
            enc.num
        ),
    );
    let f = SimpleFont::from_resource(&doc, "F0", &Object::Ref(font)).unwrap();
    assert_eq!(f.glyph('A').unwrap().width, 0.5);
    assert_eq!(f.glyph('•').unwrap().code, 128);
    assert_eq!(f.glyph('\'').unwrap().code, 39);
    assert!(f.glyph('Ж').is_none());
}
