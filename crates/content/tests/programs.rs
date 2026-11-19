//! Embedded font programs read through `pagebara-font`: built-in encodings, glyph names, cmaps
//! and widths fill in what the font dictionaries leave out. The programs come from system
//! fonts at test time (skipped when missing).

mod common;

use common::*;
use pagebara_content::{Interpreter, extract_text};
use pagebara_font::subset::Embedder;
use pagebara_syntax::{Document, ObjRef, Object};

const DEJAVU: &str = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf";
const SYMBOL_T1: &str = "/usr/share/fonts/type1/urw-base35/StandardSymbolsPS.t1";
const NIMBUS_OTF: &str = "/usr/share/fonts/opentype/urw-base35/NimbusSans-Regular.otf";

fn system(path: &str) -> Option<Vec<u8>> {
    let d = std::fs::read(path).ok();
    if d.is_none() {
        eprintln!("skipping: {path} not installed");
    }
    d
}

/// Text and widths of the page with and without reading font programs.
fn extract(doc: &Document, programs: bool) -> (String, Vec<f32>) {
    let (page, res) = page_and_resources(doc);
    let it = Interpreter::new(doc);
    let mut it = if programs {
        it
    } else {
        it.without_font_programs()
    };
    let l = it.page(&page, &res);
    let t = extract_text(doc, &l);
    let font = &l.fonts[0].font;
    let widths = l.glyphs.iter().map(|g| font.width(g.code)).collect();
    (t.text, widths)
}

#[test]
fn subset_truetype_cid_font_without_to_unicode() {
    let Some(data) = system(DEJAVU) else { return };
    let text = "Héllo wörld Привет Ωμέγα";
    let mut e = Embedder::new(data, 0).unwrap();
    let codes = e.encode(text);
    let hex: String = codes.iter().map(|b| format!("{b:02X}")).collect();
    let content = format!("BT /F0 12 Tf 10 100 Td <{hex}> Tj ET");
    let mut doc = page_doc(&content, "<</Font<</F0 5 0 R>>>>", &["null".into()], "");
    let font = e.write(&mut doc).unwrap();
    let mut dict = doc.get(font).unwrap().as_dict().unwrap().clone();
    assert!(dict.get(b"ToUnicode").is_some());
    dict.remove(b"ToUnicode");
    doc.set(ObjRef::new(5, 0), Object::Dict(dict));
    let doc = Document::open(doc.save_full().unwrap()).unwrap();

    let (without, _) = extract(&doc, false);
    assert!(without.contains('\u{FFFD}'), "{without}");
    // The CIDToGIDMap leads to the subset's glyphs, its cmap back to the characters.
    let (with, _) = extract(&doc, true);
    assert_eq!(with, text);
}

/// A Type 1 program with a built-in encoding, embedded under a name that is not a standard
/// font's, without `/Encoding`, `/Widths` or `/ToUnicode`.
#[test]
fn type1_builtin_encoding_and_widths() {
    let Some(data) = system(SYMBOL_T1) else {
        return;
    };
    let mut doc = page_doc(
        "BT /F0 10 Tf 10 100 Td (abG) Tj ET",
        "<</Font<</F0 5 0 R>>>>",
        &[
            "<</Type/Font/Subtype/Type1/BaseFont/ABCDEF+Greek/FontDescriptor 6 0 R>>".into(),
            "<</Type/FontDescriptor/FontName/ABCDEF+Greek/Flags 4/FontFile 7 0 R\
              /FontBBox[0 -200 1000 900]/Ascent 900/Descent -200/ItalicAngle 0/StemV 80>>"
                .into(),
            "null".into(),
        ],
        "",
    );
    let mut ff = pagebara_syntax::Dict::new();
    ff.set(b"Length1", Object::Int(data.len() as i64));
    ff.set(b"Length2", Object::Int(0));
    ff.set(b"Length3", Object::Int(0));
    doc.set(
        ObjRef::new(7, 0),
        Object::Stream(pagebara_syntax::Stream::new(ff, data)),
    );

    let (without, w0) = extract(&doc, false);
    assert_eq!(without, "abG");
    assert_eq!(w0, [0.0; 3]);
    let (with, w) = extract(&doc, true);
    assert_eq!(with, "αβΓ");
    let afm = [0.631, 0.549, 0.603];
    assert!(
        w.iter().zip(afm).all(|(a, b)| (a - b).abs() < 1e-5),
        "{w:?}"
    );
}

/// `/Widths` and `/MissingWidth` win over the program's advances.
#[test]
fn widths_fall_back_only_without_widths_array() {
    let Some(data) = system(DEJAVU) else { return };
    let mut doc = page_doc(
        "BT /F0 10 Tf 10 100 Td (AB) Tj ET",
        "<</Font<</F0 5 0 R>>>>",
        &[
            "<</Type/Font/Subtype/TrueType/BaseFont/DejaVuSans/FirstChar 65/LastChar 65\
              /Widths[500]/FontDescriptor 6 0 R>>"
                .into(),
            "<</Type/FontDescriptor/FontName/DejaVuSans/Flags 32/FontFile2 7 0 R\
              /MissingWidth 250>>"
                .into(),
            "null".into(),
        ],
        "",
    );
    doc.set(
        ObjRef::new(7, 0),
        Object::Stream(pagebara_syntax::Stream::new(
            pagebara_syntax::Dict::new(),
            data,
        )),
    );
    let (t, w) = extract(&doc, true);
    assert_eq!(t, "AB");
    assert_eq!(w, [0.5, 0.25]);
}

/// A bare CFF program (`/FontFile3 /Type1C`) without `/Widths`: advances come from its
/// charstrings, glyph names from its charset.
#[test]
fn cff_widths_and_glyph_ids() {
    let Some(data) = system(NIMBUS_OTF) else {
        return;
    };
    let otf = pagebara_font::Font::parse(data).unwrap();
    let cff = otf.as_sfnt().unwrap().table(b"CFF ").unwrap().to_vec();
    let mut doc = page_doc(
        "BT /F0 10 Tf 10 100 Td (Hi!) Tj ET",
        "<</Font<</F0 5 0 R>>>>",
        &[
            "<</Type/Font/Subtype/Type1/BaseFont/ABCDEF+Nimbus/FontDescriptor 6 0 R>>".into(),
            "<</Type/FontDescriptor/FontName/ABCDEF+Nimbus/Flags 32/FontFile3 7 0 R>>".into(),
            "null".into(),
        ],
        "",
    );
    let mut ff = pagebara_syntax::Dict::new();
    ff.set(b"Subtype", Object::name(b"Type1C"));
    doc.set(
        ObjRef::new(7, 0),
        Object::Stream(pagebara_syntax::Stream::new(ff, cff)),
    );
    let (t, w) = extract(&doc, true);
    assert_eq!(t, "Hi!");
    for (c, w) in "Hi!".chars().zip(w) {
        let want = otf.advance(otf.glyph_for_char(c).unwrap()).unwrap() / 1000.0;
        assert!((w - want).abs() < 1e-4, "{c}: {w} vs {want}");
    }
    let (page, res) = page_and_resources(&doc);
    let l = Interpreter::new(&doc).page(&page, &res);
    let font = &l.fonts[0].font;
    let gid = |c: char| font.glyph_id(c as u32).map(|g| g as u16);
    // The bare CFF keeps the OpenType font's glyph order.
    assert_eq!(gid('H'), otf.glyph_for_char('H'));
    assert_eq!(gid('!'), otf.glyph_for_char('!'));
}
