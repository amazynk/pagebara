//! Reading order on synthetic pages: columns, paragraphs, rotation, right-to-left and
//! vertical writing.

mod common;

use common::*;
use pagebara_content::{TextPage, extract_text};

/// A simple font, every glyph 500 units wide; codes 1–4 are Hebrew letters alef to dalet.
fn font() -> String {
    let widths = vec!["500"; 255].join(" ");
    format!(
        "<</Type/Font/Subtype/Type1/BaseFont/Test/FirstChar 1/LastChar 255/Widths[{widths}]\
          /Encoding<</BaseEncoding/WinAnsiEncoding\
          /Differences[1/alefhebrew/bethebrew/gimelhebrew/dalethebrew]>>\
          /FontDescriptor<</Flags 32/Ascent 750/Descent -250/FontFile 9 0 R>>>>"
    )
}

fn extract(content: &str) -> TextPage {
    let (doc, l) = interpret(content, "<</Font<</F1 5 0 R>>>>", &[font()]);
    extract_text(&doc, &l)
}

/// `BT … ET` showing each `(x, y, text)` at size 10.
fn show(items: &[(f32, f32, &str)]) -> String {
    items
        .iter()
        .map(|(x, y, t)| format!("BT /F1 10 Tf 1 0 0 1 {x} {y} Tm ({t}) Tj ET\n"))
        .collect()
}

#[test]
fn two_columns_under_a_heading() {
    // Drawn row by row, as many generators do; baselines align across the gutter.
    let l = |s: &str| format!("{s} is a line of column text, words.");
    let (l1, l2, l3, r1, r2, r3) = (
        l("Left one"),
        l("Left two"),
        l("Left three"),
        l("Right one"),
        l("Right two"),
        l("Right three"),
    );
    let footer = "Footer across both columns of the page, long enough";
    let page = extract(&show(&[
        (220.0, 340.0, "Heading"),
        (20.0, 300.0, &l1),
        (260.0, 300.0, &r1),
        (20.0, 288.0, &l2),
        (260.0, 288.0, &r2),
        (20.0, 276.0, &l3),
        (260.0, 276.0, &r3),
        (20.0, 240.0, footer),
    ]));
    assert_eq!(
        page.text,
        format!("Heading\n\n{l1}\n{l2}\n{l3}\n\n{r1}\n{r2}\n{r3}\n\n{footer}")
    );
    assert_eq!(page.blocks.len(), 4);
    assert_eq!(page.lines.len(), 8);
    let b = &page.blocks[1];
    assert_eq!(b.lines, (1, 4));
    assert!(b.bbox.x0 == 20.0 && b.bbox.x1 < 260.0);
}

#[test]
fn narrow_aligned_columns_are_table_rows() {
    // A table of contents: section numbers beside titles read row by row.
    let page = extract(&show(&[
        (20.0, 300.0, "2.4"),
        (80.0, 300.0, "Contents"),
        (20.0, 288.0, "2.4.1"),
        (80.0, 288.0, "projectGetProperty"),
        (20.0, 276.0, "2.4.2"),
        (80.0, 276.0, "projectSetProperty"),
    ]));
    assert_eq!(
        page.text,
        "2.4 Contents\n2.4.1 projectGetProperty\n2.4.2 projectSetProperty"
    );
}

#[test]
fn paragraphs_and_words() {
    let page = extract(&show(&[
        (35.0, 300.0, "First paragraph"),
        (20.0, 288.0, "goes on here."),
        (35.0, 276.0, "Second one"),
        (20.0, 264.0, "ends."),
    ]));
    assert_eq!(
        page.text,
        "First paragraph\ngoes on here.\n\nSecond one\nends."
    );
    assert_eq!(page.blocks.len(), 2);
    let words: Vec<&str> = page.words.iter().map(|w| page.word_text(w)).collect();
    assert_eq!(
        words,
        [
            "First",
            "paragraph",
            "goes",
            "on",
            "here.",
            "Second",
            "one",
            "ends."
        ]
    );
    let l = &page.lines[0];
    assert_eq!(l.words, (0, 2));
    assert_eq!(page.line_text(l), "First paragraph");
    // Word boxes cover their letters only.
    let w = &page.words[1];
    assert_eq!((w.bbox.x0, w.bbox.x1), (35.0 + 30.0, 35.0 + 75.0));
}

/// Hanging paragraphs (first line out, the rest indented) are not split at their second
/// line; the next line out starts the next one.
#[test]
fn hanging_paragraphs() {
    let page = extract(&show(&[
        (20.0, 300.0, "AC Alignment check flag text"),
        (50.0, 288.0, "continues the item here"),
        (50.0, 276.0, "and ends."),
        (20.0, 264.0, "VM Next item starts here, ok"),
        (50.0, 252.0, "and goes on."),
    ]));
    assert_eq!(
        page.text,
        "AC Alignment check flag text\ncontinues the item here\nand ends.\n\n\
         VM Next item starts here, ok\nand goes on."
    );
    assert_eq!(page.blocks.len(), 2);
    // Two lines, the second indented after a full first line: one paragraph.
    let page = extract(&show(&[
        (20.0, 300.0, "Syntax with full tuple type, see"),
        (50.0, 288.0, "Conditions."),
    ]));
    assert_eq!(page.blocks.len(), 1, "{:?}", page.text);
}

#[test]
fn gaps_become_spaces_and_rows_stay_together() {
    let page = extract(&show(&[
        (20.0, 300.0, "Name"),
        (120.0, 300.0, "Value"),
        (20.0, 250.0, "Tight"),
        (48.0, 250.0, "gap"),
    ]));
    assert_eq!(page.text, "Name Value\n\nTight gap");
    let gaps: Vec<(f32, f32)> = page
        .chars
        .iter()
        .filter(|c| c.glyph == pagebara_content::display::NONE)
        .map(|c| (c.bbox().x0, c.bbox().x1))
        .collect();
    assert_eq!(gaps, [(40.0, 120.0), (45.0, 48.0)]);
}

#[test]
fn rotated_text() {
    // Lines rotated 90° read bottom to top; the next line lies to the right.
    let content = "BT /F1 10 Tf 0 1 -1 0 88 20 Tm (Up one) Tj ET \
                   BT /F1 10 Tf 0 1 -1 0 100 20 Tm (Up two) Tj ET \
                   BT /F1 10 Tf 1 0 0 1 120 150 Tm (Flat) Tj ET";
    let page = extract(content);
    assert_eq!(page.text, "Up one\nUp two\n\nFlat");
    // Upside down text.
    let content = "BT /F1 10 Tf -1 0 0 -1 150 100 Tm (Turned) Tj ET \
                   BT /F1 10 Tf -1 0 0 -1 150 112 Tm (around) Tj ET";
    assert_eq!(extract(content).text, "Turned\naround");
}

#[test]
fn right_to_left_lines() {
    // Glyphs in visual order: gimel bet alef reads alef bet gimel.
    let page = extract(&show(&[(20.0, 300.0, "\\003\\002\\001")]));
    assert_eq!(page.text, "אבג");
    assert!(page.lines[0].rtl);
    // Numbers and Latin keep their own order inside a right-to-left line.
    let page = extract(&show(&[(20.0, 300.0, "123 PDF \\004\\003\\002\\001")]));
    assert_eq!(page.text, "אבגד PDF 123");
    // The first glyph of the text is the rightmost one.
    let first = &page.chars[0];
    assert_eq!(page.char_text(first), "א");
    assert_eq!(first.bbox().x0, 20.0 + 11.0 * 5.0);
}

#[test]
fn right_to_left_columns() {
    // Columns of 30 letters; the right one is read first.
    let (ab, ba, gd, dg) = ("\\002\\001", "\\001\\002", "\\004\\003", "\\003\\004");
    let page = extract(&show(&[
        (20.0, 300.0, &ab.repeat(15)),
        (200.0, 300.0, &gd.repeat(15)),
        (20.0, 288.0, &ba.repeat(15)),
        (200.0, 288.0, &dg.repeat(15)),
    ]));
    let (a, b) = ("אב".repeat(15), "בא".repeat(15));
    let (c, d) = ("גד".repeat(15), "דג".repeat(15));
    assert_eq!(page.text, format!("{c}\n{d}\n\n{a}\n{b}"));
}

#[test]
fn vertical_writing() {
    const TO_UNICODE: &str = "/CIDInit /ProcSet findresource begin 12 dict begin begincmap
1 begincodespacerange <0000> <FFFF> endcodespacerange
4 beginbfchar <0001> <65E5> <0002> <672C> <0003> <8A9E> <0004> <6587> endbfchar
endcmap CMapName currentdict /CMap defineresource pop end end";
    let font = "<</Type/Font/Subtype/Type0/BaseFont/V/Encoding/Identity-V/ToUnicode 6 0 R\
        /DescendantFonts[<</Subtype/CIDFontType0/BaseFont/V/DW 1000\
        /FontDescriptor<</Ascent 880/Descent -120/Flags 4>>>>]>>";
    // Two columns, right to left, each read top to bottom.
    let content = "BT /F0 10 Tf 1 0 0 1 100 180 Tm <00010002> Tj ET \
                   BT /F0 10 Tf 1 0 0 1 86 180 Tm <00030004> Tj ET";
    let (doc, l) = interpret(
        content,
        "<</Font<</F0 5 0 R>>>>",
        &[font.into(), stream("", TO_UNICODE)],
    );
    let page = extract_text(&doc, &l);
    assert_eq!(page.text, "日本\n語文");
    assert!(page.lines.iter().all(|l| l.vertical));
    // The first column's glyphs run down the page.
    let (a, b) = (page.chars[0].bbox(), page.chars[1].bbox());
    assert!(a.y0 > b.y0 && a.x0 == b.x0);
}
