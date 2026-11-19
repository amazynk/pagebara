//! Paragraph editing in embedded TrueType, CFF, Type 0 and standard fonts, tagged content,
//! rotated pages and fonts shared between pages.

mod common;

use common::*;
use pagebara_content::Rect;
use pagebara_edit::{
    Align, Editor, Fonts, ObjectId, PageObjects, SpanFont, SpanStyle, StyledText, TextFrame,
};
use pagebara_syntax::Document;

const H: f32 = 200.0;
const DEJAVU: &str = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf";
const NIMBUS_OTF: &str = "/usr/share/fonts/opentype/urw-base35/NimbusSans-Regular.otf";

fn block(objs: &PageObjects, s: &str) -> ObjectId {
    objs.objects
        .iter()
        .find(|o| o.text().is_some_and(|t| t.text.contains(s)))
        .expect("block")
        .id
}

fn lines_of(doc: &Document, s: &str) -> Vec<String> {
    let objs = pagebara_edit::parse(doc, 0).unwrap();
    let b = block(&objs, s);
    objs.get(b)
        .unwrap()
        .text()
        .unwrap()
        .lines
        .iter()
        .map(|l| l.text.trim().to_string())
        .collect()
}

fn system(path: &str) -> Option<Vec<u8>> {
    let d = std::fs::read(path).ok();
    if d.is_none() {
        eprintln!("skipped: {path} not installed");
    }
    d
}

fn finish(doc: &Document, texts: &[&str]) -> Document {
    let (d, file) = reopen(doc);
    external_checks(&file, texts);
    if let Some(pt) = pdftotext(&file) {
        for t in texts {
            assert!(flat(&pt).contains(t), "pdftotext lacks {t:?}: {pt}");
        }
    }
    d
}

/// Program widths (thousandths) of WinAnsi codes 32..=126.
fn ascii_widths(program: &[u8]) -> String {
    let f = pagebara_font::Font::parse(program.to_vec()).unwrap();
    let m = f.font_matrix();
    (32u8..=126)
        .map(|c| {
            let g = f.glyph_for_char(char::from(c)).unwrap_or(0);
            let w = f.advance(g).unwrap_or(0.0);
            format!(
                "{} ",
                ((m.apply(w, 0.0).0 - m.apply(0.0, 0.0).0) * 1000.0).round()
            )
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    TrueType,
    Cff,
    Type0,
    Standard,
}

fn descriptor(name: &str, key: &str, file: usize) -> String {
    format!(
        "<< /Type /FontDescriptor /FontName /{name} /Flags 32 /FontBBox [-1000 -300 2000 1100] \
         /ItalicAngle 0 /Ascent 900 /Descent -230 /CapHeight 700 /StemV 80 /{key} {file} 0 R >>"
    )
}

/// A page with font `/F1` of `kind` and a paragraph set in it (by the engine itself, in
/// codes the font has or gains), plus a line in Helvetica below. `None` when the font is
/// not installed.
fn embedded_doc(kind: Kind, text: &str, align: Align) -> Option<Document> {
    let mut p = Pdf::new();
    let font = match kind {
        Kind::TrueType | Kind::Type0 => system(DEJAVU)?,
        Kind::Cff => system(NIMBUS_OTF)?,
        Kind::Standard => Vec::new(),
    };
    let f = match kind {
        Kind::TrueType => {
            let file = p.stream(&format!("/Length1 {}", font.len()), &font);
            let w = ascii_widths(&font);
            p.add(format!(
                "<< /Type /Font /Subtype /TrueType /BaseFont /DejaVuSans /FirstChar 32 /LastChar 126 \
                 /Widths [{w}] /Encoding /WinAnsiEncoding /FontDescriptor {} >>",
                descriptor("DejaVuSans", "FontFile2", file)
            ))
        }
        Kind::Cff => {
            let file = p.stream("/Subtype /OpenType", &font);
            let w = ascii_widths(&font);
            p.add(format!(
                "<< /Type /Font /Subtype /Type1 /BaseFont /NimbusSans-Regular /FirstChar 32 /LastChar 126 \
                 /Widths [{w}] /Encoding /WinAnsiEncoding /FontDescriptor {} >>",
                descriptor("NimbusSans-Regular", "FontFile3", file)
            ))
        }
        Kind::Type0 => {
            let file = p.stream(&format!("/Length1 {}", font.len()), &font);
            let tu = p.stream(
                "",
                b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /X def \
                  1 begincodespacerange <0000> <FFFF> endcodespacerange endcmap CMapName currentdict \
                  /CMap defineresource pop end end",
            );
            p.add(format!(
                "<< /Type /Font /Subtype /Type0 /BaseFont /DejaVuSans /Encoding /Identity-H /ToUnicode {tu} 0 R \
                 /DescendantFonts [<< /Type /Font /Subtype /CIDFontType2 /BaseFont /DejaVuSans \
                 /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> \
                 /FontDescriptor {} /DW 1000 /W [] /CIDToGIDMap /Identity >>] >>",
                descriptor("DejaVuSans", "FontFile2", file)
            ))
        }
        Kind::Standard => p.add(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman /Encoding /WinAnsiEncoding >>",
        ),
    };
    p.page(
        &format!(
            "/Font << /F1 {f} 0 R /F2 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>"
        ),
        "BT /F2 8 Tf 20 20 Td (Below the paragraph) Tj ET\n",
        "",
    );
    let mut doc = open(p.finish());
    let r = doc.reference(f as u32).unwrap();
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let mut frame = TextFrame::new_box(&objs, Rect::new(20.0, 60.0, 270.0, 185.0));
    frame.align = align;
    let style = SpanStyle::new(SpanFont::Pdf(pagebara_syntax::Object::Ref(r)), 10.0);
    let spans = vec![StyledText {
        text: text.into(),
        style,
    }];
    let mut f = Fonts::new(None);
    let mut e = Editor::open(&mut doc, 0).unwrap();
    e.add_text_box(&frame, &spans, &mut f).unwrap();
    e.commit().unwrap();
    Some(reopen(&doc).0)
}

const LONG: &str = "Typesetting keeps the font of the paragraph, adds the glyphs it lacks and \
                    falls back to a matching face only for what the font cannot show at all.";

fn pdftotext(file: &[u8]) -> Option<String> {
    let dir = std::env::temp_dir().join(format!("pagebara-para-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let path = dir.join(format!(
        "p{}.pdf",
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&path, file).ok()?;
    let o = std::process::Command::new("pdftotext")
        .arg(&path)
        .arg("-")
        .output()
        .ok()?;
    let _ = std::fs::remove_file(&path);
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn edits_in_each_font_kind() {
    let cases = [
        (
            Align::Left,
            "Typesetting now keeps the font of the edited paragraph: Łódź, Ωμέγα and Жук.",
        ),
        (Align::Justify, "Short."),
        (Align::Center, LONG),
    ];
    for kind in [Kind::TrueType, Kind::Cff, Kind::Type0, Kind::Standard] {
        for (align, edit) in cases {
            let Some(doc) = embedded_doc(kind, LONG, align) else {
                return;
            };
            let before = render(&doc, 0);
            let objs = pagebara_edit::parse(&doc, 0).unwrap();
            let b = block(&objs, "Typesetting");
            let bt = pagebara_edit::block_text(&objs, b).unwrap();
            assert_eq!(flat(&bt.text()), flat(LONG), "{kind:?} {align:?}");
            let bbox = objs.get(b).unwrap().bbox;
            let mut d = doc.clone();
            let mut f = Fonts::system();
            let mut e = Editor::open(&mut d, 0).unwrap();
            let rep = e.edit_block_text(b, edit, &mut f).unwrap();
            e.commit().unwrap();
            let (d2, file) = reopen(&d);
            external_checks(&file, &["Below the paragraph"]);
            let t = flat(&page_text(&d2, 0));
            assert!(t.contains(edit), "{kind:?} {align:?}: {t}");
            if let Some(pt) = pdftotext(&file) {
                let first_word = edit.split(' ').next().unwrap();
                assert!(
                    flat(&pt).contains(first_word),
                    "pdftotext {kind:?} {align:?}: {pt}"
                );
                if align == Align::Left {
                    assert!(flat(&pt).contains("Łódź, Ωμέγα and Жук."), "{kind:?}: {pt}");
                }
            }
            // The paragraph's own font draws the text.
            let objs = pagebara_edit::parse(&d2, 0).unwrap();
            let first = objs
                .runs
                .iter()
                .find(|r| r.text.starts_with(&edit[..5]))
                .unwrap();
            let base = pagebara_typeset::font_style(
                &d2,
                d2.resolve(&first.font.as_ref().unwrap().object)
                    .as_dict()
                    .unwrap(),
            );
            let expect = match kind {
                Kind::Standard => "Times",
                Kind::Cff => "Nimbus",
                _ => "DejaVu",
            };
            assert!(base.family.contains(expect), "{kind:?}: {base:?}");
            // Nothing outside the paragraph changed.
            let after = render(&d2, 0);
            let area = Rect::new(0.0, rep.bbox.y0.min(bbox.y0) - 2.0, 300.0, bbox.y1 + 3.0);
            assert_eq!(
                diff_outside(&before, &after, &[device_box(&area, H, 3)], 0),
                0,
                "{kind:?} {align:?}"
            );
            if align == Align::Center {
                // The same text again breaks into the same lines.
                assert_eq!(
                    lines_of(&d2, "Typesetting"),
                    lines_of(&doc, "Typesetting"),
                    "{kind:?}"
                );
            }
        }
    }
}

#[test]
fn indented_paragraphs_keep_indent_and_breaks() {
    let mut p = Pdf::new();
    let c = "BT /F1 10 Tf 12 TL 40 170 Td (First paragraph starts indented, and it then) Tj \
             -20 -12 Td (wraps onto a second line here.) Tj T* (Second paragraph follows right after the first one) Tj \
             T* (and ends.) Tj ET\n";
    p.page(HELV, c, "");
    let doc = open(p.finish());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let b = block(&objs, "First paragraph");
    let bt = pagebara_edit::block_text(&objs, b).unwrap();
    assert!((bt.frame.first_indent - 20.0).abs() < 0.5, "{:?}", bt.frame);
    let text = bt.text();
    assert_eq!(
        text,
        "First paragraph starts indented, and it then wraps onto a second line here.\nSecond paragraph follows right after the first one and ends."
    );
    let mut d = doc.clone();
    let mut f = Fonts::system();
    let mut e = Editor::open(&mut d, 0).unwrap();
    let new = text.replace("First", "The first");
    e.edit_block_text(b, &new, &mut f).unwrap();
    e.commit().unwrap();
    let d = finish(&d, &["The first paragraph"]);
    let objs = pagebara_edit::parse(&d, 0).unwrap();
    let r = objs
        .runs
        .iter()
        .find(|r| r.text.starts_with("The first"))
        .unwrap();
    assert!((r.glyphs[0].origin.x - 40.0).abs() < 0.1);
    let r = objs
        .runs
        .iter()
        .find(|r| r.text.starts_with("Second"))
        .unwrap();
    assert!(
        (r.glyphs[0].origin.x - 20.0).abs() < 0.1,
        "{:?}",
        r.glyphs[0].origin
    );
}

#[test]
fn tagged_paragraph_keeps_its_mcid() {
    let mut p = Pdf::new();
    let c = "/P <</MCID 0>> BDC BT /F1 10 Tf 12 TL 20 150 Td (Tagged text in a) Tj T* (structure element.) Tj ET EMC\n\
             /P <</MCID 1>> BDC BT /F1 10 Tf 20 100 Td (Other paragraph) Tj ET EMC\n";
    let root = p.reserve();
    let e0 = p.reserve();
    let e1 = p.reserve();
    let page = p.page(HELV, c, "/StructParents 0");
    p.set(
        e0,
        format!(
            "<< /Type /StructElem /S /P /P {root} 0 R /Pg {page} 0 R /K 0 /ActualText (Tagged text in a structure element.) >>"
        ),
    );
    p.set(
        e1,
        format!("<< /Type /StructElem /S /P /P {root} 0 R /Pg {page} 0 R /K 1 >>"),
    );
    p.set(
        root,
        format!("<< /Type /StructTreeRoot /K [{e0} 0 R {e1} 0 R] >>"),
    );
    p.catalog = format!("/StructTreeRoot {root} 0 R /MarkInfo << /Marked true >>");
    let doc = open(p.finish());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let b = block(&objs, "Tagged");
    let mut d = doc.clone();
    let mut f = Fonts::system();
    let mut e = Editor::open(&mut d, 0).unwrap();
    e.edit_block_text(b, "Edited tagged text.", &mut f).unwrap();
    e.commit().unwrap();
    let d = finish(&d, &["Edited tagged text.", "Other paragraph"]);
    let objs = pagebara_edit::parse(&d, 0).unwrap();
    let r = objs
        .runs
        .iter()
        .find(|r| r.text.contains("Edited"))
        .unwrap();
    assert_eq!(r.marked.last().and_then(|m| m.mcid), Some(0));
    let el = d.get(d.reference(e0 as u32).unwrap()).unwrap();
    let at = d.get_in(el.as_dict().unwrap(), b"ActualText");
    assert_eq!(at.as_string().unwrap(), b"Edited tagged text.");
}

#[test]
fn rotated_text_on_a_rotated_page() {
    let mut p = Pdf::new();
    let c = "BT /F1 10 Tf 0 1 -1 0 150 20 Tm 12 TL (Text running up the page) Tj T* (in two lines.) Tj ET\n";
    p.page(HELV, c, "/Rotate 90");
    let doc = open(p.finish());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let b = block(&objs, "running up");
    let bt = pagebara_edit::block_text(&objs, b).unwrap();
    assert!(
        (bt.frame.matrix.b - 1.0).abs() < 1e-4,
        "{:?}",
        bt.frame.matrix
    );
    let mut d = doc.clone();
    let mut f = Fonts::system();
    let mut e = Editor::open(&mut d, 0).unwrap();
    e.edit_block_text(b, "Text still running up the page.", &mut f)
        .unwrap();
    e.commit().unwrap();
    let d = finish(&d, &["still running"]);
    let objs = pagebara_edit::parse(&d, 0).unwrap();
    let r = objs
        .runs
        .iter()
        .find(|r| r.text.starts_with("Text still"))
        .unwrap();
    let o = r.glyphs[0].origin;
    assert!(
        (o.x - 150.0).abs() < 0.1 && (o.y - 20.0).abs() < 0.1,
        "{o:?}"
    );
    let m = r.matrix();
    assert!(m.a.abs() < 1e-4 && (m.b - 1.0).abs() < 1e-4, "{m:?}");
    // A new box on the rotated page reads upright as displayed.
    let mut d2 = d.clone();
    let mut e = Editor::open(&mut d2, 0).unwrap();
    let frame = TextFrame::new_box(e.objects(), Rect::new(200.0, 20.0, 280.0, 180.0));
    let style = SpanStyle::new(SpanFont::helvetica(), 10.0);
    e.add_text_box(
        &frame,
        &[StyledText {
            text: "Upright".into(),
            style,
        }],
        &mut f,
    )
    .unwrap();
    e.commit().unwrap();
    let d2 = finish(&d2, &["Upright"]);
    let objs = pagebara_edit::parse(&d2, 0).unwrap();
    let r = objs.runs.iter().find(|r| r.text == "Upright").unwrap();
    let up = objs.frame.to_view().apply_vector(
        r.matrix()
            .apply_vector(pagebara_content::Point::new(1.0, 0.0)),
    );
    assert!(up.x > 0.99 && up.y.abs() < 1e-3, "{up:?}");
}

#[test]
fn shared_font_is_copied_not_changed() {
    let Some(ttf) = system(DEJAVU) else { return };
    let mut p = Pdf::new();
    let file = p.stream(&format!("/Length1 {}", ttf.len()), &ttf);
    let w = ascii_widths(&ttf);
    let f = p.add(format!(
        "<< /Type /Font /Subtype /TrueType /BaseFont /DejaVuSans /FirstChar 32 /LastChar 126 \
         /Widths [{w}] /Encoding /WinAnsiEncoding /FontDescriptor {} >>",
        descriptor("DejaVuSans", "FontFile2", file)
    ));
    let res = format!("/Font << /F1 {f} 0 R >>");
    p.page(&res, "BT /F1 10 Tf 20 150 Td (Page one text) Tj ET\n", "");
    p.page(&res, "BT /F1 10 Tf 20 150 Td (Page two text) Tj ET\n", "");
    let doc = open(p.finish());
    let fr = doc.reference(f as u32).unwrap();
    let font_before = doc.get(fr).unwrap();
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let b = block(&objs, "Page one");
    let mut d = doc.clone();
    let mut fonts = Fonts::system();
    let mut e = Editor::open(&mut d, 0).unwrap();
    e.edit_block_text(b, "Page one Ωμέγα", &mut fonts).unwrap();
    e.commit().unwrap();
    assert_eq!(d.get(fr).unwrap(), font_before);
    let d = finish(&d, &["Page one", "Page two text"]);
    assert!(page_text(&d, 0).contains("Page one Ωμέγα"));
    let objs = pagebara_edit::parse(&d, 0).unwrap();
    for r in objs.runs.iter().filter(|r| r.text.contains('Ω')) {
        let s = pagebara_typeset::font_style(
            &d,
            d.resolve(&r.font.as_ref().unwrap().object)
                .as_dict()
                .unwrap(),
        );
        assert!(s.family.contains("DejaVu"), "{s:?}");
    }
}
