//! Paragraph editing: blocks read as paragraphs, edited and laid out again in their box,
//! in each kind of font, saved, read back and checked.

mod common;

use common::*;
use pagebara_content::Rect;
use pagebara_edit::{
    Align, Editor, Fonts, ObjectId, PageObjects, SpanFont, SpanStyle, StyleChange, StyledText,
    TextFrame,
};
use pagebara_syntax::Document;

const H: f32 = 200.0;

fn block(objs: &PageObjects, s: &str) -> ObjectId {
    objs.objects
        .iter()
        .find(|o| o.text().is_some_and(|t| t.text.contains(s)))
        .expect("block")
        .id
}

fn fonts() -> Fonts {
    Fonts::system()
}

fn finish(doc: &Document, texts: &[&str]) -> Document {
    let (d, file) = reopen(doc);
    if let Ok(dir) = std::env::var("EDIT_DUMP") {
        let name = std::thread::current()
            .name()
            .unwrap_or("t")
            .replace("::", "-");
        let _ = std::fs::write(
            std::path::Path::new(&dir).join(format!("{name}.pdf")),
            &file,
        );
    }
    external_checks(&file, texts);
    d
}

/// Lines of the block containing `s`, as the object model reads them.
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

const PARA: [&str; 4] = [
    "The quick brown fox jumps over the lazy",
    "dog. Pack my box with five dozen liquor",
    "jugs. How vexingly quick daft zebras",
    "jump!",
];

/// A left-aligned Helvetica paragraph with a heading above and a footer line below.
fn helvetica_doc() -> Vec<u8> {
    let mut p = Pdf::new();
    let mut c = String::from("BT /F1 14 Tf 20 180 Td (Heading) Tj ET\n");
    c.push_str("BT /F1 10 Tf 12 TL 20 150 Td\n");
    for (i, l) in PARA.iter().enumerate() {
        if i > 0 {
            c.push_str("T* ");
        }
        c.push_str(&format!("({l}) Tj\n"));
    }
    c.push_str("ET\nBT /F1 8 Tf 20 20 Td (Footer stays put) Tj ET\n");
    p.page(HELV, &c, "");
    p.finish()
}

#[test]
fn reads_a_paragraph() {
    let doc = open(helvetica_doc());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let b = block(&objs, "quick brown");
    let bt = pagebara_edit::block_text(&objs, b).unwrap();
    assert_eq!(bt.text(), PARA.join(" "));
    assert_eq!(bt.frame.align, Align::Left);
    assert!((bt.frame.left - 20.0).abs() < 0.5, "{:?}", bt.frame);
    assert!((bt.frame.line_factor * 10.0 - 12.0).abs() < 0.1);
}

/// A block's language comes from its marked content, its structure element or the catalog,
/// and its hyphenated words are joined by that language's patterns.
#[test]
fn language_of_text() {
    let lang_of = |content: &str, catalog: &str, tagged: bool| {
        let mut p = Pdf::new();
        p.catalog = catalog.into();
        let extra = if tagged {
            let root = p.reserve();
            let elem = p.add(format!(
                "<< /Type /StructElem /S /P /P {root} 0 R /Lang (fr-CA) >>"
            ));
            p.set(
                root,
                format!("<< /Type /StructTreeRoot /K {elem} 0 R /ParentTree << /Nums [0 [{elem} 0 R]] >> >>"),
            );
            p.catalog += &format!(" /StructTreeRoot {root} 0 R");
            "/StructParents 0"
        } else {
            ""
        };
        p.page(HELV, content, extra);
        let doc = open(p.finish());
        let objs = pagebara_edit::parse(&doc, 0).unwrap();
        let bt = pagebara_edit::block_text(&objs, block(&objs, "Silben")).unwrap();
        (bt.frame.lang.clone(), bt.text())
    };
    let lines = "BT /F1 10 Tf 12 TL 20 150 Td (Die Silben-) Tj T* (trennung ist gut.) Tj ET";
    assert_eq!(lang_of(lines, "", false).0, None);
    let (lang, text) = lang_of(lines, "/Lang (de-DE)", false);
    assert_eq!(lang.as_deref(), Some("de-DE"));
    assert_eq!(text, "Die Silbentrennung ist gut.");
    // A language without patterns keeps the hyphen.
    let (_, text) = lang_of(lines, "/Lang (ja)", false);
    assert_eq!(text, "Die Silben-trennung ist gut.");
    let marked = format!("/P << /MCID 0 >> BDC {lines} EMC");
    let (lang, _) = lang_of(&marked, "/Lang (de)", true);
    assert_eq!(lang.as_deref(), Some("fr-CA"));
    let span = format!("/Span << /Lang (uk) >> BDC {marked} EMC");
    assert_eq!(lang_of(&span, "", true).0.as_deref(), Some("fr-CA"));
    let inner = format!("/P << /MCID 0 >> BDC /Span << /Lang (uk) >> BDC {lines} EMC EMC");
    assert_eq!(lang_of(&inner, "", true).0.as_deref(), Some("uk"));
}

/// The paragraphs of a block of 10-point Helvetica lines (indent, text), 12 points apart.
fn paragraphs_of(lines: &[(f32, &str)]) -> Vec<String> {
    let mut p = Pdf::new();
    let mut c = String::new();
    for (i, (indent, l)) in lines.iter().enumerate() {
        let y = 170.0 - 12.0 * i as f32;
        c.push_str(&format!(
            "BT /F1 10 Tf {} {y} Td ({l}) Tj ET\n",
            20.0 + indent
        ));
    }
    p.page(HELV, &c, "");
    let doc = open(p.finish());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let first = lines[0].1.split(' ').next().unwrap();
    let bt = pagebara_edit::block_text(&objs, block(&objs, first)).unwrap();
    bt.text().split('\n').map(str::to_string).collect()
}

/// Ragged-right lines: room for the next word alone is no paragraph break.
#[test]
fn ragged_lines_break_paragraphs_only_on_evidence() {
    // The flyer: "is" would fit after "Entry", yet the sentence goes on.
    let flyer = [
        (0.0, "Join us on Saturday for plants, seeds"),
        (0.0, "and tools from local growers. Entry"),
        (0.0, "is free for members of the garden club."),
    ];
    assert_eq!(
        paragraphs_of(&flyer).len(),
        1,
        "{:?}",
        paragraphs_of(&flyer)
    );
    // Short lines in the middle that end mid-sentence.
    let ragged = [
        (0.0, "Ragged right text is set line by line"),
        (0.0, "and some lines end"),
        (0.0, "early because the author broke them by"),
        (0.0, "hand, while the sentence continues to"),
        (0.0, "the end of the paragraph here."),
    ];
    assert_eq!(
        paragraphs_of(&ragged).len(),
        1,
        "{:?}",
        paragraphs_of(&ragged)
    );
    // A sentence ending well short of the edge, then a new one: two paragraphs.
    let two = [
        (0.0, "The first paragraph is a few words long and"),
        (0.0, "ends here."),
        (0.0, "The second paragraph starts on this line and"),
        (0.0, "keeps going for a while longer than that."),
    ];
    let p = paragraphs_of(&two);
    assert_eq!(p.len(), 2, "{p:?}");
    assert!(p[1].starts_with("The second"), "{p:?}");
    // An indented first line starts a paragraph even without a full stop before it.
    let indented = [
        (0.0, "Items listed without punctuation at the"),
        (0.0, "end of a short line"),
        (15.0, "Then an indented line opens the next one"),
        (0.0, "which carries on below it as usual here."),
    ];
    let p = paragraphs_of(&indented);
    assert_eq!(p.len(), 2, "{p:?}");
    assert!(p[1].starts_with("Then"), "{p:?}");
}

#[test]
fn longer_text_reflows_and_grows() {
    let doc = open(helvetica_doc());
    let before = render(&doc, 0);
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let b = block(&objs, "quick brown");
    let bbox = objs.get(b).unwrap().bbox;
    let text = PARA
        .join(" ")
        .replace("lazy dog", "very lazy and sleepy old dog");
    let mut d = doc.clone();
    let mut f = fonts();
    let mut e = Editor::open(&mut d, 0).unwrap();
    let rep = e.edit_block_text(b, &text, &mut f).unwrap();
    e.commit().unwrap();
    assert!(rep.lines >= 4, "{rep:?}");
    let d = finish(&d, &["sleepy old dog", "Footer stays put"]);
    let lines = lines_of(&d, "sleepy");
    assert_eq!(lines.join(" "), text, "{lines:?}");
    assert_ne!(lines[0], PARA[0], "{lines:?}");
    // Every line fits the original width.
    let objs = pagebara_edit::parse(&d, 0).unwrap();
    let nb = objs.get(block(&objs, "sleepy")).unwrap();
    assert!(nb.bbox.x1 <= bbox.x1 + 2.0, "{:?} vs {bbox:?}", nb.bbox);
    assert!(
        (nb.bbox.y1 - bbox.y1).abs() < 1.5,
        "{:?} vs {bbox:?}",
        nb.bbox
    );
    // Heading and footer unchanged.
    let after = render(&d, 0);
    let grown = Rect::new(bbox.x0, nb.bbox.y0.min(bbox.y0), 300.0, bbox.y1);
    assert_eq!(
        diff_outside(&before, &after, &[device_box(&grown, H, 3)], 0),
        0
    );
}

#[test]
fn shorter_text_and_undo_is_exact() {
    let doc = open(helvetica_doc());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let b = block(&objs, "quick brown");
    let mut d = doc.clone();
    let mut f = fonts();
    let mut e = Editor::open(&mut d, 0).unwrap();
    let rep = e.edit_block_text(b, "Short now.", &mut f).unwrap();
    e.commit().unwrap();
    assert_eq!(rep.lines, 1);
    assert_eq!(rep.overflow, 0.0);
    let d2 = finish(&d, &["Short now.", "Heading", "Footer stays put"]);
    let t = page_text(&d2, 0);
    assert!(!t.contains("quick"), "{t}");
    // The original document is untouched (edits work on a copy).
    assert_eq!(
        page_streams(&doc, 0),
        page_streams(&open(helvetica_doc()), 0)
    );
}

/// A new text box reads back with bounds covering all of its ink.
#[test]
fn new_text_box_bounds_cover_the_text() {
    let mut p = Pdf::new();
    p.page(HELV, "", "");
    let mut doc = open(p.finish());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let frame = TextFrame::new_box(&objs, Rect::new(40.0, 100.0, 280.0, 120.0));
    let spans = vec![StyledText {
        text: "Hello from pagebara".into(),
        style: SpanStyle::new(SpanFont::helvetica(), 14.0),
    }];
    let mut f = fonts();
    let mut e = Editor::open(&mut doc, 0).unwrap();
    let rep = e.add_text_box(&frame, &spans, &mut f).unwrap();
    e.commit().unwrap();
    let doc = finish(&doc, &["Hello from pagebara"]);
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let b = objs.get(block(&objs, "Hello")).unwrap().bbox;
    let img = render(&doc, 0);
    let (mut x0, mut x1) = (usize::MAX, 0);
    for y in 0..img.1 as usize {
        for x in 0..img.0 as usize {
            if img.2[(y * img.0 as usize + x) * 4] < 128 {
                x0 = x0.min(x);
                x1 = x1.max(x);
            }
        }
    }
    let (ink0, ink1) = (x0 as f32 / 2.0, (x1 + 1) as f32 / 2.0);
    assert!(
        b.x0 <= ink0 + 0.5 && b.x1 >= ink1 - 0.5,
        "{b:?} ink {ink0}..{ink1}"
    );
    assert!(b.x1 - ink1 < 2.0, "{b:?} ink {ink0}..{ink1}");
    assert!(
        (rep.bbox.x1 - b.x1).abs() < 1.0,
        "report {:?} read {b:?}",
        rep.bbox
    );
}

#[test]
fn centered_and_justified_keep_alignment() {
    for (align, name) in [(Align::Center, "center"), (Align::Justify, "justify")] {
        let mut p = Pdf::new();
        p.page(HELV, "BT /F1 9 Tf 10 190 Td (x) Tj ET\n", "");
        let mut doc = open(p.finish());
        let objs = pagebara_edit::parse(&doc, 0).unwrap();
        let mut frame = TextFrame::new_box(&objs, Rect::new(30.0, 60.0, 250.0, 160.0));
        frame.align = align;
        let style = SpanStyle::new(SpanFont::helvetica(), 10.0);
        let spans = vec![StyledText {
            text: if align == Align::Center { "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore." } else { "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua." }.into(),
            style,
        }];
        let mut f = fonts();
        let mut e = Editor::open(&mut doc, 0).unwrap();
        e.add_text_box(&frame, &spans, &mut f).unwrap();
        e.commit().unwrap();
        let doc = finish(&doc, &["Lorem", "ut labore"]);
        let objs = pagebara_edit::parse(&doc, 0).unwrap();
        let b = block(&objs, "Lorem");
        let bt = pagebara_edit::block_text(&objs, b).unwrap();
        assert_eq!(bt.frame.align, align, "{name}: {:?}", bt.frame);
        // Edit it: alignment survives.
        let mut d = doc.clone();
        let mut e = Editor::open(&mut d, 0).unwrap();
        let t = bt.text().replace("dolor sit", "DOLOR SIT AMET");
        e.edit_block_text(b, &t, &mut f).unwrap();
        e.commit().unwrap();
        let d = finish(&d, &["DOLOR", "AMET"]);
        let objs = pagebara_edit::parse(&d, 0).unwrap();
        let bt2 = pagebara_edit::block_text(&objs, block(&objs, "DOLOR")).unwrap();
        assert_eq!(bt2.frame.align, align, "{name}: {:?}", bt2.frame);
        assert!(
            (bt2.frame.width - bt.frame.width).abs() < 20.0,
            "{name}: {:?} {:?}",
            bt.frame,
            bt2.frame
        );
        let all: Vec<_> = objs
            .objects
            .iter()
            .filter_map(|o| o.text().map(|t| t.text.clone()))
            .collect();
        assert_eq!(bt2.text(), t, "{all:?}");
    }
}

#[test]
fn missing_glyphs_come_from_fallbacks() {
    let doc = open(helvetica_doc());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let b = block(&objs, "quick brown");
    let mut d = doc.clone();
    let mut f = fonts();
    let mut e = Editor::open(&mut d, 0).unwrap();
    let text = "Grüße Ωμέγα Łódź 漢字 done.";
    e.edit_block_text(b, text, &mut f).unwrap();
    e.commit().unwrap();
    let d = finish(&d, &["Grüße", "Ωμέγα", "Łódź", "done."]);
    let t = page_text(&d, 0);
    assert!(t.contains(text), "{t}");
}

#[test]
fn bold_selection() {
    let doc = open(helvetica_doc());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let b = block(&objs, "quick brown");
    let bt = pagebara_edit::block_text(&objs, b).unwrap();
    let mut f = fonts();
    let text = bt.text();
    let at = text.find("brown").unwrap();
    let spans = pagebara_edit::restyle_range(
        &doc,
        &mut f,
        &bt.spans,
        at..at + 5,
        &StyleChange::Bold(true),
    )
    .unwrap();
    assert_eq!(spans.len(), 3);
    let mut d = doc.clone();
    let mut e = Editor::open(&mut d, 0).unwrap();
    e.edit_block(b, &spans, &mut f).unwrap();
    e.commit().unwrap();
    let d = finish(&d, &["quick brown fox"]);
    let objs = pagebara_edit::parse(&d, 0).unwrap();
    let r = objs
        .runs
        .iter()
        .find(|r| r.text == "brown")
        .expect("bold run");
    let style = |r: &pagebara_edit::TextRun| {
        let o = d.resolve(&r.font.as_ref().unwrap().object);
        pagebara_typeset::font_style(&d, o.as_dict().unwrap())
    };
    assert!(style(r).is_bold(), "{:?}", style(r));
    let other = objs
        .runs
        .iter()
        .find(|r| r.text.starts_with("The quick"))
        .unwrap();
    assert!(!style(other).is_bold());
}
