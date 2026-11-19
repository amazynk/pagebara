//! Text in any script with [`EmbeddedFont`]: system fonts embedded as Type 0 subsets, checked
//! with qpdf, rendered by MuPDF and Poppler, and extracted back exactly. Skipped when the
//! system has no usable fonts.

mod common;

use common::*;
use pagebara_annot::*;
use pagebara_syntax::{Document, ObjRef};

const PAGE: ObjRef = ObjRef::new(3, 0);
const TEXT: &str = "pässwörd 密码 Привет مرحبا";

fn free_text(text: &str, rect: Rect) -> Annotation {
    let mut a = Annotation::new(
        Kind::FreeText {
            font_size: 16.0,
            text_color: Color::BLACK,
            align: Align::Left,
        },
        rect,
    );
    a.contents = Some(text.into());
    a
}

fn system_font() -> Option<&'static EmbeddedFont> {
    let f = EmbeddedFont::system();
    if f.is_none() {
        eprintln!("no system fonts; skipping");
    }
    f
}

/// Whitespace-separated words, so line breaks and spacing do not matter. Poppler wraps
/// right-to-left runs in embedding controls (U+202B … U+202C), which are dropped.
fn words(s: &str) -> Vec<String> {
    s.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| !('\u{202A}'..='\u{202E}').contains(c))
                .collect()
        })
        .collect()
}

#[test]
fn default_font_choice() {
    assert_eq!(default_font("Grüße, 1 € — “quoted”").family(), "Helvetica");
    if let Some(f) = system_font() {
        assert_eq!(default_font(TEXT).family(), f.family());
        assert!(unsupported_chars(f, TEXT).is_empty());
        assert_eq!(unsupported_chars(&Helvetica, TEXT).len(), 13);
    }
}

#[test]
fn free_text_in_any_script() {
    let Some(font) = system_font() else { return };
    let mut doc = Document::open(one_page(400, 300, "")).unwrap();
    // The whole text as an annotation through the default path, and each word flattened
    // into the page on its own line, for pixel checks per script.
    let whole = add(
        &mut doc,
        PAGE,
        &free_text(TEXT, Rect::new(10.0, 250.0, 390.0, 290.0)),
    )
    .unwrap();
    let mut rows = Vec::new();
    for (i, w) in TEXT.split(' ').enumerate() {
        let y = 200.0 - 45.0 * i as f64;
        let r = Rect::new(10.0, y, 200.0, y + 30.0);
        let missing = add_flattened_with(&mut doc, PAGE, &free_text(w, r), font).unwrap();
        assert!(missing.is_empty(), "{w}: {missing:?}");
        rows.push((w, r));
    }
    let faces = font.faces();
    assert!(faces.len() >= 2, "{faces:?}");
    eprintln!(
        "faces: {:?}",
        faces.iter().map(|f| &f.full_name).collect::<Vec<_>>()
    );

    let bytes = doc.save_incremental().unwrap();
    qpdf_check(&bytes, "unicode-free-text");
    let doc = Document::open(bytes.clone()).unwrap();
    assert_eq!(read(&doc, whole).unwrap().contents.as_deref(), Some(TEXT));

    for (renderer, img) in render_all(&bytes, "unicode-free-text") {
        for (w, r) in &rows {
            let b = image_box((r.x0, r.y0, r.x1, r.y1), 400.0, 300.0, 0);
            let (n, bounds) = img.count_px(b, is_dark);
            assert!(n > 40, "{renderer}: {w} not drawn ({n} dark pixels)");
            // Glyphs are spread out, not one blob: at least a word's width of ink.
            let (x0, _, x1, _) = bounds.unwrap();
            assert!(x1 - x0 > 20, "{renderer}: {w} too narrow");
        }
        let (n, _) = img.count_px(
            image_box((10.0, 250.0, 390.0, 290.0), 400.0, 300.0, 0),
            is_dark,
        );
        assert!(n > 200, "{renderer}: annotation not drawn ({n})");
    }

    for (tool, text) in extract_all(&bytes, "unicode-free-text") {
        let got = words(&text);
        // The annotation's line, then the flattened words in order.
        let expected: Vec<String> = words(TEXT).into_iter().chain(words(TEXT)).collect();
        assert_eq!(got, expected, "{tool}:\n{text}");
    }
}

/// Faces are subset per appearance: only the glyphs shown are embedded, with widths and a
/// `/ToUnicode` map for each.
#[test]
fn subsets_with_widths_and_to_unicode() {
    let Some(font) = system_font() else { return };
    let mut doc = Document::open(one_page(400, 300, "")).unwrap();
    let w = add_with(
        &mut doc,
        PAGE,
        &free_text("Привет 密码", Rect::new(10.0, 10.0, 390.0, 60.0)),
        font,
    )
    .unwrap();
    assert!(w.missing.is_empty());
    let d = doc.get(w.id).unwrap();
    let ap = doc.get_in(d.as_dict().unwrap(), b"AP");
    let n = doc.get_in(ap.as_dict().unwrap(), b"N");
    let res = doc.get_in(&n.as_stream().unwrap().dict, b"Resources");
    let fonts = doc.get_in(res.as_dict().unwrap(), b"Font");
    let fonts = fonts.as_dict().unwrap();
    assert!(fonts.len() >= 2, "Cyrillic and CJK from different faces");
    for (_, f) in fonts.iter() {
        let f = doc.resolve(f);
        let f = f.as_dict().unwrap();
        assert_eq!(f.name(b"Subtype"), Some(&b"Type0"[..]));
        assert_eq!(f.name(b"Encoding"), Some(&b"Identity-H"[..]));
        assert!(doc.get_in(f, b"ToUnicode").as_stream().is_some());
        let cid = doc.get_in(f, b"DescendantFonts");
        let cid = doc.resolve(&cid.as_array().unwrap()[0]);
        let cid = cid.as_dict().unwrap();
        // Widths that differ from /DW are listed.
        assert!(doc.get_in(cid, b"DW").as_f64().is_some());
        assert!(doc.get_in(cid, b"W").as_array().is_some());
        let fd = doc.get_in(cid, b"FontDescriptor");
        let fd = fd.as_dict().unwrap();
        let file = [&b"FontFile2"[..], b"FontFile3"]
            .iter()
            .find_map(|k| doc.get_in(fd, k).as_stream().cloned())
            .unwrap();
        let data = doc.decode(&file).unwrap().data;
        assert!(data.len() < 200_000, "subset of {} bytes", data.len());
    }
}

/// Flattening an annotation without an appearance generates one in the default font for
/// its text, so Cyrillic is drawn instead of dropped.
#[test]
fn flatten_generates_missing_appearances() {
    if system_font().is_none() {
        return;
    }
    let mut doc = Document::open(one_page(400, 300, "")).unwrap();
    let r = add(
        &mut doc,
        PAGE,
        &free_text("Привет мир", Rect::new(10.0, 200.0, 390.0, 240.0)),
    )
    .unwrap();
    let mut d = doc.get(r).unwrap().as_dict().unwrap().clone();
    d.remove(b"AP");
    doc.set(r, d);
    flatten(&mut doc, PAGE, r).unwrap();
    assert!(doc.get(r).is_none_or(|o| o.is_null()));
    let bytes = doc.save_incremental().unwrap();
    qpdf_check(&bytes, "unicode-flatten");
    for (tool, text) in extract_all(&bytes, "unicode-flatten") {
        assert_eq!(words(&text), ["Привет", "мир"], "{tool}");
    }
}
