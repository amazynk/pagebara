//! Encoding new text in each kind of PDF font: the text extracts exactly, renders, and
//! is placed by the advances the encoder reports.

mod common;

use common::*;
use pagebara_syntax::{Document, ObjRef, Object};
use pagebara_typeset::{CharStatus, Encoded, FontSession, RunFont, WriteMode};

/// Outcome of [`check`].
struct Checked {
    enc: Encoded,
    statuses: Vec<(char, CharStatus)>,
    doc: Document,
    session: FontSession,
}

/// Encodes `text` in the font, writes it on the page at y = 200 below the original line,
/// and checks extraction (ours and external), widths and rendering.
fn check(
    mut doc: Document,
    font: ObjRef,
    original: &str,
    text: &str,
    render_check: bool,
) -> Checked {
    let mut session = FontSession::new(Some(db()));
    let id = session.open_font(&doc, font).unwrap();
    let cov = session.coverage(id, text);
    let enc = session.encode(id, text);
    assert!(enc.is_complete(), "missing {:?}", enc.missing);
    let statuses: Vec<(char, CharStatus)> = cov.iter().map(|c| (c.ch, c.status)).collect();
    // Coverage predicted what encoding did.
    for (c, s) in &statuses {
        let in_fallback = enc.glyphs().any(|(f, g)| {
            matches!(f, RunFont::Fallback(_)) && enc.text[g.cluster.clone()] == *c.to_string()
        });
        assert_eq!(*s == CharStatus::Fallback, in_fallback, "{c}: {s:?}");
    }
    let changes = session.finish(&mut doc, WriteMode::InPlace);
    let file = add_to_page(&mut doc, &changes, &show(&enc, 20.0, 200.0, 16.0));
    let doc = Document::open(file.clone()).unwrap();

    let page = page_text(&doc);
    let orig: String = line_chars(&page, 250.0).into_iter().map(|c| c.2).collect();
    assert_eq!(orig, original, "original line changed");
    let chars = line_chars(&page, 200.0);
    let got: String = chars.iter().map(|c| c.2.as_str()).collect();
    let want: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let got_ns: String = got.chars().filter(|c| !c.is_whitespace()).collect();
    assert_eq!(got_ns, want, "extracted {got:?}");

    // Advances: every glyph's origin where the encoder's widths put it.
    let mut x = 20.0f32;
    let mut expect = Vec::new();
    for (_, g) in enc.glyphs() {
        expect.push((
            x,
            x + g.advance * 16.0,
            enc.text[g.cluster.clone()].to_string(),
        ));
        x += (g.advance + g.kern) * 16.0;
    }
    let placed: Vec<_> = expect.iter().filter(|e| !e.2.trim().is_empty()).collect();
    let found: Vec<_> = chars.iter().filter(|c| !c.2.trim().is_empty()).collect();
    assert_eq!(placed.len(), found.len());
    for (e, f) in placed.iter().zip(&found) {
        assert_eq!(e.2, f.2);
        assert!(
            (e.0 - f.0).abs() < 0.01,
            "{}: origin {} vs {}",
            e.2,
            e.0,
            f.0
        );
        assert!(
            (e.1 - f.1).abs() < 0.01,
            "{}: advance end {} vs {}",
            e.2,
            e.1,
            f.1
        );
    }

    if render_check {
        let img = render(&doc);
        for (x0, x1, t) in &found {
            if t.chars().all(|c| ".,'".contains(c)) {
                continue;
            }
            let n = ink(&img, *x0, 196.0, x1.max(x0 + 1.0), 214.0);
            assert!(n > 0, "glyph {t} at {x0} renders blank");
        }
    } else {
        let img = render(&doc);
        eprintln!(
            "not checked: the line renders {} ink pixels",
            ink(&img, 0.0, 196.0, 400.0, 214.0)
        );
    }
    external_checks(
        &file,
        &[original, text.split_whitespace().next().unwrap_or("")],
    );
    Checked {
        enc,
        statuses,
        doc,
        session,
    }
}

fn status_of(c: &Checked, ch: char) -> CharStatus {
    c.statuses.iter().find(|s| s.0 == ch).unwrap().1
}

fn descriptor(name: &str, flags: u32, file_key: &str, file: usize) -> String {
    format!(
        "<< /Type /FontDescriptor /FontName /{name} /Flags {flags} /FontBBox [-1000 -300 2000 1100] \
         /ItalicAngle 0 /Ascent 900 /Descent -230 /CapHeight 700 /StemV 80 /{file_key} {file} 0 R >>"
    )
}

/// A simple font with an embedded program and WinAnsi codes 32..=126.
fn simple_doc(
    subtype: &str,
    name: &str,
    file_dict: &str,
    program: Vec<u8>,
    key: &str,
    original: &str,
) -> (Document, ObjRef) {
    let widths = ascii_widths(&program);
    let fd = descriptor(name, 32, key, 0);
    doc_with_font(
        &[(file_dict, program)],
        &|n| {
            let fd = fd.replace("0 0 R", &format!("{} 0 R", n[0]));
            format!(
                "<< /Type /Font /Subtype /{subtype} /BaseFont /{name} /FirstChar 32 /LastChar 126 \
                 /Widths [{widths}] /Encoding /WinAnsiEncoding /FontDescriptor {fd} >>"
            )
        },
        original.as_bytes(),
    )
}

#[test]
fn truetype_full_program_gains_codes() {
    let Some(ttf) = system(DEJAVU) else { return };
    let len = ttf.len();
    let (doc, f) = simple_doc(
        "TrueType",
        "DejaVuSans",
        &format!("/Length1 {len}"),
        ttf,
        "FontFile2",
        "Hello",
    );
    let text = "Hello Łódź – Ωμέγα Жук €";
    let c = check(doc, f, "Hello", text, true);
    assert_eq!(status_of(&c, 'H'), CharStatus::Existing);
    for ch in ['Ł', 'ó', 'Ω', 'Ж', '€', '–'] {
        assert_eq!(status_of(&c, ch), CharStatus::Added, "{ch}");
    }
    assert!(
        c.enc
            .runs
            .iter()
            .all(|r| r.font == RunFont::Font(pagebara_typeset::FontId(0)))
    );
    // Differences hold the new glyph names; Widths cover the new codes.
    let tree = pagebara_pages::PageTree::load(&c.doc);
    let pd = c.doc.get(tree.pages()[0].r).unwrap();
    let res = c.doc.get_in(pd.as_dict().unwrap(), b"Resources");
    let fonts = c.doc.get_in(res.as_dict().unwrap(), b"Font");
    let fd = c.doc.get_in(fonts.as_dict().unwrap(), b"F1");
    let enc = c.doc.get_in(fd.as_dict().unwrap(), b"Encoding");
    let diffs = c.doc.get_in(enc.as_dict().unwrap(), b"Differences");
    let names: Vec<String> = diffs
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|o| o.as_name().map(|n| String::from_utf8_lossy(n).into_owned()))
        .collect();
    assert!(names.contains(&"Lslash".to_string()), "{names:?}");
    let _ = c.session;
}

#[test]
fn truetype_subset_uses_installed_family() {
    let Some(ttf) = system(DEJAVU) else { return };
    let font = pagebara_font::Font::parse(ttf.clone()).unwrap();
    let s = font.as_sfnt().unwrap();
    let chars: Vec<(char, u16)> = " Helo"
        .chars()
        .map(|c| (c, s.glyph_for_char(c).unwrap()))
        .collect();
    let sub =
        pagebara_font::subset::truetype::subset(s, chars.iter().map(|c| c.1), &chars).unwrap();
    let len = sub.data.len();
    let (doc, f) = simple_doc(
        "TrueType",
        "ABCDEF+DejaVuSans",
        &format!("/Length1 {len}"),
        sub.data,
        "FontFile2",
        "Hello",
    );
    let c = check(doc, f, "Hello", "Hello Wörld Ж", true);
    assert_eq!(status_of(&c, 'e'), CharStatus::Existing);
    assert_eq!(status_of(&c, 'W'), CharStatus::Fallback);
    assert_eq!(status_of(&c, 'Ж'), CharStatus::Fallback);
    let fb = c
        .enc
        .runs
        .iter()
        .find_map(|r| match r.font {
            RunFont::Fallback(id) => Some(id),
            _ => None,
        })
        .unwrap();
    assert_eq!(c.session.fallback_face(fb).unwrap().family, "DejaVu Sans");
}

/// Sets the first cmap encoding record of an sfnt from (3,1) to (3,0).
fn make_symbol_cmap(mut data: Vec<u8>) -> Vec<u8> {
    let n = u16::from_be_bytes([data[4], data[5]]) as usize;
    for i in 0..n {
        let rec = 12 + i * 16;
        if &data[rec..rec + 4] == b"cmap" {
            let off = u32::from_be_bytes(data[rec + 8..rec + 12].try_into().unwrap()) as usize;
            let count = u16::from_be_bytes([data[off + 2], data[off + 3]]) as usize;
            for k in 0..count {
                let r = off + 4 + k * 8;
                if data[r..r + 4] == [0, 3, 0, 1] {
                    data[r + 3] = 0;
                }
            }
        }
    }
    data
}

#[test]
fn symbolic_truetype_reuses_codes() {
    let Some(ttf) = system(DEJAVU) else { return };
    let font = pagebara_font::Font::parse(ttf.clone()).unwrap();
    let s = font.as_sfnt().unwrap();
    let greek = ['α', 'β', 'γ'];
    let cmap: Vec<(char, u16)> = greek
        .iter()
        .enumerate()
        .map(|(i, c)| {
            (
                char::from_u32(0xF061 + i as u32).unwrap(),
                s.glyph_for_char(*c).unwrap(),
            )
        })
        .collect();
    let sub = pagebara_font::subset::truetype::subset(s, cmap.iter().map(|c| c.1), &cmap).unwrap();
    let data = make_symbol_cmap(sub.data);
    let len = data.len();
    let tu = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /X def 1 begincodespacerange <00> <FF> endcodespacerange 3 beginbfchar <61> <03B1> <62> <03B2> <63> <03B3> endbfchar endcmap CMapName currentdict /CMap defineresource pop end end".to_vec();
    let fd = descriptor("ABCDEF+DejaVuSans", 4, "FontFile2", 0);
    let (doc, f) = doc_with_font(
        &[(&format!("/Length1 {len}"), data), ("", tu)],
        &|n| {
            let fd = fd.replace("0 0 R", &format!("{} 0 R", n[0]));
            format!(
                "<< /Type /Font /Subtype /TrueType /BaseFont /ABCDEF+DejaVuSans /FirstChar 97 /LastChar 99 \
                 /Widths [634 603 579] /FontDescriptor {fd} /ToUnicode {} 0 R >>",
                n[1]
            )
        },
        b"abc",
    );
    let c = check(doc, f, "αβγ", "γβα δ", true);
    assert_eq!(status_of(&c, 'γ'), CharStatus::Existing);
    assert_eq!(status_of(&c, 'δ'), CharStatus::Fallback);
}

#[test]
fn type1_embedded_gains_codes() {
    let Some(t1) = system(NIMBUS_T1) else { return };
    let (data, [l1, l2, l3]) = type1_file(&t1);
    let (doc, f) = simple_doc(
        "Type1",
        "NimbusSans-Regular",
        &format!("/Length1 {l1} /Length2 {l2} /Length3 {l3}"),
        data,
        "FontFile",
        "Hello",
    );
    let c = check(doc, f, "Hello", "Hello Łódź Ωμέγα Жук", true);
    for ch in ['Ł', 'Ω', 'Ж'] {
        assert_eq!(status_of(&c, ch), CharStatus::Added, "{ch}");
    }
}

#[test]
fn cff_type1c_gains_codes() {
    let Some(otf) = system(NIMBUS_OTF) else {
        return;
    };
    let cff = cff_table(&otf);
    let (doc, f) = simple_doc(
        "Type1",
        "NimbusSans-Regular",
        "/Subtype /Type1C",
        cff,
        "FontFile3",
        "Hello",
    );
    let c = check(doc, f, "Hello", "Hello Łódź Жук", true);
    assert_eq!(status_of(&c, 'Ж'), CharStatus::Added);
}

#[test]
fn opentype_cff_simple_gains_codes() {
    let Some(otf) = system(NIMBUS_OTF) else {
        return;
    };
    let (doc, f) = simple_doc(
        "Type1",
        "NimbusSans-Regular",
        "/Subtype /OpenType",
        otf,
        "FontFile3",
        "Hello",
    );
    let c = check(doc, f, "Hello", "Hello Ţară Жук", true);
    assert_eq!(status_of(&c, 'Ţ'), CharStatus::Added);
}

/// A Type 0 font over the whole program (`Identity-H`, `/W` and `/ToUnicode` for `shown`).
fn type0_full(
    program: Vec<u8>,
    file_dict: &str,
    key: &str,
    cid_type: &str,
    cid_map: Option<Vec<u8>>,
    shown: &str,
) -> (Document, ObjRef) {
    let f = pagebara_font::Font::parse(program.clone()).unwrap();
    let s = f.as_sfnt().unwrap();
    let scale = 1000.0 / f32::from(s.units_per_em());
    let gids: Vec<u16> = shown
        .chars()
        .map(|c| s.glyph_for_char(c).unwrap())
        .collect();
    let codes: Vec<u8> = gids.iter().flat_map(|g| g.to_be_bytes()).collect();
    let w: String = gids
        .iter()
        .map(|&g| {
            let adv = f.advance(g).unwrap() * scale;
            format!("{g} [{}] ", adv.round())
        })
        .collect();
    let mut tu = String::from(
        "/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /X def 1 begincodespacerange <0000> <FFFF> endcodespacerange ",
    );
    tu.push_str(&format!("{} beginbfchar ", gids.len()));
    for (g, c) in gids.iter().zip(shown.chars()) {
        tu.push_str(&format!("<{g:04X}> <{:04X}> ", c as u32));
    }
    tu.push_str("endbfchar endcmap CMapName currentdict /CMap defineresource pop end end");
    let mut extra: Vec<(&str, Vec<u8>)> = vec![(file_dict, program), ("", tu.into_bytes())];
    if let Some(m) = &cid_map {
        extra.push(("", m.clone()));
    }
    let fd = descriptor("Test", 32, key, 0);
    doc_with_font(
        &extra,
        &|n| {
            let fd = fd.replace("0 0 R", &format!("{} 0 R", n[0]));
            let map = if n.len() > 2 {
                format!("{} 0 R", n[2])
            } else {
                "/Identity".into()
            };
            format!(
                "<< /Type /Font /Subtype /Type0 /BaseFont /Test /Encoding /Identity-H /ToUnicode {} 0 R \
                 /DescendantFonts [<< /Type /Font /Subtype /{cid_type} /BaseFont /Test \
                 /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
                 /FontDescriptor {fd} /DW 1000 /W [{w}] /CIDToGIDMap {map} >>] >>",
                n[1]
            )
        },
        &codes,
    )
}

#[test]
fn type0_truetype_identity_gains_cids() {
    let Some(ttf) = system(DEJAVU) else { return };
    let len = ttf.len();
    let (doc, f) = type0_full(
        ttf,
        &format!("/Length1 {len}"),
        "FontFile2",
        "CIDFontType2",
        None,
        "Hello",
    );
    let c = check(doc, f, "Hello", "Hello Жук ω", true);
    assert_eq!(status_of(&c, 'Ж'), CharStatus::Added);
    assert_eq!(status_of(&c, 'l'), CharStatus::Existing);
}

#[test]
fn type0_truetype_cid_map_extends() {
    let Some(ttf) = system(DEJAVU) else { return };
    let len = ttf.len();
    let map: Vec<u8> = (0u16..300).flat_map(|g| g.to_be_bytes()).collect();
    let (doc, f) = type0_full(
        ttf,
        &format!("/Length1 {len}"),
        "FontFile2",
        "CIDFontType2",
        Some(map),
        "Hello",
    );
    let c = check(doc, f, "Hello", "Hello Жук", true);
    assert_eq!(status_of(&c, 'Ж'), CharStatus::Added);
    // Ж (glyph > 300) got a CID past the old map.
    let zhe = c
        .enc
        .glyphs()
        .find(|(_, g)| c.enc.text[g.cluster.clone()] == *"Ж")
        .unwrap()
        .1
        .code;
    assert!(zhe >= 300, "{zhe}");
}

#[test]
fn type0_opentype_cff_gains_cids() {
    let Some(otf) = system(NIMBUS_OTF) else {
        return;
    };
    let (doc, f) = type0_full(
        otf,
        "/Subtype /OpenType",
        "FontFile3",
        "CIDFontType0",
        None,
        "Hello",
    );
    let c = check(doc, f, "Hello", "Hello Жук", true);
    assert_eq!(status_of(&c, 'Ж'), CharStatus::Added);
}

/// A subset Type 0 font written by `pagebara-font`'s embedder.
fn type0_subset(path: &str, shown: &str) -> Option<(Document, ObjRef, Vec<u8>)> {
    let data = system(path)?;
    let mut p = Pdf::new();
    p.page("", "");
    let mut doc = Document::open(p.finish()).unwrap();
    let mut emb = pagebara_font::subset::Embedder::new(data, 0).unwrap();
    let codes = emb.encode(shown);
    let f = emb.write(&mut doc).unwrap();
    let tree = pagebara_pages::PageTree::load(&doc);
    let pr = tree.pages()[0].r;
    let mut pd = doc.get(pr).unwrap().as_dict().unwrap().clone();
    let mut fonts = pagebara_syntax::Dict::new();
    fonts.set(b"F1", Object::Ref(f));
    let mut res = pagebara_syntax::Dict::new();
    res.set(b"Font", Object::Dict(fonts));
    pd.set(b"Resources", Object::Dict(res));
    let c = doc.add(pagebara_syntax::Stream::new(
        pagebara_syntax::Dict::new(),
        format!("BT /F1 16 Tf 20 250 Td <{}> Tj ET\n", hex(&codes)).into_bytes(),
    ));
    pd.set(b"Contents", Object::Ref(c));
    doc.set(pr, pd);
    let doc = Document::open(doc.save_full().unwrap()).unwrap();
    Some((doc, f, codes))
}

#[test]
fn type0_truetype_subset_falls_back() {
    let Some((doc, f, _)) = type0_subset(DEJAVU, "Hello") else {
        return;
    };
    let c = check(doc, f, "Hello", "Hello Wörld", true);
    assert_eq!(status_of(&c, 'o'), CharStatus::Existing);
    assert_eq!(status_of(&c, 'W'), CharStatus::Fallback);
}

#[test]
fn type0_cff_subset_falls_back() {
    let Some((doc, f, _)) = type0_subset(NIMBUS_OTF, "Hello") else {
        return;
    };
    let c = check(doc, f, "Hello", "Hello Wörld Жук", true);
    assert_eq!(status_of(&c, 'H'), CharStatus::Existing);
    assert_eq!(status_of(&c, 'Ж'), CharStatus::Fallback);
}

fn standard_doc(name: &str, encoding: &str, original: &str) -> (Document, ObjRef) {
    doc_with_font(
        &[],
        &|_| format!("<< /Type /Font /Subtype /Type1 /BaseFont /{name} {encoding} >>"),
        original.as_bytes(),
    )
}

#[test]
fn standard_helvetica_winansi() {
    let (doc, f) = standard_doc("Helvetica", "/Encoding /WinAnsiEncoding", "Hello");
    let c = check(doc, f, "Hello", "Łódź naïve € Ωmega", true);
    assert_eq!(status_of(&c, 'ó'), CharStatus::Existing);
    assert_eq!(status_of(&c, '€'), CharStatus::Existing);
    assert_eq!(status_of(&c, 'Ł'), CharStatus::Added);
    assert_eq!(status_of(&c, 'ź'), CharStatus::Added);
    assert_eq!(status_of(&c, 'Ω'), CharStatus::Fallback);
    // AFM widths: Helvetica 'Ł' (Lslash) is 556, 'm' 833.
    let w = |ch: &str| {
        c.enc
            .glyphs()
            .find(|(_, g)| c.enc.text[g.cluster.clone()] == *ch)
            .unwrap()
            .1
            .advance
    };
    assert!((w("Ł") - 0.556).abs() < 1e-4);
    assert!((w("m") - 0.833).abs() < 1e-4);
}

#[test]
fn standard_times_standard_encoding() {
    let (doc, f) = standard_doc("Times-Roman", "", "Hello");
    let c = check(doc, f, "Hello", "Café résumé", true);
    assert_eq!(status_of(&c, 'C'), CharStatus::Existing);
    assert_eq!(status_of(&c, 'é'), CharStatus::Added);
}

/// Non-embedded Courier: the substitute font may not render on every system, so only
/// extraction, widths and the external checks are verified.
#[test]
fn standard_courier() {
    let (doc, f) = standard_doc("Courier", "/Encoding /WinAnsiEncoding", "Hello");
    let c = check(doc, f, "Hello", "Œuvre ŁÓDŹ", false);
    assert_eq!(status_of(&c, 'Œ'), CharStatus::Existing);
    assert_eq!(status_of(&c, 'Ł'), CharStatus::Added);
    assert!(
        c.enc
            .glyphs()
            .all(|(_, g)| (g.advance - 0.6).abs() < 1e-6 || g.code_len == 2)
    );
}

/// Helvetica with `/Widths` for ASCII only: WinAnsi characters outside them keep their
/// code and gain a width; new codes are consistent across calls.
#[test]
fn standard_with_partial_widths() {
    let widths = "278 ".repeat(95);
    let (doc, f) = standard_doc(
        "Helvetica",
        &format!("/Encoding /WinAnsiEncoding /FirstChar 32 /LastChar 126 /Widths [{widths}]"),
        "Hello",
    );
    let mut s = FontSession::new(Some(db()));
    let id = s.open_font(&doc, f).unwrap();
    let a = s.encode(id, "éŁ");
    let b = s.encode(id, "Łé");
    assert_eq!(a.runs[0].glyphs[0].code, 233);
    assert_eq!(a.runs[0].glyphs[1].code, b.runs[0].glyphs[0].code);
    assert!((a.runs[0].glyphs[0].advance - 0.556).abs() < 1e-6);
    let c = check(doc, f, "Hello", "Été Łąka", true);
    assert_eq!(status_of(&c, 'é'), CharStatus::Added);
}

/// A non-embedded CJK font with a predefined CMap: existing codes only.
#[test]
fn predefined_cmap_cjk() {
    let (doc, f) = doc_with_font(
        &[],
        &|_| {
            "<< /Type /Font /Subtype /Type0 /BaseFont /STSong-Light /Encoding /UniGB-UCS2-H \
             /DescendantFonts [<< /Type /Font /Subtype /CIDFontType0 /BaseFont /STSong-Light \
             /CIDSystemInfo << /Registry (Adobe) /Ordering (GB1) /Supplement 4 >> /DW 1000 \
             /FontDescriptor << /Type /FontDescriptor /FontName /STSong-Light /Flags 6 \
             /FontBBox [0 -200 1000 900] /ItalicAngle 0 /Ascent 880 /Descent -120 /CapHeight 700 /StemV 80 >> >>] >>"
                .into()
        },
        &[0x4E, 0x2D],
    );
    let mut s = FontSession::new(Some(db()));
    let id = s.open_font(&doc, f).unwrap();
    s.options.fallback = false;
    let e = s.encode(id, "中文");
    assert!(e.is_complete());
    assert_eq!(e.runs[0].bytes(), [0x4E, 0x2D, 0x65, 0x87]);
    let cov = s.coverage(id, "中文");
    assert!(cov.iter().all(|c| c.status == CharStatus::Existing));
    let render_ok = db().faces().iter().any(|f| f.family.contains("CJK"));
    check(doc, f, "中", "中文", render_ok);
}

#[test]
fn type3_reuses_glyphs_only() {
    let proc_a = b"600 0 0 0 500 700 d1 50 0 400 700 re f".to_vec();
    let proc_b = b"700 0 0 0 600 700 d1 50 0 500 350 re f".to_vec();
    let (doc, f) = doc_with_font(
        &[("", proc_a), ("", proc_b)],
        &|n| {
            format!(
                "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] /FontMatrix [0.001 0 0 0.001 0 0] \
                 /CharProcs << /a {} 0 R /b {} 0 R >> /Encoding << /Type /Encoding /Differences [97 /a /b] >> \
                 /FirstChar 97 /LastChar 98 /Widths [600 700] /Resources << >> >>",
                n[0], n[1]
            )
        },
        b"ab",
    );
    let mut s = FontSession::new(Some(db()));
    let id = s.open_font(&doc, f).unwrap();
    s.options.fallback = false;
    let e = s.encode(id, "abc");
    assert_eq!(e.missing, [(2, 'c')]);
    assert_eq!(e.runs[0].bytes(), b"ab");
    assert!((e.runs[0].glyphs[1].advance - 0.7).abs() < 1e-6);
    let c = check(doc, f, "ab", "abba c", true);
    assert_eq!(status_of(&c, 'b'), CharStatus::Existing);
    assert_eq!(status_of(&c, 'c'), CharStatus::Fallback);
}

#[test]
fn cjk_falls_back_to_cjk_face() {
    let (doc, f) = standard_doc("Helvetica", "/Encoding /WinAnsiEncoding", "Hello");
    let mut s = FontSession::new(Some(db()));
    let id = s.open_font(&doc, f).unwrap();
    if s.coverage(id, "漢")[0].status == pagebara_typeset::CharStatus::Unsupported {
        eprintln!("skipped: no CJK font installed");
        return;
    }
    let c = check(doc, f, "Hello", "漢字テスト", true);
    assert!(c.statuses.iter().all(|s| s.1 == CharStatus::Fallback));
}

#[test]
fn decomposed_input_is_normalized() {
    let (doc, f) = standard_doc("Helvetica", "/Encoding /WinAnsiEncoding", "Hello");
    let mut s = FontSession::new(Some(db()));
    let id = s.open_font(&doc, f).unwrap();
    let e = s.encode(id, "Cafe\u{301}");
    assert_eq!(e.text, "Café");
    assert_eq!(e.runs.len(), 1);
    assert_eq!(e.runs[0].bytes(), b"Caf\xE9");
}

#[test]
fn fallback_kerning_becomes_tj() {
    let (doc, f) = standard_doc("Helvetica", "/Encoding /WinAnsiEncoding", "Hello");
    let mut s = FontSession::new(Some(db()));
    let id = s.open_font(&doc, f).unwrap();
    let Some(face) = db()
        .faces()
        .iter()
        .find(|f| f.path.ends_with("DejaVuSans.ttf"))
        .cloned()
    else {
        eprintln!("skipped: DejaVu Sans not installed");
        return;
    };
    let dv = s.open_face(&face).unwrap();
    let e = s.encode(dv, "AVA");
    assert!(e.runs[0].glyphs[0].kern < 0.0, "{:?}", e.runs[0].glyphs);
    assert!(e.runs[0].tj_operand().contains('>'));
    let _ = id;
    let mut doc = doc;
    let changes = s.finish(&mut doc, WriteMode::InPlace);
    let file = add_to_page(&mut doc, &changes, &show(&e, 20.0, 200.0, 16.0));
    let doc = Document::open(file.clone()).unwrap();
    let chars = line_chars(&page_text(&doc), 200.0);
    let x1 = 20.0 + (e.runs[0].glyphs[0].advance + e.runs[0].glyphs[0].kern) * 16.0;
    assert!((chars[1].0 - x1).abs() < 0.01, "{chars:?} vs {x1}");
    external_checks(&file, &["AVA"]);
}

#[test]
fn shared_font_is_copied_on_write() {
    let mut p = Pdf::new();
    let f =
        p.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    p.page(
        &format!("/Font << /F1 {f} 0 R >>"),
        "BT /F1 16 Tf 20 250 Td (Hello) Tj ET\n",
    );
    p.page(
        &format!("/Font << /F1 {f} 0 R >>"),
        "BT /F1 16 Tf 20 250 Td (Other) Tj ET\n",
    );
    let mut doc = Document::open(p.finish()).unwrap();
    let fr = doc.reference(f as u32).unwrap();
    let before = doc.get(fr).unwrap();
    let mut s = FontSession::new(Some(db()));
    let id = s.open_font(&doc, fr).unwrap();
    let e = s.encode(id, "Ł");
    assert_eq!(e.runs[0].font, RunFont::Font(id));
    let ch = s.finish(&mut doc, WriteMode::CopyIfShared);
    assert_eq!(ch.fonts.len(), 1);
    assert!(ch.fonts[0].copied);
    assert_ne!(ch.fonts[0].object, fr);
    assert_eq!(doc.get(fr).unwrap(), before);
    let file = add_to_page(&mut doc, &ch, &show(&e, 20.0, 200.0, 16.0));
    let doc = Document::open(file.clone()).unwrap();
    let got: String = line_chars(&page_text(&doc), 200.0)
        .into_iter()
        .map(|c| c.2)
        .collect();
    assert_eq!(got, "Ł");
    external_checks(&file, &["Ł"]);
}
